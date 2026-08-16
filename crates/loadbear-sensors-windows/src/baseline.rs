//! What this machine's disk normally does, learned from the machine itself.
//!
//! # Why there is no constant to use instead
//!
//! Clock has a vendor guarantee and temperature has TjMax. Disk latency has
//! neither. Twenty milliseconds per transfer is ordinary on a mechanical disk,
//! five on a SATA SSD, half of one on NVMe, so a threshold that catches a
//! struggling hard disk is blind on NVMe and one that catches NVMe screams
//! constantly on anything older.
//!
//! Measured on this machine 2026-08-15: four threads reading files
//! continuously peaked at 3.4 ms per transfer against a 50 ms constant, so the
//! normalised bar never left single digits under a load built specifically to
//! saturate the disk. One of the three stall signals was dead on any modern
//! machine, and it did not look dead. It looked like a disk with nothing to do.
//!
//! `DESIGN.md` section 4 names the third source for exactly this case: the
//! machine's own history. This module is that history.
//!
//! # What is learned
//!
//! The median latency of transfers this disk served while it was not backed
//! up. Saturation is then a multiple of that, so the question becomes "is this
//! disk answering far slower than it normally does", which is meaningful on
//! every device class rather than on one.
//!
//! The multiple is still a chosen number and is documented as one. It is a
//! ratio rather than an absolute, which is the whole difference: it travels.

use std::path::PathBuf;

use crate::counters::CounterSample;

/// How many qualifying samples make a baseline worth trusting.
///
/// At roughly one sample per one and a half seconds this is a few minutes of
/// disk activity, which need not be continuous and accumulates across runs.
/// Below it the signal reports absent, in the same way temperature does with
/// no TjMax to measure headroom against.
const MIN_SAMPLES: usize = 120;

/// How many qualifying samples are kept.
///
/// Enough for a stable median, small enough that the file stays a few
/// kilobytes, and a moving window rather than a lifetime average so replacing
/// the disk eventually reteaches it rather than being averaged away forever.
const WINDOW: usize = 512;

/// A disk with more than this many requests outstanding is not idle, and its
/// latency is a queueing figure rather than a baseline one.
///
/// One is deliberately generous. The counter is an instantaneous depth, so a
/// disk serving a single request reads one, and demanding zero would learn
/// only from samples where nothing happened.
const QUIET_QUEUE: f64 = 1.0;

/// How many times its own normal latency a disk has to be answering in before
/// this counts as saturated.
///
/// **A chosen number, and the only one here.** An order of magnitude slower
/// than the disk's own normal is a recognisable definition of struggling, and
/// unlike an absolute threshold it means the same thing on every device class.
/// On this NVMe a roughly 0.3 ms baseline puts saturation near 3 ms, which the
/// measured saturating load reached. On a mechanical disk a 20 ms baseline puts
/// it at 200 ms, which is a disk in trouble by any account.
///
/// It moves the tier and never a verdict, which is what makes a chosen number
/// acceptable here at all.
const SATURATION_RATIO: f64 = 10.0;

/// The learned latency window for this machine's disk.
#[derive(Debug, Default, Clone)]
pub struct DiskBaseline {
    /// Qualifying latencies in seconds, oldest first, at most `WINDOW` of them.
    samples: Vec<f64>,
    /// Set when `samples` has changed since the last write.
    dirty: bool,
}

impl DiskBaseline {
    /// An empty baseline, which reports no saturation point at all.
    pub fn new() -> Self {
        Self::default()
    }

    /// Offer a sample. Only quiet ones with real traffic are kept.
    ///
    /// Two rejections matter. A latency of zero means no transfer completed in
    /// the window, and counting those would drag the median toward nothing.
    /// A queue above `QUIET_QUEUE` means the figure describes a backlog, which
    /// is the very thing the baseline exists to be compared against.
    pub fn observe(&mut self, s: &CounterSample) {
        let latency = s.disk_seconds_per_transfer;
        if !latency.is_finite() || latency <= 0.0 || s.disk_queue_length > QUIET_QUEUE {
            return;
        }

        if self.samples.len() == WINDOW {
            self.samples.remove(0);
        }
        self.samples.push(latency);
        self.dirty = true;
    }

    /// How many qualifying samples have been collected.
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// The median quiet latency, in seconds, once there is enough to say.
    pub fn median(&self) -> Option<f64> {
        if self.samples.len() < MIN_SAMPLES {
            return None;
        }
        let mut sorted = self.samples.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        Some(sorted[sorted.len() / 2])
    }

    /// The latency at which I/O stall counts as fully saturated on this
    /// machine, or nothing while the baseline is still being learned.
    pub fn saturation_point(&self) -> Option<f64> {
        self.median().map(|m| m * SATURATION_RATIO)
    }

    /// Where the baseline is kept between runs.
    ///
    /// Under the user's own local application data, because the interface is
    /// unprivileged by design and this is the interface's knowledge. The helper
    /// never reads it.
    pub fn path() -> Option<PathBuf> {
        let local = std::env::var_os("LOCALAPPDATA")?;
        Some(PathBuf::from(local).join("LoadBear").join("disk-baseline.json"))
    }

    /// Read the stored baseline, or an empty one.
    ///
    /// Every failure returns empty rather than an error. A missing file is the
    /// ordinary first run, and a corrupt one costs a few minutes of relearning,
    /// which is not worth refusing to start over.
    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::new();
        };
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::new();
        };
        let Ok(samples) = serde_json::from_str::<Vec<f64>>(&text) else {
            return Self::new();
        };

        // Anything stored that would not be accepted now is dropped, so
        // tightening what qualifies cannot be defeated by an old file.
        let mut samples: Vec<f64> = samples
            .into_iter()
            .filter(|v| v.is_finite() && *v > 0.0)
            .collect();
        if samples.len() > WINDOW {
            let excess = samples.len() - WINDOW;
            samples.drain(..excess);
        }
        Self {
            samples,
            dirty: false,
        }
    }

    /// Write the baseline out, if it has changed since the last write.
    ///
    /// Returns whether anything was written. Failures are silent: a machine
    /// that cannot write here still measures its disk perfectly well and only
    /// forgets what it learned when it closes.
    pub fn save_if_changed(&mut self) -> bool {
        if !self.dirty {
            return false;
        }
        let Some(path) = Self::path() else {
            return false;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let Ok(text) = serde_json::to_string(&self.samples) else {
            return false;
        };
        if std::fs::write(&path, text).is_ok() {
            self.dirty = false;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(latency: f64, queue: f64) -> CounterSample {
        CounterSample {
            disk_seconds_per_transfer: latency,
            disk_queue_length: queue,
            ..Default::default()
        }
    }

    #[test]
    fn a_fresh_baseline_says_nothing_rather_than_guessing() {
        let b = DiskBaseline::new();
        assert_eq!(b.median(), None);
        assert_eq!(b.saturation_point(), None);
    }

    #[test]
    fn it_stays_silent_until_it_has_enough_to_say() {
        let mut b = DiskBaseline::new();
        for _ in 0..MIN_SAMPLES - 1 {
            b.observe(&sample(0.0003, 0.0));
        }
        assert_eq!(b.median(), None, "one short of enough is not enough");
        b.observe(&sample(0.0003, 0.0));
        assert!(b.median().is_some(), "the last sample must complete it");
    }

    #[test]
    fn a_busy_disk_teaches_it_nothing() {
        // The counter reports a queueing figure once requests are backing up,
        // and learning from those would define normal as whatever the machine
        // was doing while struggling.
        let mut b = DiskBaseline::new();
        for _ in 0..MIN_SAMPLES * 2 {
            b.observe(&sample(0.004, 8.0));
        }
        assert!(b.is_empty(), "a backed up disk must not be learned from");
    }

    #[test]
    fn an_idle_disk_with_no_transfers_teaches_it_nothing() {
        // Zero latency means nothing completed, not that the disk answered
        // instantly. Counting those would drag the median to nothing and make
        // every later reading look catastrophic.
        let mut b = DiskBaseline::new();
        for _ in 0..MIN_SAMPLES * 2 {
            b.observe(&sample(0.0, 0.0));
        }
        assert!(b.is_empty(), "a sample with no transfer is not a measurement");
    }

    #[test]
    fn saturation_is_an_order_of_magnitude_off_this_disks_own_normal() {
        let mut b = DiskBaseline::new();
        for _ in 0..MIN_SAMPLES {
            b.observe(&sample(0.0003, 0.0));
        }
        let point = b.saturation_point().expect("a baseline must have formed");
        assert!((point - 0.003).abs() < 1e-9, "0.3 ms normal, 3 ms saturated");
    }

    #[test]
    fn the_measured_nvme_load_now_registers_where_it_did_not_before() {
        // The regression this whole module exists for. Measured 2026-08-15:
        // quiet latency 0.1 to 0.9 ms, and a load built to saturate the disk
        // peaked at 3.4 ms. Against the old 50 ms constant that peak scored
        // 0.068 and the bar could not leave single digits.
        let mut b = DiskBaseline::new();
        for i in 0..MIN_SAMPLES {
            // Spread across the measured quiet range rather than one value, so
            // the median is doing real work here.
            b.observe(&sample(0.0001 + (i % 9) as f64 * 0.0001, 0.0));
        }
        let point = b.saturation_point().expect("a baseline must have formed");
        let peak_under_load = 0.0034;
        let stall = (peak_under_load / point).clamp(0.0, 1.0);

        let old_constant_stall = peak_under_load / 0.050;
        assert!(
            old_constant_stall < 0.07,
            "the old constant scored {old_constant_stall}, which is the bug"
        );
        assert!(
            stall > 0.5,
            "the measured saturating load must move the bar, got {stall}"
        );
    }

    #[test]
    fn the_window_slides_rather_than_growing_forever() {
        // A lifetime average would take a replaced disk months to relearn.
        let mut b = DiskBaseline::new();
        for _ in 0..WINDOW * 2 {
            b.observe(&sample(0.0005, 0.0));
        }
        assert_eq!(b.len(), WINDOW);
    }

    #[test]
    fn a_replaced_disk_is_eventually_relearned() {
        let mut b = DiskBaseline::new();
        for _ in 0..WINDOW {
            b.observe(&sample(0.020, 0.0)); // a mechanical disk
        }
        let mechanical = b.median().expect("a baseline must have formed");
        for _ in 0..WINDOW {
            b.observe(&sample(0.0003, 0.0)); // swapped for NVMe
        }
        let nvme = b.median().expect("a baseline must have formed");
        assert!(
            nvme < mechanical / 10.0,
            "the window must forget the old disk, {mechanical} then {nvme}"
        );
    }

    #[test]
    fn nothing_is_written_when_nothing_changed() {
        let mut b = DiskBaseline::new();
        assert!(!b.save_if_changed(), "an untouched baseline has no work to do");
    }

    #[test]
    fn a_corrupt_or_missing_store_costs_relearning_rather_than_starting() {
        // load() has no failure path by design. Whatever is on disk, the
        // application starts and measures.
        let b = DiskBaseline::load();
        assert!(b.len() <= WINDOW);
    }
}
