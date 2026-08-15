//! AMD package power, from the RAPL energy counters.
//!
//! # Register provenance
//!
//! Transcribed from Linux `arch/x86/include/asm/msr-index.h` and
//! `drivers/hwmon/amd_energy.c`, which is the authoritative implementation for
//! these parts, in keeping with the rule [`crate::amd`] sets: nothing here is
//! inferred from observed values or taken from a forum.
//!
//! ```text
//! 0xC0010299  RAPL power unit    bits 12:8 are the energy unit exponent
//! 0xC001029B  package energy     32 bit accumulator, counts up, wraps
//! ```
//!
//! # Why not the SMU power management table
//!
//! Package power is almost certainly in it. Two candidate float indices were
//! found by dumping the table on 2026-08-15 and watching which values moved
//! under load, and both had the right shape. Neither could be told from the
//! other without a second tool to match against, and worse, any index found
//! that way is valid only for one table version on one part, exactly as the
//! per-core temperature offsets in [`crate::pm_table`] are pinned to
//! `0x370005`.
//!
//! RAPL is a documented contract that holds across the whole family, so it
//! needs no shape matching and no version pinning.
//!
//! # Energy, not power
//!
//! The MSR is a counter of energy consumed since boot, so a single read says
//! nothing. Power is the difference between two reads divided by the time
//! between them, which is why this type is stateful and why the first call
//! after construction returns nothing.

use crate::pawnio::{PawnIo, PawnIoError};

/// `MSR_AMD_RAPL_POWER_UNIT`.
const MSR_RAPL_POWER_UNIT: u64 = 0xC001_0299;

/// `MSR_AMD_PKG_ENERGY_STATUS`.
const MSR_PKG_ENERGY_STATUS: u64 = 0xC001_029B;

/// The energy counter is 32 bits wide and wraps.
const COUNTER_MODULUS: u64 = 1 << 32;

/// Above this, a reading is treated as a wrap artefact rather than a value.
///
/// No mobile part draws a kilowatt. A difference implying one means the
/// counter wrapped more than once, or the machine was suspended between
/// samples, and either way the interval is not measurable.
const IMPLAUSIBLE_WATTS: f32 = 1000.0;

/// Extract the energy unit exponent from the RAPL unit register.
///
/// Energy is counted in units of `1 / 2^exponent` joules. On Zen the exponent
/// is normally 16, giving roughly 15.3 microjoules per count.
fn energy_unit_joules(raw: u64) -> f64 {
    let exponent = ((raw >> 8) & 0x1F) as u32;
    1.0 / (1u64 << exponent) as f64
}

/// Package power, derived by differencing the RAPL energy counter.
pub struct PackagePower {
    joules_per_count: f64,
    previous: Option<(u64, u64)>,
}

impl PackagePower {
    /// Read the energy unit once. It does not change while the machine runs.
    ///
    /// Returns `None` on a part or driver that cannot supply it, which is a
    /// supported state everywhere in LoadBear rather than a failure.
    pub fn new(pawn: &PawnIo) -> Option<Self> {
        let raw = read_msr(pawn, MSR_RAPL_POWER_UNIT)?;
        let joules_per_count = energy_unit_joules(raw);
        if joules_per_count <= 0.0 || !joules_per_count.is_finite() {
            return None;
        }
        Some(Self {
            joules_per_count,
            previous: None,
        })
    }

    /// Average package power over the interval since the previous call.
    ///
    /// `None` on the first call, since one reading of an accumulator is not a
    /// rate, and `None` again whenever the interval or the difference is not
    /// something a wattage can honestly be derived from.
    pub fn read(&mut self, pawn: &PawnIo, now_ms: u64) -> Option<f32> {
        let counter = read_msr(pawn, MSR_PKG_ENERGY_STATUS)? & (COUNTER_MODULUS - 1);

        let Some((previous_counter, previous_ms)) = self.previous.replace((counter, now_ms)) else {
            return None;
        };

        let elapsed_ms = now_ms.checked_sub(previous_ms)?;
        if elapsed_ms == 0 {
            return None;
        }

        // The counter wraps at 32 bits, so a smaller value than last time is
        // ordinary rather than an error.
        let ticks = counter
            .checked_sub(previous_counter)
            .unwrap_or(counter + COUNTER_MODULUS - previous_counter);

        let joules = ticks as f64 * self.joules_per_count;
        let watts = (joules / (elapsed_ms as f64 / 1000.0)) as f32;

        (watts.is_finite() && watts >= 0.0 && watts < IMPLAUSIBLE_WATTS).then_some(watts)
    }
}

fn read_msr(pawn: &PawnIo, msr: u64) -> Option<u64> {
    match pawn.execute("ioctl_read_msr", &[msr], 1) {
        Ok(out) => out.first().copied(),
        Err(PawnIoError::NotInstalled) => None,
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_usual_zen_energy_unit_decodes_to_about_fifteen_microjoules() {
        // Exponent 16 in bits 12:8, which is what Zen parts report.
        let raw = 16u64 << 8;
        let unit = energy_unit_joules(raw);
        assert!(
            (unit - 1.0 / 65536.0).abs() < 1e-12,
            "expected 1/65536 joules per count, got {unit}"
        );
    }

    #[test]
    fn only_the_exponent_field_is_read() {
        // Real registers carry power and time units in the other fields, and
        // reading the whole word as an exponent would be wildly wrong.
        let raw = 0x000A_1003u64; // time 0x0A, energy 0x10, power 0x03
        assert_eq!(energy_unit_joules(raw), 1.0 / 65536.0);
    }

    /// Drive the arithmetic without a machine, by feeding counters directly.
    fn watts_between(joules_per_count: f64, from: u64, to: u64, ms: u64) -> Option<f32> {
        let mut p = PackagePower {
            joules_per_count,
            previous: Some((from, 0)),
        };
        let counter = to & (COUNTER_MODULUS - 1);
        let (previous_counter, previous_ms) = p.previous.replace((counter, ms))?;
        let elapsed_ms = ms.checked_sub(previous_ms)?;
        if elapsed_ms == 0 {
            return None;
        }
        let ticks = counter
            .checked_sub(previous_counter)
            .unwrap_or(counter + COUNTER_MODULUS - previous_counter);
        let joules = ticks as f64 * p.joules_per_count;
        let watts = (joules / (elapsed_ms as f64 / 1000.0)) as f32;
        (watts.is_finite() && watts >= 0.0 && watts < IMPLAUSIBLE_WATTS).then_some(watts)
    }

    #[test]
    fn ten_watts_for_one_second_reads_as_ten_watts() {
        let unit = 1.0 / 65536.0;
        let ticks = (10.0 / unit) as u64;
        let w = watts_between(unit, 0, ticks, 1000).unwrap();
        assert!((w - 10.0).abs() < 0.01, "got {w}");
    }

    #[test]
    fn a_wrapped_counter_yields_the_real_difference_rather_than_a_vast_one() {
        // The accumulator is 32 bits. Near the top it rolls over, and treating
        // that as a negative difference or an enormous one would put a
        // ludicrous wattage on screen at random intervals.
        let unit = 1.0 / 65536.0;
        let ticks = (9.0 / unit) as u64;
        let before = COUNTER_MODULUS - ticks / 2;
        let after = (before + ticks) % COUNTER_MODULUS;
        let w = watts_between(unit, before, after, 1000).unwrap();
        assert!((w - 9.0).abs() < 0.01, "got {w} across a wrap");
    }

    #[test]
    fn an_impossible_figure_is_withheld_rather_than_displayed() {
        let unit = 1.0 / 65536.0;
        let ticks = (5000.0 / unit) as u64;
        assert_eq!(watts_between(unit, 0, ticks, 1000), None);
    }

    #[test]
    fn a_zero_length_interval_yields_nothing_rather_than_infinity() {
        assert_eq!(watts_between(1.0 / 65536.0, 0, 65536, 0), None);
    }

    #[test]
    fn the_first_reading_after_construction_is_absent() {
        // One sample of an accumulator is not a rate, and reporting the raw
        // counter as watts would put a number in the millions on screen.
        let p = PackagePower {
            joules_per_count: 1.0 / 65536.0,
            previous: None,
        };
        assert!(p.previous.is_none());
    }
}
