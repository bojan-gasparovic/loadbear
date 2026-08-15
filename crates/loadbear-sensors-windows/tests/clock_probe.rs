//! Temporary. Clock and utilization, idle then all-core.
use loadbear_sensors_windows::counters::Counters;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[test]
#[ignore]
fn probe() {
    let c = Counters::open().unwrap();
    let logical = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    println!("phase,util%,perf%,nominal,actual_mhz");
    let mut go = |label: &str, n: usize, ticks: usize| {
        let f = Arc::new(AtomicBool::new(true));
        let mut hs = vec![];
        for _ in 0..n {
            let f = f.clone();
            hs.push(std::thread::spawn(move || {
                let mut x: u64 = 0;
                while f.load(Ordering::Relaxed) {
                    x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
                }
                x
            }));
        }
        for _ in 0..ticks {
            let s = c.sample(Duration::from_millis(1000)).unwrap();
            println!(
                "{label},{:.1},{:.1},{:.0},{}",
                s.processor_time_pct,
                s.processor_performance_pct,
                s.processor_frequency_mhz,
                s.actual_mhz().unwrap_or(0)
            );
        }
        f.store(false, Ordering::Relaxed);
        for h in hs {
            let _ = h.join();
        }
    };
    // Does the sustained power budget recover when left alone? STAPM moves
    // over minutes, so this samples once a minute for six with nothing running.
    // Charger comparison. Baseline on the USB-C supply alone was a flat
    // 9.309 W PPT, roughly 1100 to 1200 MHz all-core, and 57 to 58 C.
    go("settle", 0, 5);
    go("ALL-CORE", logical, 40);
    go("after", 0, 8);
}
