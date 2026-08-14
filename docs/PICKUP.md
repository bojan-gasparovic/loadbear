# Picking LoadBear back up

Written 2026-08-14 at the end of the first working session. Read this before
`DESIGN.md`, because several things in the spec were overtaken by what the
machine actually did.

## Where it got to

It runs. `loadbear-app.exe` is a Tauri tray application showing tier, clock
against guaranteed base clock, stall bars, findings with their basis, and CPU
temperature including per-core. 79 tests, clippy and rustfmt clean.

```
cd context-library/hobby-projects/loadbear
cargo run -p loadbear-app
```

Four crates:

| Crate | Role |
|---|---|
| `loadbear-core` | Pure diagnosis. No OS calls. Verdicts, tiers, interruption contract |
| `loadbear-sensors-windows` | Everything platform specific. The only crate allowed near a driver |
| `loadbear-service` | Elevated helper. Runs as Local System, publishes to shared memory |
| `loadbear-app` | Unprivileged interface |

## The five things that cost the most time

Each of these was found by running the thing, not by reasoning about it. Several
contradicted an explanation I had given confidently minutes earlier.

1. **WinRing0 is dead.** Defender classifies it as a vulnerable driver and
   Windows 11 22H2 blocks it. The only workaround disables a Microsoft security
   control. Do not let anyone reintroduce it. PawnIO replaced it.

2. **PawnIO admits only SYSTEM and Administrators.** From its own INF:
   `D:P(A;;GA;;;SY)(A;;GA;;;BA)`. An unprivileged process cannot read
   temperature at all. This is why the helper service exists, and it is the
   same architecture Core Temp and HWiNFO use.

3. **Per-core temperature lives in the SMU PM table, not in SMN registers.**
   `k10temp` and LibreHardwareMonitor both read SMN, which yields a die
   temperature and one reading per CCD. Renoir is single-CCD, hence one value.
   Core Temp reads the PM table. Indices **215 to 222** at PM table version
   **`0x370005`** are per-core, established by dumping the table and matching
   the *shape* against Core Temp, not from any documentation.

4. **AMD does not publish TjMax.** Zero across all 128 slots on the 4980U. It
   has to come from the specification database. Intel exposes it via MSR
   `0x1A2`.

5. **The base clock verdict fires correctly on this machine.** The Surface
   sustains roughly 1400 MHz against a guaranteed 2000 under load. That is a
   true positive and matches the reviewed 20 W shared power budget.

## Bugs I created and how they presented, so they are recognisable

- **CPUID double-count.** `raw-cpuid` already folds extended family and model
  in. Adding them again gave family 31 model 192, the database lookup silently
  returned `None`, and every check needing published data quietly stopped.
- **Two different clocks.** Helper stamped readings with its process uptime,
  interface checked against its own. Everything looked stale forever. Both now
  use `GetTickCount64`.
- **Silent failed upgrade.** Copied the new helper over a running one, which
  Windows refuses, and treated the failure as tolerable. Setup reported success
  while leaving the old binary in place, so a shipped feature never ran.
- **Layout bump swallowed the case.** Bumping the shared layout version made
  the reader reject the running helper's records entirely, showing
  "unavailable" for a helper that was fine and merely needed updating.

The pattern is the same every time: something failed in a way that looked like
absence rather than error.

## Development gotchas

- **The helper holds its own binary.** It installs to `%ProgramFiles%\LoadBear`
  and runs as SYSTEM. Rebuilding is fine, but if it ever gets registered from
  `target/debug` again you will need an elevated `sc.exe stop LoadBearHelper`
  before you can build.
- **Updating the helper needs one elevated click.** The interface detects a
  stale helper by revision and offers "Update helper". Bump
  `shared::HELPER_REVISION` whenever the helper starts producing something new,
  or the interface will never notice.
- **Bump `LAYOUT_VERSION` when `SharedTemperature` changes**, and remember the
  reader must still be able to detect the mismatch. `version` is the first
  field and must stay there.

## What is not done

Roughly in the order I would pick it up.

1. **Attribution.** The whole thesis is "here is what is causing it" and none
   of it exists. Process level and Docker via its socket API. Without this it is
   a nicer Task Manager.
2. **Utilization is not measured at all.** Only the run queue, which is why an
   earlier diagnosis got "your machine is idle" badly wrong on a machine at 97%.
3. **Single-sample noise.** Queue length swings between 0 and 30 second to
   second. Everything should average over several samples.
4. **Notifications.** The interruption contract is fully built and tested in
   `loadbear-core` and nothing calls it.
5. **The sample store**, which v1 scope said to write from day one.
6. **Intel temperature.** Module loads, path is reachable, code unwritten.
7. **macOS and Linux backends.** Designed for, not started.

## Things to remove or revisit

- `loadbear-service --dump-pmtable` is diagnostic scaffolding. Useful for
  widening PM table support to other CPUs, but it should not ship.
- The stall normalisation constants in `counters::scale` are unsourced first
  guesses, marked as such. They move the tier but never a verdict.
- The five minute sustained window in the interruption contract is also a first
  guess, never tuned against real use.
- **The Beacon tickets are stale.** LB-10 to LB-14 describe a design that
  changed substantially once the PawnIO security descriptor was discovered.
  Reconcile them before treating them as a plan.

## Decisions that were made and should not be relitigated without reason

- Never invent a "normal" temperature range. Chassis and ambient account for
  around 20 degrees C of variance that has nothing to do with the CPU model,
  which is why no vendor publishes one.
- Every verdict traces to a vendor guarantee, a hardware bit, or the machine's
  own history. The `basis` field on `Verdict` exists to keep that honest.
- Notify only when sustained, diagnosable and actionable all hold. Notification
  fatigue is what kills tools in this category.
- Temperature is optional everywhere. `WindowsTemperature::read` returns a
  reading rather than a `Result` so a caller cannot treat absence as failure.
- LoadBear bundles the PawnIO installer and the LGPL-2.1 modules, but not the
  driver or `PawnIOLib.dll`, which arrive with the user's own install.
