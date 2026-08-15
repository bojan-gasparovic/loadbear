#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! LoadBear desktop application.
//!
//! A sampling thread reads the sensors and runs the diagnosis engine. The tray
//! icon reflects the tier through the bear's posture, and the window shows the
//! detail. Both read the same shared status, so they can never disagree.
//!
//! The bear artwork is a crude placeholder. It stays crude until the tier
//! transitions have been watched firing on a real machine over normal work,
//! because that is the only way to write an honest brief for an illustrator.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use loadbear_core::{
    classify, diagnose, evaluate, Assessment, ContainerReading, CpuReading, Reading, Resource,
    SpecDb, ThrottleState, Tier, TierReason, TierTracker, VerdictKind,
};
use loadbear_sensors_windows::counters::{to_stall, Counters, SampleWindow};
use loadbear_sensors_windows::cpuid::{brand_string, current_cpu_key};
use loadbear_sensors_windows::docker;
use loadbear_sensors_windows::installer;
use loadbear_sensors_windows::mapping::TemperatureReader;
use loadbear_sensors_windows::processes::ProcessSampler;
use loadbear_sensors_windows::service_control;
use loadbear_sensors_windows::shared::{now_ms, SharedTemperature};
use serde::Serialize;
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, State};

const SAMPLE_INTERVAL: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone, Serialize)]
struct VerdictView {
    kind: String,
    severity: String,
    detail: String,
    basis: String,
    /// What is responsible, when the evidence supports naming it.
    ///
    /// Absent is a real and common answer rather than a gap waiting to be
    /// filled, so the interface says so plainly instead of leaving a blank.
    cause: Option<String>,
    /// What the user can do about it, phrased as the thing they do.
    action: Option<String>,
}

/// One tick of the graph.
///
/// Kept per tick rather than reduced, because the graph exists to show the real
/// shape of the last few minutes. The tier is drawn against it so that a spike
/// which did not move the tier is visibly a spike that did not move the tier.
#[derive(Debug, Clone, Copy, Serialize)]
struct HistoryPoint {
    utilization: f32,
    cpu: f32,
    memory: f32,
    io: f32,
    tier: u8,
}

/// Roughly five minutes at the sampling interval.
const HISTORY_POINTS: usize = 200;

/// One row of the panel that explains the tier.
///
/// Containers are rows here rather than a section of their own. A standing list
/// of containers is an inventory, and it also disappears the moment Docker
/// stops, which in a fixed size window leaves a hole where a panel used to be.
/// As rows they appear underneath Docker exactly when Docker is one of the
/// things responsible, which is the only moment knowing the container helps.
#[derive(Debug, Clone, Serialize)]
struct Contributor {
    name: String,
    cpu: f32,
    memory_mb: f64,
    /// Rendered indented beneath the Docker row that produced it.
    is_container: bool,
    /// Present only for a container with a real limit, which changes the
    /// remedy from stopping it to giving it less.
    limit_mb: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
struct Status {
    tier: String,
    brand: Option<String>,
    matched: Option<String>,
    cores: u8,
    threads: u8,
    base_mhz: u32,
    tdp_watts: u32,
    mhz: Option<u32>,
    logical: u32,
    utilization: f64,
    queue: f64,
    available_mb: f64,
    hard_faults: f64,
    disk_ms: f64,
    disk_queue: f64,
    stall_cpu: f32,
    stall_memory: f32,
    stall_io: f32,
    verdicts: Vec<VerdictView>,
    /// Why the tier is what it is, in one sentence, always populated.
    ///
    /// The tier can be driven by the stall signal, which produces no verdict at
    /// all, so a red icon with an empty findings list was a reachable and
    /// completely unexplained state. This is the sentence that fixes it.
    reason: String,
    /// The last few minutes, per tick.
    history: Vec<HistoryPoint>,
    /// The heaviest processes on the resource that drove the tier.
    ///
    /// Empty while the machine is fine. A list of what is running is Task
    /// Manager and helps nobody; the same list shown because a specific
    /// resource is under pressure is an explanation.
    contributors: Vec<Contributor>,
    temp_available: bool,
    /// Whether the unavailable state is one the user can act on.
    temp_offerable: bool,
    /// Labelled per-zone readings, so the interface can lay them out rather
    /// than parse them back out of a sentence.
    temp_zones: Vec<(String, f32)>,
    temp_reason: String,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            tier: "Easy".into(),
            brand: None,
            matched: None,
            cores: 0,
            threads: 0,
            base_mhz: 0,
            tdp_watts: 0,
            mhz: None,
            logical: 1,
            utilization: 0.0,
            queue: 0.0,
            available_mb: 0.0,
            hard_faults: 0.0,
            disk_ms: 0.0,
            disk_queue: 0.0,
            stall_cpu: 0.0,
            stall_memory: 0.0,
            stall_io: 0.0,
            verdicts: vec![],
            reason: "Watching. Nothing has held long enough to judge yet.".into(),
            history: vec![],
            contributors: vec![],
            temp_available: false,
            temp_offerable: false,
            temp_zones: vec![],
            temp_reason: String::new(),
        }
    }
}

type Shared = Arc<Mutex<Status>>;

/// The last answer the container runtime gave.
///
/// Behind its own lock and refreshed by its own thread, because asking Docker
/// blocks and the loop that produces the tier must not.
type Containers = Arc<Mutex<Vec<ContainerReading>>>;

/// How often the container list is refreshed.
///
/// Slower than the sampling loop on purpose. Container memory does not move on
/// a one second timescale, and every refresh is one request per container.
const DOCKER_POLL: Duration = Duration::from_secs(10);

/// Set when temperature needs re-probing, which is after an install.
///
/// The sampling thread owns the temperature source, so the command thread
/// cannot replace it directly. It raises this instead and the loop picks it up
/// on its next tick.
static REPROBE: AtomicBool = AtomicBool::new(false);

#[tauri::command]
fn get_status(state: State<'_, Shared>) -> Status {
    state.lock().map(|s| s.clone()).unwrap_or_default()
}

/// Download, verify and run the official PawnIO installer.
///
/// Blocks until the installer exits, which is what lets the UI report a real
/// outcome rather than an optimistic one. LoadBear redistributes nothing: the
/// file comes from the official release URL and its signature is checked before
/// it is executed.
#[tauri::command]
async fn enable_temperature() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(|| {
        // Everything privileged happens inside one elevated child, so the user
        // sees a single consent prompt. Registering a service from this
        // process would fail: the interface is deliberately unprivileged.
        let helper = service_control::helper_path().map_err(|e| e.to_string())?;
        let code = installer::run_elevated_with(&helper, "--setup").map_err(|e| e.to_string())?;

        match code {
            0 => {
                REPROBE.store(true, Ordering::Relaxed);
                Ok("Temperature enabled.".to_string())
            }
            2 => Err("Setup was declined.".to_string()),
            3 => Err("The hardware driver could not be installed.".to_string()),
            4 => Err("The background helper could not be registered.".to_string()),
            _ => Err("Setup did not complete.".to_string()),
        }
    })
    .await
    .map_err(|_| "the installer could not be started".to_string())?
}

/// The remediation as a sentence naming the thing the user does.
///
/// Every variant has to end in an action. A finding that cannot be phrased this
/// way has no remediation and is not allowed to interrupt anyone, which is the
/// rule the enum exists to enforce.
fn remediation_text(r: loadbear_core::Remediation) -> String {
    use loadbear_core::Remediation::*;
    match r {
        Stop => "Stop it, if you no longer need it running.",
        ReconfigureLimit => "Lower what it is allowed to take, in Docker Desktop or .wslconfig.",
        AddExclusion => "Exclude your build directory from it.",
        Defer => "Postpone it until you have finished.",
        ChangePowerState => "Plug in, or change the power profile.",
        Physical => "Check airflow and clear any dust.",
    }
    .to_string()
}

/// Why the tier is what it is, as a sentence.
///
/// Always says something. A tier with no explanation behind it is an assertion,
/// and an assertion the user cannot check is one they stop believing.
fn reason_text(assessment: Assessment, settled: bool) -> String {
    match assessment.reason {
        TierReason::Clear if !settled => {
            "Watching. Nothing has held long enough to judge yet.".to_string()
        }
        TierReason::Clear => "Running within spec, with headroom.".to_string(),
        TierReason::Verdict(kind) => format!(
            "{} has held for at least {} seconds. See the finding below.",
            match kind {
                VerdictKind::BelowBaseClock => "A clock below the guaranteed base",
                VerdictKind::Throttling => "A hardware throttle signal",
                VerdictKind::PowerOutsideBand => "Package power outside its band",
                VerdictKind::ThermalHeadroomLow => "Low thermal headroom",
            },
            loadbear_core::tier::ESCALATE_MS / 1000
        ),
        TierReason::Stall(resource) => format!(
            "No published limit has been crossed. {} for at least {} seconds, which is measured rather than compared against a specification.",
            match resource {
                Resource::Cpu => "Work has been waiting for a processor",
                Resource::Memory => "Work has been waiting on memory, which means hard page faults",
                Resource::Io => "Work has been waiting on the disk",
            },
            loadbear_core::tier::ESCALATE_MS / 1000
        ),
    }
}

/// The heaviest processes on the resource that actually drove the tier.
///
/// Deliberately empty while the machine is fine. A list of what is running,
/// refreshing every second and reordering itself, is Task Manager and tells
/// nobody anything. The same list, shown because a named resource is under
/// pressure and ranked by that resource, is an explanation of the tier.
///
/// This is not the attribution. A cause is only *named* when the evidence
/// clears the bar in `loadbear_core::attribution`, and this list appears
/// whether or not it did, so the user can see what LoadBear was looking at when
/// it declined to name anything.
fn contributors(reading: &Reading, assessment: Assessment) -> Vec<Contributor> {
    let resource = match assessment.reason {
        TierReason::Clear => return Vec::new(),
        TierReason::Stall(r) => r,
        // A verdict is about the processor, so rank by what is demanding it.
        TierReason::Verdict(_) => Resource::Cpu,
    };

    let mut groups = loadbear_core::group_by_name(&reading.processes);
    match resource {
        Resource::Cpu => groups.sort_by(|a, b| b.cpu_percent.total_cmp(&a.cpu_percent)),
        Resource::Memory | Resource::Io => {
            groups.sort_by_key(|g| std::cmp::Reverse(g.working_set_bytes))
        }
    }

    let mut rows = Vec::new();
    for g in groups.iter().take(4) {
        let docker = is_docker_name(&g.name);
        rows.push(Contributor {
            name: if docker {
                "Docker".to_string()
            } else {
                g.label()
            },
            cpu: g.cpu_percent,
            memory_mb: g.working_set_bytes as f64 / 1_048_576.0,
            is_container: false,
            limit_mb: None,
        });

        // The one case where a second source says something the OS cannot.
        // Windows sees one `vmmem` process and can never say which container
        // is inside it, so this is the whole reason the Docker API is read.
        if docker {
            let mut containers: Vec<&ContainerReading> = reading.containers.iter().collect();
            match resource {
                Resource::Cpu => containers.sort_by(|a, b| b.cpu_percent.total_cmp(&a.cpu_percent)),
                _ => containers.sort_by_key(|c| std::cmp::Reverse(c.memory_bytes)),
            }
            rows.extend(containers.iter().take(4).map(|c| Contributor {
                name: c.name.clone(),
                cpu: c.cpu_percent,
                memory_mb: c.memory_bytes as f64 / 1_048_576.0,
                is_container: true,
                limit_mb: c.memory_limit_bytes.map(|l| l as f64 / 1_048_576.0),
            }));
        }
    }
    rows
}

/// Whether a process name means Docker rather than an ordinary application.
///
/// Kept in step with the list attribution uses, since a row that expands into
/// containers and a cause that resolves to one have to agree about what counts
/// as Docker.
fn is_docker_name(name: &str) -> bool {
    let n = name.to_lowercase();
    ["vmmem", "docker desktop", "com.docker"]
        .iter()
        .any(|d| n.starts_with(d))
}

fn tray_icon(tier: Tier) -> Option<Image<'static>> {
    let bytes: &[u8] = match tier {
        Tier::Easy => include_bytes!("../icons/bear-easy-32.png"),
        Tier::Braced => include_bytes!("../icons/bear-braced-32.png"),
        Tier::Strained => include_bytes!("../icons/bear-strained-32.png"),
    };
    Image::from_bytes(bytes).ok()
}

fn main() {
    let shared: Shared = Arc::new(Mutex::new(Status::default()));

    tauri::Builder::default()
        .manage(shared.clone())
        .invoke_handler(tauri::generate_handler![get_status, enable_temperature])
        .setup(move |app| {
            let quit = MenuItem::with_id(app, "quit", "Quit LoadBear", true, None::<&str>)?;
            let show = MenuItem::with_id(app, "show", "Show", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;

            let tray = TrayIconBuilder::with_id("loadbear")
                .icon(tray_icon(Tier::Easy).expect("the placeholder icon must decode"))
                .tooltip("LoadBear")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "quit" => app.exit(0),
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    _ => {}
                })
                .build(app)?;

            let shared = shared.clone();
            let handle = app.handle().clone();

            // Docker gets its own thread. A named pipe read has no timeout, so
            // a wedged engine parked on this thread costs a stale container
            // list and nothing else.
            let containers: Containers = Arc::new(Mutex::new(Vec::new()));
            let docker_containers = containers.clone();
            std::thread::spawn(move || loop {
                let fresh = docker::read_containers();
                if let Ok(mut c) = docker_containers.lock() {
                    *c = fresh;
                }
                std::thread::sleep(DOCKER_POLL);
            });

            std::thread::spawn(move || {
                let Ok(counters) = Counters::open() else {
                    eprintln!("LoadBear could not open performance counters.");
                    return;
                };

                let db = SpecDb::embedded().expect("the embedded database must parse");
                let key = current_cpu_key();
                let spec = key.as_ref().and_then(|k| db.lookup(k)).cloned();
                let brand = brand_string();
                let logical = std::thread::available_parallelism()
                    .map(|n| n.get() as u32)
                    .unwrap_or(1);

                // Temperature comes from the helper service through shared
                // memory, never from this process. PawnIO's device admits only
                // SYSTEM and Administrators, so an unprivileged interface
                // cannot read it directly and should not try.
                let mut reader = TemperatureReader::open().ok();

                let mut last_tier = Tier::Easy;

                // Two reductions of the same window, for two different jobs.
                // Judgement uses the median, which a lone spike cannot move.
                // Display uses the mean, which moves smoothly enough to watch.
                let mut window = SampleWindow::default();
                let mut process_sampler = ProcessSampler::new(logical);

                // The sustained requirement that `Strained` has always claimed
                // in its documentation and never enforced.
                let mut tracker = TierTracker::default();
                let mut history: VecDeque<HistoryPoint> = VecDeque::new();

                loop {
                    let Ok(raw) = counters.sample(SAMPLE_INTERVAL) else {
                        continue;
                    };
                    window.push(raw);
                    let sample = window.average().unwrap_or(raw);
                    let judged = window.median().unwrap_or(raw);

                    if REPROBE.swap(false, Ordering::Relaxed) || reader.is_none() {
                        reader = TemperatureReader::open().ok();
                    }

                    // A helper publishing a layout this build does not know
                    // cannot have its record read at all, so ask the version
                    // separately. It is the first field and has never moved.
                    let layout_mismatch = reader
                        .as_ref()
                        .map(|r| r.published_version() != loadbear_sensors_windows::shared::LAYOUT_VERSION)
                        .unwrap_or(false);

                    let published: Option<SharedTemperature> = reader
                        .as_ref()
                        .and_then(|r| r.read())
                        .filter(|s| s.is_fresh(now_ms()));

                    let (temp_available, temp_offerable, temp_reason) = match &published {
                        _ if layout_mismatch => (
                            false,
                            true,
                            "The background helper is out of date and needs updating."
                                .to_string(),
                        ),
                        // Readings are usable, but the helper predates this
                        // build, so a feature it does not know about would
                        // silently never appear.
                        Some(s) if !s.helper_is_current() => (
                            true,
                            true,
                            "An update is available for the background helper.".to_string(),
                        ),
                        Some(_) => (true, false, String::new()),
                        None if service_control::is_running() => (
                            false,
                            false,
                            "The helper is running but has not published a reading yet."
                                .to_string(),
                        ),
                        None => (
                            false,
                            true,
                            "Temperature needs a small background helper, which runs with                              system access so LoadBear itself never has to."
                                .to_string(),
                        ),
                    };

                    let now = now_ms();
                    let reading = Reading {
                        timestamp_ms: now,
                        stall: to_stall(&judged, logical),
                        cpu: CpuReading {
                            all_core_mhz: judged.actual_mhz(),
                            utilization_pct: Some(judged.processor_time_pct as f32),
                            package_watts: None,
                            package_temp_c: published.as_ref().and_then(|s| s.package()),
                            tjmax_c: spec.as_ref().and_then(|s| s.tjmax_c),
                            throttle: ThrottleState {
                                asserted: false,
                                reason: None,
                            },
                        },
                        processes: process_sampler.sample(now),
                        containers: containers.lock().map(|c| c.clone()).unwrap_or_default(),
                    };

                    let verdicts = evaluate(&reading, spec.as_ref());
                    // Two steps, deliberately separate. `classify` judges this
                    // window; the tracker decides whether that has held long
                    // enough to be worth showing anybody.
                    let assessment = tracker.observe(classify(&verdicts, &reading.stall), now);
                    let tier = assessment.tier;
                    // Attribution runs over the same reading the verdicts came
                    // from, so a finding can never name a cause measured at a
                    // different moment from the state it explains.
                    let findings = diagnose(&reading, verdicts);

                    // The graph. Recorded from the per tick sample rather than
                    // either reduction, because the point of it is to show the
                    // real shape of the last few minutes, spikes included, next
                    // to a tier that deliberately ignores them.
                    let displayed = to_stall(&sample, logical);
                    history.push_back(HistoryPoint {
                        utilization: raw.processor_time_pct as f32,
                        cpu: displayed.cpu,
                        memory: displayed.memory,
                        io: displayed.io,
                        tier: tier as u8,
                    });
                    while history.len() > HISTORY_POINTS {
                        history.pop_front();
                    }

                    if tier != last_tier {
                        if let Some(icon) = tray_icon(tier) {
                            let _ = tray.set_icon(Some(icon));
                        }
                        let _ = tray.set_tooltip(Some(format!("LoadBear: {tier:?}")));
                        last_tier = tier;
                    }

                    if let Ok(mut s) = shared.lock() {
                        *s = Status {
                            tier: format!("{tier:?}"),
                            brand: brand.clone(),
                            matched: spec.as_ref().map(|s| s.name.clone()),
                            cores: spec.as_ref().map(|s| s.cores).unwrap_or(0),
                            threads: spec.as_ref().map(|s| s.threads).unwrap_or(0),
                            base_mhz: spec.as_ref().map(|s| s.base_mhz).unwrap_or(0),
                            tdp_watts: spec.as_ref().map(|s| s.tdp_watts).unwrap_or(0),
                            mhz: reading.cpu.all_core_mhz,
                            logical,
                            utilization: sample.processor_time_pct,
                            queue: sample.processor_queue_length,
                            available_mb: sample.available_mbytes,
                            hard_faults: sample.pages_input_per_sec,
                            disk_ms: sample.disk_seconds_per_transfer * 1000.0,
                            disk_queue: sample.disk_queue_length,
                            stall_cpu: reading.stall.cpu,
                            stall_memory: reading.stall.memory,
                            stall_io: reading.stall.io,
                            verdicts: findings
                                .iter()
                                .map(|f| VerdictView {
                                    kind: format!("{:?}", f.verdict.kind),
                                    severity: format!("{:?}", f.verdict.severity),
                                    detail: f.verdict.detail.clone(),
                                    basis: f.verdict.basis.clone(),
                                    cause: f.cause.as_ref().map(|c| c.label.clone()),
                                    action: f.remediation.map(remediation_text),
                                })
                                .collect(),
                            reason: reason_text(assessment, window.is_settled()),
                            history: history.iter().copied().collect(),
                            contributors: contributors(&reading, assessment),
                            temp_available,
                            temp_offerable,
                            temp_zones: published
                                .as_ref()
                                .map(|s| s.zone_list())
                                .unwrap_or_default(),
                            temp_reason: temp_reason.clone(),
                        };
                    }

                    let _ = &handle;
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("LoadBear failed to start");
}
