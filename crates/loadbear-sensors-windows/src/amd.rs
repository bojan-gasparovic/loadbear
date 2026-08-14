//! AMD Zen temperature, read over SMN through the PawnIO `AMDFamily17` module.
//!
//! # Interface provenance
//!
//! `AMDFamily17.p` in `github.com/namazso/PawnIO.Modules`, read 2026-08-14.
//! Its `main()` accepts family `0x17` through `0x1A`, so Renoir (family 17h,
//! model 60h) loads. It exports three functions, of which this module uses one:
//!
//! ```text
//! ioctl_read_smn   in[0] = SMN offset          out[0] = value
//! ioctl_read_msr   in[0] = MSR address         out[0] = value
//! ioctl_write_msr  in[0] = MSR, in[1] = value  (unused here, and never will be)
//! ```
//!
//! The module carries an explicit warning on `ioctl_read_smn`: acquire the
//! `Access_PCI` mutant first. SMN reads work by writing an index to PCI config
//! space and then reading a data register, so two callers interleaving will
//! hand each other the wrong answer. [`PciGuard`] enforces that.
//!
//! # Register provenance
//!
//! Offsets and decoding are transcribed from Linux `drivers/hwmon/k10temp.c`,
//! read 2026-08-14, which is the authoritative implementation for these parts.
//! Nothing here was derived from a forum post or inferred from observed values.

use std::time::Duration;

use crate::pawnio::{PawnIo, PawnIoError};

/// `ZEN_REPORTED_TEMP_CTRL_BASE`, k10temp.c.
const SMN_TEMP_BASE: u64 = 0x0005_9800;

/// `ZEN_CUR_TEMP_SHIFT`, k10temp.c.
const CUR_TEMP_SHIFT: u32 = 21;

/// `ZEN_CUR_TEMP_RANGE_SEL_MASK`, BIT(19) in k10temp.c.
const CUR_TEMP_RANGE_SEL: u32 = 1 << 19;

/// `ZEN_CUR_TEMP_TJ_SEL_MASK`, GENMASK(17, 16) in k10temp.c.
const CUR_TEMP_TJ_SEL: u32 = 0b11 << 16;

/// `ccd_offset` for family 17h model 60h (Renoir) and its siblings, k10temp.c.
const RENOIR_CCD_OFFSET: u64 = 0x154;

/// Maximum CCDs probed for Renoir. k10temp calls
/// `k10temp_get_ccd_support(data, 8)` for this model.
const RENOIR_CCD_COUNT: u64 = 8;

/// `ZEN_CCD_TEMP_VALID`, BIT(11) in k10temp.c.
const CCD_TEMP_VALID: u32 = 1 << 11;

/// `ZEN_CCD_TEMP_MASK`, GENMASK(10, 0) in k10temp.c.
const CCD_TEMP_MASK: u32 = 0x7FF;

/// One labelled temperature sensor.
///
/// Deliberately not called a core. AMD exposes a die-level sensor and one
/// reading per core complex die, not one per core, so a `per_core_c` field
/// would have been a lie on this hardware. Intel does expose per-core sensors
/// and fills these zones with them, which is why the label travels with the
/// value.
#[derive(Debug, Clone, PartialEq)]
pub struct TemperatureZone {
    pub label: String,
    pub celsius: f32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TemperatureReading {
    pub package_c: Option<f32>,
    pub zones: Vec<TemperatureZone>,
}

impl TemperatureReading {
    pub fn is_empty(&self) -> bool {
        self.package_c.is_none() && self.zones.is_empty()
    }
}

/// Reject values that cannot be a running CPU temperature.
///
/// A misaddressed register returns a number, not an error. The LB-02 spike
/// caught a zeroed TjMax array exactly this way. Silicon below 5 C or above
/// 125 C while the machine is running means the read is wrong, not that the
/// CPU is remarkable.
pub fn is_plausible_celsius(v: f32) -> bool {
    (5.0..=125.0).contains(&v)
}

/// Decode the die temperature register.
///
/// k10temp `get_raw_temp`:
/// ```c
/// temp = (regval >> ZEN_CUR_TEMP_SHIFT) * 125;
/// if ((regval & data->temp_adjust_mask) ||
///     (regval & ZEN_CUR_TEMP_TJ_SEL_MASK) == ZEN_CUR_TEMP_TJ_SEL_MASK)
///         temp -= 49000;
/// ```
/// where `temp_adjust_mask` is `ZEN_CUR_TEMP_RANGE_SEL_MASK` on every Zen part.
/// Result is millidegrees, converted to degrees here.
pub fn decode_die_temp(regval: u32) -> f32 {
    let mut millideg = (regval >> CUR_TEMP_SHIFT) as i32 * 125;
    if (regval & CUR_TEMP_RANGE_SEL) != 0 || (regval & CUR_TEMP_TJ_SEL) == CUR_TEMP_TJ_SEL {
        millideg -= 49_000;
    }
    millideg as f32 / 1000.0
}

/// Decode a core complex die temperature register.
///
/// k10temp: `*val = (regval & ZEN_CCD_TEMP_MASK) * 125 - 49000`, gated on
/// `ZEN_CCD_TEMP_VALID`. Returns `None` when the valid bit is clear, which is
/// the normal case for CCD slots a given part does not populate.
pub fn decode_ccd_temp(regval: u32) -> Option<f32> {
    if regval & CCD_TEMP_VALID == 0 {
        return None;
    }
    let millideg = (regval & CCD_TEMP_MASK) as i32 * 125 - 49_000;
    Some(millideg as f32 / 1000.0)
}

/// Serialises SMN access against other tools on the machine.
///
/// An SMN read is a write to a PCI index register followed by a read of a data
/// register. Two processes interleaving those steps will each read the other's
/// target. Every hardware monitoring tool on Windows cooperates through this
/// named mutex, so LoadBear does too. Named `Access_PCI` for exactly that
/// reason: the convention predates all of us.
pub struct PciGuard {
    handle: windows_sys::Win32::Foundation::HANDLE,
    acquired: bool,
}

impl PciGuard {
    const NAME: &'static str = "Global\\Access_PCI";
    const TIMEOUT: Duration = Duration::from_millis(500);

    /// Acquire the mutex, or proceed without it after the timeout.
    ///
    /// A stuck peer must not wedge LoadBear's sampling loop forever. Proceeding
    /// risks one bad reading, which the plausibility check catches. Blocking
    /// risks the whole application.
    pub fn acquire() -> Self {
        use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
        use windows_sys::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};

        let name: Vec<u16> = Self::NAME
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: `name` is a NUL terminated UTF-16 buffer that outlives the
        // call. A null return is handled below.
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };

        if handle.is_null() {
            return Self {
                handle,
                acquired: false,
            };
        }

        // SAFETY: `handle` is a valid mutex handle from CreateMutexW.
        let wait = unsafe { WaitForSingleObject(handle, Self::TIMEOUT.as_millis() as u32) };

        Self {
            handle,
            acquired: wait == WAIT_OBJECT_0,
        }
    }
}

impl Drop for PciGuard {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::ReleaseMutex;

        if self.handle.is_null() {
            return;
        }
        // SAFETY: the handle is valid, and the mutex is released only if it was
        // actually acquired.
        unsafe {
            if self.acquired {
                let _ = ReleaseMutex(self.handle);
            }
            let _ = CloseHandle(self.handle);
        }
    }
}

/// Read AMD Zen temperature.
///
/// Returns an empty reading rather than an error when nothing plausible comes
/// back. Temperature is optional throughout LoadBear, so a part this cannot
/// read narrows the product rather than breaking it.
pub fn read_amd_temperature(pawn: &PawnIo) -> Result<TemperatureReading, PawnIoError> {
    let _pci = PciGuard::acquire();
    let mut reading = TemperatureReading::default();

    let die = pawn.execute("ioctl_read_smn", &[SMN_TEMP_BASE], 1)?;
    if let Some(&raw) = die.first() {
        let c = decode_die_temp(raw as u32);
        if is_plausible_celsius(c) {
            reading.package_c = Some(c);
        }
    }

    for ccd in 0..RENOIR_CCD_COUNT {
        let offset = SMN_TEMP_BASE + RENOIR_CCD_OFFSET + ccd * 4;
        let Ok(out) = pawn.execute("ioctl_read_smn", &[offset], 1) else {
            break;
        };
        let Some(&raw) = out.first() else { break };
        if let Some(c) = decode_ccd_temp(raw as u32) {
            if is_plausible_celsius(c) {
                reading.zones.push(TemperatureZone {
                    label: format!("CCD{ccd}"),
                    celsius: c,
                });
            }
        }
    }

    Ok(reading)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn die_temp_decodes_the_documented_shift_and_scale() {
        // 58.5 C with no adjustment: 58500 millideg / 125 = 468 raw units.
        let regval = 468u32 << CUR_TEMP_SHIFT;
        assert_eq!(decode_die_temp(regval), 58.5);
    }

    #[test]
    fn the_range_select_bit_subtracts_the_documented_offset() {
        let base = 468u32 << CUR_TEMP_SHIFT;
        assert_eq!(decode_die_temp(base | CUR_TEMP_RANGE_SEL), 58.5 - 49.0);
    }

    #[test]
    fn both_tj_select_bits_set_also_subtract_the_offset() {
        let base = 468u32 << CUR_TEMP_SHIFT;
        assert_eq!(decode_die_temp(base | CUR_TEMP_TJ_SEL), 58.5 - 49.0);
    }

    #[test]
    fn only_one_tj_select_bit_does_not_subtract() {
        // k10temp requires the full mask, not any bit within it.
        let base = 468u32 << CUR_TEMP_SHIFT;
        assert_eq!(decode_die_temp(base | (1 << 16)), 58.5);
    }

    #[test]
    fn a_ccd_register_without_the_valid_bit_yields_nothing() {
        assert_eq!(decode_ccd_temp(0), None);
        assert_eq!(decode_ccd_temp(0x1FF), None);
    }

    #[test]
    fn a_valid_ccd_register_decodes_with_the_documented_offset() {
        // 58.5 C: (raw * 125 - 49000) = 58500 millideg, so raw = 860.
        let regval = CCD_TEMP_VALID | 860;
        assert_eq!(decode_ccd_temp(regval), Some(58.5));
    }

    #[test]
    fn implausible_temperatures_are_rejected_rather_than_reported() {
        assert!(!is_plausible_celsius(0.0));
        assert!(!is_plausible_celsius(250.0));
        assert!(!is_plausible_celsius(-40.0));
        assert!(is_plausible_celsius(58.4));
    }

    #[test]
    fn a_zeroed_register_decodes_to_something_the_plausibility_check_rejects() {
        // The LB-02 spike's failure mode: a misaddressed register reads as a
        // clean zero and looks like a number rather than an error.
        assert!(!is_plausible_celsius(decode_die_temp(0)));
    }

    #[test]
    fn an_empty_reading_is_representable_rather_than_an_error() {
        let r = TemperatureReading::default();
        assert!(r.is_empty());
        assert!(r.package_c.is_none());
        assert!(r.zones.is_empty());
    }

    #[test]
    fn the_pci_guard_can_be_acquired_and_released_without_a_driver() {
        // The mutex is an ordinary named object. It does not need PawnIO, so
        // this path is testable on a machine with no driver at all.
        let g = PciGuard::acquire();
        assert!(!g.handle.is_null());
        drop(g);
    }
}
