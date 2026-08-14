#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! LoadBear desktop application.
//!
//! A sampling thread reads the sensors and runs the diagnosis engine. The tray
//! icon reflects the tier through the bear's posture, and the window shows the
//! detail. Both read the same shared status, so they can never disagree.
//!
//! The bear artwork is a crude placeholder. It stays crude until the tier
//! transitions have been watched firing on a real machine over normal work,
//! because that is the only way to write an honest brief for an illustrator.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use loadbear_core::{classify, evaluate, CpuReading, Reading, SpecDb, ThrottleState, Tier};
use loadbear_sensors_windows::counters::{to_stall, Counters};
use loadbear_sensors_windows::cpuid::{brand_string, current_cpu_key};
use loadbear_sensors_windows::installer;
use loadbear_sensors_windows::mapping::TemperatureReader;
use loadbear_sensors_windows::service_control;
use loadbear_sensors_windows::shared::SharedTemperature;
use serde::Serialize;
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, State};

const SAMPLE_INTERVAL: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone, Serialize)]
struct VerdictView {
    kind: String,
    severity: String,
    detail: String,
    basis: String,
}

#[derive(Debug, Clone, Serialize)]
struct Status {
    tier: String,
    brand: Option<String>,
    matched: Option<String>,
    cores: u8,
    threads: u8,
    base_mhz: u32,
    tdp_watts: u32,
    mhz: Option<u32>,
    logical: u32,
    queue: f64,
    available_mb: f64,
    hard_faults: f64,
    disk_ms: f64,
    disk_queue: f64,
    stall_cpu: f32,
    stall_memory: f32,
    stall_io: f32,
    verdicts: Vec<VerdictView>,
    temp_available: bool,
    /// Whether the unavailable state is one the user can act on.
    temp_offerable: bool,
    temp_summary: String,
    temp_reason: String,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            tier: "Easy".into(),
            brand: None,
            matched: None,
            cores: 0,
            threads: 0,
            base_mhz: 0,
            tdp_watts: 0,
            mhz: None,
            logical: 1,
            queue: 0.0,
            available_mb: 0.0,
            hard_faults: 0.0,
            disk_ms: 0.0,
            disk_queue: 0.0,
            stall_cpu: 0.0,
            stall_memory: 0.0,
            stall_io: 0.0,
            verdicts: vec![],
            temp_available: false,
            temp_offerable: false,
            temp_summary: String::new(),
            temp_reason: String::new(),
        }
    }
}

type Shared = Arc<Mutex<Status>>;

/// Set when temperature needs re-probing, which is after an install.
///
/// The sampling thread owns the temperature source, so the command thread
/// cannot replace it directly. It raises this instead and the loop picks it up
/// on its next tick.
static REPROBE: AtomicBool = AtomicBool::new(false);

#[tauri::command]
fn get_status(state: State<'_, Shared>) -> Status {
    state.lock().map(|s| s.clone()).unwrap_or_default()
}

/// Download, verify and run the official PawnIO installer.
///
/// Blocks until the installer exits, which is what lets the UI report a real
/// outcome rather than an optimistic one. LoadBear redistributes nothing: the
/// file comes from the official release URL and its signature is checked before
/// it is executed.
#[tauri::command]
async fn enable_temperature() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(|| {
        // The driver may already be installed from an earlier attempt, which is
        // not a failure. Only a genuinely unusable outcome should stop us.
        if let Err(e) = installer::install() {
            match e {
                installer::InstallError::AlreadyInstalled
                | installer::InstallError::InstallerFailed => {}
                other => return Err(other.to_string()),
            }
        }
        service_control::install_and_start().map_err(|e| e.to_string())?;
        REPROBE.store(true, Ordering::Relaxed);
        Ok("Temperature enabled.".to_string())
    })
    .await
    .map_err(|_| "the installer could not be started".to_string())?
}

fn tray_icon(tier: Tier) -> Option<Image<'static>> {
    let bytes: &[u8] = match tier {
        Tier::Easy => include_bytes!("../icons/bear-easy-32.png"),
        Tier::Braced => include_bytes!("../icons/bear-braced-32.png"),
        Tier::Strained => include_bytes!("../icons/bear-strained-32.png"),
    };
    Image::from_bytes(bytes).ok()
}

fn main() {
    let shared: Shared = Arc::new(Mutex::new(Status::default()));

    tauri::Builder::default()
        .manage(shared.clone())
        .invoke_handler(tauri::generate_handler![get_status, enable_temperature])
        .setup(move |app| {
            let quit = MenuItem::with_id(app, "quit", "Quit LoadBear", true, None::<&str>)?;
            let show = MenuItem::with_id(app, "show", "Show", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;

            let tray = TrayIconBuilder::with_id("loadbear")
                .icon(tray_icon(Tier::Easy).expect("the placeholder icon must decode"))
                .tooltip("LoadBear")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "quit" => app.exit(0),
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    _ => {}
                })
                .build(app)?;

            let shared = shared.clone();
            let handle = app.handle().clone();

            std::thread::spawn(move || {
                let Ok(counters) = Counters::open() else {
                    eprintln!("LoadBear could not open performance counters.");
                    return;
                };

                let db = SpecDb::embedded().expect("the embedded database must parse");
                let key = current_cpu_key();
                let spec = key.as_ref().and_then(|k| db.lookup(k)).cloned();
                let brand = brand_string();
                let logical = std::thread::available_parallelism()
                    .map(|n| n.get() as u32)
                    .unwrap_or(1);

                // Temperature comes from the helper service through shared
                // memory, never from this process. PawnIO's device admits only
                // SYSTEM and Administrators, so an unprivileged interface
                // cannot read it directly and should not try.
                let started = std::time::Instant::now();
                let mut reader = TemperatureReader::open().ok();

                let mut last_tier = Tier::Easy;

                loop {
                    let Ok(sample) = counters.sample(SAMPLE_INTERVAL) else {
                        continue;
                    };

                    if REPROBE.swap(false, Ordering::Relaxed) || reader.is_none() {
                        reader = TemperatureReader::open().ok();
                    }

                    let now_ms = started.elapsed().as_millis() as u64;
                    let published: Option<SharedTemperature> = reader
                        .as_ref()
                        .and_then(|r| r.read())
                        .filter(|s| s.is_fresh(now_ms.max(s.timestamp_ms)));

                    let (temp_available, temp_offerable, temp_reason) = match &published {
                        Some(_) => (true, false, String::new()),
                        None if service_control::is_running() => (
                            false,
                            false,
                            "The helper is running but has not published a reading yet."
                                .to_string(),
                        ),
                        None => (
                            false,
                            true,
                            "Temperature needs a small background helper, which runs with                              system access so LoadBear itself never has to."
                                .to_string(),
                        ),
                    };

                    let reading = Reading {
                        timestamp_ms: 0,
                        stall: to_stall(&sample, logical),
                        cpu: CpuReading {
                            all_core_mhz: sample.actual_mhz(),
                            package_watts: None,
                            package_temp_c: published.as_ref().and_then(|s| s.package()),
                            tjmax_c: spec.as_ref().and_then(|s| s.tjmax_c),
                            throttle: ThrottleState {
                                asserted: false,
                                reason: None,
                            },
                        },
                        processes: vec![],
                    };

                    let verdicts = evaluate(&reading, spec.as_ref());
                    let tier = classify(&verdicts, &reading.stall);

                    if tier != last_tier {
                        if let Some(icon) = tray_icon(tier) {
                            let _ = tray.set_icon(Some(icon));
                        }
                        let _ = tray.set_tooltip(Some(format!("LoadBear: {tier:?}")));
                        last_tier = tier;
                    }

                    if let Ok(mut s) = shared.lock() {
                        *s = Status {
                            tier: format!("{tier:?}"),
                            brand: brand.clone(),
                            matched: spec.as_ref().map(|s| s.name.clone()),
                            cores: spec.as_ref().map(|s| s.cores).unwrap_or(0),
                            threads: spec.as_ref().map(|s| s.threads).unwrap_or(0),
                            base_mhz: spec.as_ref().map(|s| s.base_mhz).unwrap_or(0),
                            tdp_watts: spec.as_ref().map(|s| s.tdp_watts).unwrap_or(0),
                            mhz: reading.cpu.all_core_mhz,
                            logical,
                            queue: sample.processor_queue_length,
                            available_mb: sample.available_mbytes,
                            hard_faults: sample.pages_input_per_sec,
                            disk_ms: sample.disk_seconds_per_transfer * 1000.0,
                            disk_queue: sample.disk_queue_length,
                            stall_cpu: reading.stall.cpu,
                            stall_memory: reading.stall.memory,
                            stall_io: reading.stall.io,
                            verdicts: verdicts
                                .iter()
                                .map(|v| VerdictView {
                                    kind: format!("{:?}", v.kind),
                                    severity: format!("{:?}", v.severity),
                                    detail: v.detail.clone(),
                                    basis: v.basis.clone(),
                                })
                                .collect(),
                            temp_available,
                            temp_offerable,
                            temp_summary: match published.as_ref().and_then(|s| s.package()) {
                                Some(c) => {
                                    let zones: Vec<String> = published
                                        .as_ref()
                                        .map(|s| s.zone_list())
                                        .unwrap_or_default()
                                        .iter()
                                        .map(|(l, v)| format!("{l} {v:.1} C"))
                                        .collect();
                                    if zones.is_empty() {
                                        format!("{c:.1} C package")
                                    } else {
                                        format!("{c:.1} C package, {}", zones.join(", "))
                                    }
                                }
                                None => "no reading".to_string(),
                            },
                            temp_reason: temp_reason.clone(),
                        };
                    }

                    let _ = &handle;
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("LoadBear failed to start");
}
