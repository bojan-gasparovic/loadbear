# LB-02 spike: the Windows temperature path

Throwaway investigation. Nothing in this folder is part of the LoadBear build.

Date started: 2026-08-14
Machine: Surface Laptop 4, AMD Ryzen 7 Microsoft Surface (R) Edition (Ryzen 7 4980U, family 17h model 60h stepping 1, Renoir, 8C/16T)

## Status

Partially complete. The unprivileged half is done. The driver-loading half is blocked and needs Bojan, see [Blocked](#blocked) below.

## Ground truth from Core Temp

Captured programmatically from Core Temp's shared memory rather than from the GUI, using `read-coretemp.ps1` in this folder. Core Temp was running throughout.

Shared memory object `CoreTempMappingObjectEx` opened **without elevation**.

```
CPU name      : AMD Ryzen 7 Microsoft Surface (R) Edition
Core count    : 8
CPU count     : 1
TjMax         : 0
VID           : 0.6505
CPU speed MHz : 1397.3
Fahrenheit    : 0  (values are Celsius)
DeltaToTjMax  : 0  (values are absolute, not deltas)

Core 0: 58.5 C    Core 4: 58.5 C
Core 1: 58.2 C    Core 5: 58.3 C
Core 2: 58.2 C    Core 6: 58.4 C
Core 3: 57.7 C    Core 7: 58.2 C
```

## Finding 1: per-core temperature does work on this CPU

Eight distinct per-core temperatures, all plausible. This answers spike question 2 in the affirmative for the hardware itself: this OEM-exclusive Renoir part does expose per-core temperature, and a driver can read it.

It does **not** yet prove that our chosen driver path can read it. Core Temp uses its own driver. That distinction is what the blocked half of this spike exists to settle.

## Finding 2: TjMax is not available on this part

`uiTjMax` is zero across all 128 slots. This is not an offset error. The asserted struct layout is confirmed correct by three independent checks:

- `sCPUName` at offset 2584 reads as a clean string
- `uiCoreCnt` at 1536 reads 8, matching the hardware
- `uiLoad` at offset 0, immediately **before** the TjMax array, reads 100 on all eight cores under load

Every field positioned after `uiTjMax` reads correctly, so the array is genuinely populated with zeros rather than being misaddressed.

**Consequence for the design.** This confirms the decision recorded in `docs/DESIGN.md` section 5, that TjMax is only partially chip-readable. Intel exposes it via MSR `0x1A2`. AMD does not, and on this part Core Temp does not surface it either. AMD TjMax must therefore come from the bundled specification database, keyed by CPU family, which is what `LB-04` builds.

It also means the `ThermalHeadroomLow` verdict is unavailable on any AMD part missing from the database, since it needs both a temperature and a limit. `evaluate()` already handles this: the check is skipped rather than guessed.

## Finding 3: no WinRing0 driver on this system

Neither `WinRing0x64.sys` nor `WinRing0.sys` is present in `System32\drivers`. Core Temp loads its own driver rather than sharing one. So the direct-access path would mean shipping a driver, not reusing something already installed.

## Blocked

The remaining work requires Administrator and the session shell runs unelevated as `SURFACE\bojan`. Bojan needs to run these.

| Question | What is needed |
|---|---|
| 1. Can Rust drive WinRing0 directly via `DeviceIoControl`? | Elevated shell. Ship a WinRing0 build, open the device handle, attempt an SMU read |
| 2b. Does the chosen path enumerate per-core temps? | Elevated. The hardware does expose them, per Finding 1. This is now only about the driver path |
| 3. Driver licence and Apache-2.0 compatibility | Answerable unelevated, but only meaningful once question 1 picks a path |
| 4. Defender or SmartScreen behaviour on driver load | Elevated, and needs a real load to observe |

The C# sidecar comparison (LibreHardwareMonitorLib, run elevated with Core Temp closed) is also outstanding for the same reason.

## Recommendation

Not yet reached. The unprivileged findings narrow the question but do not answer it.

What is already settled and does not depend on the blocked work:

- Per-core temperature is obtainable on this hardware, so a negative result from the driver tests would be a limitation of the path chosen, not of the machine
- AMD TjMax must come from the specification database regardless of which path wins
- LoadBear must ship a driver rather than reuse one, so the licence question is real rather than theoretical
- Temperature stays the only privileged reading, and everything else in LoadBear works unelevated, so a bad outcome here narrows the product without breaking it
