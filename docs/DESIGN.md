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
3. **Actionable.** There is something to close, kill, unplug or reconfigure.

"You are throttling" during a compile, with nothing to be done, is noise. "You have been below your guaranteed base clock for eleven minutes because Docker is holding 11 GB and you have started paging" is worth an interruption, because it ends in a decision.

Strained without a notification is a legitimate and common state: the machine is genuinely struggling, but the cause is the build the user deliberately started. The tray reflects it. LoadBear does not tap anyone on the shoulder to report that compiling is slow.

**Consequence:** attribution is a v1 requirement, not a later feature. The actionable condition cannot be evaluated without knowing the cause.

## 8. Architecture

Four layers. Only the first is platform-specific.

1. **Sensor backends.** One per OS behind a common interface. Raw readings only, no interpretation.
2. **Normalization.** Converts backend output into one platform-neutral form.
3. **Diagnosis and attribution.** Pure functions over normalized readings plus spec data. No I/O. Fully testable without hardware. All product logic lives here.
4. **Presentation.** Tray posture, notification gate, window.

This containment is the answer to the cost of cross-platform support. It triples layer one only. The layer holding all the product thinking never touches an OS API and is written once.

## 9. Privilege model

**Everything except temperature works unprivileged on all three platforms.**

| Platform | Stall and attribution | Temperature |
|---|---|---|
| Linux | PSI, `/proc`, unprivileged | `/sys/class/hwmon`, unprivileged |
| macOS | Mach host statistics, unprivileged | IOReport, unprivileged (approach proven by `macmon` and `macpow`) |
| Windows | performance counters, unprivileged | **kernel driver, requires admin** |

Temperature is the only privileged reading anywhere. LoadBear must therefore degrade to genuinely useful, not broken, when the driver is unavailable. This matters because the Windows driver path has a known history of antivirus false positives.

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
- Attribution to process and container
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

1. **Implementation stack.** Deliberately not decided. Established constraints: cross-platform, resident with low resident memory, capable of low-level sensor access and of loading or talking to a Windows kernel driver. Those constrain the choice without determining it. A monitoring tool that is itself a resource hog is self-refuting, which rules out the heaviest options.
2. **Windows temperature path.** Whether to bundle a driver and drive its interface directly, or run a thin sidecar in another language. To be settled by spike, not by argument.
3. **Mascot artwork.** Whether the bear is commissioned properly or a placeholder during the build.
4. **Attribution depth.** Process level is required. Whether v1 also attributes to container, and how it handles the case where the cause is a system service rather than something the user can close.
5. **Data terms** for any future aggregated database, required before collection begins.
