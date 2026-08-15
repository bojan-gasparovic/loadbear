//! Shared memory between the elevated helper and the unprivileged interface.
//!
//! # Why this exists
//!
//! PawnIO's device is secured by its INF as `D:P(A;;GA;;;SY)(A;;GA;;;BA)`,
//! meaning only Local System and Administrators may open it. That is a
//! deliberate choice on their part: a driver that hands MSR and SMN access to
//! unprivileged callers is the privilege escalation hole that got WinRing0
//! blocked by Microsoft.
//!
//! So temperature cannot be read from an unprivileged process, full stop. The
//! answer is the one Core Temp and HWiNFO both use: an elevated producer
//! publishes readings into shared memory, and an unprivileged consumer reads
//! them. Elevation is then paid once at install, when the service is
//! registered, and never again.
//!
//! # Torn reads
//!
//! The writer is a separate process from the reader, so a read can land
//! mid-write. `sequence` is a seqlock: odd means a write is in progress, and a
//! reader that sees the value change across its read discards the result and
//! retries. Without it a reader could observe a package temperature from one
//! sample and zones from the next.

/// Name of the mapping. `Global\` so it is visible across sessions, since the
/// service runs in session 0 and the interface does not.
pub const MAPPING_NAME: &str = "Global\\LoadBearTemperature";

/// Security descriptor for the mapping.
///
/// Full control for Local System and Administrators, **read only** for
/// Built-in Users. Granting write to unprivileged callers would let any process
/// feed the interface fabricated temperatures, which is a small thing to get
/// wrong and an irritating one to debug.
pub const MAPPING_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GR;;;BU)";

/// Layout version. Bump on any change to [`SharedTemperature`].
///
/// The reader refuses a version it does not know rather than misinterpreting
/// the bytes, which matters because the two processes are upgraded separately.
pub const LAYOUT_VERSION: u32 = 3;

/// Behaviour revision of the helper.
///
/// Bumped whenever the helper starts producing something the interface should
/// notice, such as per-core zones appearing. The interface compares this
/// against its own expectation and offers an update when they differ.
///
/// This exists because an installed helper is a separate binary with its own
/// lifetime. Without it, a helper that predates a feature keeps running
/// forever and the feature simply never appears, with nothing anywhere saying
/// why. That happened once already.
///
/// Revision 4 publishes package power. Revision 3 read it correctly and then
/// dropped it on the way into the mapping, so an unbumped revision would have
/// left those installations showing no power for ever, silently.
pub const HELPER_REVISION: u32 = 4;

/// Maximum temperature zones carried. Renoir reports one die plus up to eight
/// CCD slots, so sixteen is generous without being wasteful.
pub const MAX_ZONES: usize = 16;

/// A reading older than this is treated as stale and reported as no reading.
///
/// If the helper dies, its last sample stays in shared memory forever. Without
/// a staleness check the interface would happily display a temperature from
/// three hours ago as though it were current.
pub const STALE_AFTER_MS: u64 = 15_000;

/// Milliseconds since the system booted.
///
/// Writer and reader are separate processes, so the timestamp on a reading is
/// only meaningful if both sides read the same clock. Process uptime is not
/// that clock: the first version compared the helper's uptime against the
/// interface's, and every reading looked stale the moment the interface had
/// been running longer than the helper.
///
/// `GetTickCount64` is system uptime, identical in every process on the
/// machine, and needs no privileges.
pub fn now_ms() -> u64 {
    // SAFETY: no arguments, no failure mode, no allocation.
    unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() }
}

/// The published reading.
///
/// `#[repr(C)]` because two independently compiled binaries agree on it only if
/// the layout is fixed.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SharedTemperature {
    pub version: u32,
    /// Revision of the helper that published this. See [`HELPER_REVISION`].
    pub helper_revision: u32,
    /// Seqlock. Odd while a write is in progress.
    pub sequence: u32,
    /// Milliseconds since the helper started, for staleness detection.
    pub timestamp_ms: u64,
    /// Package temperature, or NaN when there is no reading.
    pub package_c: f32,
    /// Package power in watts, or NaN when there is no reading.
    ///
    /// Absent on the first sample after the helper starts, because it is
    /// derived by differencing an energy counter and one reading of an
    /// accumulator is not a rate.
    pub package_watts: f32,
    pub zone_count: u32,
    pub zones: [f32; MAX_ZONES],
    /// Zone labels, NUL padded, one per zone.
    pub zone_labels: [[u8; 8]; MAX_ZONES],
}

impl Default for SharedTemperature {
    fn default() -> Self {
        Self {
            version: LAYOUT_VERSION,
            helper_revision: HELPER_REVISION,
            sequence: 0,
            timestamp_ms: 0,
            package_c: f32::NAN,
            package_watts: f32::NAN,
            zone_count: 0,
            zones: [f32::NAN; MAX_ZONES],
            zone_labels: [[0u8; 8]; MAX_ZONES],
        }
    }
}

impl SharedTemperature {
    /// Package power, if there is a real one.
    pub fn watts(&self) -> Option<f32> {
        if self.package_watts.is_nan() {
            None
        } else {
            Some(self.package_watts)
        }
    }

    /// Package temperature, if there is a real one.
    pub fn package(&self) -> Option<f32> {
        if self.package_c.is_nan() {
            None
        } else {
            Some(self.package_c)
        }
    }

    /// Labelled zones actually present.
    pub fn zone_list(&self) -> Vec<(String, f32)> {
        let n = (self.zone_count as usize).min(MAX_ZONES);
        (0..n)
            .filter(|&i| !self.zones[i].is_nan())
            .map(|i| {
                let raw = &self.zone_labels[i];
                let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
                (
                    String::from_utf8_lossy(&raw[..end]).to_string(),
                    self.zones[i],
                )
            })
            .collect()
    }

    /// Whether the publishing helper is the revision this build expects.
    ///
    /// A stale helper still publishes usable readings, so this is not a
    /// failure. It is the difference between a missing feature and a missing
    /// feature nobody can explain.
    pub fn helper_is_current(&self) -> bool {
        self.helper_revision >= HELPER_REVISION
    }

    /// Whether this reading is recent enough to show.
    pub fn is_fresh(&self, now_ms: u64) -> bool {
        self.version == LAYOUT_VERSION && now_ms.saturating_sub(self.timestamp_ms) <= STALE_AFTER_MS
    }

    /// Write a label into a fixed-width slot, truncating rather than spilling.
    pub fn set_label(slot: &mut [u8; 8], label: &str) {
        slot.fill(0);
        for (i, b) in label.bytes().take(7).enumerate() {
            slot[i] = b;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_default_reading_reports_no_package_temperature() {
        let s = SharedTemperature::default();
        assert!(s.package().is_none());
        assert!(s.zone_list().is_empty());
    }

    #[test]
    fn zones_round_trip_through_their_fixed_width_labels() {
        let mut zones = [f32::NAN; MAX_ZONES];
        zones[0] = 58.4;
        zones[1] = 59.1;
        let mut s = SharedTemperature {
            zone_count: 2,
            zones,
            ..Default::default()
        };
        SharedTemperature::set_label(&mut s.zone_labels[0], "CCD0");
        SharedTemperature::set_label(&mut s.zone_labels[1], "CCD1");

        let list = s.zone_list();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].0, "CCD0");
        assert_eq!(list[0].1, 58.4);
        assert_eq!(list[1].0, "CCD1");
    }

    #[test]
    fn an_overlong_label_is_truncated_rather_than_overflowing() {
        let mut slot = [0u8; 8];
        SharedTemperature::set_label(&mut slot, "a-very-long-zone-name");
        assert_eq!(slot[7], 0, "the slot must stay NUL terminated");
        assert_eq!(&slot[..7], b"a-very-");
    }

    #[test]
    fn the_shared_clock_advances_and_is_the_same_in_any_process() {
        // Boot time rather than process time. If this ever became process
        // uptime again, every reading would go stale as soon as the interface
        // outlived the helper.
        let a = now_ms();
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(now_ms() > a);
        assert!(a > 1_000, "system uptime should be well past a second");
    }

    #[test]
    fn a_stale_reading_is_not_fresh() {
        let s = SharedTemperature {
            timestamp_ms: 1_000,
            ..Default::default()
        };
        assert!(s.is_fresh(1_000 + STALE_AFTER_MS));
        assert!(
            !s.is_fresh(1_000 + STALE_AFTER_MS + 1),
            "a dead helper leaves its last sample behind forever, so age must be checked"
        );
    }

    #[test]
    fn a_helper_predating_this_build_is_detected_as_stale() {
        // The failure this prevents: an installed helper that predates a
        // feature keeps running, the feature never appears, and nothing
        // anywhere says why.
        let old = SharedTemperature {
            helper_revision: HELPER_REVISION - 1,
            ..Default::default()
        };
        assert!(!old.helper_is_current());
        assert!(SharedTemperature::default().helper_is_current());
    }

    #[test]
    fn an_unknown_layout_version_is_never_fresh() {
        // The two processes upgrade separately, so the reader must refuse a
        // layout it does not understand rather than misreading the bytes.
        let s = SharedTemperature {
            version: LAYOUT_VERSION + 1,
            ..Default::default()
        };
        assert!(!s.is_fresh(0));
    }

    #[test]
    fn unprivileged_users_get_read_access_but_not_write() {
        assert!(
            MAPPING_SDDL.contains("(A;;GR;;;BU)"),
            "the interface runs unprivileged and must be able to read"
        );
        assert!(
            !MAPPING_SDDL.contains("(A;;GA;;;BU)") && !MAPPING_SDDL.contains("(A;;GW;;;BU)"),
            "granting write to everyone would let any process fabricate readings"
        );
    }
}
