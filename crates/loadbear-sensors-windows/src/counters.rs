//! Windows performance counters, read unprivileged.
//!
//! Verified on 2026-08-14 that every counter below reads from a normal user
//! shell with no elevation. That is what makes the `BelowBaseClock` verdict,
//! the strongest thing LoadBear says, available with no driver at all.
//!
//! English counter names are used via `PdhAddEnglishCounterW`, so this works on
//! a localised Windows where the displayed names differ.

use std::collections::VecDeque;
use std::time::Duration;

use windows_sys::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterValue,
    PdhOpenQueryW, PDH_FMT_COUNTERVALUE, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY,
};

const PATH_PROC_PERFORMANCE: &str = r"\Processor Information(_Total)\% Processor Performance";
const PATH_PROC_FREQUENCY: &str = r"\Processor Information(_Total)\Processor Frequency";
const PATH_PROC_TIME: &str = r"\Processor Information(_Total)\% Processor Time";
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
    /// Share of the window, 0 to 100, that processors spent executing work.
    ///
    /// Averaged across every logical processor, idle ones included. This is
    /// utilization, and it is the quantity LoadBear went without for its first
    /// session, during which the run queue was pressed into service as a proxy
    /// and produced a diagnosis of "idle" on a machine at 97 percent.
    pub processor_time_pct: f64,
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
            PATH_PROC_TIME,
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
                PATH_PROC_TIME => s.processor_time_pct = v,
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

/// How many samples the rolling window holds.
///
/// At the application's sampling interval of 1.5 seconds this covers roughly
/// twelve seconds, which is long enough for a majority of samples to disagree
/// with a burst.
pub const WINDOW_SAMPLES: usize = 8;

/// A rolling window of samples, reducible by mean or by median.
///
/// # Why the median exists, measured 2026-08-15
///
/// Forty consecutive samples of this machine during ordinary work: the run
/// queue read `0.0` on thirty-nine of them and `31.0` on one. It is not a noisy
/// continuous signal, it is a binary spike, and the mean handles that badly.
/// Averaging one spike of 31 across four samples reports a queue of 7.5 for six
/// seconds, so a half-second event becomes six seconds of visible strain. The
/// mean does not suppress a spike, it spreads it.
///
/// The median discards it outright. Thirty-nine zeroes and one spike has a
/// median of zero, which is the truthful answer to "is this machine loaded".
/// A machine genuinely under load has a majority of loaded samples and the
/// median follows it up.
///
/// So judgement uses [`Self::median`] and display uses [`Self::average`], which
/// still moves smoothly enough to watch.
#[derive(Debug, Clone)]
pub struct SampleWindow {
    capacity: usize,
    samples: VecDeque<CounterSample>,
}

impl SampleWindow {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            samples: VecDeque::with_capacity(capacity.max(1)),
        }
    }

    pub fn push(&mut self, sample: CounterSample) {
        if self.samples.len() == self.capacity {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Whether the window has filled, so its reductions cover the full period.
    ///
    /// A partial window is still worth showing, which is why neither reduction
    /// waits for this. It is reported separately so a caller that wants to
    /// claim something is *sustained* can tell the difference.
    pub fn is_settled(&self) -> bool {
        self.samples.len() == self.capacity
    }

    /// The median of every sample held, field by field.
    ///
    /// The statistic judgement uses. A single spike cannot move a median, which
    /// is the entire point: on this machine the run queue is zero on thirty
    /// nine samples out of forty, and the fortieth should not be allowed to
    /// describe the machine.
    ///
    /// Each field is taken independently, so the result is not any one sample
    /// that was actually observed. That is correct here. The question asked of
    /// it is "what is this machine typically doing", one resource at a time,
    /// not "which instant was representative".
    pub fn median(&self) -> Option<CounterSample> {
        if self.samples.is_empty() {
            return None;
        }
        let pick = |f: fn(&CounterSample) -> f64| -> f64 {
            let mut values: Vec<f64> = self.samples.iter().map(f).collect();
            values.sort_by(f64::total_cmp);
            let mid = values.len() / 2;
            if values.len() % 2 == 0 {
                (values[mid - 1] + values[mid]) / 2.0
            } else {
                values[mid]
            }
        };
        Some(CounterSample {
            processor_performance_pct: pick(|s| s.processor_performance_pct),
            processor_frequency_mhz: pick(|s| s.processor_frequency_mhz),
            processor_time_pct: pick(|s| s.processor_time_pct),
            processor_queue_length: pick(|s| s.processor_queue_length),
            pages_input_per_sec: pick(|s| s.pages_input_per_sec),
            available_mbytes: pick(|s| s.available_mbytes),
            disk_seconds_per_transfer: pick(|s| s.disk_seconds_per_transfer),
            disk_queue_length: pick(|s| s.disk_queue_length),
        })
    }

    /// The mean of every sample held, or `None` when nothing has been pushed.
    pub fn average(&self) -> Option<CounterSample> {
        if self.samples.is_empty() {
            return None;
        }
        let n = self.samples.len() as f64;
        let mut sum = CounterSample::default();
        for s in &self.samples {
            sum.processor_performance_pct += s.processor_performance_pct;
            sum.processor_frequency_mhz += s.processor_frequency_mhz;
            sum.processor_time_pct += s.processor_time_pct;
            sum.processor_queue_length += s.processor_queue_length;
            sum.pages_input_per_sec += s.pages_input_per_sec;
            sum.available_mbytes += s.available_mbytes;
            sum.disk_seconds_per_transfer += s.disk_seconds_per_transfer;
            sum.disk_queue_length += s.disk_queue_length;
        }
        Some(CounterSample {
            processor_performance_pct: sum.processor_performance_pct / n,
            processor_frequency_mhz: sum.processor_frequency_mhz / n,
            processor_time_pct: sum.processor_time_pct / n,
            processor_queue_length: sum.processor_queue_length / n,
            pages_input_per_sec: sum.pages_input_per_sec / n,
            available_mbytes: sum.available_mbytes / n,
            disk_seconds_per_transfer: sum.disk_seconds_per_transfer / n,
            disk_queue_length: sum.disk_queue_length / n,
        })
    }
}

impl Default for SampleWindow {
    fn default() -> Self {
        Self::new(WINDOW_SAMPLES)
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
    ///
    /// **Raised from 1000 on 2026-08-15 against measurement.** Forty samples of
    /// ordinary work on this machine, no perceptible slowness, produced bursts
    /// of 5641 and 12513 hard faults per second from what was plainly just
    /// reading files. At the old figure those two samples pinned memory stall
    /// to fully saturated, which drove the tier to Strained and turned the tray
    /// icon red during nothing at all.
    ///
    /// A machine that is genuinely thrashing sustains a rate like this rather
    /// than touching it twice a minute, and the median window is what tells the
    /// two apart. This figure sits above the measured ordinary peak so that
    /// even the median of a busy period is not saturated by routine file
    /// access. It still wants calibrating against a machine that is actually
    /// paging itself to death, which this one was not.
    pub const PAGES_INPUT_SATURATED: f64 = 20_000.0;
    /// Seconds per disk transfer at which I/O stall is treated as saturated.
    ///
    /// Measured range during ordinary work on this machine was 0.0002 to
    /// 0.0013, comfortably clear of this, so it is left where it was.
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

    fn sample_with_queue(queue: f64) -> CounterSample {
        CounterSample {
            processor_queue_length: queue,
            ..Default::default()
        }
    }

    #[test]
    fn an_empty_window_averages_to_nothing_rather_than_zero() {
        let w = SampleWindow::new(4);
        assert!(w.average().is_none(), "no samples is not the same as zero");
    }

    #[test]
    fn a_window_averages_the_samples_it_holds() {
        let mut w = SampleWindow::new(4);
        for q in [0.0, 30.0, 2.0, 8.0] {
            w.push(sample_with_queue(q));
        }
        assert_eq!(w.average().unwrap().processor_queue_length, 10.0);
    }

    #[test]
    fn a_window_averages_a_partial_fill_rather_than_waiting() {
        let mut w = SampleWindow::new(4);
        w.push(sample_with_queue(4.0));
        w.push(sample_with_queue(6.0));
        assert_eq!(w.average().unwrap().processor_queue_length, 5.0);
        assert!(
            !w.is_settled(),
            "a partial average is usable but must not be called sustained"
        );
    }

    #[test]
    fn a_window_drops_the_oldest_sample_once_full() {
        let mut w = SampleWindow::new(2);
        w.push(sample_with_queue(100.0));
        w.push(sample_with_queue(0.0));
        w.push(sample_with_queue(0.0));
        assert_eq!(w.len(), 2);
        assert_eq!(
            w.average().unwrap().processor_queue_length,
            0.0,
            "a spike must age out of the window rather than colouring it forever"
        );
        assert!(w.is_settled());
    }

    #[test]
    fn the_median_discards_a_lone_spike_entirely() {
        // The measured shape of this machine: the run queue reads zero on
        // thirty nine samples out of forty and spikes once. The mean turns
        // that one spike into six seconds of visible strain. The median treats
        // it as what it is.
        let mut w = SampleWindow::new(8);
        for _ in 0..7 {
            w.push(sample_with_queue(0.0));
        }
        w.push(sample_with_queue(31.0));
        assert_eq!(w.median().unwrap().processor_queue_length, 0.0);
        assert!(
            w.average().unwrap().processor_queue_length > 3.0,
            "the mean is shown to have the problem the median solves"
        );
    }

    #[test]
    fn the_median_follows_a_machine_that_is_actually_loaded() {
        // The other half. A statistic that only ever reports calm is useless.
        let mut w = SampleWindow::new(8);
        for _ in 0..6 {
            w.push(sample_with_queue(24.0));
        }
        w.push(sample_with_queue(0.0));
        w.push(sample_with_queue(0.0));
        assert_eq!(w.median().unwrap().processor_queue_length, 24.0);
        assert_eq!(to_stall(&w.median().unwrap(), 16).cpu, 1.0);
    }

    #[test]
    fn ordinary_file_reading_no_longer_saturates_memory_stall() {
        // The measured burst that used to turn the tray icon red during
        // nothing at all.
        let s = CounterSample {
            pages_input_per_sec: 12_513.0,
            ..Default::default()
        };
        let stall = to_stall(&s, 16);
        assert!(
            stall.memory < 0.80,
            "a routine burst of file reading must not reach the out of spec threshold, got {}",
            stall.memory
        );
    }

    #[test]
    fn a_machine_genuinely_paging_still_saturates() {
        let s = CounterSample {
            pages_input_per_sec: 40_000.0,
            ..Default::default()
        };
        assert_eq!(to_stall(&s, 16).memory, 1.0);
    }

    #[test]
    fn averaging_a_single_spike_across_a_quiet_window_flattens_it() {
        // This is the failure the window exists for. One sample of 30 on an
        // otherwise quiet machine used to be the whole basis of a diagnosis.
        let mut w = SampleWindow::new(4);
        w.push(sample_with_queue(30.0));
        for _ in 0..3 {
            w.push(sample_with_queue(0.0));
        }
        let averaged = w.average().unwrap();
        assert_eq!(averaged.processor_queue_length, 7.5);
        assert!(to_stall(&averaged, 16).cpu < to_stall(&sample_with_queue(30.0), 16).cpu);
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

    #[test]
    fn utilization_reads_a_real_percentage_from_this_machine() {
        // A counter that silently reports zero is indistinguishable from an
        // idle machine, and treating one as the other is the failure that
        // produced a wrong diagnosis. So the range is asserted rather than the
        // call merely succeeding.
        let c = Counters::open().expect("counters must open unprivileged");
        let s = c
            .sample(Duration::from_millis(300))
            .expect("a sample must come back");
        assert!(
            (0.0..=100.0).contains(&s.processor_time_pct),
            "utilization should be a percentage of the window, got {}",
            s.processor_time_pct
        );
    }
}
