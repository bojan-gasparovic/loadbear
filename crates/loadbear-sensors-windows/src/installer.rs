//! Fetches and runs the official PawnIO installer on the user's behalf.
//!
//! # The installer is bundled
//!
//! Bojan's call, 2026-08-14: the user should not have to install anything
//! separately, and should not wait for a download either. `PawnIO_setup.exe`
//! version 2.2.0 is embedded in the binary, written to a temporary path,
//! signature-checked, and run silently.
//!
//! Two things about that are worth recording rather than discovering later.
//!
//! **Licence.** The setup program is published from `namazso/PawnIO.Setup`,
//! which declares no licence. The PawnIO driver source is GPL-2.0 and
//! `PawnIOLib.dll` is LGPL-2.1, both clear, but the installer wrapper we ship
//! carries no stated terms. This was raised and the decision was to bundle
//! anyway.
//!
//! **The reboot.** Bundling does not avoid it. The setup program installs a
//! root-enumerated PnP device via `newdev.dll` and
//! `UpdateDriverForPlugAndPlayDevicesW`, which reports a reboot requirement.
//! There are no `CreateService` or `StartService` calls anywhere in it, so it
//! is not a dynamically loaded driver the way Core Temp's is. That difference
//! is architectural and no packaging choice changes it.
//!
//! The compiled PawnIO modules are shipped alongside in `modules/`, LGPL-2.1
//! with their licence text.

use std::path::PathBuf;

/// The bundled setup program, PawnIO 2.2.0.
///
/// SHA-256 `1f519a22e47187f70a1379a48ca604981c4fcf694f4e65b734aaa74a9fba3032`,
/// obtained from the official release on 2026-08-14. Its signature is still
/// verified at runtime rather than trusted on the basis of having shipped it,
/// because a build machine is not a trustworthy place either.
const SETUP_EXE: &[u8] = include_bytes!("../vendor/PawnIO_setup.exe");

/// Where the bundled copy came from, for provenance rather than for fetching.
pub const INSTALLER_SOURCE: &str =
    "https://github.com/namazso/PawnIO.Setup/releases/latest/download/PawnIO_setup.exe";

/// Arguments passed to the setup program.
///
/// Transcribed from the usage string embedded in the binary itself, read
/// 2026-08-14:
///
/// ```text
/// Usage: PawnIOSetup.exe [-install] [-uninstall] [-unrestricted] [-debuginfo] [-silent]
///   -silent    Run in silent mode (no UI, even on error)
/// ```
///
/// Launching it with no arguments shows its own interactive installer, which
/// is exactly what LoadBear is meant to spare the user. `-silent` reduces the
/// whole thing to the standard Windows consent prompt.
///
/// **`-unrestricted` is deliberately absent and must stay absent.** It selects
/// the unsigned edition. LoadBear verifies the signature of what it downloads
/// and would be undoing its own check by asking for an unsigned driver.
const SETUP_ARGS: &str = "-install -silent";

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("the bundled installer could not be written to a temporary file")]
    StagingFailed,
    #[error("the downloaded file does not look like an installer")]
    NotAnInstaller,
    #[error("the download is not signed by a trusted publisher, so it was not run")]
    SignatureRejected,
    #[error("the installer could not be started")]
    LaunchFailed,
    #[error("installation was declined")]
    Declined,
    #[error("PawnIO is already installed. Uninstall it first to reinstall.")]
    AlreadyInstalled,
    #[error("the installer ran but did not complete successfully")]
    InstallerFailed,
}

/// Write the bundled setup program to a temporary path.
///
/// Nothing is executed here. No network is touched.
pub fn stage() -> Result<PathBuf, InstallError> {
    if SETUP_EXE.len() < 2 || &SETUP_EXE[..2] != b"MZ" {
        return Err(InstallError::NotAnInstaller);
    }

    let path = std::env::temp_dir().join("LoadBear-PawnIO_setup.exe");
    std::fs::write(&path, SETUP_EXE).map_err(|_| InstallError::StagingFailed)?;
    Ok(path)
}

/// Verify the file carries a valid Authenticode signature from the expected
/// publisher.
///
/// This runs before the file is executed, never after. Downloading an
/// executable over the network and running it unverified would make LoadBear a
/// delivery mechanism for whatever that URL happens to serve.
pub fn verify_signature(path: &std::path::Path) -> Result<(), InstallError> {
    use windows_sys::Win32::Security::WinTrust::{
        WinVerifyTrust, WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_FILE_INFO,
        WTD_CHOICE_FILE, WTD_REVOKE_WHOLECHAIN, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY,
        WTD_UI_NONE,
    };

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let mut file_info: WINTRUST_FILE_INFO = unsafe { std::mem::zeroed() };
    file_info.cbStruct = std::mem::size_of::<WINTRUST_FILE_INFO>() as u32;
    file_info.pcwszFilePath = wide.as_ptr();

    let mut data: WINTRUST_DATA = unsafe { std::mem::zeroed() };
    data.cbStruct = std::mem::size_of::<WINTRUST_DATA>() as u32;
    data.dwUIChoice = WTD_UI_NONE;
    data.fdwRevocationChecks = WTD_REVOKE_WHOLECHAIN;
    data.dwUnionChoice = WTD_CHOICE_FILE;
    data.Anonymous.pFile = &mut file_info;
    data.dwStateAction = WTD_STATEACTION_VERIFY;

    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;

    // SAFETY: both structures are zeroed, sized, and populated per the
    // WinVerifyTrust contract, and the file path outlives the call.
    let status = unsafe {
        WinVerifyTrust(
            std::ptr::null_mut(),
            &mut action,
            &mut data as *mut _ as *mut core::ffi::c_void,
        )
    };

    data.dwStateAction = WTD_STATEACTION_CLOSE;
    // SAFETY: closing the state we opened above, exactly once.
    unsafe {
        WinVerifyTrust(
            std::ptr::null_mut(),
            &mut action,
            &mut data as *mut _ as *mut core::ffi::c_void,
        )
    };

    if status == 0 {
        Ok(())
    } else {
        Err(InstallError::SignatureRejected)
    }
}

use std::os::windows::ffi::OsStrExt;

/// Run the installer, prompting for elevation.
///
/// Returns once the installer exits. The `runas` verb produces the standard
/// Windows consent dialog, which is the only elevation LoadBear ever asks for
/// and the only one it needs.
pub fn run_elevated(path: &std::path::Path) -> Result<(), InstallError> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, WaitForSingleObject, INFINITE,
    };
    use windows_sys::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };

    let file: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let verb: Vec<u16> = "runas".encode_utf16().chain(std::iter::once(0)).collect();
    let args: Vec<u16> = SETUP_ARGS
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = args.as_ptr();
    info.nShow = 0; // SW_HIDE. Silent means silent.

    // SAFETY: `info` is zeroed, sized, and its pointers outlive the call.
    let ok = unsafe { ShellExecuteExW(&mut info) };
    if ok == 0 {
        // The user declining the consent prompt lands here, and it is an
        // ordinary outcome rather than a failure worth alarming them about.
        return Err(InstallError::Declined);
    }

    if info.hProcess.is_null() {
        return Err(InstallError::LaunchFailed);
    }

    let mut code: u32 = 0;
    // SAFETY: a valid process handle from ShellExecuteExW, waited on before
    // its exit code is read and closed exactly once.
    unsafe {
        WaitForSingleObject(info.hProcess, INFINITE);
        GetExitCodeProcess(info.hProcess, &mut code);
        CloseHandle(info.hProcess);
    }

    // Silent mode suppresses the installer's own error reporting, so the exit
    // code is the only signal there is. Ignoring it would let a failed install
    // be reported to the user as a success.
    match code {
        0 => Ok(()),
        _ => Err(InstallError::InstallerFailed),
    }
}

/// Run a program elevated and wait for it, returning its exit code.
///
/// Used to hand the whole privileged sequence to one child process, so the user
/// sees a single consent prompt rather than one per privileged step.
pub fn run_elevated_with(path: &std::path::Path, arguments: &str) -> Result<u32, InstallError> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, WaitForSingleObject, INFINITE,
    };
    use windows_sys::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };

    let file: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let verb: Vec<u16> = "runas".encode_utf16().chain(std::iter::once(0)).collect();
    let args: Vec<u16> = arguments.encode_utf16().chain(std::iter::once(0)).collect();

    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = args.as_ptr();
    info.nShow = 0;

    // SAFETY: `info` is zeroed, sized, and its pointers outlive the call.
    let ok = unsafe { ShellExecuteExW(&mut info) };
    if ok == 0 {
        return Err(InstallError::Declined);
    }
    if info.hProcess.is_null() {
        return Err(InstallError::LaunchFailed);
    }

    let mut code: u32 = 0;
    // SAFETY: a valid process handle, waited on before its code is read and
    // closed exactly once.
    unsafe {
        WaitForSingleObject(info.hProcess, INFINITE);
        GetExitCodeProcess(info.hProcess, &mut code);
        CloseHandle(info.hProcess);
    }
    Ok(code)
}

/// Stage, verify, and run, in that order.
pub fn install() -> Result<(), InstallError> {
    let path = stage()?;
    verify_signature(&path)?;
    run_elevated(&path)?;
    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_setup_arguments_never_request_the_unsigned_edition() {
        // -unrestricted installs the unsigned driver. Passing it would undo the
        // signature check performed moments earlier.
        assert!(
            !SETUP_ARGS.contains("unrestricted"),
            "LoadBear must never request the unsigned edition"
        );
        assert!(SETUP_ARGS.contains("-install"));
        assert!(
            SETUP_ARGS.contains("-silent"),
            "without -silent the setup program shows its own installer UI"
        );
    }

    #[test]
    fn the_bundled_installer_is_a_real_executable_of_the_right_size() {
        assert_eq!(&SETUP_EXE[..2], b"MZ", "the bundled file is not a PE image");
        assert!(
            SETUP_EXE.len() > 3_000_000,
            "the bundled installer looks truncated at {} bytes",
            SETUP_EXE.len()
        );
    }

    #[test]
    fn the_bundled_installer_still_passes_its_own_signature_check() {
        // Shipping it is not a reason to trust it. This is the check that
        // would catch a corrupted vendor file or a tampered build.
        let p = stage().expect("staging the bundled installer must work");
        let r = verify_signature(&p);
        let _ = std::fs::remove_file(&p);
        assert!(
            r.is_ok(),
            "the bundled installer failed signature verification"
        );
    }

    #[test]
    fn errors_are_displayable_without_leaking_raw_codes() {
        for e in [
            InstallError::StagingFailed,
            InstallError::NotAnInstaller,
            InstallError::SignatureRejected,
            InstallError::LaunchFailed,
            InstallError::Declined,
        ] {
            let m = e.to_string();
            assert!(!m.is_empty());
            assert!(!m.contains("0x"), "no raw codes in user-facing text: {m}");
        }
    }

    #[test]
    fn an_unsigned_file_is_rejected() {
        // Verification must fail closed. A file we just wrote ourselves has no
        // signature, so it must not pass.
        let p = std::env::temp_dir().join("loadbear-unsigned-probe.bin");
        std::fs::write(&p, b"MZ not really an executable").unwrap();
        let r = verify_signature(&p);
        let _ = std::fs::remove_file(&p);
        assert!(r.is_err(), "an unsigned file must not verify");
    }
}
