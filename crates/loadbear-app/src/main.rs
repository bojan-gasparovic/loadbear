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

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use loadbear_core::thermal_band;
use loadbear_core::{
    classify, diagnose, evaluate, Assessment, ContainerReading, CpuReading, Reading, Resource,
    SpecDb, ThrottleState, Tier, TierReason, TierTracker, VerdictKind,
};
use loadbear_sensors_windows::counters::{to_stall, total_physical_mb, Counters, SampleWindow};
use loadbear_sensors_windows::cpuid::{brand_string, current_cpu_key};
use loadbear_sensors_windows::docker;
use loadbear_sensors_windows::installer;
use loadbear_sensors_windows::mapping::TemperatureReader;
use loadbear_sensors_windows::processes::ProcessSampler;
use loadbear_sensors_windows::service_control;
use loadbear_sensors_windows::shared::{now_ms, SharedTemperature};
use serde::Serialize;
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, State};

/// How often the counters are read.
///
/// Fast enough that "sustained for three seconds" is six observations rather
/// than two, which is what makes a short rule mean anything. Process
/// enumeration does not run this often; see `PROCESS_EVERY`.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(500);

/// Enumerate processes every Nth tick rather than every tick.
///
/// Counters are a cheap read. Opening a hundred and fifty processes is not,
/// and attribution does not need half second resolution to name what is heavy.
const PROCESS_EVERY: u32 = 4;

/// Record a graph point every Nth tick, so five minutes still fits.
const HISTORY_EVERY: u32 = 2;

#[derive(Debug, Clone, Serialize)]
struct VerdictView {
    kind: String,
    severity: String,
    detail: String,
    basis: String,
    /// What is responsible, when the evidence supports naming it.
    ///
    /// Absent is a real and common answer rather than a gap waiting to be
    /// filled, so the interface says so plainly instead of leaving a blank.
    cause: Option<String>,
    /// What the user can do about it, phrased as the thing they do.
    action: Option<String>,
}

/// One tick of the graph.
///
/// Kept per tick rather than reduced, because the graph exists to show the real
/// shape of the last few minutes. The tier is drawn against it so that a spike
/// which did not move the tier is visibly a spike that did not move the tier.
#[derive(Debug, Clone, Copy, Serialize)]
struct HistoryPoint {
    utilization: f32,
    cpu: f32,
    memory: f32,
    io: f32,
    tier: u8,
}

/// Roughly five minutes, at one point per `HISTORY_EVERY` ticks.
const HISTORY_POINTS: usize = 300;

/// How many process groups the running list holds.
///
/// The panel has a fixed height and nothing scrolls, so this is what fits.
const RUNNING_ROWS: usize = 14;

/// One temperature tile.
///
/// The band is decided here rather than in the interface, because which colour
/// a reading deserves is a judgement about hardware and every judgement in
/// LoadBear traces to a published figure. `band` is absent when the part
/// publishes no TjMax, and the tile is then drawn uncoloured rather than
/// against an invented scale.
#[derive(Debug, Clone, Serialize)]
struct ZoneView {
    label: String,
    celsius: f32,
    band: Option<String>,
}

/// One row of the panel that explains the tier.
///
/// Containers are rows here rather than a section of their own. A standing list
/// of containers is an inventory, and it also disappears the moment Docker
/// stops, which in a fixed size window leaves a hole where a panel used to be.
/// As rows they appear underneath Docker exactly when Docker is one of the
/// things responsible, which is the only moment knowing the container helps.
#[derive(Debug, Clone, Serialize)]
struct Contributor {
    name: String,
    cpu: f32,
    memory_mb: f64,
    /// Rendered indented beneath the Docker row that produced it.
    is_container: bool,
    /// Present only for a container with a real limit, which changes the
    /// remedy from stopping it to giving it less.
    limit_mb: Option<f64>,
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
    utilization: f64,
    /// Package power in watts, absent until the helper publishes one.
    watts: Option<f32>,
    /// The floor of the processor's configurable power band, for comparison.
    ctdp_min_watts: u32,
    queue: f64,
    available_mb: f64,
    /// Installed physical memory, so the free figure means something.
    total_mb: f64,
    hard_faults: f64,
    disk_ms: f64,
    disk_queue: f64,
    stall_cpu: f32,
    stall_memory: f32,
    stall_io: f32,
    verdicts: Vec<VerdictView>,
    /// Which resource drove the tier, as a bare word, or empty when nothing
    /// did. The interface uses it to head the contributor list with the thing
    /// being measured rather than a vague pointer at the panel above.
    driver: String,
    /// Why the tier is what it is, in one sentence, always populated.
    ///
    /// The tier can be driven by the stall signal, which produces no verdict at
    /// all, so a red icon with an empty findings list was a reachable and
    /// completely unexplained state. This is the sentence that fixes it.
    reason: String,
    /// The last few minutes, per tick.
    history: Vec<HistoryPoint>,
    /// Everything running, heaviest first.
    ///
    /// Sorted by whichever resource is the bottleneck when there is one, so
    /// the list reorders itself to answer the question being asked rather than
    /// always ranking by processor.
    running: Vec<Contributor>,
    temp_available: bool,
    /// Whether the unavailable state is one the user can act on.
    temp_offerable: bool,
    /// Labelled per-zone readings, so the interface can lay them out rather
    /// than parse them back out of a sentence.
    temp_zones: Vec<ZoneView>,
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
            utilization: 0.0,
            watts: None,
            ctdp_min_watts: 0,
            queue: 0.0,
            available_mb: 0.0,
            total_mb: 0.0,
            hard_faults: 0.0,
            disk_ms: 0.0,
            disk_queue: 0.0,
            stall_cpu: 0.0,
            stall_memory: 0.0,
            stall_io: 0.0,
            verdicts: vec![],
            driver: String::new(),
            reason: "Starting up.".into(),
            history: vec![],
            running: vec![],
            temp_available: false,
            temp_offerable: false,
            temp_zones: vec![],
            temp_reason: String::new(),
        }
    }
}

type Shared = Arc<Mutex<Status>>;

/// The last answer the container runtime gave.
///
/// Behind its own lock and refreshed by its own thread, because asking Docker
/// blocks and the loop that produces the tier must not.
type Containers = Arc<Mutex<Vec<ContainerReading>>>;

/// How often the container list is refreshed.
///
/// Slower than the sampling loop on purpose. Container memory does not move on
/// a one second timescale, and every refresh is one request per container.
const DOCKER_POLL: Duration = Duration::from_secs(10);

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
        // Everything privileged happens inside one elevated child, so the user
        // sees a single consent prompt. Registering a service from this
        // process would fail: the interface is deliberately unprivileged.
        let helper = service_control::helper_path().map_err(|e| e.to_string())?;
        let code = installer::run_elevated_with(&helper, "--setup").map_err(|e| e.to_string())?;

        match code {
            0 => {
                REPROBE.store(true, Ordering::Relaxed);
                Ok("Temperature enabled.".to_string())
            }
            2 => Err("Setup was declined.".to_string()),
            3 => Err("The hardware driver could not be installed.".to_string()),
            4 => Err("The background helper could not be registered.".to_string()),
            _ => Err("Setup did not complete.".to_string()),
        }
    })
    .await
    .map_err(|_| "the installer could not be started".to_string())?
}

/// Where the footer links point.
///
/// Hardcoded rather than passed in from the page. The commands below hand a
/// string to the shell, and a command that accepts a URL from the interface is
/// a command that will one day be handed something else. Nothing here is
/// user-supplied, so nothing needs escaping or trusting.
const REPOSITORY_URL: &str = "https://github.com/bojan-gasparovic/loadbear";
const CONTACT_MAILTO: &str = "mailto:bojan@zeroemdashes.com";

/// Hand a fixed target to whatever the system has registered for it.
///
/// The window is a webview with a `default-src 'self'` policy, so an ordinary
/// link would either be blocked or navigate the application away from itself
/// and leave the user staring at GitHub inside the tray app.
fn open_externally(target: &'static str) -> Result<(), String> {
    std::process::Command::new("explorer")
        .arg(target)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn open_repository() -> Result<(), String> {
    open_externally(REPOSITORY_URL)
}

#[tauri::command]
fn open_contact() -> Result<(), String> {
    open_externally(CONTACT_MAILTO)
}

/// The remediation as a sentence naming the thing the user does.
///
/// Every variant has to end in an action. A finding that cannot be phrased this
/// way has no remediation and is not allowed to interrupt anyone, which is the
/// rule the enum exists to enforce.
fn remediation_text(r: loadbear_core::Remediation) -> String {
    use loadbear_core::Remediation::*;
    match r {
        Stop => "Close it if you are not using it.",
        ReconfigureLimit => "Give it less memory in Docker Desktop settings, or in .wslconfig.",
        AddExclusion => "Add your project folder to its exclusion list so it stops scanning it.",
        Defer => "Pause it until you have finished what you are doing.",
        ChangePowerState => {
            "Check the charger. An underpowered supply limits how hard the processor may work."
        }
        Physical => "Check the vents are clear and the fans are not blocked.",
    }
    .to_string()
}

/// Why the tier is what it is, as a sentence.
///
/// Always says something. A tier with no explanation behind it is an assertion,
/// and an assertion the user cannot check is one they stop believing.
/// Why the tier is what it is, as a sentence.
///
/// Takes both the adopted tier and the window just measured. The adopted tier
/// deliberately lags behind a change; nothing else in the interface does. So
/// when the two disagree this says the state is moving rather than asserting a
/// calm that the bars beside it plainly contradict.
fn reason_text(adopted: Assessment, latest: Assessment, settled: bool) -> String {
    if !settled && adopted.reason == TierReason::Clear && latest.reason == TierReason::Clear {
        return "Starting up.".to_string();
    }

    // A verdict already has a card of its own below, with more detail than
    // this line could carry. Repeating its title above it says the same thing
    // twice in one panel and reads as though two separate things are wrong.
    if latest.tier == adopted.tier && matches!(adopted.reason, TierReason::Verdict(_)) {
        return String::new();
    }

    // The tier has not caught up with the window just measured. Describe what
    // is actually happening, not the tier that is on its way out.
    if latest.tier != adopted.tier {
        let moving = describe(latest.reason);
        return if latest.tier > adopted.tier {
            format!("{moving} Watching to see whether it holds.")
        } else {
            format!("{moving} Easing off.")
        };
    }

    describe(adopted.reason)
}

/// One clause naming what is happening, with no claim about how long for.
fn describe(reason: TierReason) -> String {
    match reason {
        TierReason::Clear => "Nothing is waiting for a resource.".to_string(),
        TierReason::Verdict(kind) => format!(
            "{}.",
            match kind {
                VerdictKind::BelowBaseClock => "Running slower than the manufacturer promises",
                VerdictKind::Throttling => "The processor is holding itself back",
                VerdictKind::PowerOutsideBand => "Power draw outside its rated range",
                VerdictKind::PowerBelowRating =>
                    "The machine is not getting the power it is rated for",
                VerdictKind::ThermalHeadroomLow => "Close to the temperature limit",
            }
        ),
        TierReason::Stall(resource) => format!(
            "{}.",
            match resource {
                Resource::Cpu => "Work is queueing for a free processor",
                Resource::Memory => "Work is stopping to fetch memory from disk",
                Resource::Io => "Work is waiting on the disk",
            }
        ),
    }
}

/// The resource the contributor list is ranked by, as a bare word.
///
/// Empty when nothing is under pressure, which is also when the list is empty.
fn driver_word(assessment: Assessment) -> &'static str {
    match assessment.reason {
        TierReason::Clear => "",
        TierReason::Stall(Resource::Memory) => "memory",
        TierReason::Stall(Resource::Io) => "disk",
        TierReason::Stall(Resource::Cpu) | TierReason::Verdict(_) => "cpu",
    }
}

/// A verdict kind as a person would say it.
///
/// The variant names are how the engine talks about itself and are not how
/// anybody else would. `BelowBaseClock` on screen is the code leaking out.
fn verdict_title(kind: VerdictKind) -> &'static str {
    match kind {
        VerdictKind::BelowBaseClock => "Running below the promised speed",
        VerdictKind::Throttling => "The processor is holding itself back",
        VerdictKind::PowerOutsideBand => "Power draw outside its rated range",
        VerdictKind::PowerBelowRating => "Not getting the power it is rated for",
        VerdictKind::ThermalHeadroomLow => "Close to the temperature limit",
    }
}

/// Everything running, heaviest first, with containers under Docker.
///
/// Ranked by whichever resource is the bottleneck. When nothing is wrong that
/// is the processor, which is what a person means by "what is my machine
/// doing"; when memory is the bottleneck it ranks by memory, so the list
/// answers the question the rest of the window is asking.
///
/// This is not the attribution. A cause is only *named* when the evidence
/// clears the bar in `loadbear_core::attribution`, and this list is always
/// here, so a user can see what LoadBear was looking at when it declined to
/// name anything.
fn running_list(reading: &Reading, assessment: Assessment) -> Vec<Contributor> {
    let resource = match assessment.reason {
        TierReason::Stall(r) => r,
        TierReason::Clear | TierReason::Verdict(_) => Resource::Cpu,
    };

    let mut groups = loadbear_core::group_by_name(&reading.processes);
    match resource {
        Resource::Cpu => groups.sort_by(|a, b| {
            b.cpu_percent
                .total_cmp(&a.cpu_percent)
                .then(b.working_set_bytes.cmp(&a.working_set_bytes))
        }),
        Resource::Memory | Resource::Io => {
            groups.sort_by_key(|g| std::cmp::Reverse(g.working_set_bytes))
        }
    }

    let mut rows = Vec::new();
    for g in groups.iter().take(RUNNING_ROWS) {
        let docker = is_docker_name(&g.name);
        rows.push(Contributor {
            name: if docker {
                "Docker".to_string()
            } else {
                g.label()
            },
            cpu: g.cpu_percent,
            memory_mb: g.working_set_bytes as f64 / 1_048_576.0,
            is_container: false,
            limit_mb: None,
        });

        // The one case where a second source says something the OS cannot.
        // Windows sees one `vmmem` process and can never say which container
        // is inside it, so this is the whole reason the Docker API is read.
        if docker {
            let mut containers: Vec<&ContainerReading> = reading.containers.iter().collect();
            match resource {
                Resource::Cpu => containers.sort_by(|a, b| b.cpu_percent.total_cmp(&a.cpu_percent)),
                _ => containers.sort_by_key(|c| std::cmp::Reverse(c.memory_bytes)),
            }
            rows.extend(containers.iter().take(4).map(|c| Contributor {
                name: c.name.clone(),
                cpu: c.cpu_percent,
                memory_mb: c.memory_bytes as f64 / 1_048_576.0,
                is_container: true,
                limit_mb: c.memory_limit_bytes.map(|l| l as f64 / 1_048_576.0),
            }));
        }
    }
    rows
}

/// Whether a process name means Docker rather than an ordinary application.
///
/// Kept in step with the list attribution uses, since a row that expands into
/// containers and a cause that resolves to one have to agree about what counts
/// as Docker.
fn is_docker_name(name: &str) -> bool {
    let n = name.to_lowercase();
    ["vmmem", "docker desktop", "com.docker"]
        .iter()
        .any(|d| n.starts_with(d))
}

/// The tier's colour for an icon.
///
/// Deliberately not the window's palette. Those colours are chosen against a
/// known light background; an icon sits on a taskbar whose colour LoadBear
/// neither controls nor gets told about, so these are the same three hues
/// lifted far enough to survive a dark one as well.
const fn tier_rgb(tier: Tier) -> (u8, u8, u8) {
    match tier {
        Tier::Easy => (0x3f, 0x9e, 0x6d),
        Tier::Braced => (0xd2, 0x92, 0x1c),
        Tier::Strained => (0xdb, 0x53, 0x42),
    }
}

/// The bear shape at the size wanted.
///
/// The tray is drawn at 16 or 32 pixels and the taskbar at 32 or more,
/// depending on the display. Handing the larger shape to the window means
/// Windows downsamples rather than stretching a 32 pixel image.
fn bear_shape(tier: Tier, large: bool) -> &'static [u8] {
    match (tier, large) {
        (Tier::Easy, false) => include_bytes!("../icons/bear-easy-32.png"),
        (Tier::Easy, true) => include_bytes!("../icons/bear-easy-128.png"),
        (Tier::Braced, false) => include_bytes!("../icons/bear-braced-32.png"),
        (Tier::Braced, true) => include_bytes!("../icons/bear-braced-128.png"),
        (Tier::Strained, false) => include_bytes!("../icons/bear-strained-32.png"),
        (Tier::Strained, true) => include_bytes!("../icons/bear-strained-128.png"),
    }
}

/// Paint a bear shape in its tier's colour.
///
/// The artwork is a white silhouette on transparency, so multiplying by the
/// colour yields that colour exactly while leaving the edge alpha alone.
///
/// Colour carries the tier here, not posture. The three shapes differ by a
/// handful of pixels and at 32 across, let alone 16, that difference is not
/// visible to anyone. Flat white told the user nothing at all.
fn painted(tier: Tier, large: bool) -> Option<Image<'static>> {
    let src = Image::from_bytes(bear_shape(tier, large)).ok()?;
    let (r, g, b) = tier_rgb(tier);

    let mut rgba = src.rgba().to_vec();
    for px in rgba.chunks_exact_mut(4) {
        px[0] = (px[0] as u16 * r as u16 / 255) as u8;
        px[1] = (px[1] as u16 * g as u16 / 255) as u8;
        px[2] = (px[2] as u16 * b as u16 / 255) as u8;
    }

    Some(Image::new_owned(rgba, src.width(), src.height()))
}

fn tray_icon(tier: Tier) -> Option<Image<'static>> {
    painted(tier, false)
}

/// The taskbar button's icon.
///
/// Separate from the tray because Windows keeps a per-window icon, and leaving
/// it unset meant the taskbar showed the bundled application icon for as long
/// as LoadBear ran. That icon is a byte for byte copy of the easy bear, so the
/// taskbar quietly reported an idle machine while the window beside it was red.
fn window_icon(tier: Tier) -> Option<Image<'static>> {
    painted(tier, true)
}

fn main() {
    let shared: Shared = Arc::new(Mutex::new(Status::default()));

    tauri::Builder::default()
        .manage(shared.clone())
        .invoke_handler(tauri::generate_handler![
            get_status,
            enable_temperature,
            open_repository,
            open_contact
        ])
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

            // Paint the taskbar before the first tier change, so it never shows
            // the bundled application icon even briefly.
            if let (Some(w), Some(icon)) =
                (app.get_webview_window("main"), window_icon(Tier::Easy))
            {
                let _ = w.set_icon(icon);
            }

            let shared = shared.clone();
            let handle = app.handle().clone();

            // Docker gets its own thread. A named pipe read has no timeout, so
            // a wedged engine parked on this thread costs a stale container
            // list and nothing else.
            let containers: Containers = Arc::new(Mutex::new(Vec::new()));
            let docker_containers = containers.clone();
            std::thread::spawn(move || loop {
                let fresh = docker::read_containers();
                if let Ok(mut c) = docker_containers.lock() {
                    *c = fresh;
                }
                std::thread::sleep(DOCKER_POLL);
            });

            std::thread::spawn(move || {
                let Ok(counters) = Counters::open() else {
                    eprintln!("LoadBear could not open performance counters.");
                    return;
                };

                let db = SpecDb::embedded().expect("the embedded database must parse");
                let key = current_cpu_key();
                let brand = brand_string();
                let logical = std::thread::available_parallelism()
                    .map(|n| n.get() as u32)
                    .unwrap_or(1);
                // Resolved against the thread count, not looked up by CPUID
                // alone. A whole product line shares one processor id, and
                // taking the first entry that matches would hold this machine
                // to another model's guarantees.
                let resolved = key
                    .as_ref()
                    .and_then(|k| db.resolve(k, logical.min(u8::MAX as u32) as u8));
                let spec = resolved.as_ref().map(|m| m.spec.clone());
                let matched_label = resolved.as_ref().map(|m| m.label());

                // Installed memory does not change while the application runs.
                let total_mb = total_physical_mb().unwrap_or(0.0);

                // Temperature comes from the helper service through shared
                // memory, never from this process. PawnIO's device admits only
                // SYSTEM and Administrators, so an unprivileged interface
                // cannot read it directly and should not try.
                let mut reader = TemperatureReader::open().ok();

                let mut last_tier = Tier::Easy;

                // Two reductions of the same window, for two different jobs.
                // Judgement uses the median, which a lone spike cannot move.
                // Display uses the mean, which moves smoothly enough to watch.
                let mut window = SampleWindow::default();
                let mut process_sampler = ProcessSampler::new(logical);

                // The sustained requirement that `Strained` has always claimed
                // in its documentation and never enforced.
                let mut tracker = TierTracker::default();
                let mut history: VecDeque<HistoryPoint> = VecDeque::new();
                let mut tick: u32 = 0;
                let mut processes = Vec::new();

                loop {
                    let Ok(raw) = counters.sample(SAMPLE_INTERVAL) else {
                        continue;
                    };
                    tick = tick.wrapping_add(1);
                    window.push(raw);
                    let sample = window.average().unwrap_or(raw);
                    let judged = window.median().unwrap_or(raw);

                    if REPROBE.swap(false, Ordering::Relaxed) || reader.is_none() {
                        reader = TemperatureReader::open().ok();
                    }

                    // A helper publishing a layout this build does not know
                    // cannot have its record read at all, so ask the version
                    // separately. It is the first field and has never moved.
                    let layout_mismatch = reader
                        .as_ref()
                        .map(|r| r.published_version() != loadbear_sensors_windows::shared::LAYOUT_VERSION)
                        .unwrap_or(false);

                    let published: Option<SharedTemperature> = reader
                        .as_ref()
                        .and_then(|r| r.read())
                        .filter(|s| s.is_fresh(now_ms()));

                    let (temp_available, temp_offerable, temp_reason) = match &published {
                        _ if layout_mismatch => (
                            false,
                            true,
                            "The background helper is out of date and needs updating."
                                .to_string(),
                        ),
                        // Readings are usable, but the helper predates this
                        // build, so a feature it does not know about would
                        // silently never appear.
                        Some(s) if !s.helper_is_current() => (
                            true,
                            true,
                            "An update is available for the background helper.".to_string(),
                        ),
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

                    let now = now_ms();
                    let reading = Reading {
                        timestamp_ms: now,
                        stall: to_stall(&judged, logical),
                        cpu: CpuReading {
                            all_core_mhz: judged.actual_mhz(),
                            reported_base_mhz: None,
            utilization_pct: Some(judged.processor_time_pct as f32),
                            package_watts: published.as_ref().and_then(|s| s.watts()),
                            package_temp_c: published.as_ref().and_then(|s| s.package()),
                            tjmax_c: spec.as_ref().and_then(|s| s.tjmax_c),
                            throttle: ThrottleState {
                                asserted: false,
                                reason: None,
                            },
                        },
                        processes: {
                            if tick % PROCESS_EVERY == 0 || processes.is_empty() {
                                processes = process_sampler.sample(now);
                            }
                            processes.clone()
                        },
                        containers: containers.lock().map(|c| c.clone()).unwrap_or_default(),
                    };

                    let verdicts = evaluate(&reading, spec.as_ref());
                    // Two steps, deliberately separate. `classify` judges this
                    // window; the tracker decides whether that has held long
                    // enough to be worth showing anybody.
                    let assessment = tracker.observe(classify(&verdicts, &reading.stall), now);
                    let tier = assessment.tier;
                    // Attribution runs over the same reading the verdicts came
                    // from, so a finding can never name a cause measured at a
                    // different moment from the state it explains.
                    let findings = diagnose(&reading, verdicts);

                    // The graph. Recorded from the per tick sample rather than
                    // either reduction, because the point of it is to show the
                    // real shape of the last few minutes, spikes included, next
                    // to a tier that deliberately ignores them.
                    let displayed = to_stall(&sample, logical);
                    if tick % HISTORY_EVERY == 0 {
                        history.push_back(HistoryPoint {
                            utilization: raw.processor_time_pct as f32,
                            cpu: displayed.cpu,
                            memory: displayed.memory,
                            io: displayed.io,
                            tier: tier as u8,
                        });
                        while history.len() > HISTORY_POINTS {
                            history.pop_front();
                        }
                    }

                    if tier != last_tier {
                        if let Some(icon) = tray_icon(tier) {
                            let _ = tray.set_icon(Some(icon));
                        }
                        if let (Some(w), Some(icon)) =
                            (handle.get_webview_window("main"), window_icon(tier))
                        {
                            let _ = w.set_icon(icon);
                        }
                        let _ = tray.set_tooltip(Some(format!("LoadBear: {tier:?}")));
                        last_tier = tier;
                    }

                    if let Ok(mut s) = shared.lock() {
                        *s = Status {
                            tier: format!("{tier:?}"),
                            brand: brand.clone(),
                            matched: matched_label.clone(),
                            cores: spec.as_ref().map(|s| s.cores).unwrap_or(0),
                            threads: spec.as_ref().map(|s| s.threads).unwrap_or(0),
                            base_mhz: spec.as_ref().map(|s| s.base_mhz).unwrap_or(0),
                            tdp_watts: spec.as_ref().map(|s| s.tdp_watts).unwrap_or(0),
                            mhz: reading.cpu.all_core_mhz,
                            logical,
                            utilization: sample.processor_time_pct,
                            watts: reading.cpu.package_watts,
                            ctdp_min_watts: spec
                                .as_ref()
                                .and_then(|s| s.ctdp_min_watts)
                                .unwrap_or(0),
                            queue: sample.processor_queue_length,
                            available_mb: sample.available_mbytes,
                            total_mb,
                            hard_faults: sample.pages_input_per_sec,
                            disk_ms: sample.disk_seconds_per_transfer * 1000.0,
                            disk_queue: sample.disk_queue_length,
                            stall_cpu: reading.stall.cpu,
                            stall_memory: reading.stall.memory,
                            stall_io: reading.stall.io,
                            verdicts: findings
                                .iter()
                                .map(|f| VerdictView {
                                    kind: verdict_title(f.verdict.kind).to_string(),
                                    severity: format!("{:?}", f.verdict.severity),
                                    detail: f.verdict.detail.clone(),
                                    basis: f.verdict.basis.clone(),
                                    cause: f.cause.as_ref().map(|c| c.label.clone()),
                                    action: f.remediation.map(remediation_text),
                                })
                                .collect(),
                            driver: driver_word(tracker.latest()).to_string(),
                            reason: reason_text(assessment, tracker.latest(), window.is_settled()),
                            history: history.iter().copied().collect(),
                            running: running_list(&reading, assessment),
                            temp_available,
                            temp_offerable,
                            temp_zones: published
                                .as_ref()
                                .map(|s| s.zone_list())
                                .unwrap_or_default()
                                .into_iter()
                                .map(|(label, celsius)| ZoneView {
                                    label,
                                    celsius,
                                    band: thermal_band(celsius, reading.cpu.tjmax_c)
                                        .map(|b| format!("{b:?}").to_lowercase()),
                                })
                                .collect(),
                            temp_reason: temp_reason.clone(),
                        };
                    }
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("LoadBear failed to start");
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIERS: [Tier; 3] = [Tier::Easy, Tier::Braced, Tier::Strained];

    /// The colour of the first fully opaque pixel, which is the bear itself.
    fn body_colour(image: &Image<'_>) -> (u8, u8, u8) {
        let rgba = image.rgba();
        let px = rgba
            .chunks_exact(4)
            .find(|p| p[3] == 255)
            .expect("the silhouette must have opaque pixels");
        (px[0], px[1], px[2])
    }

    #[test]
    fn every_icon_is_painted_in_its_own_tier_colour() {
        // The regression: all three shipped as flat white on transparency, so
        // the icon said nothing about the tier and barely rendered at all.
        for large in [false, true] {
            for tier in TIERS {
                let image = painted(tier, large).expect("the icon must decode");
                assert_eq!(
                    body_colour(&image),
                    tier_rgb(tier),
                    "{tier:?} at large={large} is not wearing its own colour"
                );
            }
        }
    }

    #[test]
    fn the_three_tiers_are_told_apart_by_colour_rather_than_shape() {
        // The shapes differ by a handful of pixels, which is invisible at the
        // sizes a tray and a taskbar actually draw. Colour has to do the work,
        // so no two tiers may share one.
        let colours: Vec<_> = TIERS.iter().map(|t| tier_rgb(*t)).collect();
        for (i, a) in colours.iter().enumerate() {
            for b in colours.iter().skip(i + 1) {
                assert_ne!(a, b, "two tiers share a colour");
            }
        }
    }

    #[test]
    fn painting_leaves_the_transparent_background_alone() {
        // Multiplying must not turn transparency into a black square, which is
        // what a taskbar would show if alpha were touched.
        let image = painted(Tier::Strained, false).expect("the icon must decode");
        let clear = image.rgba().chunks_exact(4).filter(|p| p[3] == 0).count();
        assert!(clear > 0, "the silhouette must keep a transparent surround");
    }

    #[test]
    fn the_taskbar_gets_a_larger_shape_than_the_tray() {
        // Windows draws the taskbar button well above 32 pixels on a scaled
        // display, and stretching the tray image there looks like a mistake.
        let tray = tray_icon(Tier::Easy).expect("the icon must decode");
        let window = window_icon(Tier::Easy).expect("the icon must decode");
        assert!(
            window.width() > tray.width(),
            "the window icon must not be the tray icon"
        );
    }
}
