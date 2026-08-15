//! Per-process readings, gathered unprivileged.
//!
//! Enumerates processes and turns cumulative CPU time into a share of the
//! machine by differencing against the previous sample. Nothing here judges
//! anything: ranking, dominance and whether a cause may be named at all live in
//! `loadbear_core::attribution`, which is testable without a machine.
//!
//! # What an unprivileged process cannot see
//!
//! Protected processes, and processes belonging to other users, refuse to open.
//! LoadBear runs unprivileged by design, so this is the normal case rather than
//! a fault, and the list this module returns is honestly incomplete. That
//! incompleteness is measurable: the CPU shares here are comparable with the
//! machine's own utilization figure, and the attribution layer compares them
//! before it will name anything. Raising privileges to close the gap is not on
//! the table, because the whole architecture exists to keep the interface
//! unprivileged.
//!
//! **Measured on 2026-08-15**, unprivileged, 16 logical processors, 150 to 160
//! processes visible:
//!
//! | Machine state | Utilization | Accounted for | Coverage |
//! |---|---|---|---|
//! | Idle to light | 24.8 % | 9.8 % | 0.39 |
//! | All cores busy | 100 % | 92.3 % | 0.92 |
//!
//! Coverage is poor at idle and excellent under load, which is the opposite of
//! a problem: the gap at idle is kernel, interrupt and DPC time that belongs to
//! no process at all, and it is a large share of a small number. Attribution
//! only ever runs when something is wrong, and under load the process list
//! accounts for almost everything. This is why the coverage gate is set where
//! it is, and the figures are recorded here so the next person does not have to
//! rediscover them.
//!
//! # Hard faults are not reported
//!
//! `PROCESS_MEMORY_COUNTERS::PageFaultCount` counts soft and hard faults
//! together, and soft faults are ordinary memory access that never touches a
//! disk. Reporting it as a hard fault rate would be substituting a quantity
//! that is easy to measure for one that is not, which is the mistake that
//! produced a diagnosis of "idle" on a machine at 97 percent. So the field is
//! left absent and I/O attribution stays silent until a real source exists.

use std::collections::HashMap;

use loadbear_core::ProcessReading;
use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, MAX_PATH};
use windows_sys::Win32::System::ProcessStatus::{
    K32EnumProcesses, K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
};
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// How many pids to ask for. Windows truncates silently rather than failing, so
/// this is deliberately far above any plausible process count and the result is
/// checked against it.
const MAX_PROCESSES: usize = 4096;

/// Combine the kernel and user halves of a process's CPU time.
///
/// Both are 100 nanosecond counts since the process started, split across two
/// 32 bit halves because the API predates a 64 bit integer being convenient.
fn filetime_to_u64(ft: FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
}

/// The executable name without its path or extension, and the full path.
///
/// Attribution groups by the name, so `C:\Program Files\...\Code.exe` and a
/// second copy of it from another directory are treated as the same thing,
/// which is what a user means by "VS Code is using the CPU". The full path is
/// kept because the friendly name lives inside the file itself.
fn image_name(handle: HANDLE) -> Option<(String, String)> {
    let mut buf = [0u16; MAX_PATH as usize];
    let mut len = buf.len() as u32;
    // SAFETY: `handle` is open with PROCESS_QUERY_LIMITED_INFORMATION, which is
    // what this call requires, and `buf`/`len` are a matched buffer and size.
    let ok = unsafe { QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut len) };
    if ok == 0 || len == 0 {
        return None;
    }
    let full = String::from_utf16_lossy(&buf[..len as usize]);
    let file = full.rsplit(['\\', '/']).next().unwrap_or(&full);
    let stem = file.strip_suffix(".exe").unwrap_or(file);
    if stem.is_empty() {
        None
    } else {
        Some((stem.to_string(), full.clone()))
    }
}

/// The name a person would call the application, read from the executable.
///
/// Windows executables carry a version resource, and `FileDescription` inside
/// it is the string the vendor wrote for humans. It is where Task Manager gets
/// "Antimalware Service Executable" from `MsMpEng.exe`, and it means LoadBear
/// needs no mapping table of its own to maintain and can name applications it
/// has never heard of.
///
/// Returns `None` for anything with no version resource, which covers much of
/// the operating system's own processes. The caller falls back to the file
/// name, so an unnamed process is still identified, just less pleasantly.
fn file_description(path: &str) -> Option<String> {
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
    };

    let wide_path: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();

    // SAFETY: `wide_path` is NUL terminated and outlives the call. A null
    // second argument is documented as ignored.
    let size = unsafe { GetFileVersionInfoSizeW(wide_path.as_ptr(), std::ptr::null_mut()) };
    if size == 0 {
        return None;
    }

    let mut buffer = vec![0u8; size as usize];
    // SAFETY: `buffer` is `size` bytes, which is what the call above asked for.
    let ok = unsafe {
        GetFileVersionInfoW(
            wide_path.as_ptr(),
            0,
            size,
            buffer.as_mut_ptr() as *mut std::ffi::c_void,
        )
    };
    if ok == 0 {
        return None;
    }

    // The description is filed under a language and codepage pair, and which
    // pair depends on how the vendor built the binary. The translation table
    // says which pairs exist, so ask it rather than assuming US English.
    let (language, codepage) = translation(&buffer)?;
    let key = format!("\\StringFileInfo\\{language:04x}{codepage:04x}\\FileDescription");
    let wide_key: Vec<u16> = key.encode_utf16().chain(std::iter::once(0)).collect();

    let mut value: *mut std::ffi::c_void = std::ptr::null_mut();
    let mut len: u32 = 0;
    // SAFETY: `buffer` holds a version resource, `wide_key` is NUL terminated,
    // and both outputs are writable. The returned pointer borrows `buffer`.
    let ok = unsafe {
        VerQueryValueW(
            buffer.as_ptr() as *const std::ffi::c_void,
            wide_key.as_ptr(),
            &mut value,
            &mut len,
        )
    };
    if ok == 0 || value.is_null() || len == 0 {
        return None;
    }

    // SAFETY: on success this points into `buffer` and holds `len` UTF-16 code
    // units, including a trailing NUL that is trimmed below.
    let text = unsafe { std::slice::from_raw_parts(value as *const u16, len as usize) };
    let text = String::from_utf16_lossy(text);
    let text = text.trim_end_matches('\0').trim();

    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// The first language and codepage pair the version resource declares.
fn translation(buffer: &[u8]) -> Option<(u16, u16)> {
    use windows_sys::Win32::Storage::FileSystem::VerQueryValueW;

    let key: Vec<u16> = "\\VarFileInfo\\Translation"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut value: *mut std::ffi::c_void = std::ptr::null_mut();
    let mut len: u32 = 0;
    // SAFETY: as above. `len` comes back as a byte count.
    let ok = unsafe {
        VerQueryValueW(
            buffer.as_ptr() as *const std::ffi::c_void,
            key.as_ptr(),
            &mut value,
            &mut len,
        )
    };
    if ok == 0 || value.is_null() || len < 4 {
        return None;
    }
    // SAFETY: the block is a sequence of two `u16` fields per translation, and
    // `len` was checked to hold at least one pair.
    let pair = unsafe { std::slice::from_raw_parts(value as *const u16, 2) };
    Some((pair[0], pair[1]))
}

/// One process as the OS reports it, before any differencing.
struct RawProcess {
    pid: u32,
    name: String,
    path: String,
    cpu_100ns: u64,
    working_set_bytes: u64,
}

fn read_process(pid: u32) -> Option<RawProcess> {
    // SAFETY: opening by pid with the narrowest right that serves. Returns null
    // for processes this token may not touch, which is expected and handled.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }

    let result = (|| {
        let (name, path) = image_name(handle)?;

        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        // SAFETY: `handle` is valid and all four outputs are writable.
        let ok =
            unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) };
        if ok == 0 {
            return None;
        }

        let mut counters = PROCESS_MEMORY_COUNTERS::default();
        let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        // SAFETY: `counters` is a correctly sized buffer of the type named by
        // `size`, which is how this API is told what it was given.
        let ok = unsafe { K32GetProcessMemoryInfo(handle, &mut counters, size) };
        if ok == 0 {
            return None;
        }

        Some(RawProcess {
            pid,
            name,
            path,
            cpu_100ns: filetime_to_u64(kernel) + filetime_to_u64(user),
            working_set_bytes: counters.WorkingSetSize as u64,
        })
    })();

    // SAFETY: closing a handle we opened, exactly once, on every path.
    unsafe { CloseHandle(handle) };
    result
}

fn enumerate_pids() -> Vec<u32> {
    let mut pids = vec![0u32; MAX_PROCESSES];
    let mut returned_bytes = 0u32;
    let size_bytes = (pids.len() * std::mem::size_of::<u32>()) as u32;

    // SAFETY: `pids` is a buffer of `size_bytes` and `returned_bytes` is
    // writable.
    let ok = unsafe { K32EnumProcesses(pids.as_mut_ptr(), size_bytes, &mut returned_bytes) };
    if ok == 0 {
        return Vec::new();
    }

    let count = returned_bytes as usize / std::mem::size_of::<u32>();
    pids.truncate(count);
    pids
}

/// Turns cumulative per-process CPU time into a share of the machine.
///
/// Stateful because CPU time is cumulative: a single reading says how much
/// processor a process has used since it started, which is not what anybody
/// wants to know. Two readings and the wall clock between them give the rate.
pub struct ProcessSampler {
    previous: HashMap<u32, u64>,
    last_sample_ms: Option<u64>,
    logical_cpus: u32,
    /// Friendly names by executable path.
    ///
    /// Reading a version resource means opening and parsing the file, which is
    /// far too expensive to repeat for every process on every tick. The answer
    /// cannot change while a binary stays where it is, so it is read once. A
    /// path with no description caches the failure too, so a process without
    /// one is not retried forever.
    names: HashMap<String, Option<String>>,
}

impl ProcessSampler {
    pub fn new(logical_cpus: u32) -> Self {
        Self {
            previous: HashMap::new(),
            last_sample_ms: None,
            logical_cpus: logical_cpus.max(1),
            names: HashMap::new(),
        }
    }

    /// Enumerate every visible process and report its share of the machine.
    ///
    /// The first call has no previous sample to difference against, so every
    /// CPU share is zero. That is not a special case needing handling: the
    /// attribution layer compares the total against the machine's utilization
    /// and declines to name anything when the two do not agree, so a first
    /// sample simply produces no attribution.
    ///
    /// `now_ms` is passed in rather than read here so the rate arithmetic can
    /// be tested without waiting.
    pub fn sample(&mut self, now_ms: u64) -> Vec<ProcessReading> {
        let raw: Vec<RawProcess> = enumerate_pids()
            .into_iter()
            .filter_map(read_process)
            .collect();

        let elapsed_ms = self
            .last_sample_ms
            .map(|last| now_ms.saturating_sub(last))
            .unwrap_or(0);

        let shares: Vec<f32> = raw
            .iter()
            .map(|p| self.share_of_machine(p, elapsed_ms))
            .collect();

        let readings = raw
            .iter()
            .zip(shares)
            .map(|(p, cpu_percent)| {
                let display_name = match self.names.get(&p.path) {
                    Some(cached) => cached.clone(),
                    None => {
                        let found = file_description(&p.path);
                        self.names.insert(p.path.clone(), found.clone());
                        found
                    }
                };
                ProcessReading {
                    pid: p.pid,
                    name: p.name.clone(),
                    display_name,
                    working_set_bytes: p.working_set_bytes,
                    hard_faults_per_sec: None,
                    cpu_percent,
                }
            })
            .collect();

        // Rebuilt rather than updated, so processes that have exited drop out
        // instead of accumulating for the lifetime of the application.
        self.previous = raw.iter().map(|p| (p.pid, p.cpu_100ns)).collect();
        self.last_sample_ms = Some(now_ms);

        readings
    }

    /// This process's CPU use over the interval, as a percentage of the whole
    /// machine rather than of one core.
    ///
    /// Machine-normalized so the figures are comparable with each other and
    /// with `% Processor Time`, which is what lets the attribution layer tell
    /// whether the process list accounts for what the machine is doing.
    fn share_of_machine(&self, p: &RawProcess, elapsed_ms: u64) -> f32 {
        if elapsed_ms == 0 {
            return 0.0;
        }
        let Some(previous) = self.previous.get(&p.pid) else {
            // A process that started during the interval has no baseline. Its
            // cumulative time is not a delta and using it as one would credit
            // it with everything it has ever done.
            return 0.0;
        };

        let delta_100ns = p.cpu_100ns.saturating_sub(*previous) as f64;
        let elapsed_100ns = elapsed_ms as f64 * 10_000.0;
        let cores = self.logical_cpus as f64;

        ((delta_100ns / elapsed_100ns / cores) * 100.0).clamp(0.0, 100.0) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_filetime_recombines_both_halves() {
        let ft = FILETIME {
            dwHighDateTime: 1,
            dwLowDateTime: 0,
        };
        assert_eq!(filetime_to_u64(ft), 1u64 << 32);
    }

    #[test]
    fn the_first_sample_credits_nothing_because_there_is_no_baseline() {
        let sampler = ProcessSampler::new(16);
        let p = RawProcess {
            pid: 1,
            name: "test".to_string(),
            path: String::new(),
            cpu_100ns: 999_999_999,
            working_set_bytes: 0,
        };
        assert_eq!(
            sampler.share_of_machine(&p, 1500),
            0.0,
            "cumulative time since a process started is not a rate"
        );
    }

    #[test]
    fn one_fully_busy_core_is_reported_as_its_share_of_the_machine() {
        let mut sampler = ProcessSampler::new(16);
        sampler.previous.insert(1, 0);
        let p = RawProcess {
            pid: 1,
            name: "test".to_string(),
            path: String::new(),
            // One second of CPU in 100ns units, over one second of wall clock.
            cpu_100ns: 10_000_000,
            working_set_bytes: 0,
        };
        let share = sampler.share_of_machine(&p, 1000);
        assert!(
            (share - 6.25).abs() < 0.01,
            "one core of sixteen is 6.25 percent of the machine, got {share}"
        );
    }

    #[test]
    fn every_core_busy_is_reported_as_the_whole_machine() {
        let mut sampler = ProcessSampler::new(8);
        sampler.previous.insert(1, 0);
        let p = RawProcess {
            pid: 1,
            name: "test".to_string(),
            path: String::new(),
            cpu_100ns: 8 * 10_000_000,
            working_set_bytes: 0,
        };
        assert_eq!(sampler.share_of_machine(&p, 1000), 100.0);
    }

    #[test]
    fn a_zero_interval_yields_no_share_rather_than_a_division_artefact() {
        let mut sampler = ProcessSampler::new(16);
        sampler.previous.insert(1, 0);
        let p = RawProcess {
            pid: 1,
            name: "test".to_string(),
            path: String::new(),
            cpu_100ns: 10_000_000,
            working_set_bytes: 0,
        };
        assert_eq!(sampler.share_of_machine(&p, 0), 0.0);
    }

    #[test]
    fn a_counter_that_went_backwards_yields_zero_rather_than_wrapping() {
        // Pid reuse can present a fresh process carrying an old pid's baseline.
        let mut sampler = ProcessSampler::new(16);
        sampler.previous.insert(1, 500_000_000);
        let p = RawProcess {
            pid: 1,
            name: "test".to_string(),
            path: String::new(),
            cpu_100ns: 1_000,
            working_set_bytes: 0,
        };
        assert_eq!(sampler.share_of_machine(&p, 1500), 0.0);
    }

    #[test]
    fn this_machine_names_real_applications_rather_than_executables() {
        // The claim the whole friendly naming idea rests on, so it is measured
        // rather than assumed. Every vendor ships a version resource; much of
        // Windows itself does not, which is why the fallback exists.
        let mut sampler = ProcessSampler::new(16);
        let readings = sampler.sample(1_000);
        let named = readings.iter().filter(|p| p.display_name.is_some()).count();
        assert!(
            named * 2 > readings.len(),
            "most processes should carry a description, got {named} of {}",
            readings.len()
        );
        for p in readings.iter().filter_map(|p| p.display_name.as_ref()) {
            assert!(!p.is_empty(), "a description must not be blank");
        }
    }

    #[test]
    fn this_machine_enumerates_processes_without_elevation() {
        // The claim the whole attribution feature rests on, so it is asserted
        // rather than assumed.
        let pids = enumerate_pids();
        assert!(
            pids.len() > 10,
            "a running Windows machine has more than ten processes, got {}",
            pids.len()
        );
        assert!(
            pids.len() < MAX_PROCESSES,
            "the pid buffer was filled, so the list may be silently truncated"
        );
    }

    #[test]
    fn this_machine_reports_named_processes_with_a_working_set() {
        let mut sampler = ProcessSampler::new(16);
        let readings = sampler.sample(1_000);
        assert!(
            readings.len() > 5,
            "an unprivileged token should still open plenty of processes, got {}",
            readings.len()
        );
        assert!(
            readings.iter().all(|p| !p.name.is_empty()),
            "a process with no name cannot be attributed to anything"
        );
        assert!(
            readings.iter().any(|p| p.working_set_bytes > 0),
            "at least one process must report memory it holds"
        );
        assert!(
            readings.iter().all(|p| p.hard_faults_per_sec.is_none()),
            "Windows has no documented per-process hard fault rate, so it must not be invented"
        );
    }

    #[test]
    fn a_second_sample_produces_real_cpu_shares_on_this_machine() {
        let mut sampler = ProcessSampler::new(
            std::thread::available_parallelism()
                .map(|n| n.get() as u32)
                .unwrap_or(1),
        );
        // Real elapsed time, not a fabricated interval. Sampling itself takes
        // a variable amount of wall clock, and feeding a shorter figure than
        // actually passed divides real CPU time by a made up denominator and
        // inflates every share. That produced a test which passed alone and
        // failed whenever the machine was busy with the rest of the suite.
        let started = std::time::Instant::now();
        let first = sampler.sample(0);
        assert!(
            first.iter().all(|p| p.cpu_percent == 0.0),
            "the first sample has no baseline to difference against"
        );

        // Give the machine something to have done between samples.
        std::thread::sleep(std::time::Duration::from_millis(400));
        let second = sampler.sample(started.elapsed().as_millis() as u64);

        let total: f32 = second.iter().map(|p| p.cpu_percent).sum();
        assert!(
            (0.0..=100.0).contains(&total),
            "shares are of the whole machine and cannot exceed it, got {total}"
        );
    }
}
