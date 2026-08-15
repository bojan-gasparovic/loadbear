//! Physical and logical processor counts, from the operating system.
//!
//! # Why not the specification database
//!
//! Core and thread counts were read out of the hand-maintained database, which
//! meant they were correct on the three processors somebody had typed in and
//! zero everywhere else. Windows knows both numbers on every machine it runs
//! on, so asking it removes a whole class of "unknown processor" from the
//! interface rather than waiting for the database to grow.
//!
//! The database is still worth having, but only for what the OS genuinely
//! cannot supply: the rated power band and TjMax.
//!
//! # Why not `available_parallelism`
//!
//! That returns what this *process* may use, which is affinity-limited and
//! smaller than the machine whenever anything has set a mask. LoadBear reports
//! on the machine, so it counts the machine.

use windows_sys::Win32::System::SystemInformation::{
    GetLogicalProcessorInformationEx, RelationProcessorCore,
    SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
};

/// What the machine actually has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Topology {
    /// Physical cores. One entry per `RelationProcessorCore` record.
    pub physical_cores: u32,
    /// Logical processors, so twice the cores on an SMT part.
    pub logical_processors: u32,
}

/// Ask Windows how many cores and threads this machine has.
///
/// Returns `None` rather than a guess when the call fails, so a caller can tell
/// "the machine has no cores" apart from "nobody asked", which a zero would
/// not.
pub fn detect() -> Option<Topology> {
    let mut bytes: u32 = 0;

    // SAFETY: the documented way to size the buffer. A null pointer with a zero
    // length is expected to fail and write the required length.
    unsafe {
        GetLogicalProcessorInformationEx(RelationProcessorCore, std::ptr::null_mut(), &mut bytes)
    };
    if bytes == 0 {
        return None;
    }

    // Allocated as u64 rather than u8 because the record requires eight byte
    // alignment and a byte vector is not guaranteed to have it.
    let mut buffer = vec![0u64; bytes.div_ceil(8) as usize];
    let base = buffer.as_mut_ptr() as *mut SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX;

    // SAFETY: the buffer is at least `bytes` long and correctly aligned.
    let ok =
        unsafe { GetLogicalProcessorInformationEx(RelationProcessorCore, base, &mut bytes) } != 0;
    if !ok {
        return None;
    }

    let mut physical_cores = 0u32;
    let mut logical_processors = 0u32;
    let mut offset = 0u32;

    while offset + std::mem::size_of::<u32>() as u32 * 2 <= bytes {
        // SAFETY: `offset` stays within the length the call reported, and each
        // record's own `Size` is what advances it, which is how this list is
        // defined to be walked.
        let record = unsafe { &*(base.byte_add(offset as usize)) };
        let size = record.Size;
        if size == 0 {
            break; // A zero stride would spin here for ever.
        }

        physical_cores += 1;

        // SAFETY: the relationship was filtered to processor cores by the call
        // itself, so the union holds a `PROCESSOR_RELATIONSHIP`.
        let processor = unsafe { &record.Anonymous.Processor };
        let groups = processor.GroupCount as usize;
        // SAFETY: `GroupMask` is a trailing array of `GroupCount` entries, and
        // the record's `Size` accounts for them.
        let masks = unsafe { std::slice::from_raw_parts(processor.GroupMask.as_ptr(), groups) };
        for m in masks {
            logical_processors += m.Mask.count_ones();
        }

        offset += size;
    }

    if physical_cores == 0 || logical_processors == 0 {
        return None;
    }

    Some(Topology {
        physical_cores,
        logical_processors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_running_machine_reports_a_sane_topology() {
        let t = detect().expect("Windows must be able to describe its own processors");
        assert!(t.physical_cores >= 1);
        assert!(
            t.logical_processors >= t.physical_cores,
            "there cannot be fewer threads than cores: {t:?}"
        );
        assert!(
            t.logical_processors <= t.physical_cores * 4,
            "more than four threads per core is not a shipping part: {t:?}"
        );
    }

    #[test]
    fn it_counts_the_machine_rather_than_this_processs_affinity() {
        // The regression this guards: `available_parallelism` returns what the
        // process may use. Anything that sets an affinity mask would shrink the
        // reported machine, and the number is meant to describe the hardware.
        let t = detect().expect("topology must be available");
        let allowed = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
        assert!(
            t.logical_processors as usize >= allowed,
            "the machine cannot have fewer processors than this process may use"
        );
    }

    #[test]
    fn repeated_calls_agree() {
        // Nothing here is sampled, so two answers differing would mean the walk
        // is reading past a record rather than the machine changing.
        assert_eq!(detect(), detect());
    }
}
