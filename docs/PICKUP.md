# Picking LoadBear back up

Written 2026-08-14, rewritten 2026-08-16 at the end of the third working
session, corrected the same day at the end of the fourth. Read this before
`DESIGN.md`, because several things in that spec were overtaken by what the
machine actually did, and one whole section of it describes a feature that has
since been cancelled.

## Start here

**LoadBear is now installable, and that is no longer the blocker.**
`build-installer.ps1` builds the helper, stages it as a Tauri sidecar under the
target triple, and bundles an NSIS installer to
`target\release\bundle\nsis\LoadBear_<version>_x64-setup.exe`. The
`externalBin` entry is what makes `service_control::helper_path()` find
`loadbear-service.exe` beside the interface, so an installed copy reads
temperature and power rather than reporting them permanently dead.

**The blocker moved one step downstream. Nobody can get the installer.** The
README tells people to download it from GitHub Releases. There are no releases
and no tags, and the file lands under `target/`, which is gitignored, so it is
in no repository either. The plan is to host it on zeroemdashes.com. That is
LB-23, and it is Bojan's to do, since it is a publishing step.

Everything else on the list is a quality gap. This one is the difference
between a repository and a product.

## Where it got to

It runs and it is used daily. **206 tests**, clippy clean.

```
cd context-library/desktop-apps/loadbear
cargo build --release
target\release\loadbear-app.exe
```

**Neither repository is pushed.** As of the end of the fourth session this one
is four commits ahead of `origin/main` and zeroemdashes is three. Check the
remotes rather than local refs before repeating either number, which is the
mistake that cost most of an earlier session.

| Crate | Role |
|---|---|
| `loadbear-core` | Pure diagnosis. No OS calls. Verdicts, tiers, attribution |
| `loadbear-sensors-windows` | Everything platform specific. The only crate allowed near a driver |
| `loadbear-service` | Elevated helper. Runs as Local System, publishes to shared memory |
| `loadbear-app` | Unprivileged interface |

## The bug pattern that has now cost three separate faults

**A value computed correctly in one crate, then dropped by hand-written field
copying on its way across a boundary. It never looks like a bug. It looks like
a sensor that does not work.**

1. **`package_watts` was never published.** `mapping::publish` copied the
   shared record field by field and omitted it, so the mapping kept the `NaN`
   it was initialised with at creation. The helper read power correctly for a
   full day while the interface showed nothing. The tell was that the raw bits
   were byte-identical on every sample, which is what an unwritten field looks
   like and not what a failing sensor looks like. `--probe-power` read 17 W
   from the very binary that was publishing nothing, because the probe
   exercises the sensor and never exercises the writer.
2. **`reported_base_mhz: None` was hardcoded** in `loadbear-app/src/main.rs`.
   The core engine could judge the clock on any processor; the application
   never passed it the number, so the feature could not fire at all in the
   shipped product.
3. **Cores, threads and base clock came from the specification database**,
   which holds three processors, so every other machine showed zeros and the
   power row rendered "of 0 W rated".

The fix for the first was structural and worth keeping: `mapping::payload` now
takes everything but `version`, `helper_revision` and `sequence` wholesale via
`..*reading`, so omitting a field is unrepresentable rather than merely fixed
once. **When adding a field to a struct that crosses a process boundary, check
whether anything copies it by hand.**

## The five things that cost the most time originally

Each was found by running the thing, not by reasoning about it.

1. **WinRing0 is dead.** Defender classifies it as a vulnerable driver and
   Windows 11 22H2 blocks it. The only workaround disables a Microsoft security
   control. Do not let anyone reintroduce it. PawnIO replaced it.
2. **PawnIO admits only SYSTEM and Administrators.** From its own INF:
   `D:P(A;;GA;;;SY)(A;;GA;;;BA)`. An unprivileged process cannot read
   temperature at all. This is why the helper service exists, and it is the
   same architecture Core Temp and HWiNFO use.
3. **Per-core temperature lives in the SMU PM table, not in SMN registers.**
   Indices **215 to 222** at PM table version **`0x370005`**, established by
   dumping the table and matching the shape against Core Temp.
4. **AMD does not publish TjMax.** Zero across all 128 slots on the 4980U, so
   it comes from the specification database. Intel exposes it via MSR `0x1A2`
   and LoadBear now reads it from there.
5. **The base clock verdict fires correctly on this machine.** The Surface
   sustains well under its guaranteed 2000 MHz on a long load. True positive.

## The charger finding, which is the best story in the project

Measured 2026-08-15. On a USB-C charger the package sat pinned at a flat
**9.309 W against a 15 W rating** and the processor held **exactly 70.0%
performance, 1400 MHz**, even at 47% utilization and only 57 to 58 C. On the
proper charger the same machine idled at 3200 to 3640 MHz and decayed under a
five minute all-core load like this:

| Time | Clock |
|---|---|
| t+10s | 3377 MHz |
| t+120s | 2337 MHz |
| **t+200s** | **1999 MHz, crossing the 2000 base** |
| t+300s | 1799 MHz, still falling |

The defensible claims are the flat 9.309 W ceiling and the light-load
difference, 1400 against 3400 MHz. **Not** "40% faster builds", which was an
early overreach from a 40 second burst presented as a sustained measurement.
This is what produced the `PowerBelowRating` verdict.

## Done in the 2026-08-16 session

- **Package power published** (LB-21 half). The `publish` fault above.
- **Intel temperature and TjMax** (LB-12). Written, wired, unit tested, and
  **never run on Intel silicon**, because this is an AMD machine. Tracked as
  LB-22. Per-core needs the thread pinned, since the MSR is per logical
  processor; `topology::on_processor` pins and then confirms with
  `GetCurrentProcessorNumber` that it landed, declining rather than guessing.
  **The failure to look for is per-core values that are all identical**, which
  would look entirely reasonable on screen.
- **Cores, threads and base clock now come from the OS**, not the database.
  New `topology` module walking `GetLogicalProcessorInformationEx`. The
  database is now an enhancement carrying only the rated power band and TjMax.
- **Icons.** The taskbar button was showing the bundled application icon, a
  byte-identical copy of the easy bear, so it always read "easy" whatever the
  machine was doing. It now follows the tier. All three tray icons were flat
  white on transparency and are now painted by tier at runtime. The application
  icon is the strained bear in red, and `icon.ico` went from a single 16 pixel
  entry to six.
- **Closing the window hides it** rather than ending the process.
- **README rewritten** to describe what LoadBear actually does, with a Known
  gaps section.
- **Every LB ticket reconciled** with reality. The board had fourteen tickets
  sitting on `Now` that had been finished for days.

## Done after this document was last rewritten

The first two landed in the same session, after the rewrite above was written,
which is why the old Start here section described them as missing.

- **A collapsed window shape**, 900x124, and the page draws its own 28px title
  bar so there is a chevron to reach it. `EXPANDED_SIZE` and `COLLAPSED_SIZE`
  live at `loadbear-app/src/main.rs:296`. The mode is not remembered across
  restarts and the window always opens expanded, deliberately.
- **The installer**, plus an uninstaller that stops and removes the service
  rather than leaving it registered. The uninstall path stops the service with
  `sc.exe` and not with the old binary, because the binary it would have called
  is one of the files being deleted.
- **LB-19: `--dump-pmtable` is behind the `diagnostics` feature**, which is off
  by default, so no shipped helper contains it. `pmtable.rs` is kept rather than
  deleted, since mapping a PM table version other than `0x370005` needs exactly
  this tool. Verified by building both ways and checking the binary: the module's
  own output strings appear once with the feature and zero times without it.

  ```
  cargo run -p loadbear-service --features diagnostics -- --dump-pmtable
  ```

  **An already installed helper still has the old mode** until
  `loadbear-service.exe --setup` is re-run elevated, which replaces the copy
  under `%ProgramFiles%\LoadBear`.
- **LB-24: a third window shape**, 360x48, dragged on top of the taskbar and
  left there. `TASKBAR_SIZE` beside the other two, `set_collapsed` replaced by
  `set_mode` over three named strings, and `size_for_mode` returning an
  `Option` so a name nothing knows is refused rather than resized to something
  plausible. Plan and measurements in
  `docs/plans/2026-08-16-loadbear-taskbar-mode.md`.

  **No native code and no overlay.** The window is already `alwaysOnTop`, and
  `data-tauri-drag-region="deep"` on `#compact` makes the whole strip drag.
  That value was read out of `tauri-2.11.5/src/window/scripts/drag.js` rather
  than remembered: `isDragRegion` walks the composed path upward, a bare
  attribute matches only a direct click, `"deep"` matches the subtree, and the
  walk stops at any clickable element with no attribute of its own, which is
  what leaves the restore chevron clickable without asking for it.

  **The title bar carries one control per shape, not one that cycles.** The
  cycling version was built first and rejected on sight: a button whose meaning
  depends on the shape you are already in has to be clicked to find out what it
  does. `#tb-collapse` is a plain expanded and collapsed toggle again,
  `#tb-strip` goes to the taskbar shape, and `#tb-restore` inside the strip is
  the only way back out of it, since the strip hides the title bar.

  **Bojan has not judged the strip itself yet.** Whether a 12px number in a coloured 19px
  tile reads at a glance, and whether clicking the taskbar raises it above the
  strip, are the two things that decide whether the shape works at all.

## Decisions that should not be relitigated without reason

- **Notifications are cancelled.** Bojan's call, 2026-08-16. LoadBear is a
  monitor you look at. `NotificationGate` was removed from `loadbear-core` the
  same day; `Cause`, `CauseKind`, `Finding` and `Remediation` survive because
  attribution uses them. It is one `git revert` away if this ever changes.
  **`docs/DESIGN.md` still documents the interruption contract at length and
  has not been updated.**
- Never invent a "normal" temperature range. Chassis and ambient account for
  around 20 C of variance that has nothing to do with the CPU model.
- Every verdict traces to a vendor guarantee or a hardware bit. The `basis`
  field exists to keep that honest, and nothing is derived from a forum post.
- Temperature is optional everywhere. `WindowsTemperature::read` returns a
  reading rather than a `Result`, so absence cannot be treated as failure.
- LoadBear ships the LGPL-2.1 PawnIO modules but not the driver or
  `PawnIOLib.dll`, which arrive with the user's own install.
- Unprivileged process coverage is **0.39 at idle and 0.92 under load**. The
  idle gap is kernel and interrupt time belonging to no process. Attribution
  only runs under load, where coverage is good. Do not raise privileges.

## Development gotchas

- **The helper holds its own binary.** It installs to `%ProgramFiles%\LoadBear`
  and runs as SYSTEM. Re-register with an elevated
  `target\release\loadbear-service.exe --setup`, which stops, deletes, copies
  and restarts in that order.
- **Bump `HELPER_REVISION`** whenever the helper starts producing something new,
  or an installed older helper will silently never produce it and nothing
  anywhere will say why. Currently **5**.
- **Bump `LAYOUT_VERSION`** when `SharedTemperature` changes. Currently **4**.
  `version` is the first field and must stay there so a reader can detect a
  mismatch it cannot otherwise interpret.
- **Rebuilding the app while it runs fails**, since Windows holds the exe open.
  Stop it first.
- Changing `icon.ico` needs `cargo clean -p loadbear-app` to re-run the build
  script that embeds it.
- **The debug `target/` held absolute paths from before the submodule moved**
  out of `hobby-projects/`. `cargo test --workspace` failed with
  `failed to read plugin permissions` naming a `hobby-projects\loadbear\target`
  path that no longer exists. It is a stale cache and not a code fault.
  `cargo clean -p tauri -p tauri-build -p loadbear-app` clears it, at the cost
  of recompiling Tauri. Fixed 2026-08-16, so it should not recur, but the error
  message points at a missing file rather than at the real cause.

## What is not done

Roughly in the order I would pick it up.

1. **Nothing is installable.** See Start here. This is the only item that
   blocks other people entirely.
2. **LB-22: verify Intel temperature on Intel hardware.** Needs a machine
   nobody here has.
3. **LB-21 second half: the throttle verdict never fires.** The check is
   written and tested. No register source has been found that meets the
   provenance rule.
4. **LB-20: disk stall is scaled at 50 ms per transfer**, a mechanical disk
   figure. An NVMe under a load built to saturate it peaked at 3.4 ms, so that
   bar cannot leave single digits.
5. **`docs/DESIGN.md`** still describes the cancelled notification gate.
6. **Docker CPU always reads zero.** `one-shot=true` ships no `precpu_stats`
   baseline. Memory is exact.
7. **Per-process hard faults.** Written but always returns nothing; Windows
   exposes no documented per-process hard fault rate. Needs ETW or stays
   silent.
8. **The bear is a placeholder**, and the silhouettes have no outline, so they
   are at the mercy of whatever they sit on. The tier transitions have now been
   watched firing on real hardware over normal work, which the design said was
   the precondition for briefing an illustrator honestly.
9. **The repository is still private.** The application footer links to it and
   will 404 for anyone else until that changes.
10. **macOS and Linux backends.** Designed for, not started.

## Smaller things noticed and left

- `Status.ctdp_min_watts` in `loadbear-app` is serialised and rendered by
  nothing.
- The three CPU database entries are all marked `UNVERIFIED` and still want
  checking against vendor product pages. This matters much less than it did,
  since only the power band and TjMax now come from there.
- The stall normalisation constants in `counters` are unsourced first guesses,
  marked as such. They move the tier but never a verdict.
