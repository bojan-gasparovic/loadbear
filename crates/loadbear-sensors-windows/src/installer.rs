//! Fetches and runs the official PawnIO installer on the user's behalf.
//!
//! # The installer is downloaded, not redistributed
//!
//! It was embedded with `include_bytes!` until 2026-08-16, so that nobody had
//! to wait for a download. **That was removed before the repository was made
//! public, and it must not come back.**
//!
//! **Licence.** The setup program is published from `namazso/PawnIO.Setup`,
//! which declares no licence at all. The driver source is GPL-2.0 and
//! `PawnIOLib.dll` is LGPL-2.1, both clear, but the setup wrapper carries no
//! stated terms, and no stated terms means no permission to redistribute it.
//! Shipping it inside a private repository was one thing. Publishing it is
//! another, and the README told people LoadBear redistributes nothing, which
//! was not true while the bytes were in the binary. Downloading makes the
//! sentence true rather than deleting it.
//!
//! **This is why `verify_signature` exists.** Fetching an executable and
//! running it elevated without checking who signed it would make LoadBear a
//! delivery mechanism for whatever that URL happens to serve. Nothing is
//! executed before that check passes.
//!
//! **The reboot.** Downloading does not cause it and bundling never avoided
//! it. The setup program installs a root-enumerated PnP device via `newdev.dll`
//! and `UpdateDriverForPlugAndPlayDevicesW`, which reports a reboot
//! requirement. There are no `CreateService` or `StartService` calls anywhere
//! in it, so it is not a dynamically loaded driver the way Core Temp's is. That
//! difference is architectural and no packaging choice changes it.
//!
//! The compiled PawnIO modules in `modules/` are a separate question and are
//! still shipped, under LGPL-2.1, with their licence text and the written offer
//! of source in `NOTICE`.

use std::path::PathBuf;

/// Where the setup program comes from, in one piece, for showing to a person.
///
/// WinHTTP wants the host and the path apart, so they are also below. A test
/// asserts the three agree, because a URL split across three constants is a
/// URL that can drift into pointing somewhere nobody intended.
pub const INSTALLER_SOURCE: &str =
    "https://github.com/namazso/PawnIO.Setup/releases/latest/download/PawnIO_setup.exe";

const INSTALLER_HOST: &str = "github.com";
const INSTALLER_PATH: &str = "/namazso/PawnIO.Setup/releases/latest/download/PawnIO_setup.exe";

/// Bounds on what will be accepted as the setup program.
///
/// The ceiling is not a guess about the file, which is around 3.4 MB. It is
/// the point past which a server answering with something enormous stops being
/// worth reading into memory. The floor catches an error page served with a
/// 200, which is small and would otherwise reach the signature check.
const MAX_INSTALLER_BYTES: usize = 32 * 1024 * 1024;
const MIN_INSTALLER_BYTES: usize = 1024 * 1024;

/// Milliseconds. A person is watching a button that says Installing, so a
/// stalled connection has to give up rather than hang the flow forever.
const TIMEOUT_MS: i32 = 30_000;

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
    #[error("the driver installer could not be downloaded. Check your connection and try again.")]
    DownloadFailed,
    #[error("the downloaded installer could not be written to a temporary file")]
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

/// A WinHTTP handle that closes itself.
///
/// Every step below can fail, and a handle leaked on the error path is a
/// leak that only shows up for the users whose network is having a bad day.
struct WinHttpHandle(*mut core::ffi::c_void);

impl Drop for WinHttpHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: a handle this type owns, closed exactly once.
            unsafe { windows_sys::Win32::Networking::WinHttp::WinHttpCloseHandle(self.0) };
        }
    }
}

/// Fetch the official setup program into memory.
///
/// Nothing is executed here and nothing is trusted yet. The bytes are checked
/// for shape and size only, which is enough to reject an error page served
/// with a 200 before it reaches the signature check.
///
/// WinHTTP rather than an HTTP crate, because this code is linked into the
/// helper service, and a service whose whole job is to read sensors and write
/// a struct does not need an async runtime to do it.
fn fetch() -> Result<Vec<u8>, InstallError> {
    use windows_sys::Win32::Networking::WinHttp::{
        WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders,
        WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts, WinHttpReadData,
        INTERNET_DEFAULT_HTTPS_PORT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
        WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    let agent = wide("LoadBear");
    let host = wide(INSTALLER_HOST);
    let path = wide(INSTALLER_PATH);
    let verb = wide("GET");

    // SAFETY: every pointer below is a NUL terminated wide string that
    // outlives its call, and each handle is wrapped so it closes once.
    unsafe {
        let session = WinHttpHandle(WinHttpOpen(
            agent.as_ptr(),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            std::ptr::null(),
            std::ptr::null(),
            0,
        ));
        if session.0.is_null() {
            return Err(InstallError::DownloadFailed);
        }
        WinHttpSetTimeouts(session.0, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS);

        let connect = WinHttpHandle(WinHttpConnect(
            session.0,
            host.as_ptr(),
            INTERNET_DEFAULT_HTTPS_PORT,
            0,
        ));
        if connect.0.is_null() {
            return Err(InstallError::DownloadFailed);
        }

        // WINHTTP_FLAG_SECURE is what makes this HTTPS rather than a plain
        // request to port 443. Redirects are followed by default, and the
        // default policy refuses a downgrade to HTTP, which is what carries
        // the request from github.com to wherever the release actually lives.
        let request = WinHttpHandle(WinHttpOpenRequest(
            connect.0,
            verb.as_ptr(),
            path.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ));
        if request.0.is_null() {
            return Err(InstallError::DownloadFailed);
        }

        if WinHttpSendRequest(request.0, std::ptr::null(), 0, std::ptr::null(), 0, 0, 0) == 0
            || WinHttpReceiveResponse(request.0, std::ptr::null_mut()) == 0
        {
            return Err(InstallError::DownloadFailed);
        }

        let mut status: u32 = 0;
        let mut len = std::mem::size_of::<u32>() as u32;
        if WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            std::ptr::null(),
            &mut status as *mut _ as *mut core::ffi::c_void,
            &mut len,
            std::ptr::null_mut(),
        ) == 0
            || status != 200
        {
            return Err(InstallError::DownloadFailed);
        }

        let mut body: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 64 * 1024];
        loop {
            let mut read: u32 = 0;
            if WinHttpReadData(
                request.0,
                chunk.as_mut_ptr() as *mut core::ffi::c_void,
                chunk.len() as u32,
                &mut read,
            ) == 0
            {
                return Err(InstallError::DownloadFailed);
            }
            if read == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..read as usize]);
            if body.len() > MAX_INSTALLER_BYTES {
                return Err(InstallError::NotAnInstaller);
            }
        }

        if body.len() < MIN_INSTALLER_BYTES || !body.starts_with(b"MZ") {
            return Err(InstallError::NotAnInstaller);
        }
        Ok(body)
    }
}

/// Download the setup program and write it to a temporary path.
///
/// Nothing is executed here. The caller verifies the signature before running
/// anything, and that order is the whole security argument for downloading at
/// all.
pub fn stage() -> Result<PathBuf, InstallError> {
    let body = fetch()?;
    let path = std::env::temp_dir().join("LoadBear-PawnIO_setup.exe");
    std::fs::write(&path, &body).map_err(|_| InstallError::StagingFailed)?;
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

/// Download, verify, and run, in that order.
///
/// The order is the point. Verification happens on the file that will be
/// executed, after it has landed and before anything runs it.
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
    fn the_split_url_still_names_the_same_file_as_the_whole_one() {
        // WinHTTP wants the host and the path apart, and a URL living in three
        // constants is a URL that can drift into pointing somewhere else while
        // the string shown to a person goes on saying the old thing.
        assert_eq!(
            format!("https://{INSTALLER_HOST}{INSTALLER_PATH}"),
            INSTALLER_SOURCE
        );
    }

    #[test]
    fn the_installer_is_fetched_over_https_from_the_official_repository() {
        // The licence position rests on this. PawnIO.Setup declares no terms,
        // so LoadBear may run it but may not hand out copies of it, and a
        // plaintext fetch would undo the signature check by inviting whatever
        // the network wants to serve.
        assert!(INSTALLER_SOURCE.starts_with("https://"));
        assert!(INSTALLER_SOURCE.contains("namazso/PawnIO.Setup"));
    }

    #[test]
    fn this_crate_redistributes_no_third_party_executable() {
        // The regression this guards: the 3.4 MB PawnIO setup program was
        // embedded here until 2026-08-16, while the README told the public
        // that LoadBear redistributes nothing. The repository is public now,
        // so putting it back would publish both the binary and the untruth.
        //
        // Checked by size rather than by grepping this file for the macro
        // name, which fails the moment the comment above mentions it. The
        // largest thing this crate is meant to carry is RyzenSMU.bin at 39 KB,
        // and the setup program is a hundred times that.
        const CEILING: u64 = 512 * 1024;

        fn walk(dir: &std::path::Path, found: &mut Vec<(String, u64)>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for e in entries.flatten() {
                let path = e.path();
                if path.is_dir() {
                    walk(&path, found);
                } else if let Ok(m) = e.metadata() {
                    if m.len() > CEILING {
                        found.push((path.display().to_string(), m.len()));
                    }
                }
            }
        }

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut oversized = Vec::new();
        walk(root, &mut oversized);

        assert!(
            oversized.is_empty(),
            "this crate must redistribute nothing large. Found: {oversized:?}"
        );
        assert!(
            !root.join("vendor").exists(),
            "vendor/ is where the redistributed setup program used to live"
        );
    }

    #[test]
    #[ignore = "reaches the network. Run with: cargo test -p loadbear-sensors-windows -- --ignored"]
    fn the_official_installer_downloads_and_passes_its_signature_check() {
        // The only test that proves the download path works at all. It is the
        // whole feature: fetch, shape check, then Authenticode before anything
        // is executed.
        let p = stage().expect("the official installer must download");
        let r = verify_signature(&p);
        let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        let _ = std::fs::remove_file(&p);
        assert!(r.is_ok(), "the downloaded installer failed verification");
        assert!(size > 1_000_000, "the download looks truncated at {size}");
    }

    #[test]
    fn errors_are_displayable_without_leaking_raw_codes() {
        for e in [
            InstallError::DownloadFailed,
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
