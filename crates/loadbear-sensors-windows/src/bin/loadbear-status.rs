//! A terminal readout of what LoadBear currently knows about this machine.
//!
//! Not the product. The product is a resident tray application, and this exists
//! so the diagnosis engine can be watched working against real hardware before
//! any of that is built.
//!
//! Run once:      cargo run -p loadbear-sensors-windows --bin loadbear-status
//! Run watching:  cargo run -p loadbear-sensors-windows --bin loadbear-status -- --watch

use std::time::Duration;

use loadbear_core::{
    classify, evaluate, CpuReading, Reading, SpecDb, ThrottleState, Tier, Verdict,
};
use loadbear_sensors_windows::counters::{to_stall, CounterSample, Counters, SampleWindow};
use loadbear_sensors_windows::cpuid::{brand_string, current_cpu_key};
use loadbear_sensors_windows::pawnio::{PawnIo, PawnIoError};
use loadbear_sensors_windows::processes::ProcessSampler;
use loadbear_sensors_windows::shared::now_ms;

const SAMPLE_INTERVAL: Duration = Duration::from_millis(1000);

fn main() {
    let watch = std::env::args().any(|a| a == "--watch");

    let counters = match Counters::open() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Could not read performance counters: {e}");
            std::process::exit(1);
        }
    };

    let db = SpecDb::embedded().expect("the embedded specification database must parse");
    let key = current_cpu_key();
    let spec = key.as_ref().and_then(|k| db.lookup(k));
    let logical = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1);

    // Temperature is optional and absent by default, because LoadBear ships no
    // driver. Probe once rather than on every tick.
    let temp_status = probe_temperature();

    // Averaged for the same reason the application averages: a single reading
    // of the run queue is noise, and this is the tool used to watch the engine
    // behave, so it must not show something the engine would never see.
    let mut window = SampleWindow::default();
    let mut processes = ProcessSampler::new(logical);

    loop {
        let raw = match counters.sample(SAMPLE_INTERVAL) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Sampling failed: {e}");
                break;
            }
        };
        window.push(raw);
        let sample = window.average().unwrap_or(raw);

        let reading = Reading {
            timestamp_ms: 0,
            stall: to_stall(&sample, logical),
            cpu: CpuReading {
                all_core_mhz: sample.actual_mhz(),
                utilization_pct: Some(sample.processor_time_pct as f32),
                package_watts: None,
                package_temp_c: None,
                tjmax_c: spec.and_then(|s| s.tjmax_c),
                throttle: ThrottleState {
                    asserted: false,
                    reason: None,
                },
            },
            processes: processes.sample(now_ms()),
            containers: vec![],
        };

        let verdicts = evaluate(&reading, spec);
        let tier = classify(&verdicts, &reading.stall);

        if watch {
            print!("\x1B[2J\x1B[H");
        }
        render(
            &reading,
            &sample,
            spec,
            &verdicts,
            tier,
            &temp_status,
            logical,
        );

        if !watch {
            break;
        }
    }
}

fn probe_temperature() -> String {
    match PawnIo::open() {
        Ok(_) => "driver present, reading not wired up yet".to_string(),
        Err(PawnIoError::NotInstalled) => {
            "unavailable. The PawnIO driver is not installed.\n             \
             Install it from https://pawnio.eu to enable temperature."
                .to_string()
        }
        Err(e) => format!("unavailable. {e}"),
    }
}

#[allow(clippy::too_many_arguments)]
fn render(
    reading: &Reading,
    sample: &CounterSample,
    spec: Option<&loadbear_core::CpuSpec>,
    verdicts: &[Verdict],
    tier: Tier,
    temp_status: &str,
    logical: u32,
) {
    let badge = match tier {
        Tier::Easy => "EASY",
        Tier::Braced => "BRACED",
        Tier::Strained => "STRAINED",
    };

    println!();
    println!("  LoadBear                                       [ {badge} ]");
    println!();

    if let Some(b) = brand_string() {
        println!("  {b}");
    }
    match spec {
        Some(s) => println!(
            "  matched to {} ({}C/{}T, base {} MHz, {} W)",
            s.name, s.cores, s.threads, s.base_mhz, s.tdp_watts
        ),
        None => {
            println!("  not in the specification database, so published limits are unavailable")
        }
    }
    println!();

    match (reading.cpu.all_core_mhz, spec) {
        (Some(mhz), Some(s)) => println!(
            "  Clock      {mhz} MHz sustained            base {} MHz guaranteed",
            s.base_mhz
        ),
        (Some(mhz), None) => println!("  Clock      {mhz} MHz sustained"),
        (None, _) => println!("  Clock      unavailable"),
    }

    println!(
        "  Stall      cpu {:>3}%   memory {:>3}%   io {:>3}%",
        (reading.stall.cpu * 100.0).round(),
        (reading.stall.memory * 100.0).round(),
        (reading.stall.io * 100.0).round()
    );
    println!(
        "  Queue      {:.1} threads waiting across {logical} logical processors",
        sample.processor_queue_length
    );
    println!(
        "  Memory     {:.0} MB available, {:.0} hard faults/sec",
        sample.available_mbytes, sample.pages_input_per_sec
    );
    println!(
        "  Disk       {:.2} ms per transfer, queue {:.1}",
        sample.disk_seconds_per_transfer * 1000.0,
        sample.disk_queue_length
    );
    println!("  Temp       {temp_status}");
    println!();

    if verdicts.is_empty() {
        println!("  Nothing to report.");
    } else {
        println!("  Verdicts");
        for v in verdicts {
            println!();
            println!("  ! {:?}  [{:?}]", v.kind, v.severity);
            println!("    {}", v.detail);
            println!("    basis: {}", v.basis);
        }
    }
    println!();
}
