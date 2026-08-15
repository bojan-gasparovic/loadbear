# LoadBear

A resident monitor that shows you what is overloading your machine, not merely that it is busy.

> Status: working on Windows and used daily on the machine it was written on. There is no installer yet, so running it means building it. See [Building it](#building-it).

## Why

Developers run Docker containers, dev servers, builds, IDE indexers, browsers and increasingly local models, all at once. Machines get overloaded constantly, and the first sign is usually that everything feels slow. By then you are guessing at the cause.

Existing tools show you numbers. Task Manager, HWiNFO, btop and nvtop will all happily tell you the CPU is at 100%, which is not the same as telling you something is wrong.

The distinction LoadBear is built around:

- 100% CPU with a short run queue is a machine working well
- 40% CPU while paging to disk is a machine dying, and most tools make that look fine

## What it does

- **Measures stall rather than utilization.** Stall is time that work spent waiting on a resource instead of progressing, reported for the processor, memory and disk.
- **Names what is responsible.** Process groups carry the name a person would use rather than the executable, and Docker containers are resolved through the engine API. This is the part that separates it from Task Manager: "you are overloaded" is something you already knew by the time you looked.
- **Judges against published limits.** Every finding carries the authority its threshold rests on, so you can check it.
- **Waits before it panics.** A condition has to hold before the state escalates, so a two second spike does not turn the bear red.
- **Reads the hardware where it can.** Per-core temperature, package power, and the sustained all-core clock against the guaranteed base clock.

It is a window you look at. It does not send notifications and is not going to.

## How it judges

Judgements trace to a vendor guarantee or a hardware bit, never to an invented threshold:

| Check | Status |
|---|---|
| Sustained clock against the guaranteed base clock | working |
| Package power against the rated and configurable TDP band | working |
| Package power far below the rating, which means something is starving it | working |
| Headroom to the junction temperature limit | working |
| Whether the hardware is asserting a throttle signal | not wired, see [Known gaps](#known-gaps) |

LoadBear does not tell you what is "normal" for your CPU, because nobody publishes that and it is not a chassis-independent quantity. It tells you what is out of spec.

Cores, threads and the base clock come from the operating system, so they are right on any processor. A small embedded database adds the two things a machine cannot report about itself, the rated power band and the junction limit, and its absence costs those two judgements rather than the whole reading.

## Building it

Requirements: Rust 1.75 or later, Windows 10 or 11 on x86-64.

```
cargo build --release
```

Then run `target\release\loadbear-app.exe`.

### Temperature and package power

Both need ring-0 access, which LoadBear does not ship a driver for. Two extra steps enable them:

1. Install [PawnIO](https://pawnio.eu). It is signed, HVCI compatible, and runs sandboxed bytecode modules rather than exposing raw ring-0 primitives. LoadBear deliberately does not bundle it, which is what keeps the licensing clean. WinRing0, the traditional answer, is on the Windows vulnerable driver blocklist and is not an option.
2. Register the helper service once, from an elevated prompt:

```
target\release\loadbear-service.exe --setup
```

Elevation is paid once, at install. The helper runs as Local System, reads the sensors, and publishes them into shared memory that the interface reads without any privileges of its own. Without these steps everything else still works and temperature and power report as unavailable, with a link to fix it.

## Known gaps

Stated plainly, because a monitor that overstates what it knows is worse than no monitor:

- **Windows only.** The core diagnosis crate makes no operating system call and is written for three platforms, but only the Windows backend exists.
- **The throttle verdict never fires.** The check is written and tested. No register source has been found that meets the rule the sensor crate holds itself to, which is that nothing is derived from a forum post.
- **Disk stall reads low on an SSD.** It is scaled against 50 ms per transfer, which is a mechanical disk figure. Measured against a load built to saturate it, an NVMe peaked at 3.4 ms, so that bar cannot leave single digits. The measured latency is shown regardless, and is the number worth reading.
- **Intel temperature is unverified.** It is written, wired and unit tested, and no value in it has yet been read off an Intel part, because it was written on an AMD machine. It fails closed rather than guessing.
- **A container's CPU reads zero.** Its memory is exact. The engine's one-shot statistics ship no baseline to difference against.
- **The bear is a placeholder.** It is a crude silhouette awaiting an illustrator.

## Platforms

Designed for three, shipping in order:

| Platform | Status |
|---|---|
| Windows | working |
| macOS | designed, not started |
| Linux | designed, not started |

## Licence

Apache-2.0. See [LICENSE](LICENSE).
