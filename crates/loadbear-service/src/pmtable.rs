//! Diagnostic dump of the AMD SMU power management table.
//!
//! # Why
//!
//! The SMN register path gives a die temperature and one reading per core
//! complex die. Renoir has a single CCD, so it yields exactly one value, while
//! Core Temp shows eight. Neither `k10temp`, nor LibreHardwareMonitor's SMN
//! path, nor LHM's PM table structure reads per-core temperature on AMD, so the
//! mechanism Core Temp uses is not documented anywhere readable.
//!
//! The PM table is where per-core data would live. Its layout is version
//! specific and undocumented, so this dumps the raw floats and lets the values
//! be matched against a known-good source rather than guessed at.
//!
//! This is diagnostic scaffolding, not product code. It runs only when the
//! helper is invoked with `--dump-pmtable`.

use loadbear_sensors_windows::pawnio::PawnIo;

const MODULE_RYZEN_SMU: &[u8] =
    include_bytes!("../../loadbear-sensors-windows/modules/RyzenSMU.bin");

/// Ranges worth printing, with what a value in each might be.
///
/// The first pass of this tool printed only 20 to 110, because it was hunting
/// core temperatures and found them. That window hides package power, which on
/// a 15 W part reads in single digits, so the table was dumped once and the
/// power figures were never in the output.
const BANDS: [(&str, f32, f32); 4] = [
    ("power W", 0.5, 65.0),
    ("temp C", 20.0, 110.0),
    ("volts", 0.4, 1.6),
    ("clock MHz", 400.0, 5200.0),
];

pub fn dump() -> i32 {
    let pawn = match PawnIo::open() {
        Ok(p) => p,
        Err(e) => {
            println!("could not open PawnIO: {e}");
            return 1;
        }
    };

    if let Err(e) = pawn.load_module(MODULE_RYZEN_SMU) {
        println!("could not load RyzenSMU: {e}");
        return 1;
    }

    match pawn.execute("ioctl_get_code_name", &[], 1) {
        Ok(v) => println!("code name id : {:?}", v.first()),
        Err(e) => println!("code name    : unavailable ({e})"),
    }
    match pawn.execute("ioctl_get_smu_version", &[], 1) {
        Ok(v) => println!("smu version  : {:#x?}", v.first()),
        Err(e) => println!("smu version  : unavailable ({e})"),
    }

    let resolved = match pawn.execute("ioctl_resolve_pm_table", &[], 2) {
        Ok(v) => v,
        Err(e) => {
            println!("could not resolve the PM table: {e}");
            return 1;
        }
    };
    println!(
        "pm table     : version {:#x}, base {:#x}",
        resolved.first().copied().unwrap_or(0),
        resolved.get(1).copied().unwrap_or(0)
    );

    if let Err(e) = pawn.execute("ioctl_update_pm_table", &[], 0) {
        println!("could not refresh the PM table: {e}");
    }

    // The table length is not reported, so ask for a generous window and take
    // whatever comes back.
    let words = match pawn.execute("ioctl_read_pm_table", &[], 1024) {
        Ok(v) => v,
        Err(e) => {
            println!("could not read the PM table: {e}");
            return 1;
        }
    };
    println!("read back    : {} words", words.len());
    println!();

    // Each 64-bit word carries two 32-bit floats. Print only those in a range
    // that could be a temperature, with their index, so they can be correlated
    // against a known-good reading.
    // Printed per band rather than in one list, so a value can be recognised
    // by what it plausibly is rather than only by where it sits.
    for (label, lo, hi) in BANDS {
        println!("{label} shaped values ({lo} to {hi}), by float index:");
        let mut shown = 0;
        for (i, w) in words.iter().enumerate() {
            for half in 0..2 {
                let bits = (*w >> (32 * half)) as u32;
                let f = f32::from_bits(bits);
                if f.is_finite() && f >= lo && f <= hi {
                    println!("  [{:4}]  {:10.3}", i * 2 + half, f);
                    shown += 1;
                }
            }
        }
        if shown == 0 {
            println!("  none.");
        }
        println!();
    }

    println!("Per-core temperatures are already known: indices 215 to 222 at table");
    println!("version 0x370005. What is wanted now is package power, which should");
    println!("track the wattage another tool reports and should move under load.");
    0
}
