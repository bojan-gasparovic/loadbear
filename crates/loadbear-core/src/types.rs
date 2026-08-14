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
    pub io: f32,
}

impl StallSignal {
    /// The resource under the most pressure, and its value.
    ///
    /// Ties resolve in the order cpu, memory, io, which makes the result
    /// deterministic. The ordering carries no meaning beyond that.
    pub fn worst(&self) -> (Resource, f32) {
        let mut worst = (Resource::Cpu, self.cpu);
        if self.memory > worst.1 {
            worst = (Resource::Memory, self.memory);
        }
        if self.io > worst.1 {
            worst = (Resource::Io, self.io);
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
    pub package_watts: Option<f32>,
    pub package_temp_c: Option<f32>,
    pub tjmax_c: Option<f32>,
    pub throttle: ThrottleState,
}

/// A single process, as seen by the platform backend.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessReading {
    pub pid: u32,
    pub name: String,
    pub working_set_bytes: u64,
    pub hard_faults_per_sec: Option<f32>,
    pub cpu_percent: f32,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stall_signal_reports_its_worst_resource() {
        let stall = StallSignal { cpu: 0.10, memory: 0.72, io: 0.31 };
        assert_eq!(stall.worst(), (Resource::Memory, 0.72));
    }

    #[test]
    fn stall_signal_with_no_pressure_reports_cpu_at_zero() {
        let stall = StallSignal { cpu: 0.0, memory: 0.0, io: 0.0 };
        assert_eq!(stall.worst(), (Resource::Cpu, 0.0));
    }
}
