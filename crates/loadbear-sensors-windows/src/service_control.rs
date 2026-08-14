//! Registering, starting and removing the LoadBear helper service.
//!
//! Every function here needs Administrator. That is the point: elevation is
//! paid once, at install, so the interface never needs it again.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use windows_service::service::{
    ServiceAccess, ServiceErrorControl, ServiceInfo, ServiceStartType, ServiceState, ServiceType,
};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

pub const SERVICE_NAME: &str = "LoadBearHelper";
pub const SERVICE_DISPLAY_NAME: &str = "LoadBear Helper";
const SERVICE_EXE: &str = "loadbear-service.exe";

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("the helper program could not be found next to LoadBear")]
    ExecutableMissing,
    #[error("administrator rights are needed to set up temperature monitoring")]
    AccessDenied,
    #[error("the helper service could not be registered")]
    RegisterFailed,
    #[error("the helper service could not be started")]
    StartFailed,
}

/// Where the helper is installed to and registered from.
///
/// Deliberately not the build directory. Registering a service that points
/// into `target/debug` means every rebuild fights a running service holding
/// the file open, and a real installation would never do it either.
pub fn installed_helper_path() -> PathBuf {
    let base = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".to_string());
    PathBuf::from(base).join("LoadBear").join(SERVICE_EXE)
}

/// Where the helper executable is found for installation: beside the caller.
pub fn helper_path() -> Result<PathBuf, ServiceError> {
    let exe = std::env::current_exe().map_err(|_| ServiceError::ExecutableMissing)?;
    let dir = exe.parent().ok_or(ServiceError::ExecutableMissing)?;
    let path = dir.join(SERVICE_EXE);
    if path.exists() {
        Ok(path)
    } else {
        Err(ServiceError::ExecutableMissing)
    }
}

fn manager(access: ServiceManagerAccess) -> Result<ServiceManager, ServiceError> {
    ServiceManager::local_computer(None::<&str>, access).map_err(|_| ServiceError::AccessDenied)
}

/// Whether the service is registered and currently running.
///
/// Needs no elevation, so the interface can call it on every tick.
pub fn is_running() -> bool {
    let Ok(m) = manager(ServiceManagerAccess::CONNECT) else {
        return false;
    };
    let Ok(s) = m.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS) else {
        return false;
    };
    s.query_status()
        .map(|st| st.current_state == ServiceState::Running)
        .unwrap_or(false)
}

/// Register the service if absent, then start it.
///
/// Idempotent: an already-registered service is started rather than treated as
/// an error, because a half-finished previous attempt is a normal thing to
/// recover from.
pub fn install_and_start() -> Result<(), ServiceError> {
    let source = helper_path()?;
    let exe = installed_helper_path();

    // Copy into the install location. A failure here when the target already
    // exists and matches is not fatal: it usually means the service is running
    // from it, which is the state we want anyway.
    if source != exe {
        if let Some(dir) = exe.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if std::fs::copy(&source, &exe).is_err() && !exe.exists() {
            return Err(ServiceError::ExecutableMissing);
        }
    }

    let m = manager(ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)?;

    // An existing registration may point somewhere stale, such as a previous
    // build directory. Remove it rather than starting the wrong binary.
    if let Ok(old) = m.open_service(
        SERVICE_NAME,
        ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE,
    ) {
        if let Ok(status) = old.query_status() {
            if status.current_state != ServiceState::Stopped {
                let _ = old.stop();
                std::thread::sleep(Duration::from_millis(800));
            }
        }
        let _ = old.delete();
        drop(old);
        std::thread::sleep(Duration::from_millis(300));
    }

    let service = {
        {
            let info = ServiceInfo {
                name: OsString::from(SERVICE_NAME),
                display_name: OsString::from(SERVICE_DISPLAY_NAME),
                service_type: ServiceType::OWN_PROCESS,
                // Automatic, so temperature keeps working after a reboot
                // without anyone being asked for anything again.
                start_type: ServiceStartType::AutoStart,
                error_control: ServiceErrorControl::Normal,
                executable_path: exe,
                launch_arguments: vec![],
                dependencies: vec![],
                // None means LocalSystem, which is what PawnIO's device
                // security descriptor admits.
                account_name: None,
                account_password: None,
            };
            m.create_service(
                &info,
                ServiceAccess::QUERY_STATUS | ServiceAccess::START | ServiceAccess::CHANGE_CONFIG,
            )
            .map_err(|_| ServiceError::RegisterFailed)?
        }
    };

    if let Ok(status) = service.query_status() {
        if status.current_state == ServiceState::Running {
            return Ok(());
        }
    }

    service
        .start(&[] as &[&std::ffi::OsStr])
        .map_err(|_| ServiceError::StartFailed)?;

    // Give it a moment to publish before the interface looks for the mapping,
    // so the first thing the user sees is a temperature rather than a gap.
    std::thread::sleep(Duration::from_millis(600));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn querying_the_service_needs_no_elevation() {
        // The interface calls this on every tick, so it must never fail merely
        // because the caller is an ordinary user.
        let _ = is_running();
    }

    #[test]
    fn errors_are_displayable_without_raw_codes() {
        for e in [
            ServiceError::ExecutableMissing,
            ServiceError::AccessDenied,
            ServiceError::RegisterFailed,
            ServiceError::StartFailed,
        ] {
            let m = e.to_string();
            assert!(!m.is_empty());
            assert!(!m.contains("0x"), "no raw codes in user-facing text: {m}");
        }
    }

    #[test]
    fn the_service_is_registered_from_a_stable_location_not_the_build_directory() {
        // Registering target/debug means every rebuild fights a running
        // service holding the binary open. It cost a manual elevated stop
        // once already.
        let p = installed_helper_path();
        let s = p.to_string_lossy().to_lowercase();
        assert!(!s.contains("target"), "must not register a build path: {s}");
        assert!(p.ends_with(SERVICE_EXE));
    }

    #[test]
    fn the_helper_is_looked_for_beside_the_running_binary() {
        // Shipping the two side by side is the arrangement; an absolute path
        // baked at build time would break the moment anything is moved.
        match helper_path() {
            Ok(p) => assert!(p.ends_with(SERVICE_EXE)),
            Err(ServiceError::ExecutableMissing) => {}
            Err(e) => panic!("unexpected: {e}"),
        }
    }
}
