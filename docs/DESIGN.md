# LoadBear design

Status: approved, pre-implementation.
Date: 2026-08-14

## 1. Problem

Developers overload their machines constantly and find out only when everything feels slow, at which point they guess at the cause and usually guess wrong. A typical machine is running an IDE and its indexer, several Docker containers, a dev server, a build, a browser with a large tab set, and increasingly a local model, all at once.

Every existing tool answers the question "what are my resources doing." None answer the question people actually have, which is "why is this slow, and what do I close."

## 2. Thesis

**Utilization percentages lie.**

- 100% CPU with a short run queue is a machine working well. Nothing is wrong.
- 40% CPU while paging to disk is a machine dying, and most tools present that as healthy.

The distinction between *working hard* and *overloaded* is the entire product. Everything else follows from it.

## 3. Non-goals and the honesty rule

LoadBear never tells a user what is "normal" for their hardware, because no such data exists and it is not a chassis-independent quantity.

Research finding that fixes this position: chassis and airflow account for roughly 8 to 10 degrees C of variance on identical silicon, and ambient room temperature for a further 5 to 10 degrees C. Around 20 degrees C of spread therefore has nothing to do with the CPU model. Any per-CPU "normal range" narrower than that is fabricated, and any range wide enough to be honest is too wide to be useful. This is why no vendor publishes one, and why the abundant blog content stating "60 to 85 C under load" is unsourced filler.

**Every judgement LoadBear makes must trace to a vendor guarantee, a hardware bit, or the user's own machine history.** Nothing else is permitted to drive a verdict.

Explicit non-goals:

- Benchmarking or scoring hardware
- Overclocking, fan control, or any form of hardware control. LoadBear observes and never actuates
- Replacing Task Manager, htop, or nvtop as a general-purpose resource browser

## 4. Core measurement: stall

The measured quantity is **resource stall**, meaning time that work spent waiting on a resource rather than progressing. This is not utilization and not temperature.

Linux exposes this directly as Pressure Stall Information (PSI, kernel 4.20+) at `/proc/pressure/{cpu,memory,io}`, with `some` and `full` variants averaged over 10, 60 and 300 seconds. PSI was built to answer exactly this question and is already consumed by Kubernetes and Netdata.

**PSI is Linux's implementation of the concept, not the specification.** Windows and macOS express the same phenomenon through their own native counters. No platform imitates another, and no platform's numbers are calibrated against another's. Each backend reports the concept and every layer above sees one normalised form.

| Platform | Memory stall | I/O stall | CPU contention |
|---|---|---|---|
| Linux | PSI memory | PSI io | PSI cpu |
| Windows | hard page faults/sec | disk latency per transfer | processor queue length |
| macOS | Mach page-in counters | disk latency | run queue |

Windows hard fault rate is a direct measurement of a thread stopping to wait for memory. It is the same phenomenon PSI describes, measured differently, not an approximation of it.

## 5. Absolute verdicts

Alongside stall, LoadBear evaluates four conditions whose thresholds are not ours to invent:

| Check | Basis | Needs spec data |
|---|---|---|
| Sustained all-core clock below guaranteed base clock | **Vendor guarantee.** Both Intel and AMD commit that the part sustains base clock at rated TDP. Below it under load is objectively out of spec, in any chassis, at any ambient | Yes |
| Throttle signal asserted, and which one | Hardware bit. Not inferred from temperature | No |
| Package power against rated TDP and configurable TDP band | Published per SKU. cTDP is a genuine vendor-published range | Yes |
| Headroom to junction temperature limit | TjMax. Intel exposes it via MSR `0x1A2`; AMD needs a small per-family table | Partially |

The base clock check is the strongest signal LoadBear has and should be treated as the flagship verdict.

## 6. Tier model

Three tiers. These are severity states, not colours, and the visual language is the mascot's posture rather than a colour code. Silhouette must carry the meaning unaided, since tray icons render at 16 to 32 pixels and macOS menu bar icons are template images rendered monochrome and system-tinted by convention.

| Tier | Meaning | Notifies |
|---|---|---|
| **Easy** | Within spec, headroom available | Never |
| **Braced** | Degraded, or evidence still accumulating | Never |
| **Strained** | Sustained and out of spec | Only if the interruption contract is met |

## 7. The interruption contract

Notification fatigue is the failure mode that kills tools in this category. A tool that pops during a routine build is muted the same day.

**LoadBear interrupts only when there is something the user could actually do about it.** All three conditions must hold:

1. **Sustained.** Spikes never notify. A build pegging every core for ninety seconds is a machine working correctly. Starting value: the condition must hold continuously for **5 minutes** before it can escalate to a notification. This number is a deliberate first guess to be tuned against real dogfooding, not a derived constant, and it should be configurable but not prominent.
2. **Diagnosable.** The cause is identified, not merely the symptom.
3. **Actionable.** There is something the user can do about it. Deliberately *not* "there is a process the user can end." Being unable to kill something is not the same as there being nothing to do, and the difference covers some of the highest-value findings LoadBear can make.

### Remediation classes

"Actionable" is only meaningful if it is testable. A finding qualifies when it maps to one of these, each of which carries a concrete action rather than an observation:

| Class | Example |
|---|---|
| **Stop** | A process or container the user started and can end |
| **Reconfigure a limit** | Docker Desktop memory allocation, WSL2 `.wslconfig` |
| **Add an exclusion** | Antivirus scanning `node_modules` or a Rust `target/` directory during builds. Extremely common, rarely noticed, and often dramatic when fixed |
| **Defer** | Search indexing or an OS update running during the workday |
| **Change power state** | Running on battery at a reduced power limit, which accounts for a large share of "why is my build slow today" |
| **Physical** | Baseline-driven, for example a rising idle temperature delta over months indicating dust or paste degradation |

**Guard against drift.** Widening "actionable" raises fatigue risk, which makes the other two conditions load-bearing rather than incidental. Every class above names a specific action. "Something is using memory" is not a remediation class and must never become one. If a finding cannot be stated as a sentence ending in a thing the user does, it does not notify.

"You are throttling" during a compile, with nothing to be done, is noise. "You have been below your guaranteed base clock for eleven minutes because Docker is holding 11 GB and you have started paging" is worth an interruption, because it ends in a decision.

Strained without a notification is a legitimate and common state: the machine is genuinely struggling, but the cause is the build the user deliberately started. The tray reflects it. LoadBear does not tap anyone on the shoulder to report that compiling is slow.

**Consequence:** attribution is a v1 requirement, not a later feature. The actionable condition cannot be evaluated without knowing the cause.

### Attribution

Two levels, both in v1.

**Process level** comes from OS-native process enumeration and per-process counters.

**Container level** requires a second data source, and this is not a refinement of process attribution. On Windows, Docker Desktop runs inside WSL2, so every container on the machine presents to Windows as a single `vmmem` process. Windows can report that Docker is holding 11 GB and can never report which container holds it. Resolving that means querying the Docker socket API directly and correlating.

It is worth the extra source because Docker is the most common single cause of overload for this audience, which is exactly where process-only attribution is weakest. The Docker API is HTTP over a pipe or socket, is well documented, and is identical on all three platforms, so this is one implementation rather than three.

**Correctness bar.** A confident wrong attribution is worse than none. Where the evidence supports naming a cause, name it. Where it does not, LoadBear reports the state and stays silent on the cause rather than guessing, and a finding with no attribution cannot satisfy the actionable condition and therefore cannot notify.

## 8. Architecture

Four layers. Only the first is platform-specific.

1. **Sensor backends.** One per OS behind a common interface. Raw readings only, no interpretation.
2. **Normalization.** Converts backend output into one platform-neutral form.
3. **Diagnosis and attribution.** Pure functions over normalized readings plus spec data. No I/O. Fully testable without hardware. All product logic lives here.
4. **Presentation.** Tray posture, notification gate, window.

This containment is the answer to the cost of cross-platform support. It triples layer one only. The layer holding all the product thinking never touches an OS API and is written once.

### Stack

**Rust core, Tauri v2 shell.**

The work is not evenly distributed across platforms, so the choice came down to which platform's sensor problem should be the easy one. Rust makes Linux nearly free (`/proc`, `/sys`) and macOS close to solved, since `macmon` and `macpow` already demonstrate sudoless Apple Silicon metrics through IOReport. It concentrates the difficulty on Windows, which requires a kernel driver under any stack, so the hard part stays hard regardless.

C# with Avalonia was the serious alternative, because LibreHardwareMonitorLib is .NET and is the strongest Windows sensor library available, making the first shipping platform the easy one. It was rejected because it front-loads convenience onto the platform Bojan can test and back-loads the two he cannot, and its footprint works against the constraint below.

Tauri v2 provides a unified tray API across all three desktops with an Ayatana fallback on Linux, and uses roughly half the resident memory of an equivalent Electron build. That is not cosmetic here: **a monitoring tool that is itself a resource hog is self-refuting.** Electron was rejected on that basis alone.

Known weak point: WebKitGTK is the Linux webview and is the least consistent of the three. Layer four is the only layer exposed to that.

## 9. Privilege model

**Everything except temperature works unprivileged on all three platforms.**

| Platform | Stall and attribution | Temperature |
|---|---|---|
| Linux | PSI, `/proc`, unprivileged | `/sys/class/hwmon`, unprivileged |
| macOS | Mach host statistics, unprivileged | IOReport, unprivileged (approach proven by `macmon` and `macpow`) |
| Windows | performance counters, unprivileged | **kernel driver, requires admin** |

Temperature is the only privileged reading anywhere. LoadBear must therefore degrade to genuinely useful, not broken, when the driver is unavailable.

**Verified on Windows, 2026-08-14.** Seven performance counters were read from an unelevated shell, including `\Processor Information(_Total)\Processor Frequency` and `\% Processor Performance`. Multiplying those gives the real sustained all-core frequency, which means **the `BelowBaseClock` verdict, the strongest thing LoadBear says, needs no driver at all.** So do the stall signal, available memory, process attribution and Docker attribution. Only package temperature, package power and the throttle reason bits require the driver.

**Elevation is an install-time cost, not a runtime one.** The driver is registered once as a service by the installer, and LoadBear then talks to the running device unprivileged. This was demonstrated during the LB-02 spike by reading eight per-core temperatures out of Core Temp's shared memory from an unelevated shell. A design requiring administrator rights on every run is rejected.

Docker attribution uses the Docker socket API and is identical across all three platforms. One implementation, not three.

## 10. Bundled CPU specification database

Ships embedded. Works offline. No network dependency at runtime.

**Fields:** TjMax, base clock, boost clock, TDP, configurable TDP range, cores and threads. Keyed by CPUID family, model and stepping.

**Sources:** TechPowerUp offers an official REST API and MCP server with a free tier for researchers, and is hand-curated against datasheets. Their site actively blocks scraping, so the API is the only sanctioned route. Community datasets exist as fallbacks (`felixsteinke/cpu-spec-dataset`, `barbalion/ark_csv`, `divinity76/intel-cpu-database`). Intel publishes no official API or bulk export for ARK, so every Intel community source is a scrape.

**Known coverage gap:** OEM-exclusive parts frequently have no public vendor page at all. The development machine's own CPU, an AMD Ryzen 7 4980U (Surface Laptop 4, Renoir, family 17h model 60h), is one such part. The tool must degrade gracefully when a SKU is absent, falling back to chip-read values.

**Explicitly not in the database:** normal or expected operating temperatures. See section 3.

## 11. Learned per-machine baseline

The absolute verdicts work on day one. The baseline makes LoadBear smarter over time and answers questions the absolute checks cannot.

Keyed by CPU **and chassis model**, both of which the OS already reports. Learns idle temperature delta over ambient, sustained clock under load, and time to first throttle.

This is the only defensible way to answer "is this worse than it used to be." A rising idle delta over months is the earliest available warning of dust accumulation or thermal paste degradation, long before any performance drop is noticeable.

The baseline is not a prerequisite for shipping. It cannot decide whether to notify on day one, because it takes days to become useful.

## 12. Local sample store

v1 writes samples to local storage that nothing reads yet.

Continuous sampling already happens, so collection costs nothing. The only real cost of the later history and baseline features is UI. Collecting from day one means that when those features ship, every existing install already has weeks of data rather than starting empty.

Same principle applies to any future aggregated comparison database: collect early, surface later, never rework. **Data terms must be settled before a single reading leaves a user's machine.** Retroactively changing data terms is what actually damages communities, far more than relicensing does.

## 13. v1 scope

**In:**

- Windows sensor backend
- Normalization layer
- Diagnosis engine, including the four absolute verdicts
- Attribution to process, and to container via the Docker socket API
- Tray with three postures
- Notification gate implementing the interruption contract
- Bundled CPU specification database
- Local sample store, written but not read

**Out of v1, designed for:**

- macOS and Linux backends
- History UI
- Learned baseline
- Any hosted service or aggregated data

## 14. Distribution

- Repository: `github.com/bojan-gasparovic/loadbear`, private during build, public on Bojan's own action
- Licence: Apache-2.0 from the first commit, so the flip to public needs no history rewrite
- The app is free and open permanently. Any future revenue lives in a hosted service that does not exist yet: history beyond the local window, fleet view, aggregated comparison. The app is not the sellable part, so opening it costs nothing
- Code signing: SignPath Foundation signs qualifying open source projects free, though it stamps its own publisher identity into the SmartScreen dialog. Azure Artifact Signing is roughly $10/month if LoadBear's own name in that dialog is worth paying for. Neither is needed until an installer reaches someone else's machine
- Brand is **LoadBear**. Repository, binary, and any package identifier are `loadbear`

## 15. Risks

| Risk | Notes |
|---|---|
| **Notification fatigue** | The category killer. The interruption contract exists solely to address it and must not be relaxed under pressure to seem more useful |
| **Windows driver** | Antivirus false positives are documented and expected. Licence compatibility of whatever driver is bundled must be verified, since it is the one component not authored here |
| **Sensor enumeration on semi-custom parts** | Unverified whether available libraries enumerate per-core temperatures on the development machine's OEM-exclusive Ryzen. Everything downstream of the Windows temperature path depends on it |
| **No Apple hardware** | Bojan owns no Mac and cannot build or test the macOS backend. Going public is the only realistic route to that platform |
| **Attribution correctness** | A confident wrong attribution is worse than no attribution. Naming the wrong process destroys trust faster than staying quiet |

## 16. Open decisions

1. **Data terms** for any future aggregated database, required before collection begins. Deferred until collection is actually built.

### Resolved

- **Implementation stack.** Rust core with a Tauri v2 shell. See section 8.
- **Attribution depth.** Process and container, both in v1. See section 7.
- **Actionable definition.** Widened from "can be closed" to "the user can do something about it", constrained by the remediation classes in section 7.
- **Windows temperature path.** PawnIO, not WinRing0. WinRing0 is eliminated on evidence rather than preference: Microsoft Defender has classified it as `VulnerableDriver:WinNT/Winring0` since March 2025 with several catalogued variants, CVE-2020-14979 lets an unprivileged process read and write arbitrary memory which made it a Bring Your Own Vulnerable Driver target, and the Windows 11 22H2 blocklist blocks it outright. The only workaround is a registry change disabling that blocklist, and **LoadBear will never instruct a user to weaken a security control.** PawnIO is signed, HVCI-compatible, and runs sandboxed bytecode modules exposing narrow ioctls rather than raw ring-0 primitives. LibreHardwareMonitor migrated to it in 0.9.5, FanControl in v238, OpenRGB alongside. Modules exist for the targets that matter: `AMDFamily17.p` covers Zen family 17h including Renoir, `IntelMSR.p` covers Intel. See `docs/plans/2026-08-14-loadbear-windows-temperature.md`.
- **LoadBear does not ship PawnIO.** Detect and prompt, never bundle. The driver is GPL-2.0 and the modules LGPL-2.1, and redistribution is the entire source of the resulting legal dependency. Since redistribution is optional, not bundling removes the GPL obligations, the need for bundling permission that PawnIO's public documentation does not grant, and any licence mixing in the tree. What remains is covered by an explicit exception in PawnIO's own licence for independent programs communicating solely over the device ioctl interface, which is the ordinary relationship any application has with a driver the user installed. The cost is a worse first run and it is accepted.
- **Mascot artwork.** Crude placeholder silhouettes throughout v1, commissioned before public release. Three postures legible at 16 pixels as silhouette alone is a demanding brief, and it cannot be written honestly until the tier transitions have been watched firing on a real machine over a period of normal work.
