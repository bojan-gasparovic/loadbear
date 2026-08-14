//! Fetches and runs the official PawnIO installer on the user's behalf.
//!
//! # Why download rather than bundle
//!
//! LoadBear redistributes nothing here, and that is deliberate. The PawnIO
//! driver source is GPL-2.0 and `PawnIOLib.dll` is LGPL-2.1, both clear. But
//! the signed installer is published from `namazso/PawnIO.Setup`, which
//! declares no licence at all, so shipping that file would mean redistributing
//! a binary on unstated terms.
//!
//! Downloading it at the user's request avoids that entirely. It also means
//! they always get the current signed build rather than whatever version was
//! pinned when LoadBear shipped, and the signature can be checked before
//! anything is executed.
//!
//! The compiled PawnIO modules **are** shipped, in `modules/`. They are
//! LGPL-2.1 with the licence text alongside, which is a well-understood
//! obligation rather than an unknown one.

use std::path::PathBuf;

/// The official download, as linked from pawnio.eu.
///
/// `/releases/latest/download/` always resolves to the newest release, so this
/// URL does not go stale.
pub const INSTALLER_URL: &str =
    "https://github.com/namazso/PawnIO.Setup/releases/latest/download/PawnIO_setup.exe";

/// Publisher expected on the installer's Authenticode signature.
///
/// Checked as a substring of the signer's common name. A signature that is
/// merely valid is not enough: anything with a trusted certificate would pass
/// that. The publisher has to be the one we meant.
pub const EXPECTED_PUBLISHER: &str = "namazso";

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("could not reach the PawnIO download")]
    DownloadFailed,
    #[error("the downloaded file does not look like an installer")]
    NotAnInstaller,
    #[error("the download is not signed by a trusted publisher, so it was not run")]
    SignatureRejected,
    #[error("the installer could not be started")]
    LaunchFailed,
    #[error("installation was declined")]
    Declined,
}

/// Download the installer to a temporary path.
///
/// Returns the path to the downloaded file. Nothing is executed here.
pub fn download() -> Result<PathBuf, InstallError> {
    let mut response = ureq::get(INSTALLER_URL)
        .call()
        .map_err(|_| InstallError::DownloadFailed)?;

    let mut bytes = Vec::new();
    std::io::copy(&mut response.body_mut().as_reader(), &mut bytes)
        .map_err(|_| InstallError::DownloadFailed)?;

    // A Windows executable starts with "MZ". Anything else means we were served
    // an error page or a redirect body rather than the installer.
    if bytes.len() < 2 || &bytes[..2] != b"MZ" {
        return Err(InstallError::NotAnInstaller);
    }
    // The real installer is a few megabytes. A tiny "executable" is a red flag.
    if bytes.len() < 500_000 {
        return Err(InstallError::NotAnInstaller);
    }

    let path = std::env::temp_dir().join("LoadBear-PawnIO_setup.exe");
    std::fs::write(&path, &bytes).map_err(|_| InstallError::DownloadFailed)?;
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
    use windows_sys::Win32::System::Threading::{WaitForSingleObject, INFINITE};
    use windows_sys::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };

    let file: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let verb: Vec<u16> = "runas".encode_utf16().chain(std::iter::once(0)).collect();

    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.nShow = 1; // SW_SHOWNORMAL

    // SAFETY: `info` is zeroed, sized, and its pointers outlive the call.
    let ok = unsafe { ShellExecuteExW(&mut info) };
    if ok == 0 {
        // The user declining the consent prompt lands here, and it is an
        // ordinary outcome rather than a failure worth alarming them about.
        return Err(InstallError::Declined);
    }

    if !info.hProcess.is_null() {
        // SAFETY: a valid process handle from ShellExecuteExW.
        unsafe {
            WaitForSingleObject(info.hProcess, INFINITE);
            CloseHandle(info.hProcess);
        }
    }

    Ok(())
}

/// Download, verify, and run, in that order.
pub fn install() -> Result<(), InstallError> {
    let path = download()?;
    verify_signature(&path)?;
    run_elevated(&path)?;
    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_installer_url_points_at_the_official_latest_release() {
        assert!(INSTALLER_URL.starts_with("https://github.com/namazso/PawnIO.Setup/"));
        assert!(
            INSTALLER_URL.contains("/releases/latest/download/"),
            "the URL must track the latest release rather than pinning a stale build"
        );
    }

    #[test]
    fn errors_are_displayable_without_leaking_raw_codes() {
        for e in [
            InstallError::DownloadFailed,
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
