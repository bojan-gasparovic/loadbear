//! LoadBear helper service.
//!
//! Runs as Local System, reads temperature through PawnIO, and publishes it to
//! shared memory that the unprivileged interface can read.
//!
//! # Why this process exists
//!
//! PawnIO secures its device as `D:P(A;;GA;;;SY)(A;;GA;;;BA)`, so only Local
//! System and Administrators may open it. Temperature therefore cannot be read
//! from an ordinary user process, and asking someone to run a monitoring tool
//! elevated forever is a poor trade.
//!
//! Splitting it means elevation is paid once, when this service is registered,
//! and never again. It is the same arrangement Core Temp and HWiNFO use, and
//! the reason they feel frictionless.
//!
//! This process does as little as possible on purpose. It reads sensors and
//! writes a struct. No diagnosis, no interface, no network. Everything that
//! could be a bug in privileged code lives somewhere unprivileged instead.

mod pmtable;

use std::ffi::OsString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use loadbear_sensors_windows::cpuid::current_cpu_key;
use loadbear_sensors_windows::mapping::TemperaturePublisher;
use loadbear_sensors_windows::shared::{now_ms, SharedTemperature, MAX_ZONES};
use loadbear_sensors_windows::temperature::WindowsTemperature;
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

pub const SERVICE_NAME: &str = "LoadBearHelper";
const SAMPLE_INTERVAL: Duration = Duration::from_millis(1500);

define_windows_service!(ffi_service_main, service_main);

fn main() -> Result<(), windows_service::Error> {
    // Two modes. With `--setup` this is an ordinary elevated program that
    // installs the driver and registers itself, then exits. With no arguments
    // it is the service itself, started by the SCM.
    //
    // Doing setup here rather than in the interface is what keeps it to a
    // single consent prompt: one elevated process does the driver and the
    // service registration together.
    if std::env::args().any(|a| a == "--setup") {
        std::process::exit(setup());
    }
    // The uninstaller's half of the same bargain. It runs elevated, from the
    // installation directory, immediately before the files are removed.
    if std::env::args().any(|a| a == "--teardown") {
        std::process::exit(teardown());
    }
    // Diagnostic scaffolding, not product behaviour. Needs elevation, which is
    // why it lives in the helper rather than the interface.
    // Verifies the RAPL energy counters before they are trusted in the helper.
    // Elevation is required for the driver, which is why this cannot simply be
    // a test in the sensors crate.
    if std::env::args().any(|a| a == "--probe-power") {
        std::process::exit(probe_power());
    }

    if std::env::args().any(|a| a == "--dump-pmtable") {
        std::process::exit(pmtable::dump());
    }
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
}

/// Install the driver if needed, then register and start this service.
///
/// Returns a process exit code, because the caller can only see that.
fn setup() -> i32 {
    use loadbear_sensors_windows::installer::{self, InstallError};

    // An existing PawnIO installation is not a failure. The setup program
    // refuses to install over itself and that is the expected outcome on any
    // second run.
    if let Err(e) = installer::install() {
        match e {
            InstallError::AlreadyInstalled | InstallError::InstallerFailed => {}
            InstallError::Declined => return 2,
            _ => return 3,
        }
    }

    match loadbear_sensors_windows::service_control::install_and_start() {
        Ok(()) => 0,
        Err(_) => 4,
    }
}

/// Stop the service and remove it, so uninstalling LoadBear actually does.
///
/// Always succeeds from the uninstaller's point of view. A machine that has
/// never run setup has nothing to remove, and an uninstall that halts on that
/// would leave the user with a half-removed application and an error they can
/// do nothing about.
fn teardown() -> i32 {
    let _ = loadbear_sensors_windows::service_control::stop_and_remove();
    0
}

fn service_main(_args: Vec<OsString>) {
    let _ = run();
}

fn run() -> Result<(), windows_service::Error> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_handler = stop.clone();

    let status_handle =
        service_control_handler::register(SERVICE_NAME, move |control| match control {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                stop_handler.store(true, Ordering::Relaxed);
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        })?;

    let running = |state: ServiceState, controls: ServiceControlAccept| ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: controls,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    };

    status_handle.set_service_status(running(ServiceState::Running, ServiceControlAccept::STOP))?;

    sample_loop(&stop);

    status_handle.set_service_status(running(
        ServiceState::Stopped,
        ServiceControlAccept::empty(),
    ))?;
    Ok(())
}

/// Read package power a few times and print it, so the numbers can be judged
/// against something else before anything depends on them.
fn probe_power() -> i32 {
    let key = current_cpu_key();
    let mut temp = WindowsTemperature::new(key.as_ref());

    println!("package power from the RAPL energy counters");
    println!("first reading is absent by design: power is a difference, not a level");
    println!();

    // Reads temperature first, exactly as the helper does, so the probe
    // exercises the same call order rather than a friendlier one.
    let mut seen = 0;
    for i in 0..8 {
        let now = now_ms();
        let t = temp.read();
        match temp.package_watts(now) {
            Some(w) => {
                println!("  {i:>2}  {w:>7.2} W   temp {:?}", t.package_c);
                seen += 1;
            }
            None => println!("  {i:>2}        -     temp {:?}", t.package_c),
        }
        std::thread::sleep(Duration::from_millis(1000));
    }

    println!();
    if seen == 0 {
        println!("No readings. Either this part has no RAPL counters or the driver refused.");
        return 1;
    }
    println!("This probe reads the sensor. It does not exercise publishing, which is");
    println!("where package power was once lost on its way into shared memory, so a");
    println!("healthy probe alongside no power in the interface means look at the");
    println!("writer rather than the sensor.");
    0
}

fn sample_loop(stop: &AtomicBool) {
    // If the mapping cannot be created there is nothing useful to do, and
    // failing quietly is better than a service that spins doing nothing.
    let Ok(mut publisher) = TemperaturePublisher::create() else {
        return;
    };

    let key = current_cpu_key();
    let mut temp = WindowsTemperature::new(key.as_ref());

    // A property of the part, not of the moment, so it is read once. On Intel
    // this comes from the silicon and beats anything the database carries; on
    // AMD it is absent and the database stays the only source.
    let tjmax = temp.tjmax_c();

    while !stop.load(Ordering::Relaxed) {
        let now = now_ms();
        let reading = temp.read();
        // Absent on the first pass, since power is the difference between two
        // energy readings and there has only been one.
        let watts = temp.package_watts(now);

        let mut out = SharedTemperature {
            timestamp_ms: now,
            package_c: reading.package_c.unwrap_or(f32::NAN),
            package_watts: watts.unwrap_or(f32::NAN),
            tjmax_c: tjmax.unwrap_or(f32::NAN),
            ..Default::default()
        };

        let n = reading.zones.len().min(MAX_ZONES);
        out.zone_count = n as u32;
        for (i, z) in reading.zones.iter().take(n).enumerate() {
            out.zones[i] = z.celsius;
            SharedTemperature::set_label(&mut out.zone_labels[i], &z.label);
        }

        publisher.publish(&out);

        // Sleep in slices so a stop request is honoured promptly rather than
        // after a full sampling interval.
        for _ in 0..15 {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(SAMPLE_INTERVAL / 15);
        }
    }
}
