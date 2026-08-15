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

/// How many snapshots to take, and how far apart.
///
/// A limit and a measurement look identical in one snapshot. What separates
/// them is that one moves. This is the whole reason the mode exists.
const SAMPLES: usize = 10;
const INTERVAL_MS: u64 = 1200;

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

    // Take several snapshots rather than one. In a single dump a hard limit
    // and a live measurement are indistinguishable: both are just a float in a
    // plausible range. Across snapshots one holds still and the other does not,
    // which is the only evidence available for telling them apart.
    println!("sampling {SAMPLES} times, {INTERVAL_MS} ms apart. Load the machine now.");
    println!();

    let mut series: Vec<Vec<f32>> = Vec::new();
    for _ in 0..SAMPLES {
        if pawn.execute("ioctl_update_pm_table", &[], 0).is_err() {
            println!("could not refresh the PM table");
            return 1;
        }
        let Ok(words) = pawn.execute("ioctl_read_pm_table", &[], 1024) else {
            println!("could not read the PM table");
            return 1;
        };
        let floats: Vec<f32> = words
            .iter()
            .flat_map(|w| [f32::from_bits(*w as u32), f32::from_bits((*w >> 32) as u32)])
            .collect();
        series.push(floats);
        std::thread::sleep(std::time::Duration::from_millis(INTERVAL_MS));
    }

    let width = series.iter().map(|s| s.len()).min().unwrap_or(0);
    println!("read back    : {width} floats per snapshot");
    println!();

    // Anything that never moved is a limit, a constant, or unused, and none of
    // those are what is being looked for.
    println!("power-shaped values that MOVED across the run:");
    println!(
        "  {:>5}  {:>9}  {:>9}  {:>9}  {:>7}",
        "index", "min", "max", "last", "swing"
    );
    let mut found = 0;
    for i in 0..width {
        let vals: Vec<f32> = series.iter().map(|s| s[i]).collect();
        if vals.iter().any(|v| !v.is_finite()) {
            continue;
        }
        let min = vals.iter().copied().fold(f32::INFINITY, f32::min);
        let max = vals.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        if !(0.2..=65.0).contains(&min) || max > 65.0 {
            continue;
        }
        let swing = max - min;
        // A measurement of a 15 W part swings by watts. Noise does not.
        if swing < 0.75 {
            continue;
        }
        println!(
            "  {:>5}  {:>9.3}  {:>9.3}  {:>9.3}  {:>7.3}",
            i,
            min,
            max,
            vals[vals.len() - 1],
            swing
        );
        found += 1;
    }
    if found == 0 {
        println!("  none moved. Either the machine stayed idle or the table did not refresh.");
    }
    println!();

    println!("the two leading candidates, whatever they did:");
    for i in [0usize, 1] {
        let vals: Vec<f32> = series.iter().map(|s| s[i]).collect();
        let shown: Vec<String> = vals.iter().map(|v| format!("{v:.3}")).collect();
        println!("  [{i:>3}]  {}", shown.join("  "));
    }
    println!();

    println!("Package power is a single value that rises and falls with load and");
    println!("sits in single digits to low tens of watts on this part. A value that");
    println!("held still is a limit. Index 1 is expected to be it, with index 0 its");
    println!("limit, but the movement is the evidence rather than the position.");
    0
}
