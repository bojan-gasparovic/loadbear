# LoadBear

A resident cross-platform monitor that tells developers when they are overloading their machine, and what is causing it.

> Status: early. Design in progress, nothing to install yet.

## Why

Developers run Docker containers, dev servers, builds, IDE indexers, browsers and increasingly local models, all at once. Machines get overloaded constantly, and the first sign is usually that everything feels slow. By then you are guessing at the cause.

Existing tools show you numbers. Task Manager, HWiNFO, btop and nvtop will all happily tell you the CPU is at 100%, which is not the same as telling you something is wrong.

The distinction LoadBear is built around:

- 100% CPU with a short run queue is a machine working well
- 40% CPU while paging to disk is a machine dying, and most tools make that look fine

## Approach

The core measurement is resource **stall**, meaning time that work spent waiting on a resource rather than progressing. Linux exposes this directly as Pressure Stall Information. Windows and macOS express the same phenomenon through their own native counters. Each platform backend reports the concept, and every layer above sees one normalised form.

Judgements are traceable to a vendor guarantee or a hardware bit, never to an invented threshold:

- Sustained clock against the guaranteed base clock, which is a contractual commitment at rated TDP
- Whether the hardware is asserting a throttle signal, and which one
- Package power against the rated and configurable TDP band
- Headroom to the junction temperature limit

LoadBear does not tell you what is "normal" for your CPU, because nobody publishes that and it is not a chassis-independent quantity. It tells you what is out of spec, and it learns your own machine's baseline over time so it can tell you what has changed.

## Platforms

Designed for three, shipping in order:

| Platform | Status |
|---|---|
| Windows | first target |
| macOS | designed, not started |
| Linux | designed, not started |

## Licence

Apache-2.0. See [LICENSE](LICENSE).
