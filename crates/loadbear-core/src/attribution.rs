//! Naming what is causing the overload.
//!
//! This is the product. "You are overloaded" is something the user already
//! knows by the time they look at anything. "Docker is holding 11 GB while your
//! build pages" is the version worth having.
//!
//! # The correctness bar
//!
//! A confident wrong attribution is worse than none. Naming the wrong process
//! destroys trust in everything else LoadBear says, and it is the kind of error
//! a user notices immediately and never forgets. So every rule in this module
//! is written to produce `None` when the evidence is thin, and the interruption
//! contract already treats a finding with no cause as unable to notify. Silence
//! is a supported outcome here rather than a failure.
//!
//! Three things can withhold a cause, and each of them is a real state rather
//! than a defect:
//!
//! 1. **Nothing dominates.** Eight compiler processes sharing a machine evenly
//!    have no single culprit. Grouping by name catches the common form of this,
//!    where the eight are all `rustc`, but a genuinely mixed workload is simply
//!    not attributable and is reported as such.
//! 2. **Too little is visible.** Protected and system processes cannot be
//!    opened by an unprivileged interface. When the processes LoadBear can see
//!    do not account for the activity it can measure, the real cause may be one
//!    of the ones it cannot see, so it does not name a runner-up as if it were
//!    the leader.
//! 3. **Nothing is large enough.** A process using three percent of the machine
//!    is not the reason the machine is struggling, whatever else is true.

use crate::contract::{Cause, CauseKind, Finding, Remediation};
use crate::tier::STALL_BRACED;
use crate::types::{ContainerReading, ProcessReading, Reading, Resource};
use crate::verdict::{Verdict, VerdictKind};

/// Share of the machine a process group must hold before it can be named the
/// cause of CPU pressure.
///
/// Below this the group is a participant rather than a cause, and naming it
/// would be pointing at whatever happened to sort first.
const MIN_CPU_SHARE_PCT: f32 = 20.0;

/// Share of all observed memory a process group must hold before it can be
/// named the cause of memory pressure.
///
/// Expressed against what was observed rather than against installed RAM,
/// because the question attribution answers is which of the things running is
/// responsible, not how full the machine is.
const MIN_MEMORY_SHARE: f32 = 0.30;

/// How far ahead of the runner-up a leader must be to be called the cause.
///
/// Two processes within this factor of each other are a workload, not a
/// culprit. This is the rule that keeps LoadBear quiet during a parallel build
/// rather than picking one of eight equal compilers and naming it.
const DOMINANCE: f32 = 1.5;

/// How much of the machine's measured CPU activity the visible processes must
/// account for before any CPU attribution is trusted.
///
/// An unprivileged process cannot open every process on Windows. When the
/// visible ones account for far less than the machine is actually doing, the
/// cause is likely something invisible, and the largest visible process is not
/// a substitute for it.
const MIN_COVERAGE: f32 = 0.60;

/// Process names that mean "this is Docker" rather than a normal application.
///
/// On Windows every container presents as one of these, which is exactly why
/// container attribution needs a second source.
const DOCKER_PROCESSES: [&str; 5] = [
    "vmmem",
    "vmmemwsl",
    "vmmemdocker",
    "docker desktop",
    "com.docker.backend",
];

/// System processes whose remediation is something other than ending them.
///
/// Each entry is a fact about what the process is, not a judgement about it.
/// Ending Defender is not the remedy for Defender scanning a build directory;
/// excluding the directory is.
const SYSTEM_PROCESSES: [(&str, Remediation); 6] = [
    ("msmpeng", Remediation::AddExclusion),
    ("antimalware service executable", Remediation::AddExclusion),
    ("searchindexer", Remediation::Defer),
    ("searchprotocolhost", Remediation::Defer),
    ("tiworker", Remediation::Defer),
    ("trustedinstaller", Remediation::Defer),
];

/// Several processes sharing a name, treated as one contributor.
///
/// A build is twelve `rustc` processes and a browser is thirty `chrome`
/// processes. Ranked individually neither ever dominates, and LoadBear would
/// stay silent through the two most common causes of overload on a developer
/// machine. Ranked as groups, both are nameable.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessGroup {
    pub name: String,
    /// The friendly name, when any member of the group carried one.
    ///
    /// Display only. Grouping and matching stay on `name`, so what LoadBear
    /// decides cannot change because a vendor edited a version resource.
    pub display: Option<String>,
    /// The largest single member, which is what a user would go looking for.
    pub representative_pid: u32,
    pub count: usize,
    pub cpu_percent: f32,
    pub working_set_bytes: u64,
}

impl ProcessGroup {
    /// How the group should be described to a person.
    ///
    /// Prefers the friendly name, so a user reads "Realtek HD Audio Universal
    /// Service" rather than `RtkAudUService64` and can tell whether they care.
    pub fn label(&self) -> String {
        let name = self.display.as_deref().unwrap_or(&self.name);
        if self.count > 1 {
            format!("{name} ({} processes)", self.count)
        } else {
            name.to_string()
        }
    }
}

/// Collapse a process list into groups sharing a name, largest member first.
pub fn group_by_name(processes: &[ProcessReading]) -> Vec<ProcessGroup> {
    let mut groups: Vec<ProcessGroup> = Vec::new();

    for p in processes {
        let key = p.name.to_lowercase();
        match groups.iter_mut().find(|g| g.name.to_lowercase() == key) {
            Some(g) => {
                g.count += 1;
                if g.display.is_none() {
                    g.display.clone_from(&p.display_name);
                }
                g.cpu_percent += p.cpu_percent;
                g.working_set_bytes = g.working_set_bytes.saturating_add(p.working_set_bytes);
                // The representative is the biggest single member, so that
                // following the pid leads somewhere recognisable.
                if p.working_set_bytes > g.working_set_bytes / g.count as u64 {
                    g.representative_pid = p.pid;
                }
            }
            None => groups.push(ProcessGroup {
                name: p.name.clone(),
                display: p.display_name.clone(),
                representative_pid: p.pid,
                count: 1,
                cpu_percent: p.cpu_percent,
                working_set_bytes: p.working_set_bytes,
            }),
        }
    }

    groups
}

/// Whether the visible processes account for enough of the machine's measured
/// CPU activity for attribution to mean anything.
///
/// Returns `None` when there is no machine figure to compare against, which is
/// a different state from poor coverage and is treated as such by the caller.
pub fn cpu_coverage(
    processes: &[ProcessReading],
    machine_utilization_pct: Option<f32>,
) -> Option<f32> {
    let machine = machine_utilization_pct?;
    if machine <= 0.0 {
        return Some(1.0);
    }
    let observed: f32 = processes.iter().map(|p| p.cpu_percent).sum();
    Some((observed / machine).clamp(0.0, 1.0))
}

/// The group responsible for CPU pressure, if one can honestly be named.
fn cpu_leader(reading: &Reading) -> Option<ProcessGroup> {
    match cpu_coverage(&reading.processes, reading.cpu.utilization_pct) {
        // Most of what the machine is doing is invisible to us. The largest
        // thing we can see is not evidence about the largest thing there is.
        Some(c) if c < MIN_COVERAGE => return None,
        // No utilization figure means no way to tell whether the list is
        // complete, and an incomplete list is what produces a wrong name.
        None => return None,
        Some(_) => {}
    }

    let mut groups = group_by_name(&reading.processes);
    groups.sort_by(|a, b| b.cpu_percent.total_cmp(&a.cpu_percent));

    let leader = groups.first()?.clone();
    if leader.cpu_percent < MIN_CPU_SHARE_PCT {
        return None;
    }
    if let Some(second) = groups.get(1) {
        if second.cpu_percent > 0.0 && leader.cpu_percent < second.cpu_percent * DOMINANCE {
            return None;
        }
    }
    Some(leader)
}

/// The group responsible for memory pressure, if one can honestly be named.
fn memory_leader(reading: &Reading) -> Option<ProcessGroup> {
    let mut groups = group_by_name(&reading.processes);
    groups.sort_by_key(|g| std::cmp::Reverse(g.working_set_bytes));

    let total: u64 = groups.iter().map(|g| g.working_set_bytes).sum();
    if total == 0 {
        return None;
    }

    let leader = groups.first()?.clone();
    if (leader.working_set_bytes as f32 / total as f32) < MIN_MEMORY_SHARE {
        return None;
    }
    if let Some(second) = groups.get(1) {
        if second.working_set_bytes > 0
            && (leader.working_set_bytes as f32) < second.working_set_bytes as f32 * DOMINANCE
        {
            return None;
        }
    }
    Some(leader)
}

/// The group responsible for I/O pressure, if one can honestly be named.
///
/// Depends on a per-process hard fault rate, which Windows does not expose
/// through any documented interface. Where a backend cannot supply it the
/// figure is absent rather than approximated from total page faults, most of
/// which are soft and have nothing to do with stalling on a disk.
fn io_leader(reading: &Reading) -> Option<ProcessGroup> {
    let reported: Vec<&ProcessReading> = reading
        .processes
        .iter()
        .filter(|p| p.hard_faults_per_sec.is_some())
        .collect();
    if reported.is_empty() {
        return None;
    }

    let mut ranked: Vec<(&ProcessReading, f32)> = reported
        .iter()
        .map(|p| (*p, p.hard_faults_per_sec.unwrap_or(0.0)))
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));

    let &(leader, rate) = ranked.first()?;
    if rate <= 0.0 {
        return None;
    }
    if let Some(&(_, second)) = ranked.get(1) {
        if second > 0.0 && rate < second * DOMINANCE {
            return None;
        }
    }

    Some(ProcessGroup {
        name: leader.name.clone(),
        display: leader.display_name.clone(),
        representative_pid: leader.pid,
        count: 1,
        cpu_percent: leader.cpu_percent,
        working_set_bytes: leader.working_set_bytes,
    })
}

fn is_docker(name: &str) -> bool {
    let n = name.to_lowercase();
    DOCKER_PROCESSES.iter().any(|d| n.starts_with(d))
}

fn system_remediation(name: &str) -> Option<Remediation> {
    let n = name.to_lowercase();
    SYSTEM_PROCESSES
        .iter()
        .find(|(p, _)| n.starts_with(p))
        .map(|(_, r)| *r)
}

/// Resolve Docker down to the container actually responsible.
///
/// Only called once the process layer has already established that Docker is
/// the cause. The container list is a second source and may be empty, stale or
/// unavailable, in which case Docker itself remains the answer and the
/// remediation becomes reconfiguring its allocation rather than stopping a
/// container that was never identified.
fn resolve_container(
    containers: &[ContainerReading],
    resource: Resource,
) -> Option<&ContainerReading> {
    if containers.is_empty() {
        return None;
    }

    let mut ranked: Vec<&ContainerReading> = containers.iter().collect();
    match resource {
        Resource::Memory | Resource::Io => {
            ranked.sort_by_key(|c| std::cmp::Reverse(c.memory_bytes))
        }
        Resource::Cpu => ranked.sort_by(|a, b| b.cpu_percent.total_cmp(&a.cpu_percent)),
    }

    let leader = ranked.first()?;
    if let Some(second) = ranked.get(1) {
        let (l, s) = match resource {
            Resource::Memory | Resource::Io => {
                (leader.memory_bytes as f32, second.memory_bytes as f32)
            }
            Resource::Cpu => (leader.cpu_percent, second.cpu_percent),
        };
        if s > 0.0 && l < s * DOMINANCE {
            return None;
        }
    }
    Some(leader)
}

/// A container at its own limit needs the limit changed. One merely using a lot
/// needs stopping. Guessing between them produces advice that does nothing.
fn container_remediation(c: &ContainerReading) -> Remediation {
    match c.memory_limit_bytes {
        Some(limit) if limit > 0 && c.memory_bytes as f32 >= limit as f32 * 0.95 => {
            Remediation::ReconfigureLimit
        }
        _ => Remediation::Stop,
    }
}

/// Name what is responsible for pressure on a given resource.
///
/// Returns the cause and the action that follows from it, or `None` when the
/// evidence does not support naming anything.
pub fn attribute(reading: &Reading, resource: Resource) -> Option<(Cause, Remediation)> {
    // I/O deliberately has no fallback. Substituting a quantity that is easy to
    // measure for one that is not is the mistake that produced a diagnosis of
    // "idle" on a machine at 97 percent, and it is not worth repeating for the
    // sake of always having an answer.
    let leader = match resource {
        Resource::Cpu => cpu_leader(reading),
        Resource::Memory => memory_leader(reading),
        Resource::Io => io_leader(reading),
    }?;

    if is_docker(&leader.name) {
        if let Some(c) = resolve_container(&reading.containers, resource) {
            return Some((
                Cause {
                    label: c.name.clone(),
                    kind: CauseKind::Container { id: c.id.clone() },
                },
                container_remediation(c),
            ));
        }
        // Docker is responsible but which container is unknown, so the honest
        // action is the one that applies to Docker as a whole.
        return Some((
            Cause {
                label: "Docker".to_string(),
                kind: CauseKind::Process {
                    pid: leader.representative_pid,
                },
            },
            Remediation::ReconfigureLimit,
        ));
    }

    if let Some(remediation) = system_remediation(&leader.name) {
        return Some((
            Cause {
                label: leader.label(),
                kind: CauseKind::SystemService,
            },
            remediation,
        ));
    }

    Some((
        Cause {
            label: leader.label(),
            kind: CauseKind::Process {
                pid: leader.representative_pid,
            },
        },
        Remediation::Stop,
    ))
}

/// Which resource a verdict should be attributed against.
///
/// The worst stalled resource wins when anything is meaningfully stalled,
/// because that is what the machine is actually struggling on. A clock verdict
/// with no stall behind it still attributes against CPU: it only fires under
/// heavy load, so something is demanding the processor by definition.
fn resource_for(reading: &Reading, verdict: &Verdict) -> Option<Resource> {
    let (worst, level) = reading.stall.worst();
    if level >= STALL_BRACED {
        return Some(worst);
    }
    match verdict.kind {
        VerdictKind::BelowBaseClock
        | VerdictKind::Throttling
        | VerdictKind::PowerOutsideBand
        | VerdictKind::ThermalHeadroomLow => Some(Resource::Cpu),
        // Nothing running is responsible for the platform supplying too little
        // power, so there is no process to rank and no resource to rank by.
        VerdictKind::PowerBelowRating => None,
    }
}

/// Turn verdicts into findings, attaching a cause wherever one can be named.
///
/// A finding always exists for every verdict. What varies is whether it carries
/// a cause, and the interruption contract reads that difference: a finding
/// without one reports the state and cannot interrupt.
pub fn diagnose(reading: &Reading, verdicts: Vec<Verdict>) -> Vec<Finding> {
    verdicts
        .into_iter()
        .map(|verdict| {
            // A starved platform has a cause, but it is not a process. This is
            // the one case where the answer is the machine's own power supply,
            // and both the cause kind and the remedy were defined for it long
            // before anything could produce them.
            if verdict.kind == VerdictKind::PowerBelowRating {
                return Finding {
                    verdict,
                    cause: Some(Cause {
                        label: "The power being supplied to this machine".to_string(),
                        kind: CauseKind::PowerState,
                    }),
                    remediation: Some(Remediation::ChangePowerState),
                };
            }
            let attributed = resource_for(reading, &verdict).and_then(|r| attribute(reading, r));
            match attributed {
                Some((cause, remediation)) => Finding {
                    verdict,
                    cause: Some(cause),
                    remediation: Some(remediation),
                },
                None => Finding {
                    verdict,
                    cause: None,
                    remediation: None,
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CpuReading, StallSignal, ThrottleState};
    use crate::verdict::Severity;

    const GB: u64 = 1024 * 1024 * 1024;

    fn proc(pid: u32, name: &str, cpu: f32, ws_gb: f32) -> ProcessReading {
        ProcessReading {
            pid,
            name: name.to_string(),
            display_name: None,
            working_set_bytes: (ws_gb * GB as f32) as u64,
            hard_faults_per_sec: None,
            cpu_percent: cpu,
        }
    }

    fn reading(processes: Vec<ProcessReading>, utilization: f32, stall: StallSignal) -> Reading {
        Reading {
            timestamp_ms: 0,
            stall,
            cpu: CpuReading {
                all_core_mhz: Some(1400),
                reported_base_mhz: None,
                utilization_pct: Some(utilization),
                package_watts: None,
                package_temp_c: None,
                tjmax_c: None,
                throttle: ThrottleState {
                    asserted: false,
                    reason: None,
                },
            },
            processes,
            containers: vec![],
        }
    }

    fn quiet() -> StallSignal {
        StallSignal {
            cpu: 0.0,
            memory: 0.0,
            io: Some(0.0),
        }
    }

    fn memory_stalled() -> StallSignal {
        StallSignal {
            cpu: 0.10,
            memory: 0.90,
            io: Some(0.20),
        }
    }

    fn verdict(kind: VerdictKind) -> Verdict {
        Verdict {
            kind,
            severity: Severity::OutOfSpec,
            detail: "test".to_string(),
            basis: "test".to_string(),
        }
    }

    #[test]
    fn a_group_is_labelled_with_the_name_a_person_would_use() {
        let mut ps = vec![proc(100, "RtkAudUService64", 5.0, 0.1)];
        ps[0].display_name = Some("Realtek HD Audio Universal Service".to_string());
        let groups = group_by_name(&ps);
        assert_eq!(groups[0].label(), "Realtek HD Audio Universal Service");
    }

    #[test]
    fn a_group_falls_back_to_the_executable_when_there_is_no_friendly_name() {
        let groups = group_by_name(&[proc(100, "vmmem", 5.0, 11.0)]);
        assert_eq!(groups[0].label(), "vmmem");
    }

    #[test]
    fn a_friendly_name_survives_grouping_and_carries_the_count() {
        let mut ps: Vec<ProcessReading> =
            (0..4).map(|i| proc(100 + i, "chrome", 5.0, 0.4)).collect();
        // Only one member happens to be readable, which is enough to name all.
        ps[2].display_name = Some("Google Chrome".to_string());
        let groups = group_by_name(&ps);
        assert_eq!(groups[0].label(), "Google Chrome (4 processes)");
    }

    #[test]
    fn matching_stays_on_the_executable_name_not_the_friendly_one() {
        // Docker resolution keys off the executable, so a vendor changing their
        // version resource must not be able to break a diagnosis.
        let mut r = reading(vec![proc(100, "vmmem", 5.0, 11.0)], 30.0, memory_stalled());
        r.processes[0].display_name = Some("Some Rebranded Thing".to_string());
        let (cause, _) = attribute(&r, Resource::Memory).expect("must still resolve as Docker");
        assert_eq!(cause.label, "Docker");
    }

    #[test]
    fn a_dominant_process_is_named() {
        let r = reading(
            vec![proc(100, "ollama", 70.0, 2.0), proc(200, "code", 5.0, 1.0)],
            80.0,
            quiet(),
        );
        let (cause, remediation) = attribute(&r, Resource::Cpu).expect("must name the cause");
        assert_eq!(cause.label, "ollama");
        assert_eq!(cause.kind, CauseKind::Process { pid: 100 });
        assert_eq!(remediation, Remediation::Stop);
    }

    #[test]
    fn processes_sharing_a_name_are_ranked_as_one_contributor() {
        // A build is many compilers. Individually none dominates, so without
        // grouping LoadBear stays silent through the single most common cause
        // of overload on a developer machine.
        let mut ps: Vec<ProcessReading> =
            (0..8).map(|i| proc(100 + i, "rustc", 9.0, 0.4)).collect();
        ps.push(proc(900, "code", 8.0, 1.0));
        let r = reading(ps, 80.0, quiet());

        let (cause, _) = attribute(&r, Resource::Cpu).expect("the group must be nameable");
        assert_eq!(cause.label, "rustc (8 processes)");
    }

    #[test]
    fn an_evenly_shared_machine_has_no_single_cause() {
        let r = reading(
            vec![
                proc(100, "rustc", 30.0, 1.0),
                proc(200, "chrome", 28.0, 1.0),
                proc(300, "docker-proxy", 25.0, 1.0),
            ],
            85.0,
            quiet(),
        );
        assert!(
            attribute(&r, Resource::Cpu).is_none(),
            "three processes within a hair of each other are a workload, not a culprit"
        );
    }

    #[test]
    fn a_small_process_is_never_named_however_it_sorts() {
        let r = reading(
            vec![proc(100, "code", 4.0, 0.5), proc(200, "explorer", 1.0, 0.2)],
            90.0,
            quiet(),
        );
        assert!(
            attribute(&r, Resource::Cpu).is_none(),
            "a process using four percent is not why the machine is at ninety"
        );
    }

    #[test]
    fn poor_coverage_withholds_a_cause_rather_than_naming_the_biggest_visible_process() {
        // The machine is at 90 percent and the processes we can open account
        // for 25 of it. Whatever is doing the other 65 is invisible, and it,
        // not the largest thing we happen to see, is the likely cause.
        let r = reading(
            vec![
                proc(100, "code", 25.0, 1.0),
                proc(200, "explorer", 0.5, 0.2),
            ],
            90.0,
            quiet(),
        );
        assert!(
            attribute(&r, Resource::Cpu).is_none(),
            "most of the machine's work is unaccounted for, so nothing may be named"
        );
    }

    #[test]
    fn without_a_utilization_figure_cpu_attribution_is_withheld() {
        let mut r = reading(vec![proc(100, "ollama", 70.0, 2.0)], 80.0, quiet());
        r.cpu.utilization_pct = None;
        assert!(
            attribute(&r, Resource::Cpu).is_none(),
            "with nothing to compare against there is no way to know the list is complete"
        );
    }

    #[test]
    fn memory_attribution_names_whoever_holds_the_memory() {
        let r = reading(
            vec![proc(100, "vmmem", 5.0, 11.0), proc(200, "code", 10.0, 1.0)],
            30.0,
            memory_stalled(),
        );
        let (cause, remediation) = attribute(&r, Resource::Memory).expect("must name the holder");
        assert_eq!(cause.label, "Docker");
        assert_eq!(
            remediation,
            Remediation::ReconfigureLimit,
            "with no container detail the action that applies is Docker's own allocation"
        );
    }

    #[test]
    fn docker_resolves_to_the_container_when_the_second_source_is_available() {
        let mut r = reading(
            vec![proc(100, "vmmem", 5.0, 11.0), proc(200, "code", 10.0, 1.0)],
            30.0,
            memory_stalled(),
        );
        r.containers = vec![
            ContainerReading {
                id: "abc123".to_string(),
                name: "postgres".to_string(),
                cpu_percent: 2.0,
                memory_bytes: 9 * GB,
                memory_limit_bytes: None,
            },
            ContainerReading {
                id: "def456".to_string(),
                name: "redis".to_string(),
                cpu_percent: 1.0,
                memory_bytes: GB / 2,
                memory_limit_bytes: None,
            },
        ];

        let (cause, remediation) = attribute(&r, Resource::Memory).expect("must name a container");
        assert_eq!(cause.label, "postgres");
        assert_eq!(
            cause.kind,
            CauseKind::Container {
                id: "abc123".to_string()
            }
        );
        assert_eq!(remediation, Remediation::Stop);
    }

    #[test]
    fn a_container_against_its_own_limit_is_told_to_reconfigure_not_to_stop() {
        let mut r = reading(vec![proc(100, "vmmem", 5.0, 11.0)], 30.0, memory_stalled());
        r.containers = vec![ContainerReading {
            id: "abc123".to_string(),
            name: "postgres".to_string(),
            cpu_percent: 2.0,
            memory_bytes: 4 * GB,
            memory_limit_bytes: Some(4 * GB),
        }];
        let (_, remediation) = attribute(&r, Resource::Memory).expect("must name a container");
        assert_eq!(remediation, Remediation::ReconfigureLimit);
    }

    #[test]
    fn two_comparable_containers_leave_docker_named_rather_than_a_guess() {
        let mut r = reading(vec![proc(100, "vmmem", 5.0, 11.0)], 30.0, memory_stalled());
        r.containers = vec![
            ContainerReading {
                id: "abc".to_string(),
                name: "postgres".to_string(),
                cpu_percent: 2.0,
                memory_bytes: 5 * GB,
                memory_limit_bytes: None,
            },
            ContainerReading {
                id: "def".to_string(),
                name: "elasticsearch".to_string(),
                cpu_percent: 2.0,
                memory_bytes: 5 * GB,
                memory_limit_bytes: None,
            },
        ];
        let (cause, _) = attribute(&r, Resource::Memory).expect("docker itself remains nameable");
        assert_eq!(
            cause.label, "Docker",
            "picking either container over the other would be a coin toss presented as a diagnosis"
        );
    }

    #[test]
    fn defender_is_told_to_add_an_exclusion_rather_than_to_be_stopped() {
        let r = reading(
            vec![
                proc(100, "MsMpEng", 60.0, 1.0),
                proc(200, "code", 10.0, 1.0),
            ],
            75.0,
            quiet(),
        );
        let (cause, remediation) = attribute(&r, Resource::Cpu).expect("must name Defender");
        assert_eq!(cause.kind, CauseKind::SystemService);
        assert_eq!(
            remediation,
            Remediation::AddExclusion,
            "ending Defender is not the remedy for Defender scanning a build directory"
        );
    }

    #[test]
    fn the_indexer_is_deferred_rather_than_stopped() {
        let r = reading(
            vec![
                proc(100, "SearchIndexer", 55.0, 1.0),
                proc(200, "code", 5.0, 1.0),
            ],
            70.0,
            quiet(),
        );
        let (_, remediation) = attribute(&r, Resource::Cpu).expect("must name the indexer");
        assert_eq!(remediation, Remediation::Defer);
    }

    #[test]
    fn io_attribution_stays_silent_when_no_backend_reports_hard_faults() {
        let mut r = reading(
            vec![proc(100, "code", 5.0, 8.0), proc(200, "chrome", 3.0, 0.2)],
            20.0,
            quiet(),
        );
        r.stall.io = Some(0.9);
        assert!(
            r.processes.iter().all(|p| p.hard_faults_per_sec.is_none()),
            "the premise of the test is that the backend cannot supply the figure"
        );
        assert!(
            attribute(&r, Resource::Io).is_none(),
            "the memory holder is not a stand-in for the process stalling on disk"
        );
    }

    #[test]
    fn io_attribution_names_a_dominant_faulter_when_the_figure_exists() {
        let mut r = reading(
            vec![proc(100, "code", 5.0, 1.0), proc(200, "chrome", 3.0, 1.0)],
            20.0,
            quiet(),
        );
        r.processes[0].hard_faults_per_sec = Some(900.0);
        r.processes[1].hard_faults_per_sec = Some(20.0);
        let (cause, _) = attribute(&r, Resource::Io).expect("a reported figure is attributable");
        assert_eq!(cause.label, "code");
    }

    #[test]
    fn a_starved_platform_blames_the_power_supply_rather_than_a_process() {
        // Every other finding names something running. This one must not: no
        // process is responsible for the machine being supplied too little
        // power, and killing the heaviest one would not help.
        let r = reading(vec![proc(100, "rustc", 70.0, 1.0)], 100.0, quiet());
        let findings = diagnose(&r, vec![verdict(VerdictKind::PowerBelowRating)]);
        let f = &findings[0];
        assert_eq!(f.cause.as_ref().unwrap().kind, CauseKind::PowerState);
        assert_eq!(f.remediation, Some(Remediation::ChangePowerState));
        assert!(
            f.is_actionable(),
            "a starved platform is one of the few findings with a fix the user can act on directly"
        );
    }

    #[test]
    fn a_diagnosed_finding_carries_both_a_cause_and_an_action() {
        let r = reading(
            vec![proc(100, "ollama", 70.0, 2.0), proc(200, "code", 5.0, 1.0)],
            85.0,
            quiet(),
        );
        let findings = diagnose(&r, vec![verdict(VerdictKind::BelowBaseClock)]);
        assert_eq!(findings.len(), 1);
        assert!(
            findings[0].is_actionable(),
            "a named cause with an action is what earns the right to interrupt"
        );
    }

    #[test]
    fn an_unattributable_finding_still_exists_but_cannot_interrupt() {
        let r = reading(
            vec![
                proc(100, "rustc", 30.0, 1.0),
                proc(200, "chrome", 28.0, 1.0),
                proc(300, "node", 27.0, 1.0),
            ],
            88.0,
            quiet(),
        );
        let findings = diagnose(&r, vec![verdict(VerdictKind::BelowBaseClock)]);
        assert_eq!(findings.len(), 1, "the state is still reported");
        assert!(findings[0].cause.is_none());
        assert!(
            !findings[0].is_actionable(),
            "reporting the state is not the same as earning an interruption"
        );
    }

    #[test]
    fn a_verdict_is_attributed_against_the_worst_stalled_resource() {
        // The machine is paging. The clock verdict is real, but the thing to
        // name is whoever is holding the memory, not whoever is using the CPU.
        let r = reading(
            vec![proc(100, "vmmem", 2.0, 11.0), proc(200, "rustc", 40.0, 0.5)],
            85.0,
            memory_stalled(),
        );
        let findings = diagnose(&r, vec![verdict(VerdictKind::BelowBaseClock)]);
        assert_eq!(findings[0].cause.as_ref().unwrap().label, "Docker");
    }

    #[test]
    fn an_empty_process_list_names_nothing() {
        let r = reading(vec![], 90.0, memory_stalled());
        assert!(attribute(&r, Resource::Cpu).is_none());
        assert!(attribute(&r, Resource::Memory).is_none());
        assert!(attribute(&r, Resource::Io).is_none());
    }
}
