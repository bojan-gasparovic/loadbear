//! Attribution against real hardware, from counters to a named cause.
//!
//! The unit tests either side of this one prove the sensor layer reads what it
//! claims and the engine ranks what it is given. Neither proves the two agree
//! about what a percentage means, and that is exactly where an attribution goes
//! wrong quietly: a process share computed per core against a machine figure
//! computed per machine would put coverage at sixteen times its real value and
//! every gate would wave it through.
//!
//! So this drives a known load through the whole path and checks the answer is
//! the process that actually caused it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use loadbear_core::{attribute, CpuReading, Reading, Resource, StallSignal, ThrottleState};
use loadbear_sensors_windows::counters::{to_stall, Counters, SampleWindow};
use loadbear_sensors_windows::processes::ProcessSampler;

/// Occupy every logical processor until told to stop.
fn burn(cores: usize) -> (Arc<AtomicBool>, Vec<thread::JoinHandle<u64>>) {
    let run = Arc::new(AtomicBool::new(true));
    let handles = (0..cores)
        .map(|_| {
            let run = run.clone();
            thread::spawn(move || {
                let mut x: u64 = 0;
                while run.load(Ordering::Relaxed) {
                    x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
                }
                x
            })
        })
        .collect();
    (run, handles)
}

#[test]
fn a_machine_pinned_by_this_test_attributes_the_load_to_this_test() {
    let logical = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let counters = Counters::open().expect("counters must open unprivileged");
    let mut sampler = ProcessSampler::new(logical as u32);
    let mut window = SampleWindow::default();

    let (run, handles) = burn(logical);

    // Prime both samplers, then measure across the same interval so the process
    // shares and the machine figure describe the same window.
    sampler.sample(0);
    let mut reading = None;
    for tick in 1..=3u64 {
        let raw = counters
            .sample(Duration::from_millis(1000))
            .expect("a sample must come back");
        window.push(raw);
        let sample = window.average().expect("a pushed sample must average");
        let processes = sampler.sample(tick * 1000);
        reading = Some(Reading {
            timestamp_ms: tick * 1000,
            // No learned disk baseline in a test run, and none needed: this
            // exercises attribution, which is driven by cpu and memory.
            stall: to_stall(&sample, logical as u32, None),
            cpu: CpuReading {
                all_core_mhz: sample.actual_mhz(),
                reported_base_mhz: None,
                utilization_pct: Some(sample.processor_time_pct as f32),
                package_watts: None,
                package_temp_c: None,
                tjmax_c: None,
                throttle: ThrottleState {
                    asserted: false,
                    reason: None,
                },
            },
            processes,
            containers: vec![],
        });
    }

    run.store(false, Ordering::Relaxed);
    for h in handles {
        let _ = h.join();
    }

    let reading = reading.expect("the loop must produce a reading");
    let utilization = reading.cpu.utilization_pct.unwrap_or(0.0);
    assert!(
        utilization > 70.0,
        "the test failed to load the machine, so it proves nothing about attribution. Utilization was {utilization}"
    );

    let observed: f32 = reading.processes.iter().map(|p| p.cpu_percent).sum();
    assert!(
        observed / utilization > 0.6,
        "process shares and the machine figure disagree about what a percentage means: {observed} observed against {utilization} measured"
    );

    let (cause, _) = attribute(&reading, Resource::Cpu)
        .expect("a machine pinned by one process is exactly the case that must be attributable");

    let me = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().to_string()))
        .expect("the test binary must have a name");
    assert!(
        cause.label.starts_with(&me),
        "the load came from this test binary, so anything else named is a wrong attribution. Got {} rather than {me}",
        cause.label
    );
}

#[test]
fn an_unloaded_machine_is_not_forced_into_naming_something() {
    // The other half of the bar. Attribution that always produces an answer is
    // not attribution, and an idle machine has no culprit to find.
    let logical = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let counters = Counters::open().expect("counters must open unprivileged");
    let mut sampler = ProcessSampler::new(logical as u32);

    sampler.sample(0);
    let sample = counters
        .sample(Duration::from_millis(1000))
        .expect("a sample must come back");
    let processes = sampler.sample(1000);

    let reading = Reading {
        timestamp_ms: 1000,
        stall: StallSignal {
            cpu: 0.0,
            memory: 0.0,
            io: Some(0.0),
        },
        cpu: CpuReading {
            all_core_mhz: sample.actual_mhz(),
            reported_base_mhz: None,
            utilization_pct: Some(sample.processor_time_pct as f32),
            package_watts: None,
            package_temp_c: None,
            tjmax_c: None,
            throttle: ThrottleState {
                asserted: false,
                reason: None,
            },
        },
        processes,
        containers: vec![],
    };

    // Whatever this machine happens to be doing, nothing may be named unless it
    // genuinely dominates. A result either way is acceptable; a name that does
    // not clear the gates is not.
    if let Some((cause, _)) = attribute(&reading, Resource::Cpu) {
        let groups = loadbear_core::group_by_name(&reading.processes);
        let named = groups
            .iter()
            .find(|g| cause.label.starts_with(&g.name))
            .expect("a named cause must correspond to a real process group");
        assert!(
            named.cpu_percent >= 20.0,
            "{} was named while using only {}% of the machine",
            cause.label,
            named.cpu_percent
        );
    }
}
