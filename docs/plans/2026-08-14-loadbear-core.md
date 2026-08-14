# LoadBear Core Engine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the pure diagnosis layer of LoadBear, which decides whether a machine is overloaded and whether that finding has earned the right to interrupt the user, plus the spike that unblocks the Windows sensor work.

**Architecture:** LoadBear is four layers, and only the sensor backend is platform-specific. This plan builds layer three, the diagnosis engine, as a standalone crate with no operating system calls whatsoever. Every function takes a normalized reading plus specification data and returns a verdict. That makes the entire product logic testable with synthetic inputs on any machine, before a single sensor exists.

**Tech Stack:** Rust (2021 edition), `raw-cpuid` 11.6, `serde` 1.0 with `serde_json` 1.0, `thiserror` 2.0. No async runtime in this crate. No Tauri in this crate.

**Spec:** `docs/DESIGN.md`

## Global Constraints

- **No em dashes or double dashes in any user-facing string, comment, or document.** Restructure the sentence instead. This is a brand rule and applies to test fixtures too.
- **Every verdict must trace to a vendor guarantee, a hardware bit, or the machine's own history.** No invented thresholds. If a number cannot be sourced, the check does not ship.
- **LoadBear never states what is "normal"** for a CPU. It states what is out of spec.
- **Notification requires all three of sustained, diagnosable, and actionable.** Sustained means the condition held continuously for 5 minutes.
- **A finding with no attributed cause cannot notify.** A confident wrong attribution is worse than none.
- **Rust edition 2021. Minimum toolchain: stable 1.75 or later.**
- Licence: Apache-2.0. Every new source file needs no licence header; the repository LICENSE covers it.

---

## File Structure

```
loadbear/
  Cargo.toml                          workspace root
  crates/
    loadbear-core/
      Cargo.toml
      src/
        lib.rs                        re-exports, crate docs
        types.rs                      Reading and its components
        spec.rs                       CpuSpec, CpuKey, SpecDb lookup
        verdict.rs                    the four absolute verdicts
        tier.rs                       Easy / Braced / Strained classification
        contract.rs                   the interruption contract
      data/
        cpu-specs.json                embedded specification database
  spikes/
    windows-temp/                     throwaway, not part of the workspace
  docs/
    DESIGN.md
    plans/
```

`loadbear-core` has one responsibility: turn observations into judgements. It must never gain a dependency that touches the operating system, and that rule is what keeps it testable.

---

## Task 1: Spike the Windows temperature path

This is an investigation, not a feature. Its output is an answer and a recommendation. Any code written here is throwaway and must be labelled as such.

**Files:**
- Create: `spikes/windows-temp/NOTES.md`
- Create: `spikes/windows-temp/` (scratch code, excluded from the workspace)

**Interfaces:**
- Consumes: nothing
- Produces: a written recommendation that determines the Windows sensing plan. No code that later tasks depend on.

**Questions the spike must answer:**

1. Can a Rust process load and talk to the WinRing0 driver directly via `DeviceIoControl`, or is a sidecar in another language required?
2. Does per-core temperature enumeration actually work on the development machine's CPU, an AMD Ryzen 7 4980U (family 17h, model 60h, Renoir, OEM-exclusive Surface part)? Everything downstream of the Windows temperature path depends on this and it is unverified.
3. What is the licence of whatever driver would be bundled, and is it compatible with Apache-2.0 distribution?
4. Does Windows Defender or SmartScreen flag the driver load on a current Windows 11 build?

- [ ] **Step 1: Install the Rust toolchain**

Rust is not currently installed on this machine.

```powershell
winget install Rustlang.Rustup
```

Then open a fresh shell and confirm:

```bash
rustc --version
cargo --version
```

Expected: both print a version of 1.75 or later.

- [ ] **Step 2: Record the baseline from a known-good tool**

Core Temp is already installed and running on this machine and successfully reads this CPU. Before testing anything, record what correct output looks like: open Core Temp and write down the per-core temperatures and the TjMax value it reports.

This is the ground truth the spike is measured against. Without it there is no way to tell a wrong reading from a right one.

Write the values into `spikes/windows-temp/NOTES.md` under a heading `## Ground truth from Core Temp`.

- [ ] **Step 3: Test direct driver access from Rust**

Create a scratch binary in `spikes/windows-temp/` that attempts to open the WinRing0 device handle and issue a read against the AMD SMU. Run it elevated.

Record in NOTES.md: whether the handle opens, whether a read returns plausible values, and the exact error if not.

- [ ] **Step 4: Test the sidecar alternative**

Write a minimal C# console application that references LibreHardwareMonitorLib, enumerates every sensor it can find, and prints them as JSON. Run it elevated with Core Temp closed.

Record in NOTES.md: the full sensor list, and specifically whether eight distinct per-core temperature sensors appear, or only a package temperature, or nothing.

- [ ] **Step 5: Check the licence and antivirus behaviour**

Find the licence of the driver binary each approach would ship. Record it verbatim in NOTES.md alongside a note on Apache-2.0 compatibility.

Check Windows Security notifications and quarantine history after running both approaches. Record what, if anything, was flagged.

- [ ] **Step 6: Write the recommendation**

Complete `spikes/windows-temp/NOTES.md` with a section `## Recommendation` stating which path to take and why, in no more than a paragraph.

If per-core temperatures do not enumerate on this CPU under either approach, say so plainly and state what LoadBear reports instead. Package temperature alone is still useful, and the design already requires graceful degradation when temperature is unavailable, so this is a survivable outcome that must be recorded rather than worked around.

- [ ] **Step 7: Commit**

```bash
git add spikes/windows-temp/
git commit -m "spike: settle the Windows temperature path

Throwaway investigation. Code here is not part of the build."
```

---

## Task 2: Workspace scaffolding and core types

**Files:**
- Create: `Cargo.toml`
- Create: `crates/loadbear-core/Cargo.toml`
- Create: `crates/loadbear-core/src/lib.rs`
- Create: `crates/loadbear-core/src/types.rs`

**Interfaces:**
- Consumes: nothing
- Produces: `Reading`, `StallSignal`, `CpuReading`, `ProcessReading`, `ThrottleState`, `ThrottleReason`. Every later task and every sensor backend depends on these exact names and field types.

- [ ] **Step 1: Create the workspace root**

Create `Cargo.toml` at the repository root:

```toml
[workspace]
members = ["crates/loadbear-core"]
resolver = "2"

[workspace.package]
edition = "2021"
rust-version = "1.75"
license = "Apache-2.0"
repository = "https://github.com/bojan-gasparovic/loadbear"

[workspace.dependencies]
serde = { version = "1.0.229", features = ["derive"] }
serde_json = "1.0.151"
thiserror = "2.0.20"
raw-cpuid = "11.6.0"
```

- [ ] **Step 2: Create the core crate manifest**

Create `crates/loadbear-core/Cargo.toml`:

```toml
[package]
name = "loadbear-core"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
description = "Pure diagnosis engine for LoadBear. No operating system calls."

[dependencies]
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
raw-cpuid = { workspace = true }
```

- [ ] **Step 3: Write the failing test**

Create `crates/loadbear-core/src/types.rs` containing only this test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stall_signal_reports_its_worst_resource() {
        let stall = StallSignal { cpu: 0.10, memory: 0.72, io: 0.31 };
        assert_eq!(stall.worst(), (Resource::Memory, 0.72));
    }

    #[test]
    fn stall_signal_with_no_pressure_reports_cpu_at_zero() {
        let stall = StallSignal { cpu: 0.0, memory: 0.0, io: 0.0 };
        assert_eq!(stall.worst(), (Resource::Cpu, 0.0));
    }
}
```

- [ ] **Step 4: Run the test to verify it fails**

Run: `cargo test -p loadbear-core`

Expected: FAIL to compile, with errors that `StallSignal` and `Resource` are not defined.

- [ ] **Step 5: Write the types**

Replace the contents of `crates/loadbear-core/src/types.rs`, keeping the test module at the bottom:

```rust
use serde::{Deserialize, Serialize};

/// Which resource a stall is attributed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Resource {
    Cpu,
    Memory,
    Io,
}

/// Resource stall, normalized across platforms.
///
/// Each value is the share of the sampling window, from 0.0 to 1.0, that work
/// spent waiting on that resource rather than progressing. This is not
/// utilization. On Linux these map to Pressure Stall Information. On Windows
/// and macOS the backend derives equivalent values from native counters.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct StallSignal {
    pub cpu: f32,
    pub memory: f32,
    pub io: f32,
}

impl StallSignal {
    /// The resource under the most pressure, and its value.
    ///
    /// Ties resolve in the order cpu, memory, io, which makes the result
    /// deterministic. The ordering carries no meaning beyond that.
    pub fn worst(&self) -> (Resource, f32) {
        let mut worst = (Resource::Cpu, self.cpu);
        if self.memory > worst.1 {
            worst = (Resource::Memory, self.memory);
        }
        if self.io > worst.1 {
            worst = (Resource::Io, self.io);
        }
        worst
    }
}

/// Why the hardware asserted a throttle signal.
///
/// Read from a hardware bit, never inferred from temperature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThrottleReason {
    Thermal,
    Power,
    Electrical,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThrottleState {
    pub asserted: bool,
    pub reason: Option<ThrottleReason>,
}

/// CPU state at a point in time.
///
/// Every field is optional because backends degrade rather than fail. On
/// Windows without an elevated driver, temperature and power are absent while
/// everything else still works.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CpuReading {
    pub all_core_mhz: Option<u32>,
    pub package_watts: Option<f32>,
    pub package_temp_c: Option<f32>,
    pub tjmax_c: Option<f32>,
    pub throttle: ThrottleState,
}

/// A single process, as seen by the platform backend.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessReading {
    pub pid: u32,
    pub name: String,
    pub working_set_bytes: u64,
    pub hard_faults_per_sec: Option<f32>,
    pub cpu_percent: f32,
}

/// One normalized observation of machine state.
///
/// Produced by a platform sensor backend, consumed by everything above it.
/// Nothing in this crate knows how it was gathered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reading {
    pub timestamp_ms: u64,
    pub stall: StallSignal,
    pub cpu: CpuReading,
    pub processes: Vec<ProcessReading>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stall_signal_reports_its_worst_resource() {
        let stall = StallSignal { cpu: 0.10, memory: 0.72, io: 0.31 };
        assert_eq!(stall.worst(), (Resource::Memory, 0.72));
    }

    #[test]
    fn stall_signal_with_no_pressure_reports_cpu_at_zero() {
        let stall = StallSignal { cpu: 0.0, memory: 0.0, io: 0.0 };
        assert_eq!(stall.worst(), (Resource::Cpu, 0.0));
    }
}
```

- [ ] **Step 6: Wire up the library root**

Create `crates/loadbear-core/src/lib.rs`:

```rust
//! LoadBear diagnosis engine.
//!
//! This crate turns observations into judgements and contains no operating
//! system calls. Every function here takes a normalized [`Reading`] plus
//! specification data and returns a verdict, which is what makes the whole of
//! LoadBear's product logic testable with synthetic input on any machine.
//!
//! The rule this crate exists to enforce: every judgement traces to a vendor
//! guarantee, a hardware bit, or the machine's own history. Nothing here may
//! invent a threshold.

pub mod types;

pub use types::{
    CpuReading, ProcessReading, Reading, Resource, StallSignal, ThrottleReason, ThrottleState,
};
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p loadbear-core`

Expected: PASS, 2 tests.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml crates/
git commit -m "feat(core): add workspace and normalized reading types

Every field on CpuReading is optional because backends degrade rather
than fail. Windows without an elevated driver loses temperature and
power while everything else keeps working."
```

---

## Task 3: CPU specification database

**Files:**
- Create: `crates/loadbear-core/src/spec.rs`
- Create: `crates/loadbear-core/data/cpu-specs.json`
- Modify: `crates/loadbear-core/src/lib.rs`

**Interfaces:**
- Consumes: nothing from earlier tasks
- Produces: `CpuKey`, `Vendor`, `CpuSpec`, `SpecDb`, and `SpecDb::lookup(&CpuKey) -> Option<&CpuSpec>`. Task 4 depends on all of these.

**Design note:** the database carries limits and vendor guarantees only. It must never carry expected or normal operating temperatures, because those do not exist as published data and are not chassis-independent. Coverage gaps are expected and normal, particularly for OEM-exclusive parts, so lookup returns `Option` and callers must handle absence.

- [ ] **Step 1: Write the failing test**

Create `crates/loadbear-core/src/spec.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> SpecDb {
        SpecDb::embedded().expect("embedded database must parse")
    }

    #[test]
    fn looks_up_a_known_cpu_by_cpuid_key() {
        let key = CpuKey { vendor: Vendor::Amd, family: 23, model: 96, stepping: 1 };
        let spec = db().lookup(&key).expect("4980U must be present");
        assert_eq!(spec.base_mhz, 2000);
        assert_eq!(spec.cores, 8);
        assert_eq!(spec.threads, 16);
    }

    #[test]
    fn returns_none_for_an_unknown_cpu() {
        let key = CpuKey { vendor: Vendor::Other, family: 999, model: 999, stepping: 0 };
        assert!(db().lookup(&key).is_none());
    }

    #[test]
    fn exposes_the_configurable_tdp_band_when_published() {
        let key = CpuKey { vendor: Vendor::Amd, family: 23, model: 96, stepping: 1 };
        let spec = db().lookup(&key).unwrap();
        assert_eq!(spec.ctdp_min_watts, Some(10));
        assert_eq!(spec.ctdp_max_watts, Some(25));
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p loadbear-core spec`

Expected: FAIL to compile, `SpecDb`, `CpuKey` and `Vendor` are not defined.

- [ ] **Step 3: Create the seed database**

Create `crates/loadbear-core/data/cpu-specs.json`. Seed it with the development machine's CPU and two widely-held parts so the lookup is exercised against more than one vendor. The full database is populated later from the TechPowerUp API; this is enough to build against.

```json
{
  "version": 1,
  "note": "Limits and vendor guarantees only. Never expected or normal temperatures, which are not published by any vendor and are not chassis-independent.",
  "entries": [
    {
      "vendor": "Amd",
      "family": 23,
      "model": 96,
      "stepping": 1,
      "name": "AMD Ryzen 7 4980U",
      "base_mhz": 2000,
      "boost_mhz": 4400,
      "tdp_watts": 15,
      "ctdp_min_watts": 10,
      "ctdp_max_watts": 25,
      "tjmax_c": 105.0,
      "cores": 8,
      "threads": 16
    },
    {
      "vendor": "Intel",
      "family": 6,
      "model": 140,
      "stepping": 1,
      "name": "Intel Core i7-1185G7",
      "base_mhz": 3000,
      "boost_mhz": 4800,
      "tdp_watts": 28,
      "ctdp_min_watts": 12,
      "ctdp_max_watts": 28,
      "tjmax_c": 100.0,
      "cores": 4,
      "threads": 8
    },
    {
      "vendor": "Amd",
      "family": 25,
      "model": 33,
      "stepping": 2,
      "name": "AMD Ryzen 7 5800X",
      "base_mhz": 3800,
      "boost_mhz": 4700,
      "tdp_watts": 105,
      "ctdp_min_watts": null,
      "ctdp_max_watts": null,
      "tjmax_c": 90.0,
      "cores": 8,
      "threads": 16
    }
  ]
}
```

- [ ] **Step 4: Write the implementation**

Replace `crates/loadbear-core/src/spec.rs`, keeping the test module at the bottom:

```rust
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SpecError {
    #[error("could not parse the embedded specification database: {0}")]
    Parse(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Vendor {
    Intel,
    Amd,
    Other,
}

/// Identifies a CPU model by the values CPUID reports.
///
/// This is the join key between a running machine and the specification
/// database. Brand strings are unreliable, particularly for OEM-exclusive
/// parts, so they are never used for lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CpuKey {
    pub vendor: Vendor,
    pub family: u32,
    pub model: u32,
    pub stepping: u32,
}

/// Published limits and guarantees for one CPU model.
///
/// `base_mhz` is the load-bearing field. Both Intel and AMD commit that the
/// part sustains this frequency at its rated TDP, which makes a sustained
/// all-core clock below it objectively out of spec in any chassis at any
/// ambient temperature. It is the only performance figure here that is a
/// contractual guarantee rather than a best case.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CpuSpec {
    pub vendor: Vendor,
    pub family: u32,
    pub model: u32,
    pub stepping: u32,
    pub name: String,
    pub base_mhz: u32,
    pub boost_mhz: Option<u32>,
    pub tdp_watts: u32,
    pub ctdp_min_watts: Option<u32>,
    pub ctdp_max_watts: Option<u32>,
    pub tjmax_c: Option<f32>,
    pub cores: u8,
    pub threads: u8,
}

impl CpuSpec {
    pub fn key(&self) -> CpuKey {
        CpuKey {
            vendor: self.vendor,
            family: self.family,
            model: self.model,
            stepping: self.stepping,
        }
    }
}

#[derive(Debug, Deserialize)]
struct SpecFile {
    entries: Vec<CpuSpec>,
}

#[derive(Debug, Clone)]
pub struct SpecDb {
    entries: Vec<CpuSpec>,
}

const EMBEDDED: &str = include_str!("../data/cpu-specs.json");

impl SpecDb {
    /// Load the database compiled into the binary.
    ///
    /// LoadBear works offline. There is no runtime network dependency.
    pub fn embedded() -> Result<Self, SpecError> {
        let file: SpecFile = serde_json::from_str(EMBEDDED)?;
        Ok(Self { entries: file.entries })
    }

    /// Find the specification for a CPU.
    ///
    /// Returns `None` for parts that are not in the database, which is an
    /// expected and common outcome rather than an error. OEM-exclusive CPUs
    /// frequently have no published specification anywhere. Callers must fall
    /// back to values read from the chip itself.
    pub fn lookup(&self, key: &CpuKey) -> Option<&CpuSpec> {
        self.entries.iter().find(|spec| spec.key() == *key)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> SpecDb {
        SpecDb::embedded().expect("embedded database must parse")
    }

    #[test]
    fn looks_up_a_known_cpu_by_cpuid_key() {
        let key = CpuKey { vendor: Vendor::Amd, family: 23, model: 96, stepping: 1 };
        let spec = db().lookup(&key).expect("4980U must be present");
        assert_eq!(spec.base_mhz, 2000);
        assert_eq!(spec.cores, 8);
        assert_eq!(spec.threads, 16);
    }

    #[test]
    fn returns_none_for_an_unknown_cpu() {
        let key = CpuKey { vendor: Vendor::Other, family: 999, model: 999, stepping: 0 };
        assert!(db().lookup(&key).is_none());
    }

    #[test]
    fn exposes_the_configurable_tdp_band_when_published() {
        let key = CpuKey { vendor: Vendor::Amd, family: 23, model: 96, stepping: 1 };
        let spec = db().lookup(&key).unwrap();
        assert_eq!(spec.ctdp_min_watts, Some(10));
        assert_eq!(spec.ctdp_max_watts, Some(25));
    }
}
```

- [ ] **Step 5: Export from the library root**

Modify `crates/loadbear-core/src/lib.rs`, adding below the existing `pub mod types;`:

```rust
pub mod spec;

pub use spec::{CpuKey, CpuSpec, SpecDb, SpecError, Vendor};
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p loadbear-core`

Expected: PASS, 5 tests.

- [ ] **Step 7: Commit**

```bash
git add crates/loadbear-core/
git commit -m "feat(core): add the CPU specification database

Limits and vendor guarantees only. Lookup returns Option because
OEM-exclusive parts frequently have no published specification, which
is an expected outcome rather than an error."
```

---

## Task 4: The four absolute verdicts

**Files:**
- Create: `crates/loadbear-core/src/verdict.rs`
- Modify: `crates/loadbear-core/src/lib.rs`

**Interfaces:**
- Consumes: `Reading`, `CpuReading`, `ThrottleState`, `ThrottleReason` from Task 2. `CpuSpec` from Task 3.
- Produces: `Severity`, `VerdictKind`, `Verdict`, and `evaluate(&Reading, Option<&CpuSpec>) -> Vec<Verdict>`. Task 5 consumes all of these.

**Design note:** each verdict names its own basis in its `detail` string, because the honesty rule has to survive contact with the user interface. If a verdict cannot say where its threshold came from, it should not exist.

- [ ] **Step 1: Write the failing test**

Create `crates/loadbear-core/src/verdict.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{CpuSpec, Vendor};
    use crate::types::{CpuReading, Reading, StallSignal, ThrottleReason, ThrottleState};

    fn spec() -> CpuSpec {
        CpuSpec {
            vendor: Vendor::Amd,
            family: 23,
            model: 96,
            stepping: 1,
            name: "AMD Ryzen 7 4980U".to_string(),
            base_mhz: 2000,
            boost_mhz: Some(4400),
            tdp_watts: 15,
            ctdp_min_watts: Some(10),
            ctdp_max_watts: Some(25),
            tjmax_c: Some(105.0),
            cores: 8,
            threads: 16,
        }
    }

    fn reading(cpu: CpuReading) -> Reading {
        Reading {
            timestamp_ms: 0,
            stall: StallSignal { cpu: 0.0, memory: 0.0, io: 0.0 },
            cpu,
            processes: vec![],
        }
    }

    fn healthy_cpu() -> CpuReading {
        CpuReading {
            all_core_mhz: Some(2400),
            package_watts: Some(15.0),
            package_temp_c: Some(70.0),
            tjmax_c: Some(105.0),
            throttle: ThrottleState { asserted: false, reason: None },
        }
    }

    #[test]
    fn a_healthy_machine_produces_no_verdicts() {
        let verdicts = evaluate(&reading(healthy_cpu()), Some(&spec()));
        assert!(verdicts.is_empty(), "got {verdicts:?}");
    }

    #[test]
    fn sustained_clock_below_guaranteed_base_is_out_of_spec() {
        let mut cpu = healthy_cpu();
        cpu.all_core_mhz = Some(1600);
        let verdicts = evaluate(&reading(cpu), Some(&spec()));
        let v = verdicts
            .iter()
            .find(|v| v.kind == VerdictKind::BelowBaseClock)
            .expect("must flag below base clock");
        assert_eq!(v.severity, Severity::OutOfSpec);
    }

    #[test]
    fn clock_at_exactly_base_is_within_spec() {
        let mut cpu = healthy_cpu();
        cpu.all_core_mhz = Some(2000);
        let verdicts = evaluate(&reading(cpu), Some(&spec()));
        assert!(!verdicts.iter().any(|v| v.kind == VerdictKind::BelowBaseClock));
    }

    #[test]
    fn an_asserted_throttle_is_reported_with_its_reason() {
        let mut cpu = healthy_cpu();
        cpu.throttle = ThrottleState {
            asserted: true,
            reason: Some(ThrottleReason::Thermal),
        };
        let verdicts = evaluate(&reading(cpu), Some(&spec()));
        assert!(verdicts.iter().any(|v| v.kind == VerdictKind::Throttling));
    }

    #[test]
    fn power_above_the_configurable_band_is_flagged() {
        let mut cpu = healthy_cpu();
        cpu.package_watts = Some(31.0);
        let verdicts = evaluate(&reading(cpu), Some(&spec()));
        assert!(verdicts.iter().any(|v| v.kind == VerdictKind::PowerOutsideBand));
    }

    #[test]
    fn low_thermal_headroom_is_degraded_not_out_of_spec() {
        let mut cpu = healthy_cpu();
        cpu.package_temp_c = Some(103.0);
        let verdicts = evaluate(&reading(cpu), Some(&spec()));
        let v = verdicts
            .iter()
            .find(|v| v.kind == VerdictKind::ThermalHeadroomLow)
            .expect("must flag low headroom");
        assert_eq!(
            v.severity,
            Severity::Degraded,
            "running at the thermal limit is by design on modern parts and is not a fault"
        );
    }

    #[test]
    fn without_a_spec_only_chip_sourced_verdicts_are_produced() {
        let mut cpu = healthy_cpu();
        cpu.all_core_mhz = Some(800);
        cpu.throttle = ThrottleState { asserted: true, reason: Some(ThrottleReason::Power) };
        let verdicts = evaluate(&reading(cpu), None);
        assert!(verdicts.iter().any(|v| v.kind == VerdictKind::Throttling));
        assert!(
            !verdicts.iter().any(|v| v.kind == VerdictKind::BelowBaseClock),
            "base clock cannot be judged without a published guarantee"
        );
    }

    #[test]
    fn missing_optional_readings_produce_no_verdicts_rather_than_panicking() {
        let cpu = CpuReading {
            all_core_mhz: None,
            package_watts: None,
            package_temp_c: None,
            tjmax_c: None,
            throttle: ThrottleState { asserted: false, reason: None },
        };
        let verdicts = evaluate(&reading(cpu), Some(&spec()));
        assert!(verdicts.is_empty());
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p loadbear-core verdict`

Expected: FAIL to compile, `evaluate`, `Verdict`, `VerdictKind` and `Severity` are not defined.

- [ ] **Step 3: Write the implementation**

Replace `crates/loadbear-core/src/verdict.rs`, keeping the test module at the bottom:

```rust
use serde::{Deserialize, Serialize};

use crate::spec::CpuSpec;
use crate::types::Reading;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Severity {
    Ok,
    Degraded,
    OutOfSpec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerdictKind {
    /// Sustained all-core frequency below the vendor-guaranteed base clock.
    BelowBaseClock,
    /// The hardware is asserting a throttle signal.
    Throttling,
    /// Package power outside the rated or configurable TDP band.
    PowerOutsideBand,
    /// Close to the junction temperature limit.
    ThermalHeadroomLow,
}

/// A single judgement about machine state.
///
/// `basis` records where the threshold came from and is not decorative. It
/// exists so the honesty rule survives into the user interface: if a verdict
/// cannot say what authority it rests on, it should not exist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    pub kind: VerdictKind,
    pub severity: Severity,
    pub detail: String,
    pub basis: String,
}

/// Headroom below TjMax, in degrees Celsius, at which headroom is called low.
///
/// This is not a health threshold and must not be presented as one. Modern
/// parts are designed to run at their thermal limit under load, and AMD states
/// this explicitly. It marks the point at which thermal headroom has stopped
/// being available, which is information, not a fault.
const THERMAL_HEADROOM_LOW_C: f32 = 5.0;

/// Evaluate every absolute check that the available data supports.
///
/// `spec` is optional because OEM-exclusive parts frequently have no published
/// specification. When it is absent, checks that depend on published figures
/// are skipped rather than guessed, and the checks that read straight from the
/// chip continue to work.
pub fn evaluate(reading: &Reading, spec: Option<&CpuSpec>) -> Vec<Verdict> {
    let mut verdicts = Vec::new();

    if reading.cpu.throttle.asserted {
        let reason = match reading.cpu.throttle.reason {
            Some(r) => format!("{r:?}"),
            None => "unspecified".to_string(),
        };
        verdicts.push(Verdict {
            kind: VerdictKind::Throttling,
            severity: Severity::OutOfSpec,
            detail: format!("The hardware is asserting a throttle signal. Reason: {reason}."),
            basis: "Hardware status bit, read directly. Not inferred from temperature.".to_string(),
        });
    }

    if let (Some(temp), Some(tjmax)) = (reading.cpu.package_temp_c, reading.cpu.tjmax_c) {
        let headroom = tjmax - temp;
        if headroom <= THERMAL_HEADROOM_LOW_C {
            verdicts.push(Verdict {
                kind: VerdictKind::ThermalHeadroomLow,
                severity: Severity::Degraded,
                detail: format!("{headroom:.1} degrees C of headroom below the limit of {tjmax:.0}."),
                basis: "TjMax as reported by the processor. Running at the limit is by design on modern parts.".to_string(),
            });
        }
    }

    let Some(spec) = spec else {
        return verdicts;
    };

    if let Some(mhz) = reading.cpu.all_core_mhz {
        if mhz < spec.base_mhz {
            verdicts.push(Verdict {
                kind: VerdictKind::BelowBaseClock,
                severity: Severity::OutOfSpec,
                detail: format!(
                    "All cores are sustaining {mhz} MHz against a guaranteed base of {} MHz.",
                    spec.base_mhz
                ),
                basis: format!(
                    "{} publishes {} MHz as the base clock, which the vendor guarantees at the rated TDP.",
                    spec.name, spec.base_mhz
                ),
            });
        }
    }

    if let Some(watts) = reading.cpu.package_watts {
        let ceiling = spec.ctdp_max_watts.unwrap_or(spec.tdp_watts) as f32;
        if watts > ceiling {
            verdicts.push(Verdict {
                kind: VerdictKind::PowerOutsideBand,
                severity: Severity::Degraded,
                detail: format!("Package power is {watts:.1} W against a ceiling of {ceiling:.0} W."),
                basis: "Rated TDP and configurable TDP band as published for this model.".to_string(),
            });
        }
    }

    verdicts
}
```

Append the test module from Step 1 unchanged.

- [ ] **Step 4: Export from the library root**

Modify `crates/loadbear-core/src/lib.rs`, adding:

```rust
pub mod verdict;

pub use verdict::{evaluate, Severity, Verdict, VerdictKind};
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p loadbear-core`

Expected: PASS, 13 tests.

- [ ] **Step 6: Commit**

```bash
git add crates/loadbear-core/
git commit -m "feat(core): add the four absolute verdicts

Every verdict carries a basis string recording what authority its
threshold rests on. Verdicts needing published data are skipped rather
than guessed when the CPU is not in the database.

Low thermal headroom is Degraded rather than OutOfSpec because modern
parts are designed to run at their thermal limit under load."
```

---

## Task 5: Tier classification

**Files:**
- Create: `crates/loadbear-core/src/tier.rs`
- Modify: `crates/loadbear-core/src/lib.rs`

**Interfaces:**
- Consumes: `StallSignal` from Task 2, `Severity` and `Verdict` from Task 4.
- Produces: `Tier` and `classify(&[Verdict], &StallSignal) -> Tier`. Task 6 consumes both.

**Design note:** the tier is severity of state and nothing else. Whether the user is interrupted is decided separately in Task 6. Coupling them produces states where Braced is worse than Strained, which is incoherent the moment a user notices it.

- [ ] **Step 1: Write the failing test**

Create `crates/loadbear-core/src/tier.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::StallSignal;
    use crate::verdict::{Severity, Verdict, VerdictKind};

    fn quiet() -> StallSignal {
        StallSignal { cpu: 0.02, memory: 0.0, io: 0.01 }
    }

    fn verdict(kind: VerdictKind, severity: Severity) -> Verdict {
        Verdict {
            kind,
            severity,
            detail: "test".to_string(),
            basis: "test".to_string(),
        }
    }

    #[test]
    fn no_verdicts_and_no_stall_is_easy() {
        assert_eq!(classify(&[], &quiet()), Tier::Easy);
    }

    #[test]
    fn an_out_of_spec_verdict_is_strained() {
        let v = [verdict(VerdictKind::BelowBaseClock, Severity::OutOfSpec)];
        assert_eq!(classify(&v, &quiet()), Tier::Strained);
    }

    #[test]
    fn a_degraded_verdict_alone_is_braced() {
        let v = [verdict(VerdictKind::ThermalHeadroomLow, Severity::Degraded)];
        assert_eq!(classify(&v, &quiet()), Tier::Braced);
    }

    #[test]
    fn heavy_stall_alone_is_braced_even_with_no_verdicts() {
        let stall = StallSignal { cpu: 0.10, memory: 0.55, io: 0.20 };
        assert_eq!(classify(&[], &stall), Tier::Braced);
    }

    #[test]
    fn severe_stall_alone_is_strained_even_with_no_verdicts() {
        let stall = StallSignal { cpu: 0.10, memory: 0.85, io: 0.20 };
        assert_eq!(
            classify(&[], &stall),
            Tier::Strained,
            "a machine paging itself to death is strained whether or not any published limit was crossed"
        );
    }

    #[test]
    fn the_worst_input_wins() {
        let v = [verdict(VerdictKind::ThermalHeadroomLow, Severity::Degraded)];
        let stall = StallSignal { cpu: 0.10, memory: 0.85, io: 0.20 };
        assert_eq!(classify(&v, &stall), Tier::Strained);
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p loadbear-core tier`

Expected: FAIL to compile, `classify` and `Tier` are not defined.

- [ ] **Step 3: Write the implementation**

Replace `crates/loadbear-core/src/tier.rs`, keeping the test module at the bottom:

```rust
use serde::{Deserialize, Serialize};

use crate::types::StallSignal;
use crate::verdict::{Severity, Verdict};

/// How the machine is doing.
///
/// This is severity of state only. Whether the user is interrupted is decided
/// separately by the interruption contract. Strained without a notification is
/// a legitimate and common state: the machine is genuinely struggling, but the
/// cause is the build the user deliberately started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Tier {
    /// Within spec, headroom available.
    Easy,
    /// Degraded, or evidence still accumulating.
    Braced,
    /// Sustained and out of spec.
    Strained,
}

/// Stall at or above this share of the window counts as degraded.
const STALL_BRACED: f32 = 0.40;

/// Stall at or above this share of the window counts as out of spec.
///
/// At this level the majority of the sampling window was spent waiting rather
/// than progressing, which is the definition of overloaded regardless of what
/// any published limit says.
const STALL_STRAINED: f32 = 0.80;

/// Classify machine state from verdicts and stall together.
///
/// The worst input wins. Either signal can drive the tier on its own, because
/// each catches conditions the other misses. Verdicts catch a machine quietly
/// running below its guaranteed clock while feeling responsive. Stall catches
/// a machine paging itself to death without crossing any published limit.
pub fn classify(verdicts: &[Verdict], stall: &StallSignal) -> Tier {
    let from_verdicts = verdicts
        .iter()
        .map(|v| match v.severity {
            Severity::OutOfSpec => Tier::Strained,
            Severity::Degraded => Tier::Braced,
            Severity::Ok => Tier::Easy,
        })
        .max()
        .unwrap_or(Tier::Easy);

    let (_, worst_stall) = stall.worst();
    let from_stall = if worst_stall >= STALL_STRAINED {
        Tier::Strained
    } else if worst_stall >= STALL_BRACED {
        Tier::Braced
    } else {
        Tier::Easy
    };

    from_verdicts.max(from_stall)
}
```

Append the test module from Step 1 unchanged.

- [ ] **Step 4: Export from the library root**

Modify `crates/loadbear-core/src/lib.rs`, adding:

```rust
pub mod tier;

pub use tier::{classify, Tier};
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p loadbear-core`

Expected: PASS, 19 tests.

- [ ] **Step 6: Commit**

```bash
git add crates/loadbear-core/
git commit -m "feat(core): add tier classification

Tier is severity of state only, decoupled from whether the user is
interrupted. Coupling them produces states where Braced is worse than
Strained, which is incoherent the moment a user notices it.

Either verdicts or stall can drive the tier alone, because each catches
what the other misses."
```

---

## Task 6: The interruption contract

**Files:**
- Create: `crates/loadbear-core/src/contract.rs`
- Modify: `crates/loadbear-core/src/lib.rs`

**Interfaces:**
- Consumes: `Tier` from Task 5, `Verdict` from Task 4.
- Produces: `CauseKind`, `Cause`, `Remediation`, `Finding`, `NotificationGate`, `NotificationGate::new(sustained_ms)`, and `NotificationGate::evaluate(&mut self, Tier, Option<&Finding>, now_ms) -> bool`. The shell consumes all of these.

**Design note:** the gate holds state across calls, which makes time the thing to be careful about. `now_ms` is passed in rather than read from the clock so the sustained window is testable without waiting five minutes.

- [ ] **Step 1: Write the failing test**

Create `crates/loadbear-core/src/contract.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::tier::Tier;
    use crate::verdict::{Severity, Verdict, VerdictKind};

    const FIVE_MIN: u64 = 5 * 60 * 1000;

    fn actionable_finding() -> Finding {
        Finding {
            verdict: Verdict {
                kind: VerdictKind::BelowBaseClock,
                severity: Severity::OutOfSpec,
                detail: "test".to_string(),
                basis: "test".to_string(),
            },
            cause: Some(Cause {
                label: "Docker Desktop".to_string(),
                kind: CauseKind::Process { pid: 4242 },
            }),
            remediation: Some(Remediation::ReconfigureLimit),
        }
    }

    fn undiagnosed_finding() -> Finding {
        Finding {
            verdict: actionable_finding().verdict,
            cause: None,
            remediation: None,
        }
    }

    #[test]
    fn a_spike_never_notifies() {
        let mut gate = NotificationGate::new(FIVE_MIN);
        assert!(!gate.evaluate(Tier::Strained, Some(&actionable_finding()), 0));
        assert!(!gate.evaluate(Tier::Strained, Some(&actionable_finding()), 90_000));
    }

    #[test]
    fn a_sustained_actionable_finding_notifies_once_the_window_elapses() {
        let mut gate = NotificationGate::new(FIVE_MIN);
        assert!(!gate.evaluate(Tier::Strained, Some(&actionable_finding()), 0));
        assert!(gate.evaluate(Tier::Strained, Some(&actionable_finding()), FIVE_MIN));
    }

    #[test]
    fn it_does_not_notify_twice_for_one_episode() {
        let mut gate = NotificationGate::new(FIVE_MIN);
        gate.evaluate(Tier::Strained, Some(&actionable_finding()), 0);
        assert!(gate.evaluate(Tier::Strained, Some(&actionable_finding()), FIVE_MIN));
        assert!(!gate.evaluate(Tier::Strained, Some(&actionable_finding()), FIVE_MIN + 1000));
    }

    #[test]
    fn recovering_resets_the_window() {
        let mut gate = NotificationGate::new(FIVE_MIN);
        gate.evaluate(Tier::Strained, Some(&actionable_finding()), 0);
        gate.evaluate(Tier::Easy, None, 60_000);
        assert!(
            !gate.evaluate(Tier::Strained, Some(&actionable_finding()), FIVE_MIN),
            "the clock restarts after recovery, so this is only one minute of strain"
        );
    }

    #[test]
    fn an_undiagnosed_finding_never_notifies() {
        let mut gate = NotificationGate::new(FIVE_MIN);
        gate.evaluate(Tier::Strained, Some(&undiagnosed_finding()), 0);
        assert!(
            !gate.evaluate(Tier::Strained, Some(&undiagnosed_finding()), FIVE_MIN),
            "a finding with no attributed cause cannot satisfy the actionable condition"
        );
    }

    #[test]
    fn strain_with_nothing_to_do_never_notifies() {
        let mut gate = NotificationGate::new(FIVE_MIN);
        let mut finding = actionable_finding();
        finding.remediation = None;
        gate.evaluate(Tier::Strained, Some(&finding), 0);
        assert!(
            !gate.evaluate(Tier::Strained, Some(&finding), FIVE_MIN),
            "compiling is slow is not worth an interruption"
        );
    }

    #[test]
    fn braced_never_notifies_however_long_it_lasts() {
        let mut gate = NotificationGate::new(FIVE_MIN);
        gate.evaluate(Tier::Braced, Some(&actionable_finding()), 0);
        assert!(!gate.evaluate(Tier::Braced, Some(&actionable_finding()), FIVE_MIN * 10));
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p loadbear-core contract`

Expected: FAIL to compile, `NotificationGate`, `Finding`, `Cause`, `CauseKind` and `Remediation` are not defined.

- [ ] **Step 3: Write the implementation**

Replace `crates/loadbear-core/src/contract.rs`, keeping the test module at the bottom:

```rust
use serde::{Deserialize, Serialize};

use crate::tier::Tier;
use crate::verdict::Verdict;

/// What kind of thing is responsible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CauseKind {
    Process { pid: u32 },
    Container { id: String },
    SystemService,
    PowerState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cause {
    pub label: String,
    pub kind: CauseKind,
}

/// What the user can actually do about it.
///
/// This deliberately does not require that something can be killed. Being
/// unable to end a process is not the same as there being nothing to do, and
/// the difference covers some of the highest-value findings available. Adding
/// an antivirus exclusion for a build directory is the clearest example.
///
/// Every variant names a specific action. A finding that cannot be stated as a
/// sentence ending in a thing the user does has no remediation and therefore
/// cannot notify. No variant may be added that means "something is happening".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Remediation {
    /// End a process or container the user started.
    Stop,
    /// Change an allocation, such as Docker Desktop memory or WSL2 config.
    ReconfigureLimit,
    /// Exclude a path from antivirus or indexing.
    AddExclusion,
    /// Postpone background work such as an update or an index rebuild.
    Defer,
    /// Plug in, or change the power profile.
    ChangePowerState,
    /// Physical intervention, such as clearing dust. Baseline-driven.
    Physical,
}

/// A verdict, what caused it, and what to do about it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub verdict: Verdict,
    pub cause: Option<Cause>,
    pub remediation: Option<Remediation>,
}

impl Finding {
    /// Whether this finding has earned the right to interrupt.
    ///
    /// Requires both a named cause and a concrete action. Either one missing
    /// means the notification would amount to telling the user their machine
    /// is busy, which they already know.
    pub fn is_actionable(&self) -> bool {
        self.cause.is_some() && self.remediation.is_some()
    }
}

/// Decides whether a finding may interrupt the user.
///
/// Notification fatigue is what kills tools in this category. A tool that pops
/// during a routine build is muted the same day and never heard from again, so
/// this gate is deliberately restrictive and stays quiet through plenty of
/// genuinely heavy load.
///
/// Time is passed in rather than read from the clock, which is what makes the
/// sustained window testable without waiting five minutes.
#[derive(Debug, Clone)]
pub struct NotificationGate {
    sustained_ms: u64,
    strained_since_ms: Option<u64>,
    notified_this_episode: bool,
}

impl NotificationGate {
    /// `sustained_ms` is how long a condition must hold continuously before it
    /// may escalate. The design specifies five minutes as a deliberate first
    /// guess to be tuned against real use, not a derived constant.
    pub fn new(sustained_ms: u64) -> Self {
        Self {
            sustained_ms,
            strained_since_ms: None,
            notified_this_episode: false,
        }
    }

    /// Returns true exactly once per episode, when all three conditions of the
    /// interruption contract are satisfied: sustained, diagnosable, actionable.
    pub fn evaluate(&mut self, tier: Tier, finding: Option<&Finding>, now_ms: u64) -> bool {
        if tier != Tier::Strained {
            self.strained_since_ms = None;
            self.notified_this_episode = false;
            return false;
        }

        let since = *self.strained_since_ms.get_or_insert(now_ms);

        if self.notified_this_episode {
            return false;
        }

        if now_ms.saturating_sub(since) < self.sustained_ms {
            return false;
        }

        let Some(finding) = finding else {
            return false;
        };

        if !finding.is_actionable() {
            return false;
        }

        self.notified_this_episode = true;
        true
    }

    /// Whether an episode of strain is currently being tracked.
    pub fn is_tracking(&self) -> bool {
        self.strained_since_ms.is_some()
    }
}
```

Append the test module from Step 1 unchanged.

- [ ] **Step 4: Export from the library root**

Modify `crates/loadbear-core/src/lib.rs`, adding:

```rust
pub mod contract;

pub use contract::{Cause, CauseKind, Finding, NotificationGate, Remediation};
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p loadbear-core`

Expected: PASS, 26 tests.

- [ ] **Step 6: Verify the whole crate is clean**

Run: `cargo clippy -p loadbear-core -- -D warnings`

Expected: no warnings.

Run: `cargo fmt --check`

Expected: no diff.

- [ ] **Step 7: Commit**

```bash
git add crates/loadbear-core/
git commit -m "feat(core): add the interruption contract

Notifies exactly once per episode, and only when sustained, diagnosable
and actionable all hold. Strained with nothing to do never notifies,
because telling someone that compiling is slow is how a tool gets muted.

Time is injected rather than read from the clock so the five minute
window is testable without waiting five minutes."
```

---

## Self-Review

**Spec coverage.** Section 4 (stall) is `StallSignal` in Task 2. Section 5 (four absolute verdicts) is Task 4. Section 6 (tiers) is Task 5. Section 7 (interruption contract, remediation classes, attribution correctness bar) is Task 6, with the correctness bar enforced by `Finding::is_actionable` requiring a cause. Section 10 (specification database, its coverage gaps, and the prohibition on normal ranges) is Task 3. Section 16 open decision 1 (Windows temperature path) is Task 1.

**Deliberately not in this plan, and why.** Sections 9 (privilege model), 11 (baseline), 12 (sample store), and the presentation half of section 13 all require operating system access or a running shell. They belong to the Windows sensing plan and the shell plan, and the Windows sensing plan cannot be written honestly until Task 1 reports back.

**Type consistency check.** `Reading`, `StallSignal`, `CpuReading`, `ThrottleState` and `ThrottleReason` are defined in Task 2 and used with identical field names in Tasks 4, 5 and 6. `CpuSpec` fields defined in Task 3 are read in Task 4 as `base_mhz`, `tdp_watts`, `ctdp_max_watts` and `name`, all of which exist. `Severity` and `Verdict` from Task 4 are consumed in Tasks 5 and 6 unchanged. `Tier` from Task 5 is consumed in Task 6. No function is referenced that is not defined.

**One known deviation from strict TDD.** Task 1 is a spike and has no tests, because its output is an answer rather than code that ships. Any code written during it is throwaway and labelled as such in the commit.
