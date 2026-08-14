//! Client for the PawnIO userspace library.
//!
//! # Interface provenance
//!
//! Every signature below is transcribed from the upstream public header,
//! `PawnIOLib/include/PawnIOLib.h` in `github.com/namazso/PawnIO`, read on
//! 2026-08-14. Nothing here is guessed.
//!
//! ```c
//! PAWNIOAPI pawnio_version(PULONG version);
//! PAWNIOAPI pawnio_open(PHANDLE handle);
//! PAWNIOAPI pawnio_load(HANDLE handle, const UCHAR* blob, SIZE_T size);
//! PAWNIOAPI pawnio_execute(HANDLE handle, PCSTR name,
//!                          const ULONG64* in,  SIZE_T in_size,
//!                          PULONG64      out, SIZE_T out_size,
//!                          PSIZE_T return_size);
//! PAWNIOAPI pawnio_close(HANDLE handle);
//! ```
//!
//! `PAWNIOAPI` expands to `EXTERN_C __declspec(dllimport) HRESULT STDAPICALLTYPE`,
//! so every entry point returns an `HRESULT` where zero means success.
//!
//! # Why a DLL rather than raw ioctls
//!
//! PawnIO exposes this library specifically so callers do not open the device
//! or issue control codes themselves. Going through it means there are no
//! ioctl constants to transcribe and no chance of a wrong one corrupting
//! state.
//!
//! `PawnIOLib.dll` is LGPL-2.1. It is loaded dynamically at runtime, which is
//! the arrangement LGPL exists to permit for a differently-licensed caller,
//! and LoadBear does not redistribute it. The driver and the library both
//! arrive with the user's own PawnIO installation.

use std::ffi::CString;

use libloading::{Library, Symbol};

/// Windows `HRESULT`. Zero is success.
type HResult = i32;
type Handle = isize;

type PawnIoOpen = unsafe extern "system" fn(*mut Handle) -> HResult;
type PawnIoLoad = unsafe extern "system" fn(Handle, *const u8, usize) -> HResult;
type PawnIoExecute = unsafe extern "system" fn(
    Handle,
    *const i8,
    *const u64,
    usize,
    *mut u64,
    usize,
    *mut usize,
) -> HResult;
type PawnIoClose = unsafe extern "system" fn(Handle) -> HResult;

/// The library file name.
const LIB_NAME: &str = "PawnIOLib.dll";

/// Where PawnIO actually installs the library.
///
/// Verified on 2026-08-14: the setup program puts `PawnIOLib.dll` in
/// `%ProgramFiles%\PawnIO`, which is **not** on the default DLL search path.
/// Loading by bare name therefore fails even while the driver service is
/// running, which reads to a user as "temperature is broken" when nothing is.
fn candidate_paths() -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    for var in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
        if let Ok(dir) = std::env::var(var) {
            v.push(std::path::Path::new(&dir).join("PawnIO").join(LIB_NAME));
        }
    }
    // Last resort: let Windows search, in case a future installer puts it
    // somewhere on the path after all.
    v.push(std::path::PathBuf::from(LIB_NAME));
    v
}

#[derive(Debug, thiserror::Error)]
pub enum PawnIoError {
    #[error("the PawnIO driver is not installed on this machine")]
    NotInstalled,
    #[error("access to PawnIO was refused")]
    AccessDenied,
    #[error("the PawnIO installation is present but incomplete")]
    LibraryIncomplete,
    #[error("the PawnIO module could not be loaded")]
    ModuleLoadFailed,
    #[error("the PawnIO function '{0}' is not available in the loaded module")]
    FunctionUnavailable(String),
    #[error("PawnIO returned an unexpected response")]
    UnexpectedResponse,
}

/// Map an `HRESULT` to a domain error.
///
/// Raw codes never reach a user. `status()` copy is shown in the interface, and
/// a hexadecimal Win32 value there tells someone nothing they can act on.
fn from_hresult(hr: HResult) -> PawnIoError {
    // E_ACCESSDENIED
    if hr == 0x8007_0005_u32 as i32 {
        PawnIoError::AccessDenied
    } else {
        PawnIoError::UnexpectedResponse
    }
}

/// An open PawnIO executor.
///
/// The library is kept alive alongside the handle because unloading it while a
/// handle is open would invalidate the function pointers.
pub struct PawnIo {
    lib: Library,
    handle: Handle,
}

impl PawnIo {
    /// Open an executor.
    ///
    /// Returns [`PawnIoError::NotInstalled`] when the library is absent, which
    /// is the ordinary state on a machine that has never installed PawnIO
    /// rather than a failure. LoadBear ships no driver, so this is where every
    /// user starts.
    pub fn open() -> Result<Self, PawnIoError> {
        // SAFETY: loading a library is sound. It may run initialisation code,
        // which is inherent to dynamic loading.
        let lib = candidate_paths()
            .into_iter()
            .find_map(|p| unsafe { Library::new(&p) }.ok())
            .ok_or(PawnIoError::NotInstalled)?;

        let mut handle: Handle = 0;
        // SAFETY: the symbol signature is transcribed from the upstream header
        // documented above, and `handle` is a valid writable location.
        let hr = unsafe {
            let open: Symbol<PawnIoOpen> = lib
                .get(b"pawnio_open\0")
                .map_err(|_| PawnIoError::LibraryIncomplete)?;
            open(&mut handle)
        };

        if hr != 0 {
            return Err(from_hresult(hr));
        }

        Ok(Self { lib, handle })
    }

    /// Load a compiled module blob into the executor.
    pub fn load_module(&self, blob: &[u8]) -> Result<(), PawnIoError> {
        // SAFETY: signature per the upstream header. `blob` is a valid slice
        // and its length is passed alongside the pointer.
        let hr = unsafe {
            let load: Symbol<PawnIoLoad> = self
                .lib
                .get(b"pawnio_load\0")
                .map_err(|_| PawnIoError::LibraryIncomplete)?;
            load(self.handle, blob.as_ptr(), blob.len())
        };

        if hr != 0 {
            return Err(PawnIoError::ModuleLoadFailed);
        }
        Ok(())
    }

    /// Execute a named function from the loaded module.
    ///
    /// `out_capacity` is the number of `u64` slots the caller is prepared to
    /// receive. The upstream API requires the caller to size the output buffer,
    /// so this parameter is not optional, which is a deviation from the
    /// signature sketched in the plan before the header was read.
    pub fn execute(
        &self,
        name: &str,
        input: &[u64],
        out_capacity: usize,
    ) -> Result<Vec<u64>, PawnIoError> {
        let cname =
            CString::new(name).map_err(|_| PawnIoError::FunctionUnavailable(name.to_string()))?;
        let mut out = vec![0u64; out_capacity];
        let mut written: usize = 0;

        // SAFETY: signature per the upstream header. Both buffers are valid for
        // their stated lengths, and `cname` is NUL terminated and outlives the
        // call.
        let hr = unsafe {
            let execute: Symbol<PawnIoExecute> = self
                .lib
                .get(b"pawnio_execute\0")
                .map_err(|_| PawnIoError::LibraryIncomplete)?;
            execute(
                self.handle,
                cname.as_ptr(),
                input.as_ptr(),
                input.len(),
                out.as_mut_ptr(),
                out.len(),
                &mut written,
            )
        };

        if hr != 0 {
            return Err(PawnIoError::FunctionUnavailable(name.to_string()));
        }
        if written > out.len() {
            return Err(PawnIoError::UnexpectedResponse);
        }

        out.truncate(written);
        Ok(out)
    }
}

impl Drop for PawnIo {
    fn drop(&mut self) {
        if self.handle == 0 {
            return;
        }
        // SAFETY: signature per the upstream header. The handle came from
        // `pawnio_open` and is closed exactly once.
        unsafe {
            if let Ok(close) = self.lib.get::<PawnIoClose>(b"pawnio_close\0") {
                let _ = close(self.handle);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unusable_driver_maps_to_a_domain_error_rather_than_a_raw_os_error() {
        // Three outcomes are all legitimate depending on the machine:
        //
        //   NotInstalled  PawnIO was never installed
        //   AccessDenied  installed, but this process is not elevated. The
        //                 device is secured to SYSTEM and Administrators only,
        //                 which is why LoadBear needs the helper service
        //   Ok            installed and this process may open it
        //
        // What must never happen is a raw OS error reaching a caller.
        match PawnIo::open() {
            Err(PawnIoError::NotInstalled) | Err(PawnIoError::AccessDenied) | Ok(_) => {}
            Err(e) => panic!("unexpected failure mode: {e:?}"),
        }
    }

    #[test]
    fn errors_are_displayable_without_leaking_raw_win32_codes() {
        let cases = [
            PawnIoError::NotInstalled,
            PawnIoError::AccessDenied,
            PawnIoError::LibraryIncomplete,
            PawnIoError::ModuleLoadFailed,
            PawnIoError::FunctionUnavailable("read_temp".to_string()),
            PawnIoError::UnexpectedResponse,
        ];
        for e in cases {
            let msg = e.to_string();
            assert!(!msg.is_empty());
            assert!(
                !msg.contains("0x"),
                "user-facing text must not contain raw error codes: {msg}"
            );
        }
    }

    #[test]
    fn the_install_directory_is_searched_not_just_the_dll_search_path() {
        // The regression this guards: PawnIO installs to a directory Windows
        // does not search, so loading by bare name alone reports the driver as
        // absent while it is running.
        let paths = candidate_paths();
        assert!(
            paths
                .iter()
                .any(|p| p.parent().is_some_and(|d| d.ends_with("PawnIO"))),
            "the PawnIO install directory must be searched explicitly"
        );
        assert!(
            paths.len() > 1,
            "a bare-name fallback should remain alongside the explicit paths"
        );
    }

    #[test]
    fn access_denied_maps_to_its_own_variant() {
        assert!(matches!(
            from_hresult(0x8007_0005_u32 as i32),
            PawnIoError::AccessDenied
        ));
    }

    #[test]
    fn an_unrecognised_hresult_does_not_masquerade_as_access_denied() {
        assert!(matches!(
            from_hresult(0x8000_4005_u32 as i32),
            PawnIoError::UnexpectedResponse
        ));
    }
}
