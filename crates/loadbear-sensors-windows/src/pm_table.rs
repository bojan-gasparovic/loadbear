//! Per-core temperature from the AMD SMU power management table.
//!
//! # Why this is separate from the SMN path
//!
//! `AMDFamily17` reads SMN registers, which carry a die temperature and one
//! reading per core complex die. Renoir has a single CCD, so that path yields
//! exactly one value no matter what. Neither Linux `k10temp` nor
//! LibreHardwareMonitor reads per-core temperature on AMD, because neither
//! reads this table.
//!
//! Core Temp does, which is why it shows eight values where they show one.
//!
//! # How the layout was established
//!
//! Not from documentation, because none exists. The table was dumped on a
//! Ryzen 7 4980U on 2026-08-14 and every temperature-shaped float printed with
//! its index. Indices 215 to 222 were an eight-wide block clustered between
//! 57.19 and 57.92 C, and, decisively, their *shape* matched Core Temp's
//! reading at the same moment: cores 0 and 1 warmer than 2 and 3, repeating
//! for 4 through 7.
//!
//! **The layout is specific to PM table version `0x370005`.** Offsets move
//! between versions and between parts, so a version this does not recognise
//! reports nothing rather than reading whatever happens to sit at index 215.
//! Returning a wrong number confidently is worse than returning none.

use crate::pawnio::{PawnIo, PawnIoError};

/// Compiled RyzenSMU module. LGPL-2.1, licence text in `modules/`.
pub const MODULE_RYZEN_SMU: &[u8] = include_bytes!("../modules/RyzenSMU.bin");

/// PM table layout verified on a Ryzen 7 4980U (Renoir).
pub const PM_TABLE_RENOIR: u64 = 0x0037_0005;

/// First float index of the per-core temperature block, for [`PM_TABLE_RENOIR`].
const CORE_TEMP_INDEX: usize = 215;

/// Width of that block.
const CORE_TEMP_COUNT: usize = 8;

/// Words requested from the table. The length is not reported, so ask for a
/// generous window; the module returns what it has.
const TABLE_WORDS: usize = 1024;

/// Reads per-core temperature, when the layout is one we have verified.
pub struct PerCoreTemperature {
    pawn: PawnIo,
    supported: bool,
}

impl PerCoreTemperature {
    /// Open an executor, load RyzenSMU, and check the table layout.
    ///
    /// A separate executor from the SMN path because a PawnIO executor holds
    /// one module at a time.
    pub fn new() -> Result<Self, PawnIoError> {
        let pawn = PawnIo::open()?;
        pawn.load_module(MODULE_RYZEN_SMU)?;

        let resolved = pawn.execute("ioctl_resolve_pm_table", &[], 2)?;
        let version = resolved.first().copied().unwrap_or(0);

        Ok(Self {
            pawn,
            supported: version == PM_TABLE_RENOIR,
        })
    }

    /// Whether this machine's table layout is one we have actually verified.
    pub fn is_supported(&self) -> bool {
        self.supported
    }

    /// Read per-core temperatures.
    ///
    /// Empty on an unverified layout, which is the honest answer rather than a
    /// plausible-looking wrong one.
    pub fn read(&self, core_count: usize) -> Vec<f32> {
        if !self.supported {
            return Vec::new();
        }

        // The table is a snapshot; without a refresh it returns stale values.
        if self.pawn.execute("ioctl_update_pm_table", &[], 0).is_err() {
            return Vec::new();
        }

        let Ok(words) = self.pawn.execute("ioctl_read_pm_table", &[], TABLE_WORDS) else {
            return Vec::new();
        };

        let n = core_count.min(CORE_TEMP_COUNT);
        (0..n)
            .filter_map(|i| float_at(&words, CORE_TEMP_INDEX + i))
            .filter(|c| crate::amd::is_plausible_celsius(*c))
            .collect()
    }
}

/// Extract one 32-bit float from a table returned as 64-bit words.
///
/// Each word carries two floats, low half first.
fn float_at(words: &[u64], float_index: usize) -> Option<f32> {
    let word = words.get(float_index / 2)?;
    let bits = (*word >> (32 * (float_index % 2))) as u32;
    let f = f32::from_bits(bits);
    f.is_finite().then_some(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_unpack_from_the_low_half_of_a_word_first() {
        let lo = 57.92f32.to_bits() as u64;
        let hi = (57.73f32.to_bits() as u64) << 32;
        let words = [lo | hi];
        assert_eq!(float_at(&words, 0), Some(57.92));
        assert_eq!(float_at(&words, 1), Some(57.73));
    }

    #[test]
    fn reading_past_the_end_of_the_table_yields_nothing() {
        assert_eq!(float_at(&[], 215), None);
        assert_eq!(float_at(&[0u64; 4], 215), None);
    }

    #[test]
    fn a_non_finite_word_is_rejected_rather_than_returned() {
        let words = [f32::NAN.to_bits() as u64];
        assert_eq!(float_at(&words, 0), None);
    }

    #[test]
    fn the_verified_layout_is_pinned_to_the_version_it_was_dumped_from() {
        // Offsets move between table versions and parts. Reading index 215 on
        // an unrecognised layout would return a confident wrong number, which
        // is worse than returning none.
        assert_eq!(PM_TABLE_RENOIR, 0x0037_0005);
        assert_eq!(CORE_TEMP_INDEX, 215);
        assert_eq!(CORE_TEMP_COUNT, 8);
    }

    #[test]
    fn the_module_blob_is_present_and_non_trivial() {
        assert!(MODULE_RYZEN_SMU.len() > 10_000);
    }
}
