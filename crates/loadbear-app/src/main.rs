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

use loadbear_core::spec::reported_base_mhz;
use loadbear_core::thermal_band;
use loadbear_core::{
    classify, diagnose, evaluate, Assessment, ContainerReading, CpuReading, Reading, Resource,
    SpecDb, ThrottleState, Tier, TierReason, TierTracker, VerdictKind,
};
use loadbear_sensors_windows::baseline::DiskBaseline;
use loadbear_sensors_windows::counters::{to_stall, total_physical_mb, Counters, SampleWindow};
use loadbear_sensors_windows::cpuid::{brand_string, current_cpu_key};
use loadbear_sensors_windows::docker;
use loadbear_sensors_windows::installer;
use loadbear_sensors_windows::mapping::TemperatureReader;
use loadbear_sensors_windows::presentation;
use loadbear_sensors_windows::processes::ProcessSampler;
use loadbear_sensors_windows::service_control;
use loadbear_sensors_windows::shared::{now_ms, SharedTemperature};
use loadbear_sensors_windows::topology;
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

/// Write the learned disk baseline out every Nth tick, so roughly once a
/// minute. Losing a minute of learning to a crash costs a minute of
/// relearning, which does not justify writing a file twice a second.
const BASELINE_SAVE_EVERY: u32 = 120;

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
    /// Absent for any point recorded before this machine's disk baseline had
    /// formed, so the graph does not draw a flat floor and call it quiet.
    io: Option<f32>,
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
    /// Physical cores, from the operating system rather than the database.
    ///
    /// These were read out of the specification database, which meant they
    /// were right on the three processors somebody had typed in and zero on
    /// every other machine in the world. Windows knows them everywhere.
    cores: u32,
    /// Logical processors. Twice `cores` on anything with SMT.
    threads: u32,
    /// The base clock, from the vendor when it is known and from the machine
    /// otherwise. Absent only when neither can supply one.
    base_mhz: Option<u32>,
    /// Rated power. Genuinely database-only, so absent on an unknown part.
    ///
    /// An `Option` rather than a zero, because the interface used to render
    /// "of 0 W rated" on any processor that was not one of the three.
    tdp_watts: Option<u32>,
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
    /// Absent while the disk baseline is still being learned. The interface
    /// says so rather than drawing an empty bar, which would read as a quiet
    /// disk instead of an unanswered question.
    stall_io: Option<f32>,
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
            cores: 0,
            threads: 0,
            base_mhz: None,
            tdp_watts: None,
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
            stall_io: None,
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
/// file is fetched from the official release URL over HTTPS and its Authenticode
/// signature is checked before it is executed. That sentence was untrue until
/// 2026-08-16, when the embedded copy was removed, so do not put one back.
///
/// This is the one step that needs a network connection. Everything else works
/// offline.
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

/// The three shapes the window takes, in logical pixels.
///
/// The first two share a width, so nothing jumps sideways on the desktop when
/// the shape changes and the strip lines up with the panel it came from.
///
/// Their heights include the 28px title bar the page draws for itself. The
/// expanded figure is 28 more than the 560 the layout was measured against,
/// which is what keeps the twelve process rows fitting exactly.
///
/// 124 is measured up from the tiles. Two rows of four are 71.6 tall and take
/// 3px of air on each side, which makes 79.6 the height of both panels; the
/// page's own padding and the title bar are the rest. The graph gave up its
/// heading to fit and still has 47 of plot, which is more than it had when the
/// strip was 44px taller.
const EXPANDED_SIZE: (f64, f64) = (900.0, 588.0);
const COLLAPSED_SIZE: (f64, f64) = (900.0, 124.0);

/// The taskbar strip, in logical pixels.
///
/// 48 is the Windows 11 taskbar height, measured rather than assumed: on
/// 2026-08-16 a 1080 pixel display reported a 1032 pixel work area at 96 DPI.
/// Windows 11 22H2 removed the small taskbar setting, so this does not vary,
/// and Tauri sizes in logical pixels so a scaled display needs no second
/// figure. Do not trim it to look tidier.
///
/// There is no title bar in this shape, so all 48 are content. 360 is the sum
/// of a 28px bear, a sparkline, and a 136px block of tiles, plus the air
/// between them.
///
/// The strip is dragged on top of the taskbar by hand. It is not embedded into
/// it: `IDeskBand` was deprecated in Windows 10 and the Windows 11 taskbar does
/// not host it, and reparenting into `Shell_TrayWnd` attaches the two threads'
/// input queues, so a stall here could stall the shell.
const TASKBAR_SIZE: (f64, f64) = (360.0, 48.0);

/// Whether the window is currently the taskbar strip.
///
/// Read by the thread below, written by `set_mode`. An atomic rather than a
/// channel because there is exactly one bit of state and one reader.
static ON_THE_TASKBAR: AtomicBool = AtomicBool::new(false);

/// How often the strip reclaims its place at the top of the topmost band.
///
/// Fast enough that a person clicking the clock does not lose sight of the
/// temperatures, slow enough to be free. `SetWindowPos` with no move, no
/// resize and no activate is a few microseconds.
const RECLAIM_INTERVAL: Duration = Duration::from_millis(400);

/// Keep the strip above the taskbar, which will not stay under it.
///
/// `alwaysOnTop` is set and is not enough. Topmost is a band, not a position,
/// and inside it ordinary z-order applies. The taskbar is topmost too, and
/// Explorer raises it whenever a person touches it, which puts it in front of
/// a strip sitting on top of it. No arrangement of window flags wins that. The
/// only answer is to raise again.
///
/// **Not `set_always_on_top(true)`**, which was tried first and does nothing at
/// all here. Tao computes the difference between the old flags and the new ones
/// and returns early when there is none, so calling it on a window that is
/// already topmost issues no `SetWindowPos` whatsoever. The raise has to be
/// made directly, which is what `presentation::raise_to_the_front` does.
///
/// Only while the strip is the shape on screen. In the other two shapes this
/// would be fighting the user for the foreground over a window they can move
/// out of the way themselves.
///
/// And never over something full screen. Raising three times a second is
/// precisely what nobody wants over a game, a video or a shared screen: a 48px
/// bar that will not go away and cannot be clicked past.
/// `should_stay_out_of_the_way` asks the shell the same question it asks before
/// showing a notification, which is the right precedent for a monitor.
fn reclaim_the_top(hwnd: isize) {
    std::thread::spawn(move || loop {
        if ON_THE_TASKBAR.load(Ordering::Relaxed) && !presentation::should_stay_out_of_the_way() {
            presentation::raise_to_the_front(hwnd);
        }
        std::thread::sleep(RECLAIM_INTERVAL);
    });
}

/// The shape a mode names, or nothing if it names no mode at all.
///
/// Returning an `Option` rather than falling back to expanded is deliberate. A
/// typo in the page would otherwise resize the window to something plausible
/// and look like a layout bug rather than like the misspelling it is.
fn size_for_mode(mode: &str) -> Option<(f64, f64)> {
    match mode {
        "expanded" => Some(EXPANDED_SIZE),
        "collapsed" => Some(COLLAPSED_SIZE),
        "taskbar" => Some(TASKBAR_SIZE),
        _ => None,
    }
}

/// Switch the window between the full panel, the always on top strip, and the
/// taskbar strip.
///
/// The page has already rearranged itself by the time this runs. Resizing is
/// the half the page cannot do for itself: the window is declared
/// non-resizable so a person cannot drag it to a shape nothing was laid out
/// for, which does not stop the application from choosing one of its own three.
///
/// A string rather than a boolean, because a boolean cannot carry three states
/// and a second boolean would allow a fourth combination that is not a shape.
///
/// Not remembered across restarts. It always opens as the full panel, since
/// that is the shape that can explain itself.
#[tauri::command]
fn set_mode(window: tauri::Window, mode: String) -> Result<(), String> {
    let (width, height) = size_for_mode(&mode).ok_or_else(|| format!("no such mode: {mode}"))?;
    window
        .set_size(tauri::LogicalSize::new(width, height))
        .map_err(|e| e.to_string())?;

    // Only the strip fights the taskbar for the top of the topmost band, and
    // only the strip is meant to. Raising it once here means it wins the moment
    // it appears rather than at the next tick.
    let strip = mode == "taskbar";
    ON_THE_TASKBAR.store(strip, Ordering::Relaxed);
    if strip {
        if let Ok(hwnd) = window.hwnd() {
            presentation::raise_to_the_front(hwnd.0 as isize);
        }
    }
    Ok(())
}

/// Minimise, from the page's own caption buttons.
///
/// Windows will not let an application put a button of its own among the
/// system caption buttons, and the collapse control has to sit beside minimise.
/// So the window is undecorated and the page draws all four. That makes these
/// two commands the cost of the arrangement: with no system chrome there is
/// nothing else left to minimise or dismiss the window.
#[tauri::command]
fn minimize_window(window: tauri::Window) -> Result<(), String> {
    window.minimize().map_err(|e| e.to_string())
}

/// Dismiss to the tray.
///
/// Hides rather than closes, which is what the `CloseRequested` handler already
/// did with the system close button. LoadBear is a resident monitor and quitting
/// stays on the tray menu, deliberately.
#[tauri::command]
fn hide_window(window: tauri::Window) -> Result<(), String> {
    window.hide().map_err(|e| e.to_string())
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
        // Closing the window hides it rather than ending the process.
        //
        // LoadBear is a resident monitor. Its whole proposition is noticing an
        // overload while the user is busy with something else, which it cannot
        // do if the obvious way to get the window off the screen also stops the
        // sampling loop. Quitting stays available, deliberately, on the tray
        // menu.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_status,
            enable_temperature,
            set_mode,
            minimize_window,
            hide_window,
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

            // Idle until the strip is asked for, then keeps it above a taskbar
            // that raises itself every time a person touches it. The handle is
            // taken once, since it is fixed for the life of the window and
            // taking it per tick would be work for nothing.
            if let Some(w) = app.get_webview_window("main") {
                if let Ok(hwnd) = w.hwnd() {
                    reclaim_the_top(hwnd.0 as isize);
                }
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

                // Cores and threads come from the machine, not the database, so
                // they are right on a processor nobody has ever entered. The
                // database is left holding only what Windows cannot report:
                // the rated power band and TjMax.
                let topology = topology::detect();

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

                // What this disk normally does, carried over from previous
                // runs. Disk latency is the one stall signal with no vendor
                // guarantee and no hardware bit behind it, so it is measured
                // against the machine's own history instead of a constant that
                // would be blind on NVMe and deafening on a mechanical disk.
                let mut disk_baseline = DiskBaseline::load();

                loop {
                    let Ok(raw) = counters.sample(SAMPLE_INTERVAL) else {
                        continue;
                    };
                    tick = tick.wrapping_add(1);
                    window.push(raw);
                    let sample = window.average().unwrap_or(raw);
                    let judged = window.median().unwrap_or(raw);

                    // The raw sample, not either reduction. A median of an
                    // averaging window is already smoothed, and what the
                    // baseline wants is what the disk actually did. `observe`
                    // rejects anything that was not quiet with real traffic.
                    disk_baseline.observe(&raw);
                    let disk_saturation = disk_baseline.saturation_point();

                    // Written back rarely. Losing a minute of learning to a
                    // crash costs a minute of relearning, and writing a file
                    // twice a second for that would be absurd.
                    if tick % BASELINE_SAVE_EVERY == 0 {
                        disk_baseline.save_if_changed();
                    }

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
                        stall: to_stall(&judged, logical, disk_saturation),
                        cpu: CpuReading {
                            all_core_mhz: judged.actual_mhz(),
                            // The machine's own base clock, cross-checked
                            // against the brand string. Without this the clock
                            // verdict could only fire on a processor somebody
                            // had entered into the database, which is what
                            // `reported_base_mhz` exists to avoid.
                            reported_base_mhz: reported_base_mhz(
                                judged.processor_frequency_mhz.round() as u32,
                                brand.as_deref(),
                            ),
                            utilization_pct: Some(judged.processor_time_pct as f32),
                            package_watts: published.as_ref().and_then(|s| s.watts()),
                            package_temp_c: published.as_ref().and_then(|s| s.package()),
                            // The silicon first, the database second. Intel
                            // publishes its junction limit in an MSR, which is
                            // a better source than a figure somebody typed in,
                            // and it is the only one available on a part the
                            // database has never seen.
                            tjmax_c: published
                                .as_ref()
                                .and_then(|s| s.tjmax())
                                .or_else(|| spec.as_ref().and_then(|s| s.tjmax_c)),
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
                    let displayed = to_stall(&sample, logical, disk_saturation);
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
                            cores: topology.map(|t| t.physical_cores).unwrap_or(0),
                            threads: topology
                                .map(|t| t.logical_processors)
                                .unwrap_or(logical),
                            // The vendor's published figure first, since it is
                            // the promise being judged, then the machine's own.
                            base_mhz: spec
                                .as_ref()
                                .map(|s| s.base_mhz)
                                .or(reading.cpu.reported_base_mhz),
                            tdp_watts: spec.as_ref().map(|s| s.tdp_watts),
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

    #[test]
    fn every_mode_has_its_own_shape() {
        // Two modes sharing a shape means one of them cannot be seen to have
        // been entered, which reads as the switch being broken.
        assert_ne!(EXPANDED_SIZE, COLLAPSED_SIZE);
        assert_ne!(COLLAPSED_SIZE, TASKBAR_SIZE);
        assert_ne!(EXPANDED_SIZE, TASKBAR_SIZE);
    }

    #[test]
    fn the_taskbar_shape_is_the_windows_11_taskbar_height() {
        // Measured on 2026-08-16: a 1080 pixel display reporting a 1032 pixel
        // work area at 96 DPI. Windows 11 22H2 removed the small taskbar
        // option, so this is not a setting that varies.
        assert_eq!(size_for_mode("taskbar"), Some((360.0, 48.0)));
    }

    #[test]
    fn an_unknown_mode_is_refused_rather_than_defaulted() {
        // Falling back to expanded would turn a typo in the page into a
        // resize that looks like a layout fault instead of a misspelling.
        assert!(size_for_mode("wobbly").is_none());
        assert!(size_for_mode("").is_none());
        assert!(size_for_mode("Expanded").is_none());
    }

    #[test]
    fn the_taskbar_strip_is_the_only_shape_with_no_title_bar() {
        // The other two draw a 28px caption for themselves and carry it in
        // their height. This one gives all 48 pixels to content, so it has to
        // be the shortest by more than the caption it does without.
        let (_, taskbar) = TASKBAR_SIZE;
        let (_, collapsed) = COLLAPSED_SIZE;
        assert!(
            collapsed - taskbar > 28.0,
            "the strip must be shorter than the collapsed shape minus its caption"
        );
    }
}
