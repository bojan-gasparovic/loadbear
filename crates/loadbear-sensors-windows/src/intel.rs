//! Intel temperature and TjMax, through the PawnIO `IntelMSR` module.
//!
//! # Interface provenance
//!
//! The exports were read out of the module blob this crate embeds, which is
//! stronger evidence than any document because it is the artefact actually
//! shipped. `modules/IntelMSR.bin` names `ioctl_read_msr` and
//! `ioctl_write_msr`. Cross-checked against `IntelMSR.p` in
//! `github.com/namazso/PawnIO.Modules`, read 2026-08-16:
//!
//! ```text
//! DEFINE_IOCTL_SIZED(ioctl_read_msr, 1, 1)   in[0] = MSR      out[0] = value
//! DEFINE_IOCTL_SIZED(ioctl_write_msr, 2, 0)  unused here, and never will be
//! ```
//!
//! The module validates every address against an allow list of its own, so a
//! register it does not permit fails rather than reading something else.
//!
//! # Register provenance
//!
//! Addresses transcribed from Linux `arch/x86/include/asm/msr-index.h`, read
//! 2026-08-16:
//!
//! ```text
//! #define MSR_IA32_THERM_STATUS           0x0000019c
//! #define MSR_IA32_PACKAGE_THERM_STATUS   0x000001b1
//! #define MSR_IA32_TEMPERATURE_TARGET     0x000001a2
//! ```
//!
//! Decoding transcribed from Linux `drivers/hwmon/coretemp.c`, read the same
//! day, which is the authoritative implementation for these parts.
//!
//! # Why Intel is upside down
//!
//! Intel does not publish a temperature. It publishes the distance below the
//! junction limit, so a reading of 30 means thirty degrees of headroom and the
//! actual temperature is `TjMax - 30`. Everything here depends on TjMax being
//! known, which is why an unreadable TjMax means no temperature at all rather
//! than a temperature measured from a guess.
//!
//! # What is not verified
//!
//! Written and tested on an AMD machine. Every decode is exercised against
//! values built from the published field layout, which is not the same thing as
//! having been read off an Intel part. Two claims in particular are owed a
//! check on real silicon before anyone leans on them:
//!
//! - that per-core pinning reaches distinct cores rather than reporting one
//!   core's reading under eight labels. The pinning itself is verified in
//!   [`crate::topology`]; what is unverified is that PawnIO executes the ioctl
//!   on the calling thread's processor
//! - that TjMax reads as a populated value on the part in question
//!
//! Both fail closed. A wrong or absent TjMax withholds the reading, and
//! [`crate::amd::is_plausible_celsius`] rejects anything outside 5 to 125 C.

use crate::amd::{is_plausible_celsius, TemperatureReading, TemperatureZone};
use crate::pawnio::PawnIo;
use crate::topology;

/// `MSR_IA32_THERM_STATUS`. Per logical processor.
const MSR_THERM_STATUS: u64 = 0x0000_019C;

/// `MSR_IA32_PACKAGE_THERM_STATUS`. One per package.
const MSR_PACKAGE_THERM_STATUS: u64 = 0x0000_01B1;

/// `MSR_IA32_TEMPERATURE_TARGET`.
const MSR_TEMPERATURE_TARGET: u64 = 0x0000_01A2;

/// `Reading Valid`, bit 31 of either thermal status register.
const THERM_STATUS_VALID: u64 = 1 << 31;

/// Lowest TjMax worth believing, in degrees C.
///
/// Zero means the field was never populated, which coretemp.c already guards
/// by falling back to a default of 100 C. This goes further and refuses a
/// small non-zero value too, because TjMax is the origin every Intel reading is
/// measured from and a wrong one silently shifts every temperature on screen.
///
/// No shipping Intel part sets it this low. LB-12 attributed this floor to
/// Intel's own turbostat; that attribution could not be confirmed against the
/// source and is therefore not claimed here. The floor stands on its own as a
/// plausibility guard.
const MIN_PLAUSIBLE_TJMAX_C: f32 = 85.0;

/// Decode TjMax from `MSR_IA32_TEMPERATURE_TARGET`.
///
/// coretemp.c: `val = (msrval >> 16) & 0xff;` then `if (val) return val * 1000;`
///
/// LB-12 specified bits 16 through 22. Both Linux and the field's documented
/// width take bits 16 through 23, so that is what is read here. The two differ
/// only above 127 C, which no part reports, but the wider read is the one with
/// a source behind it.
pub fn tjmax_from_msr(raw: u64) -> Option<f32> {
    let value = ((raw >> 16) & 0xFF) as f32;
    (value >= MIN_PLAUSIBLE_TJMAX_C).then_some(value)
}

/// Decode a thermal status register into an absolute temperature.
///
/// coretemp.c reads the digital readout as the distance below TjMax. The field
/// is bits 16 through 22, and bit 31 says whether the reading is valid at all.
///
/// Current Linux deliberately ignores the valid bit, on the grounds that the
/// register reads low or zero when it is clear. LoadBear checks it anyway: an
/// invalid register decoding to `TjMax - 0`, which is to say the junction
/// limit exactly, would be reported as a machine at its thermal ceiling. That
/// is the single most alarming thing this application can say, and it must not
/// be reachable from a register nobody populated.
pub fn decode_digital_readout(raw: u64, tjmax_c: f32) -> Option<f32> {
    if raw & THERM_STATUS_VALID == 0 {
        return None;
    }
    let below = ((raw >> 16) & 0x7F) as f32;
    let celsius = tjmax_c - below;
    is_plausible_celsius(celsius).then_some(celsius)
}

/// Read TjMax for this package.
///
/// Read from any processor, since the junction limit is a property of the part
/// rather than of a core.
pub fn read_tjmax(pawn: &PawnIo) -> Option<f32> {
    tjmax_from_msr(read_msr(pawn, MSR_TEMPERATURE_TARGET)?)
}

/// Read package and per-core temperature on an Intel part.
///
/// Returns an empty reading rather than an error whenever anything is missing,
/// exactly as the AMD path does, because temperature is optional throughout
/// LoadBear.
pub fn read_intel_temperature(pawn: &PawnIo, logical_processors: usize) -> TemperatureReading {
    let mut reading = TemperatureReading::default();

    // Without TjMax there is no origin to measure from, so there is nothing
    // honest to report.
    let Some(tjmax) = read_tjmax(pawn) else {
        return reading;
    };

    if let Some(raw) = read_msr(pawn, MSR_PACKAGE_THERM_STATUS) {
        reading.package_c = decode_digital_readout(raw, tjmax);
    }

    // One read per logical processor, each pinned. `on_processor` yields
    // nothing unless the thread is observed on the processor asked for, so a
    // core that cannot be reached is skipped rather than filled in with
    // whichever core answered.
    for i in 0..logical_processors {
        let Some(Some(raw)) = topology::on_processor(i as u32, || read_msr(pawn, MSR_THERM_STATUS))
        else {
            continue;
        };
        if let Some(celsius) = decode_digital_readout(raw, tjmax) {
            reading.zones.push(TemperatureZone {
                label: format!("Core {i}"),
                celsius,
            });
        }
    }

    reading
}

fn read_msr(pawn: &PawnIo, msr: u64) -> Option<u64> {
    pawn.execute("ioctl_read_msr", &[msr], 1)
        .ok()
        .and_then(|out| out.first().copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a thermal status register the way the hardware documents it.
    fn therm_status(below_tjmax: u64, valid: bool) -> u64 {
        let mut raw = (below_tjmax & 0x7F) << 16;
        if valid {
            raw |= THERM_STATUS_VALID;
        }
        raw
    }

    #[test]
    fn tjmax_decodes_from_the_documented_field() {
        // 100 C in bits 16 through 23, which is the common desktop value.
        assert_eq!(tjmax_from_msr(100 << 16), Some(100.0));
        assert_eq!(tjmax_from_msr(105 << 16), Some(105.0));
    }

    #[test]
    fn only_the_tjmax_field_is_read() {
        // Real registers carry a TCC activation offset above this field and
        // other status below it. Reading the whole word would be wildly wrong.
        let raw = (0x0Au64 << 24) | (100 << 16) | 0xBEEF;
        assert_eq!(tjmax_from_msr(raw), Some(100.0));
    }

    #[test]
    fn a_zero_register_yields_no_tjmax() {
        // The field is left unpopulated on some parts, and zero would make
        // every core read as its own TjMax below nothing.
        assert_eq!(tjmax_from_msr(0), None);
    }

    #[test]
    fn an_implausibly_low_tjmax_is_refused() {
        // TjMax is the origin every Intel reading is measured from, so a wrong
        // one shifts every temperature on screen rather than failing visibly.
        assert_eq!(tjmax_from_msr(50 << 16), None);
        assert_eq!(tjmax_from_msr(84 << 16), None);
        assert_eq!(tjmax_from_msr(85 << 16), Some(85.0));
    }

    #[test]
    fn temperature_is_tjmax_less_the_digital_readout() {
        // Intel publishes headroom, not temperature. Forty degrees below a
        // hundred is sixty.
        assert_eq!(decode_digital_readout(therm_status(40, true), 100.0), Some(60.0));
        assert_eq!(decode_digital_readout(therm_status(2, true), 100.0), Some(98.0));
    }

    #[test]
    fn an_invalid_reading_is_withheld_rather_than_read_as_the_thermal_limit() {
        // The failure this prevents: bit 31 clear with a zeroed readout decodes
        // to exactly TjMax, so the calmest possible machine would be reported
        // as sitting on its junction limit.
        assert_eq!(decode_digital_readout(therm_status(0, false), 100.0), None);
        assert_eq!(decode_digital_readout(0, 100.0), None);
    }

    #[test]
    fn an_implausible_result_is_rejected_by_the_same_check_the_amd_path_uses() {
        // A readout larger than TjMax would give a negative temperature.
        assert_eq!(decode_digital_readout(therm_status(120, true), 100.0), None);
        // And one at the limit gives TjMax itself, which is plausible and real.
        assert_eq!(decode_digital_readout(therm_status(0, true), 100.0), Some(100.0));
    }

    #[test]
    fn the_readout_field_stops_at_seven_bits() {
        // Bit 23 belongs to neither field on the status registers. Letting it
        // in would put a reading 128 degrees adrift.
        let raw = THERM_STATUS_VALID | (1 << 23) | (40 << 16);
        assert_eq!(decode_digital_readout(raw, 100.0), Some(60.0));
    }

    #[test]
    fn the_addresses_are_the_ones_linux_defines() {
        // Transcription is the easiest thing to get wrong here and the hardest
        // to notice, since a wrong address returns a number rather than an
        // error.
        assert_eq!(MSR_THERM_STATUS, 0x19C);
        assert_eq!(MSR_PACKAGE_THERM_STATUS, 0x1B1);
        assert_eq!(MSR_TEMPERATURE_TARGET, 0x1A2);
    }
}
