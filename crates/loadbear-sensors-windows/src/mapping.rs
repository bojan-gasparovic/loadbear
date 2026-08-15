//! Creating and reading the shared memory mapping.
//!
//! The writer half runs in the elevated helper, the reader half in the
//! unprivileged interface. Neither is useful without the other, so they live
//! together to keep the seqlock protocol in one place.

use std::ffi::c_void;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, FILE_MAP_READ,
    FILE_MAP_WRITE, PAGE_READWRITE,
};

use crate::shared::{
    SharedTemperature, HELPER_REVISION, LAYOUT_VERSION, MAPPING_NAME, MAPPING_SDDL,
};

#[derive(Debug, thiserror::Error)]
pub enum MappingError {
    #[error("the shared reading is not published")]
    NotPublished,
    #[error("the shared reading could not be created")]
    CreateFailed,
    #[error("the shared reading could not be mapped")]
    MapFailed,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Writer half. Lives in the elevated helper.
pub struct TemperaturePublisher {
    handle: HANDLE,
    view: *mut SharedTemperature,
}

// The pointer is into a private mapping owned by this struct, and access is
// confined to its own methods.
unsafe impl Send for TemperaturePublisher {}

impl TemperaturePublisher {
    /// Create the mapping with a descriptor that lets unprivileged readers in.
    ///
    /// Without the explicit descriptor the default would deny the interface
    /// access entirely, which is the whole reason this indirection exists.
    pub fn create() -> Result<Self, MappingError> {
        let sddl = wide(MAPPING_SDDL);
        let mut psd: *mut c_void = std::ptr::null_mut();

        // SAFETY: `sddl` is NUL terminated and outlives the call; `psd`
        // receives an allocation the OS owns.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut psd,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(MappingError::CreateFailed);
        }

        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: psd,
            bInheritHandle: 0,
        };

        let name = wide(MAPPING_NAME);
        let size = std::mem::size_of::<SharedTemperature>() as u32;

        // SAFETY: an invalid file handle means a pagefile-backed mapping, which
        // is what a shared block wants. Name and attributes outlive the call.
        let handle = unsafe {
            CreateFileMappingW(
                std::ptr::null_mut(),
                &sa,
                PAGE_READWRITE,
                0,
                size,
                name.as_ptr(),
            )
        };
        if handle.is_null() {
            return Err(MappingError::CreateFailed);
        }

        // SAFETY: a valid mapping handle, mapping its whole extent.
        let view = unsafe { MapViewOfFile(handle, FILE_MAP_WRITE, 0, 0, 0) };
        if view.Value.is_null() {
            // SAFETY: closing the handle we just created.
            unsafe { CloseHandle(handle) };
            return Err(MappingError::MapFailed);
        }

        let view = view.Value as *mut SharedTemperature;
        // SAFETY: the view is at least as large as the struct, by construction.
        unsafe { view.write(SharedTemperature::default()) };

        Ok(Self { handle, view })
    }

    /// Publish a reading.
    ///
    /// Raises the sequence to odd, writes, then raises it to even. A reader
    /// that observes an odd value, or a different value either side of its
    /// read, retries rather than trusting a half-written record.
    pub fn publish(&mut self, reading: &SharedTemperature) {
        // SAFETY: `self.view` is a valid mapped view for the lifetime of self.
        unsafe {
            let seq = (*self.view).sequence.wrapping_add(1);
            (*self.view).sequence = seq; // odd: write in progress
            std::sync::atomic::fence(std::sync::atomic::Ordering::Release);

            self.view.write(payload(reading, seq));

            std::sync::atomic::fence(std::sync::atomic::Ordering::Release);
            (*self.view).sequence = seq.wrapping_add(1); // even: settled
        }
    }
}

/// The record to write, given a reading and the in-progress sequence number.
///
/// Split out from [`TemperaturePublisher::publish`] because publishing needs an
/// elevated global mapping and so cannot be tested, while this can.
///
/// The version and revision are stamped here rather than trusted from the
/// caller, since they describe the binary doing the writing.
///
/// Everything else comes from `..*reading` on purpose. The previous version
/// copied field by field and omitted `package_watts`, so the helper read power
/// correctly and published the mapping's initial `NaN` on every sample for as
/// long as it ran. Taking the rest wholesale makes that class of omission
/// impossible rather than merely fixed once.
fn payload(reading: &SharedTemperature, sequence: u32) -> SharedTemperature {
    SharedTemperature {
        sequence,
        version: LAYOUT_VERSION,
        helper_revision: HELPER_REVISION,
        ..*reading
    }
}

impl Drop for TemperaturePublisher {
    fn drop(&mut self) {
        // SAFETY: unmapping and closing what this struct created, once.
        unsafe {
            UnmapViewOfFile(
                windows_sys::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS {
                    Value: self.view as *mut c_void,
                },
            );
            CloseHandle(self.handle);
        }
    }
}

/// Reader half. Lives in the unprivileged interface.
pub struct TemperatureReader {
    handle: HANDLE,
    view: *const SharedTemperature,
}

unsafe impl Send for TemperatureReader {}

impl TemperatureReader {
    /// Open the published mapping read only.
    ///
    /// Fails with [`MappingError::NotPublished`] when the helper is not
    /// running, which is an ordinary state rather than an error.
    pub fn open() -> Result<Self, MappingError> {
        let name = wide(MAPPING_NAME);
        // SAFETY: `name` is NUL terminated and outlives the call.
        let handle = unsafe { OpenFileMappingW(FILE_MAP_READ, 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(MappingError::NotPublished);
        }

        // SAFETY: a valid mapping handle, mapping its whole extent read only.
        let view = unsafe { MapViewOfFile(handle, FILE_MAP_READ, 0, 0, 0) };
        if view.Value.is_null() {
            // SAFETY: closing the handle we just opened.
            unsafe { CloseHandle(handle) };
            return Err(MappingError::MapFailed);
        }

        Ok(Self {
            handle,
            view: view.Value as *const SharedTemperature,
        })
    }

    /// Read only the layout version, which is safe across every layout.
    ///
    /// `version` is the first field and has never moved, so this stays readable
    /// even when the rest of the record cannot be interpreted. Without it a
    /// helper publishing an older layout is indistinguishable from no helper at
    /// all, and the interface reports "unavailable" for something that is
    /// running fine and merely needs updating.
    pub fn published_version(&self) -> u32 {
        // SAFETY: `self.view` is a valid mapped view, and `version` is the
        // first field of every layout this has ever had.
        unsafe { *(self.view as *const u32) }
    }

    /// Read a settled record, retrying past any in-progress write.
    ///
    /// Gives up after a few attempts rather than spinning: a stuck writer must
    /// not hang the interface, and a missed sample is invisible at a 1.5 second
    /// refresh.
    pub fn read(&self) -> Option<SharedTemperature> {
        for _ in 0..8 {
            // SAFETY: `self.view` is a valid mapped view for the lifetime of self.
            unsafe {
                let before = (*self.view).sequence;
                if before % 2 != 0 {
                    continue; // a write is in progress
                }
                std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);
                let snapshot = self.view.read();
                std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);
                if (*self.view).sequence == before {
                    return Some(snapshot);
                }
            }
        }
        None
    }
}

impl Drop for TemperatureReader {
    fn drop(&mut self) {
        // SAFETY: unmapping and closing what this struct opened, once.
        unsafe {
            UnmapViewOfFile(
                windows_sys::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS {
                    Value: self.view as *mut c_void,
                },
            );
            CloseHandle(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_an_unpublished_mapping_reports_not_published() {
        // The helper is not running during tests, and that is the state every
        // machine is in before installation.
        match TemperatureReader::open() {
            Err(MappingError::NotPublished) => {}
            Err(e) => panic!("expected NotPublished, got {e:?}"),
            Ok(_) => {
                // The helper is installed and running on this machine.
            }
        }
    }

    /// A reading with a distinct value in every field, so an unwritten one
    /// shows up as the default rather than coincidentally matching.
    fn a_full_reading() -> SharedTemperature {
        let mut r = SharedTemperature {
            timestamp_ms: 123_456,
            package_c: 64.25,
            package_watts: 11.75,
            zone_count: 2,
            ..Default::default()
        };
        r.zones[0] = 58.5;
        r.zones[1] = 59.0;
        SharedTemperature::set_label(&mut r.zone_labels[0], "Core 0");
        SharedTemperature::set_label(&mut r.zone_labels[1], "Core 1");
        r
    }

    #[test]
    fn every_measured_field_reaches_the_published_record() {
        // The regression this exists for: `publish` copied field by field and
        // left `package_watts` out, so the helper read power correctly and the
        // interface saw NaN forever. Temperature worked throughout, which is
        // what made it look like a sensor problem rather than a writer one.
        let r = a_full_reading();
        let p = payload(&r, 7);

        assert_eq!(p.package_watts, 11.75, "package power must be published");
        assert_eq!(p.package_c, 64.25);
        assert_eq!(p.timestamp_ms, 123_456);
        assert_eq!(p.zone_count, 2);
        assert_eq!(p.zones[0], 58.5);
        assert_eq!(p.zones[1], 59.0);
        assert_eq!(p.zone_labels[0], r.zone_labels[0]);
        assert_eq!(p.zone_list().len(), 2);
    }

    #[test]
    fn the_payload_carries_the_writing_binarys_own_version_and_revision() {
        // Both describe the helper doing the writing, so a caller must not be
        // able to publish a record claiming to be some other build.
        let mut r = a_full_reading();
        r.version = 99;
        r.helper_revision = 99;

        let p = payload(&r, 3);
        assert_eq!(p.version, LAYOUT_VERSION);
        assert_eq!(p.helper_revision, HELPER_REVISION);
        assert!(p.helper_is_current());
    }

    #[test]
    fn the_payload_carries_the_sequence_it_was_given() {
        // `publish` writes the whole record, so the odd in-progress sequence
        // has to survive that write or a reader would see it settled early.
        let p = payload(&a_full_reading(), 9);
        assert_eq!(p.sequence, 9);
        assert!(p.sequence % 2 != 0, "an in-progress write must read as odd");
    }

    #[test]
    fn errors_are_displayable_without_raw_codes() {
        for e in [
            MappingError::NotPublished,
            MappingError::CreateFailed,
            MappingError::MapFailed,
        ] {
            let m = e.to_string();
            assert!(!m.is_empty());
            assert!(!m.contains("0x"), "no raw codes in user-facing text: {m}");
        }
    }
}
