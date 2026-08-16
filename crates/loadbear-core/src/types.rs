use serde::{Deserialize, Serialize};

/// Which resource a stall is attributed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Resource {
    Cpu,
    Memory,
    Io,
}

/// Resource stall, normalized across platforms.
///
/// Each value is the share of the sampling window, from 0.0 to 1.0, that work
/// spent waiting on that resource rather than progressing. This is not
/// utilization. On Linux these map to Pressure Stall Information. On Windows
/// and macOS the backend derives equivalent values from native counters.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct StallSignal {
    pub cpu: f32,
    pub memory: f32,
    /// Absent until the machine has said what its own disk normally does.
    ///
    /// Every other signal here normalises against something fixed. This one
    /// cannot: twenty milliseconds per transfer is ordinary on a mechanical
    /// disk, five on a SATA SSD, half of one on NVMe, so any constant is either
    /// blind on fast storage or screaming on slow storage. There is no vendor
    /// guarantee and no hardware bit to appeal to, which leaves the machine's
    /// own history, and history takes time to collect.
    ///
    /// `None` means not yet known, and it is treated the same way an absent
    /// temperature is: reported as unavailable rather than filled in with a
    /// number nothing stands behind.
    pub io: Option<f32>,
}

impl StallSignal {
    /// The resource under the most pressure, and its value.
    ///
    /// Ties resolve in the order cpu, memory, io, which makes the result
    /// deterministic. The ordering carries no meaning beyond that.
    ///
    /// An unknown io signal does not compete. A disk with no baseline yet is
    /// not a quiet disk, and letting it stand in as zero would be the guess
    /// this type exists to avoid.
    pub fn worst(&self) -> (Resource, f32) {
        let mut worst = (Resource::Cpu, self.cpu);
        if self.memory > worst.1 {
            worst = (Resource::Memory, self.memory);
        }
        if let Some(io) = self.io {
            if io > worst.1 {
                worst = (Resource::Io, io);
            }
        }
        worst
    }
}

/// Why the hardware asserted a throttle signal.
///
/// Read from a hardware bit, never inferred from temperature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThrottleReason {
    Thermal,
    Power,
    Electrical,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThrottleState {
    pub asserted: bool,
    pub reason: Option<ThrottleReason>,
}

/// CPU state at a point in time.
///
/// Every field is optional because backends degrade rather than fail. On
/// Windows without an elevated driver, temperature and power are absent while
/// everything else still works.
///
/// `tjmax_c` is optional for a second reason found during the LB-02 spike:
/// AMD parts may not publish it at all. On the Ryzen 7 4980U it reads as zero
/// from every available source, so it has to come from the specification
/// database rather than the chip.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CpuReading {
    pub all_core_mhz: Option<u32>,
    /// Share of the window, 0 to 100, that processors spent executing work.
    ///
    /// Averaged across every logical processor, idle ones included. This is
    /// utilization, which LoadBear does not judge a machine by, and needs
    /// anyway: a clock guarantee can only be tested while the processor is
    /// being asked for performance. See [`crate::verdict`].
    pub utilization_pct: Option<f32>,
    /// The base clock the machine itself reports, already cross-checked.
    ///
    /// Present so the strongest verdict LoadBear has does not depend on
    /// somebody having hand-entered this processor into a database. See
    /// [`crate::spec::reported_base_mhz`].
    pub reported_base_mhz: Option<u32>,
    pub package_watts: Option<f32>,
    pub package_temp_c: Option<f32>,
    pub tjmax_c: Option<f32>,
    pub throttle: ThrottleState,
}

/// A single process, as seen by the platform backend.
///
/// `cpu_percent` is a share of the whole machine, not of one core, so the
/// figures across a process list are comparable with each other and with the
/// machine's own utilization.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessReading {
    pub pid: u32,
    /// The executable name, which is what LoadBear matches on.
    pub name: String,
    /// The name a person would call it, when the executable carries one.
    ///
    /// Matching stays on `name` because that is stable and is what the Docker
    /// and system process lists are written against. This is for display only,
    /// so a vendor renaming their product cannot change a diagnosis.
    pub display_name: Option<String>,
    pub working_set_bytes: u64,
    pub hard_faults_per_sec: Option<f32>,
    pub cpu_percent: f32,
}

/// A single container, read from the container runtime rather than the OS.
///
/// This is a second source and not a refinement of process attribution. On
/// Windows every container on the machine presents as one `vmmem` process, so
/// the OS can report that Docker holds eleven gigabytes and can never report
/// which container holds it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContainerReading {
    pub id: String,
    pub name: String,
    pub cpu_percent: f32,
    pub memory_bytes: u64,
    /// The container's own memory limit, when one is set.
    ///
    /// Present only when the runtime reports a real limit. A container running
    /// against its limit needs the limit changed; one merely using a lot of
    /// memory needs stopping. The two are different remediations, so the
    /// difference is carried rather than assumed.
    pub memory_limit_bytes: Option<u64>,
}

/// One normalized observation of machine state.
///
/// Produced by a platform sensor backend, consumed by everything above it.
/// Nothing in this crate knows how it was gathered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reading {
    pub timestamp_ms: u64,
    pub stall: StallSignal,
    pub cpu: CpuReading,
    pub processes: Vec<ProcessReading>,
    /// Empty when no container runtime is present, which is the common case
    /// and not an error.
    pub containers: Vec<ContainerReading>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stall_signal_reports_its_worst_resource() {
        let stall = StallSignal {
            cpu: 0.10,
            memory: 0.72,
            io: Some(0.31),
        };
        assert_eq!(stall.worst(), (Resource::Memory, 0.72));
    }

    #[test]
    fn stall_signal_with_no_pressure_reports_cpu_at_zero() {
        let stall = StallSignal {
            cpu: 0.0,
            memory: 0.0,
            io: Some(0.0),
        };
        assert_eq!(stall.worst(), (Resource::Cpu, 0.0));
    }
}
