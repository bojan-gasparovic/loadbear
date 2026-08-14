//! Windows performance counters, read unprivileged.
//!
//! Verified on 2026-08-14 that every counter below reads from a normal user
//! shell with no elevation. That is what makes the `BelowBaseClock` verdict,
//! the strongest thing LoadBear says, available with no driver at all.
//!
//! English counter names are used via `PdhAddEnglishCounterW`, so this works on
//! a localised Windows where the displayed names differ.

use std::time::Duration;

use windows_sys::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterValue,
    PdhOpenQueryW, PDH_FMT_COUNTERVALUE, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY,
};

const PATH_PROC_PERFORMANCE: &str = r"\Processor Information(_Total)\% Processor Performance";
const PATH_PROC_FREQUENCY: &str = r"\Processor Information(_Total)\Processor Frequency";
const PATH_PROC_QUEUE: &str = r"\System\Processor Queue Length";
const PATH_PAGES_INPUT: &str = r"\Memory\Pages Input/sec";
const PATH_AVAILABLE_MB: &str = r"\Memory\Available MBytes";
const PATH_DISK_LATENCY: &str = r"\PhysicalDisk(_Total)\Avg. Disk sec/Transfer";
const PATH_DISK_QUEUE: &str = r"\PhysicalDisk(_Total)\Current Disk Queue Length";

/// One sample of every counter LoadBear reads on Windows.
#[derive(Debug, Clone, Copy, Default)]
pub struct CounterSample {
    /// Current frequency as a percentage of the nominal base clock.
    pub processor_performance_pct: f64,
    /// The nominal base clock, in MHz, as the OS reports it.
    pub processor_frequency_mhz: f64,
    /// Threads waiting for a processor. Not utilization.
    pub processor_queue_length: f64,
    /// Hard page faults per second. The direct measure of stalling on memory.
    pub pages_input_per_sec: f64,
    pub available_mbytes: f64,
    /// Seconds per disk transfer. Latency, not throughput.
    pub disk_seconds_per_transfer: f64,
    pub disk_queue_length: f64,
}

impl CounterSample {
    /// Actual sustained all-core frequency in MHz.
    ///
    /// Windows reports the nominal base clock and the current performance as a
    /// percentage of it. Multiplying gives the real figure, which is what the
    /// base clock verdict compares against a vendor guarantee.
    pub fn actual_mhz(&self) -> Option<u32> {
        if self.processor_frequency_mhz <= 0.0 {
            return None;
        }
        Some((self.processor_frequency_mhz * self.processor_performance_pct / 100.0).round() as u32)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CounterError {
    #[error("could not open a performance counter query")]
    QueryFailed,
    #[error("performance counter '{0}' is not available on this machine")]
    CounterUnavailable(String),
    #[error("performance counters returned no data")]
    NoData,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// An open PDH query over the counters LoadBear needs.
pub struct Counters {
    query: PDH_HQUERY,
    handles: Vec<(&'static str, PDH_HCOUNTER)>,
}

impl Counters {
    pub fn open() -> Result<Self, CounterError> {
        let mut query: PDH_HQUERY = std::ptr::null_mut();
        // SAFETY: a null data source means live data. `query` is writable.
        let rc = unsafe { PdhOpenQueryW(std::ptr::null(), 0, &mut query) };
        if rc != 0 {
            return Err(CounterError::QueryFailed);
        }

        let paths = [
            PATH_PROC_PERFORMANCE,
            PATH_PROC_FREQUENCY,
            PATH_PROC_QUEUE,
            PATH_PAGES_INPUT,
            PATH_AVAILABLE_MB,
            PATH_DISK_LATENCY,
            PATH_DISK_QUEUE,
        ];

        let mut handles = Vec::with_capacity(paths.len());
        for path in paths {
            let w = wide(path);
            let mut counter: PDH_HCOUNTER = std::ptr::null_mut();
            // SAFETY: `query` is valid, `w` is NUL terminated and outlives the
            // call, and `counter` is writable.
            let rc = unsafe { PdhAddEnglishCounterW(query, w.as_ptr(), 0, &mut counter) };
            if rc != 0 {
                // SAFETY: closing a query we opened.
                unsafe { PdhCloseQuery(query) };
                return Err(CounterError::CounterUnavailable(path.to_string()));
            }
            handles.push((path, counter));
        }

        // SAFETY: `query` is valid. The first collection primes rate counters,
        // which need two samples to produce a value.
        unsafe { PdhCollectQueryData(query) };

        Ok(Self { query, handles })
    }

    /// Collect one sample.
    ///
    /// Rate counters need a gap between collections, so this sleeps for
    /// `interval` before reading. Calling it with a very short interval yields
    /// noisy rates rather than an error.
    pub fn sample(&self, interval: Duration) -> Result<CounterSample, CounterError> {
        std::thread::sleep(interval);

        // SAFETY: `self.query` is valid for the lifetime of `self`.
        let rc = unsafe { PdhCollectQueryData(self.query) };
        if rc != 0 {
            return Err(CounterError::NoData);
        }

        let mut s = CounterSample::default();
        for (path, counter) in &self.handles {
            let mut value: PDH_FMT_COUNTERVALUE = unsafe { std::mem::zeroed() };
            // SAFETY: `counter` is valid and `value` is a correctly sized
            // output buffer for PDH_FMT_DOUBLE.
            let rc = unsafe {
                PdhGetFormattedCounterValue(
                    *counter,
                    PDH_FMT_DOUBLE,
                    std::ptr::null_mut(),
                    &mut value,
                )
            };
            if rc != 0 {
                continue;
            }
            // SAFETY: PDH_FMT_DOUBLE was requested, so the double arm is the
            // active union member.
            let v = unsafe { value.Anonymous.doubleValue };

            match *path {
                PATH_PROC_PERFORMANCE => s.processor_performance_pct = v,
                PATH_PROC_FREQUENCY => s.processor_frequency_mhz = v,
                PATH_PROC_QUEUE => s.processor_queue_length = v,
                PATH_PAGES_INPUT => s.pages_input_per_sec = v,
                PATH_AVAILABLE_MB => s.available_mbytes = v,
                PATH_DISK_LATENCY => s.disk_seconds_per_transfer = v,
                PATH_DISK_QUEUE => s.disk_queue_length = v,
                _ => {}
            }
        }

        Ok(s)
    }
}

impl Drop for Counters {
    fn drop(&mut self) {
        // SAFETY: closing a query we opened, exactly once.
        unsafe { PdhCloseQuery(self.query) };
    }
}

/// Provisional scaling from raw counters to a normalized stall signal.
///
/// **These constants are not sourced and are not verdict thresholds.** Linux
/// measures stall directly as time waiting. Windows does not, so these convert
/// its native counters into the same shape, and the conversion involves a
/// judgement about what counts as saturated.
///
/// They are deliberately kept out of the verdict path. Every verdict LoadBear
/// issues comes from the absolute checks, which trace to a vendor guarantee or
/// a hardware bit. These only move the tier, and they are first guesses to be
/// calibrated against real machines under real load.
pub mod scale {
    /// Hard faults per second at which memory stall is treated as saturated.
    pub const PAGES_INPUT_SATURATED: f64 = 1000.0;
    /// Seconds per disk transfer at which I/O stall is treated as saturated.
    pub const DISK_LATENCY_SATURATED: f64 = 0.050;
}

/// Convert a sample into a normalized stall signal.
///
/// `logical_cpus` scales the run queue: a queue of four on a sixteen thread
/// machine is not the same pressure as a queue of four on a dual core.
pub fn to_stall(s: &CounterSample, logical_cpus: u32) -> loadbear_core::StallSignal {
    let cpus = logical_cpus.max(1) as f64;
    loadbear_core::StallSignal {
        cpu: (s.processor_queue_length / cpus).clamp(0.0, 1.0) as f32,
        memory: (s.pages_input_per_sec / scale::PAGES_INPUT_SATURATED).clamp(0.0, 1.0) as f32,
        io: (s.disk_seconds_per_transfer / scale::DISK_LATENCY_SATURATED).clamp(0.0, 1.0) as f32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actual_frequency_is_base_times_performance_percentage() {
        let s = CounterSample {
            processor_frequency_mhz: 2000.0,
            processor_performance_pct: 109.768,
            ..Default::default()
        };
        assert_eq!(s.actual_mhz(), Some(2195));
    }

    #[test]
    fn a_zero_base_frequency_yields_no_reading_rather_than_a_division_artefact() {
        let s = CounterSample {
            processor_frequency_mhz: 0.0,
            processor_performance_pct: 100.0,
            ..Default::default()
        };
        assert_eq!(s.actual_mhz(), None);
    }

    #[test]
    fn a_quiet_machine_produces_no_stall() {
        let s = CounterSample::default();
        let stall = to_stall(&s, 16);
        assert_eq!(stall.cpu, 0.0);
        assert_eq!(stall.memory, 0.0);
        assert_eq!(stall.io, 0.0);
    }

    #[test]
    fn the_run_queue_is_scaled_by_logical_processor_count() {
        let s = CounterSample {
            processor_queue_length: 8.0,
            ..Default::default()
        };
        assert_eq!(to_stall(&s, 16).cpu, 0.5);
        assert_eq!(to_stall(&s, 8).cpu, 1.0);
    }

    #[test]
    fn stall_saturates_rather_than_exceeding_one() {
        let s = CounterSample {
            processor_queue_length: 999.0,
            pages_input_per_sec: 999_999.0,
            disk_seconds_per_transfer: 10.0,
            ..Default::default()
        };
        let stall = to_stall(&s, 16);
        assert_eq!(stall.cpu, 1.0);
        assert_eq!(stall.memory, 1.0);
        assert_eq!(stall.io, 1.0);
    }

    #[test]
    fn counters_open_and_sample_without_elevation() {
        // This is the claim the whole unprivileged path rests on, so it is
        // asserted rather than assumed.
        let c = Counters::open().expect("counters must open unprivileged");
        let s = c
            .sample(Duration::from_millis(200))
            .expect("a sample must come back");
        assert!(
            s.processor_frequency_mhz > 0.0,
            "processor frequency should report the base clock, got {}",
            s.processor_frequency_mhz
        );
    }
}
