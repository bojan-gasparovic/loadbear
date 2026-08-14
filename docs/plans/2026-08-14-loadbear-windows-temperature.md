# LoadBear Windows Temperature Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Read CPU temperature on Windows through a driver that Microsoft does not block, with elevation required only at install time and never at runtime.

**Architecture:** Temperature is the only privileged reading anywhere in LoadBear. It is reached through PawnIO, a signed kernel driver that executes sandboxed bytecode modules exposing narrow ioctls, rather than through a driver that hands userspace raw ring-0 access. The driver is registered once by the installer as a service. LoadBear itself runs unprivileged and talks to the already-running device, which is the same pattern that let an unelevated shell read Core Temp's data during the LB-02 spike.

**Tech Stack:** Rust, `libloading` 0.9 to resolve `PawnIOLib.dll` at runtime, PawnIO driver plus its `AMDFamily17` and `IntelMSR` module blobs.

**Spec:** `docs/DESIGN.md`
**Prior plan:** `docs/plans/2026-08-14-loadbear-core.md`

## Global Constraints

- **No em dashes or double dashes** in any user-facing string, comment, or document.
- **Never instruct a user to weaken a security control.** Disabling the Windows vulnerable driver blocklist is not an acceptable workaround, not even behind a warning. If a driver needs that, the driver is wrong.
- **Elevation at install time only.** Any design requiring administrator rights on every run is rejected.
- **Temperature is optional.** Every code path must stay useful when the driver is absent, refused, or unsupported. The core engine already skips checks rather than guessing.
- **Rust edition 2021, minimum toolchain stable 1.75.**

---

## Why not WinRing0

Recorded here because it will be proposed again by anyone who searches this problem.

WinRing0 is the traditional answer and it is no longer viable:

- Microsoft Defender has classified it as `VulnerableDriver:WinNT/Winring0` since mid-March 2025, with several variants catalogued (`.B`, `.D`, `.G`), and blocks it automatically
- The underlying flaw, CVE-2020-14979, lets an unprivileged process read and write arbitrary memory, which is why it became a Bring Your Own Vulnerable Driver target
- On Windows 11 22H2 and later the vulnerable driver blocklist blocks it, and the only workaround is a registry change that disables that blocklist

The last point is decisive. Shipping a tool whose installation instructions include turning off a Microsoft security feature is not something LoadBear will do.

## Why PawnIO

- Signed, and compatible with HVCI and Memory Integrity, so it works on a hardened default install
- Runs modules written in Pawn that expose specific, narrow ioctls, instead of exposing raw MSR and memory primitives. That is what removes the BYOVD exposure
- Already adopted by the projects that hit this wall first: LibreHardwareMonitor swapped to it in 0.9.5, FanControl from v238, OpenRGB alongside
- Installs headless from an installer, creating the `Root\PawnIO` device
- Modules exist for the hardware LoadBear targets: `AMDFamily17.p` covers Zen family 17h, which includes the development machine's Renoir part, `IntelMSR.p` covers Intel, and `RyzenSMU.p` covers SMU access

## LoadBear does not ship the driver

**Decided 2026-08-14.** LoadBear bundles no kernel driver and no `PawnIOLib.dll`. Both arrive with the user's own PawnIO installation. LoadBear detects the library at startup, and when it is absent reports temperature as unavailable and points the user at pawnio.eu.

**It does ship the module blobs.** Amended 2026-08-14 after reading the upstream header: the compiled modules are a separate 63 KB download from the PawnIO.Modules releases, under LGPL-2.1. Requiring a second download would roughly double first-run friction for no licence benefit worth having, because LGPL redistribution is a NOTICE file and a source link rather than the GPL obligations that made bundling the driver unacceptable. `AMDFamily17` and `IntelMSR` ship with LoadBear; the driver and the library do not.

**We also do not open the device ourselves.** PawnIO publishes a userspace library precisely so callers avoid issuing control codes directly, so there are no ioctl constants to transcribe and no chance of a wrong one corrupting kernel state. `PawnIOLib.dll` is LGPL-2.1 and is loaded dynamically at runtime, which is the arrangement LGPL exists to permit for a differently-licensed caller.

This is a deliberate rejection of bundling, on the grounds that redistribution is the entire source of the legal dependency and redistribution is optional.

Not bundling removes, in one move:

- GPL-2.0 redistribution obligations, so no source offer, no NOTICE file, no pinned SHA of someone else's binary
- The need for permission to bundle, which is genuinely unanswered by PawnIO's public documentation and would otherwise require an agreement with an individual
- Any licence mixing at all. The LoadBear tree stays cleanly Apache-2.0 with nothing GPL in it

What remains needs no goodwill, because the author granted it explicitly in the licence text:

> as a special exception, the copyright holders of PawnIO give you permission to combine PawnIO program with free software programs or libraries released under the GNU LGPL and with independent modules that communicate with PawnIO **solely through the device IO control interface**

An application talking to a driver the user installed themselves is the most ordinary relationship in software.

**The cost is a worse first run**, and that is accepted. The audience already installs Docker and WSL, so "install this driver to enable temperature" is a normal ask.

**What remains is operational, not legal.** If PawnIO stops being maintained, temperature degrades. Everything else in LoadBear, including the base clock verdict, works unprivileged with no driver at all.

---

## File Structure

```
loadbear/
  crates/
    loadbear-core/                      unchanged, no OS calls
    loadbear-sensors-windows/           NEW
      Cargo.toml
      src/
        lib.rs                          public surface, WindowsTemperature
        pawnio.rs                       device handle, module load, ioctl calls
        amd.rs                          Zen family 17h temperature
        intel.rs                        Intel MSR temperature and TjMax
  docs/
    plans/
```

No `vendor/` directory. Nothing of PawnIO enters this tree.

`loadbear-sensors-windows` is the only crate permitted to touch a driver. It depends on `loadbear-core` for types and never the other way round.

---

## Task 1: PawnIO client

**Files:**
- Create: `crates/loadbear-sensors-windows/Cargo.toml`
- Create: `crates/loadbear-sensors-windows/src/lib.rs`
- Create: `crates/loadbear-sensors-windows/src/pawnio.rs`
- Modify: `Cargo.toml` (workspace members)

**Interfaces:**
- Consumes: nothing from earlier tasks
- Produces: `PawnIo`, `PawnIoError`, `PawnIo::open() -> Result<PawnIo, PawnIoError>`, `PawnIo::load_module(&self, blob: &[u8]) -> Result<(), PawnIoError>`, `PawnIo::execute(&self, name: &str, input: &[u64], out_capacity: usize) -> Result<Vec<u64>, PawnIoError>`. Tasks 2 and 3 depend on these exact signatures.

**Signature correction.** `execute` gained `out_capacity`, which the sketch above did not have. The upstream API requires the caller to size the output buffer and reports how many slots were written, so the parameter is not optional. This was found by reading the header rather than by a compile error, which is the point of Step 1.

- [x] **Step 1: Read the PawnIO userspace interface before writing any of it**

**Done 2026-08-14, and it removed the hardest part of this task.** The interface is not a set of ioctls. PawnIO publishes a userspace library, `PawnIOLib.dll`, with a documented C API in `PawnIOLib/include/PawnIOLib.h`:

```c
PAWNIOAPI pawnio_version(PULONG version);
PAWNIOAPI pawnio_open(PHANDLE handle);
PAWNIOAPI pawnio_load(HANDLE handle, const UCHAR* blob, SIZE_T size);
PAWNIOAPI pawnio_execute(HANDLE handle, PCSTR name,
                         const ULONG64* in,  SIZE_T in_size,
                         PULONG64      out, SIZE_T out_size,
                         PSIZE_T return_size);
PAWNIOAPI pawnio_close(HANDLE handle);
```

`PAWNIOAPI` expands to `EXTERN_C __declspec(dllimport) HRESULT STDAPICALLTYPE`, so every entry point returns an `HRESULT` where zero is success.

So there are no ioctl codes to invent, no device path to hardcode, and no buffer layouts to reverse. The header block at the top of `pawnio.rs` records this provenance verbatim with the date it was read.

- [ ] **Step 2: Write the failing test**

The tests that can run without a driver are the absence and error paths. Those are the ones that matter most anyway, because they are what every user without the driver hits.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_driver_reports_not_installed_rather_than_a_raw_os_error() {
        // On a machine with no PawnIO device, open() must map the underlying
        // ERROR_FILE_NOT_FOUND to a domain error the UI can explain.
        match PawnIo::open() {
            Err(PawnIoError::NotInstalled) => {}
            Err(e) => panic!("expected NotInstalled, got {e:?}"),
            Ok(_) => {
                // The driver is present on this machine, which is a valid state.
                // Nothing to assert here; the elevated verification task covers it.
            }
        }
    }

    #[test]
    fn errors_are_displayable_without_leaking_raw_win32_codes() {
        let e = PawnIoError::NotInstalled;
        let msg = e.to_string();
        assert!(!msg.is_empty());
        assert!(
            !msg.contains("0x"),
            "user-facing text must not contain raw error codes: {msg}"
        );
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p loadbear-sensors-windows`
Expected: FAIL to compile, `PawnIo` and `PawnIoError` are not defined.

- [ ] **Step 4: Implement the client**

Write `pawnio.rs` using the interface recorded in Step 1. It must:

- Open the device with `CreateFileW` on the documented path
- Map `ERROR_FILE_NOT_FOUND` to `PawnIoError::NotInstalled` and `ERROR_ACCESS_DENIED` to `PawnIoError::AccessDenied`, rather than surfacing raw codes
- Close the handle on drop
- Never panic on a driver error. Every failure returns a typed error, because the crate above must degrade rather than crash

Error type:

```rust
#[derive(Debug, thiserror::Error)]
pub enum PawnIoError {
    #[error("the PawnIO driver is not installed on this machine")]
    NotInstalled,
    #[error("access to the PawnIO device was refused")]
    AccessDenied,
    #[error("the PawnIO module could not be loaded")]
    ModuleLoadFailed,
    #[error("the PawnIO function '{0}' is not available in the loaded module")]
    FunctionUnavailable(String),
    #[error("the PawnIO device returned an unexpected response")]
    UnexpectedResponse,
}
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p loadbear-sensors-windows`
Expected: PASS. On this machine `NotInstalled` is the branch that runs, since the LB-02 spike confirmed no such driver is present.

- [ ] **Step 6: Commit**

```bash
git add crates/loadbear-sensors-windows/ Cargo.toml
git commit -m "feat(sensors): add the PawnIO client

Ioctl codes and buffer layouts are cited to upstream rather than guessed.
Absence of the driver is a typed domain error, not a raw Win32 code,
because it is the state most users will be in."
```

---

## Task 2: AMD Zen temperature

**Files:**
- Create: `crates/loadbear-sensors-windows/src/amd.rs`
- Modify: `crates/loadbear-sensors-windows/src/lib.rs`

**Interfaces:**
- Consumes: `PawnIo` from Task 1
- Produces: `read_amd_temperature(&PawnIo) -> Result<TemperatureReading, PawnIoError>` and `pub struct TemperatureReading { pub package_c: Option<f32>, pub per_core_c: Vec<f32> }`. Task 4 depends on both.

- [ ] **Step 1: Record the register path before writing it**

The Renoir layout differs from earlier Zen parts and this is where a wrong assumption costs a day.

Read `AMDFamily17.p` in `github.com/namazso/PawnIO.Modules` and record, in a comment block in `amd.rs`: the exported function names, their input and output arguments, and how they identify a CPU family and model.

Cross-check against Linux `k10temp`, which supports family 17h model 60h through a PCI match and reads Tdie and Tctl. Two things to note explicitly:

- Renoir moved the SMU region to `0x6F000` and changed its layout, so offsets published for earlier Zen parts return zeros
- Tctl carries a per-SKU offset above Tdie on some parts. Report Tdie. If only Tctl is available, subtract the documented offset for that family and say so in the `basis` string

- [ ] **Step 2: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reading_with_no_per_core_data_still_reports_a_package_temperature() {
        let r = TemperatureReading { package_c: Some(58.4), per_core_c: vec![] };
        assert_eq!(r.package_c, Some(58.4));
        assert!(r.per_core_c.is_empty());
    }

    #[test]
    fn an_empty_reading_is_representable_rather_than_an_error() {
        // Temperature is optional throughout LoadBear. A part that reports
        // nothing must produce an empty reading, never a failure that
        // propagates into the diagnosis layer.
        let r = TemperatureReading { package_c: None, per_core_c: vec![] };
        assert!(r.package_c.is_none());
    }

    #[test]
    fn implausible_temperatures_are_rejected_rather_than_reported() {
        // A misaddressed register returns garbage that looks like a number.
        // The LB-02 spike caught exactly this class of error by plausibility
        // checking rather than by trusting the read.
        assert!(!is_plausible_celsius(0.0));
        assert!(!is_plausible_celsius(250.0));
        assert!(is_plausible_celsius(58.4));
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p loadbear-sensors-windows amd`
Expected: FAIL to compile, `TemperatureReading` and `is_plausible_celsius` are not defined.

- [ ] **Step 4: Implement**

```rust
/// Reject values that cannot be a running CPU temperature.
///
/// A misaddressed register returns a number, not an error. The LB-02 spike
/// caught a zeroed TjMax array this way. Silicon below 5 C or above 125 C
/// while the machine is running means the read is wrong, not that the CPU is
/// remarkable.
pub fn is_plausible_celsius(v: f32) -> bool {
    (5.0..=125.0).contains(&v)
}
```

Then `read_amd_temperature`, loading `AMDFamily17`, calling the function recorded in Step 1, filtering every value through `is_plausible_celsius`, and returning an empty reading rather than an error when nothing plausible comes back.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p loadbear-sensors-windows`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/loadbear-sensors-windows/
git commit -m "feat(sensors): add AMD Zen temperature reading

Register path cited to AMDFamily17.p and cross-checked against k10temp.
Every value passes a plausibility check, because a misaddressed register
returns a number rather than an error."
```

---

## Task 3: Intel temperature and TjMax

**Files:**
- Create: `crates/loadbear-sensors-windows/src/intel.rs`
- Modify: `crates/loadbear-sensors-windows/src/lib.rs`

**Interfaces:**
- Consumes: `PawnIo` from Task 1, `TemperatureReading` and `is_plausible_celsius` from Task 2
- Produces: `read_intel_temperature(&PawnIo) -> Result<TemperatureReading, PawnIoError>` and `read_intel_tjmax(&PawnIo) -> Result<Option<f32>, PawnIoError>`. Task 4 depends on both.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tjmax_is_extracted_from_bits_16_to_22_of_the_msr() {
        // MSR_TEMPERATURE_TARGET (0x1A2) carries TjMax in bits 16..=22.
        // Intel's own turbostat reads exactly these seven bits.
        let raw: u64 = 100 << 16;
        assert_eq!(tjmax_from_msr(raw), Some(100.0));
    }

    #[test]
    fn an_implausibly_low_tjmax_is_rejected() {
        // turbostat only accepts the value when it is at least 85 C, because
        // a lower reading means the field was not populated.
        let raw: u64 = 40 << 16;
        assert_eq!(tjmax_from_msr(raw), None);
    }

    #[test]
    fn a_zero_msr_yields_no_tjmax() {
        assert_eq!(tjmax_from_msr(0), None);
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p loadbear-sensors-windows intel`
Expected: FAIL to compile, `tjmax_from_msr` is not defined.

- [ ] **Step 3: Implement**

```rust
/// TjMax from `MSR_TEMPERATURE_TARGET` (0x1A2), bits 16 through 22.
///
/// Intel's turbostat reads these seven bits and rejects anything below 85 C,
/// on the basis that a lower value means the field was never populated. The
/// same floor is applied here.
///
/// This is why Intel needs no database entry for TjMax while AMD does. The
/// LB-02 spike confirmed AMD publishes nothing equivalent: TjMax read as zero
/// across all 128 slots on the Ryzen 7 4980U.
pub fn tjmax_from_msr(raw: u64) -> Option<f32> {
    let v = ((raw >> 16) & 0x7f) as f32;
    if v >= 85.0 {
        Some(v)
    } else {
        None
    }
}
```

Then `read_intel_temperature` and `read_intel_tjmax`, loading `IntelMSR` and using the interface recorded in Task 1 Step 1.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p loadbear-sensors-windows`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/loadbear-sensors-windows/
git commit -m "feat(sensors): add Intel temperature and TjMax

TjMax comes from MSR 0x1A2 bits 16..=22 with turbostat's 85 C floor, which
is why Intel needs no database entry for it while AMD does."
```

---

## Task 4: Vendor dispatch, detection and graceful degradation

**Files:**
- Modify: `crates/loadbear-sensors-windows/src/lib.rs`

**Interfaces:**
- Consumes: everything from Tasks 1 through 3, `CpuKey` and `Vendor` from `loadbear-core`
- Produces: `WindowsTemperature`, `WindowsTemperature::new() -> Self`, `WindowsTemperature::read(&mut self, key: &CpuKey) -> TemperatureReading`, and `WindowsTemperature::status(&self) -> TemperatureStatus`. The shell plan depends on all three.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use loadbear_core::{CpuKey, Vendor};

    fn amd_key() -> CpuKey {
        CpuKey { vendor: Vendor::Amd, family: 23, model: 96, stepping: 1 }
    }

    #[test]
    fn without_a_driver_it_reports_unavailable_and_returns_an_empty_reading() {
        let mut t = WindowsTemperature::new();
        let r = t.read(&amd_key());
        match t.status() {
            TemperatureStatus::Unavailable { .. } => {
                assert!(r.package_c.is_none());
                assert!(r.per_core_c.is_empty());
            }
            TemperatureStatus::Available => {
                // Driver present on this machine. The elevated verification
                // task covers this branch.
            }
        }
    }

    #[test]
    fn an_unavailable_status_carries_a_reason_a_user_could_act_on() {
        let s = TemperatureStatus::Unavailable {
            reason: "the PawnIO driver is not installed on this machine".to_string(),
            remedy: Remedy::InstallDriver { url: PAWNIO_URL },
        };
        let TemperatureStatus::Unavailable { reason, remedy } = s else {
            panic!("expected Unavailable");
        };
        assert!(!reason.is_empty());
        assert!(!reason.contains("0x"), "no raw error codes in user-facing text");
        assert_eq!(remedy, Remedy::InstallDriver { url: "https://pawnio.eu" });
    }

    #[test]
    fn a_missing_driver_offers_the_install_remedy_rather_than_only_an_apology() {
        // LoadBear ships no driver, so this is the state every user starts in.
        // It is an ordinary first run, not a failure, and it must come with
        // somewhere to go.
        let mut t = WindowsTemperature::new();
        let _ = t.read(&amd_key());
        if let TemperatureStatus::Unavailable { remedy, .. } = t.status() {
            assert!(
                matches!(remedy, Remedy::InstallDriver { .. }),
                "an absent driver must point the user at the download"
            );
        }
    }

    #[test]
    fn reading_never_panics_when_the_driver_is_missing() {
        let mut t = WindowsTemperature::new();
        for _ in 0..10 {
            let _ = t.read(&amd_key());
        }
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p loadbear-sensors-windows`
Expected: FAIL to compile, `WindowsTemperature` and `TemperatureStatus` are not defined.

- [ ] **Step 3: Implement**

```rust
/// Whether temperature is available, and if not, why and what to do about it.
///
/// LoadBear never ships the PawnIO driver, so `Unavailable` with
/// `Remedy::InstallDriver` is the state every user starts in. It is the
/// ordinary first-run path rather than an error, and the copy has to read that
/// way. `remedy` exists so the UI can offer an action instead of only an
/// apology, which is the same actionable bar the interruption contract applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemperatureStatus {
    Available,
    Unavailable { reason: String, remedy: Remedy },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Remedy {
    /// The driver is not installed. Point the user at the upstream download.
    InstallDriver { url: &'static str },
    /// The driver is present but refused us, or this CPU has no module.
    None,
}

/// Where a user goes to enable temperature monitoring.
pub const PAWNIO_URL: &str = "https://pawnio.eu";

/// Windows temperature source.
///
/// Opens the driver lazily and remembers failure, so a machine without PawnIO
/// does not attempt a device open on every sampling tick. Temperature is the
/// only privileged reading in LoadBear and everything else works without it,
/// so absence is an ordinary state rather than an error condition.
pub struct WindowsTemperature {
    device: Option<PawnIo>,
    status: TemperatureStatus,
}
```

`read` dispatches on `key.vendor` to Task 2 or Task 3, returns an empty `TemperatureReading` on any failure, and records the reason in `status`. It never returns `Result`, because callers must not be able to treat missing temperature as a failure.

- [ ] **Step 4: Run the tests, clippy and fmt**

```bash
cargo test -p loadbear-sensors-windows
cargo clippy -p loadbear-sensors-windows --all-targets -- -D warnings
cargo fmt --check
```

Expected: all pass, no warnings, no diff.

- [ ] **Step 5: Commit**

```bash
git add crates/loadbear-sensors-windows/
git commit -m "feat(sensors): add vendor dispatch and graceful degradation

read() returns a reading rather than a Result, so a caller cannot treat
absent temperature as a failure. Driver absence is remembered rather than
retried every tick."
```

---

## Task 5: Elevated verification

**Assignee: Bojan.** This is the only step that needs administrator rights, and it is the gate that turns everything above from plausible into proven.

**Files:**
- Modify: `spikes/windows-temp/NOTES.md`

- [ ] **Step 1: Install PawnIO**

Download the signed PawnIO build from pawnio.eu and install it from an elevated shell. LoadBear does not ship it, so this is the same thing a user would do after seeing the unavailable-state prompt, which makes this step a test of that flow as well.

Confirm the device exists:

```powershell
Get-PnpDevice -FriendlyName "*PawnIO*" | Format-Table -AutoSize
```

Expected: a device present and started.

- [ ] **Step 2: Confirm Defender did not block it**

```powershell
Get-MpThreatDetection | Select-Object -First 5 InitialDetectionTime, ThreatID, Resources
```

Expected: nothing relating to the install. If something was flagged, record it verbatim in NOTES.md. That outcome would be decisive and needs to be known before anyone else installs LoadBear.

- [ ] **Step 3: Read a real temperature unelevated**

Close the elevated shell. From a normal one:

```bash
cargo run -p loadbear-sensors-windows --example read-temp
```

Expected: a package temperature and ideally eight per-core values, all plausible.

**This is the whole point.** If it works from an unelevated shell, the install-time-only elevation model is proven and temperature monitoring is shippable.

- [ ] **Step 4: Cross-check against Core Temp**

Run `spikes/windows-temp/read-coretemp.ps1` with Core Temp running and compare. Values should agree within a degree or two. If LoadBear disagrees by more than that, the register path is wrong even though the numbers look reasonable.

- [ ] **Step 5: Record the result**

Update NOTES.md with a `## Elevated verification` section: what was installed, whether Defender objected, the values read, and the comparison against Core Temp. Then write the `## Recommendation` section that LB-02 has been waiting for.

---

## Self-Review

**Spec coverage.** Section 9 of the design (privilege model, temperature as the only privileged reading) is Tasks 1 and 4. Section 5's `ThermalHeadroomLow` verdict gets its temperature input from Task 4 and its TjMax from Task 3 on Intel or the database on AMD. Section 16's PawnIO distribution decision is settled by the "LoadBear does not ship PawnIO" section above: detect and prompt, never bundle, with Task 5 as the proof.

**Known unknowns, deliberately left as read-first steps rather than invented.** The PawnIO ioctl interface (Task 1 Step 1) and the `AMDFamily17` register path (Task 2 Step 1) are both specified as "read upstream and cite it" rather than written out, because writing ioctl codes from memory produces code that fails silently. Every other step contains its actual content.

**Type consistency.** `TemperatureReading` and `is_plausible_celsius` are defined in Task 2 and consumed unchanged in Tasks 3 and 4. `PawnIo` and `PawnIoError` from Task 1 are consumed in Tasks 2, 3 and 4. `CpuKey` and `Vendor` come from `loadbear-core` unchanged.

**Deviation from strict TDD.** Tasks 1 through 4 test the absence and error paths rather than successful reads, because no driver is present on the development machine until Task 5. This is deliberate and arguably better: those paths are what every user without the driver experiences, and since LoadBear never ships the driver, absence is the state every user starts in.
