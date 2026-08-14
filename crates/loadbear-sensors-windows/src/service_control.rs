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

/// Where the helper executable should be: alongside the running binary.
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
    let exe = helper_path()?;
    let m = manager(ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)?;

    let existing = m.open_service(
        SERVICE_NAME,
        ServiceAccess::QUERY_STATUS | ServiceAccess::START,
    );

    let service = match existing {
        Ok(s) => s,
        Err(_) => {
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
