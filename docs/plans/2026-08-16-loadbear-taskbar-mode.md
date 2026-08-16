# LoadBear Taskbar Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a third window shape, 360x48, that a person drags on top of the Windows taskbar and leaves there. It shows the bear, the five minute graph, and all eight core temperatures in a 4x2 grid, with no title bar and nothing else.

**Status:** Designed 2026-08-16, built the same day. Both tasks are done and the
application has been rebuilt and launched. What is left is the part no test can
answer: Bojan looking at the strip on his own taskbar, listed at the end of Task 2.

**Why this works without native code:** The window already carries `alwaysOnTop: true` in `tauri.conf.json`, so a strip positioned over the taskbar draws on top of it. Nothing is embedded into the taskbar and no Win32 call is involved.

**Spec:** `docs/DESIGN.md` for the product. This document is the whole specification for the feature.

---

## Why not embed into the taskbar properly

The obvious ask is a strip that lives *inside* the taskbar beside the clock, moving and resizing with it. That is not available.

- **Deskbands are gone.** The `IDeskBand` COM interface that let third parties put toolbars in the taskbar was deprecated in Windows 10 and the Windows 11 taskbar does not host them at all. Tools built on it, such as XMeters, stopped working at Windows 11.
- **The remaining trick is reparenting.** `SetParent` the window into `Shell_TrayWnd`, which is what TrafficMonitor does. It works, and it costs: Windows attaches the two threads' input queues on a cross process reparent, so a stall in LoadBear can stall the taskbar. DPI awareness must match or the child renders at the wrong scale. Explorer restarts drop the child, so it has to re-parent on the `TaskbarCreated` broadcast. The taskbar owns its own layout and the child does not participate in it, so the position has to be recomputed on every settings, display and DPI change.
- **LoadBear hosts WebView2**, which makes all of the above less predictable rather than more.

Bojan's counter-proposal removed the entire problem: size the strip to the taskbar and drag it there by hand. That is what this plan builds. If it proves annoying to reposition after every restart, the next step is remembering the position, not reparenting.

## Measurements taken 2026-08-16

On the development machine, a 1920x1080 primary display at 96 DPI:

```
Bounds:      1920 x 1080
WorkingArea: 1920 x 1032
Taskbar:     48 physical pixels
AppliedDPI:  96, so 48 logical pixels
```

48 is the Windows 11 default and does not change with taskbar settings on 22H2 and later, which removed the small taskbar option. **Do not hardcode a different number from a different machine without re-measuring.**

## Global Constraints

- **No em dashes or double dashes in any user-facing string, comment, or document.** Restructure the sentence instead. This is a brand rule and applies to test fixtures too.
- **One copy of each node.** `render` finds `#chart`, `#zones` and the rest by id and must stay unaware that three layouts exist. Taskbar mode reparents the same three nodes into `#compact`, exactly as collapsed mode already does. Do not draw a second set.
- **Taskbar mode is a third mode, not a replacement for collapsed.** Collapsed keeps its labelled tiles and captioned graph. Whether collapsed survives long term is a decision to take after living with both, and it is not this plan's to take.
- **Mode is not remembered across restarts.** This matches the existing decision at `main.rs:307`, and the window always opens expanded. Do not add persistence in this plan.
- **The bear, the graph and the tiles are the only content.** No title, no tier word, no footer, no enable button.

## What exists today

| Mode | Size | Constant | Body class |
|---|---|---|---|
| Expanded | 900x588 | `EXPANDED_SIZE`, `main.rs:296` | none |
| Collapsed | 900x124 | `COLLAPSED_SIZE`, `main.rs:297` | `collapsed` |

Both heights include the 28px title bar the page draws for itself. `set_collapsed` (`main.rs:309`) takes a boolean and resizes. `setCollapsed` in `dist/index.html:657` reparents three nodes into `#compact`, toggles the body class, flips the chevron, then invokes the command.

`#zones` already renders eight core tiles and `body.collapsed` already folds them to `repeat(4, 1fr)`, which is the 4x2 grid this feature wants. That part is done and only needs to survive at a smaller size.

## Target shape

| Mode | Size | Chrome | Temperature |
|---|---|---|---|
| Expanded | 900x588 | Title bar | 8 tiles, one row, labelled |
| Collapsed | 900x124 | Title bar | 4x2, labelled |
| **Taskbar** | **360x48** | **None, whole strip drags** | **4x2, numbers only** |

Width budget, which is where 360 comes from:

| Part | Width |
|---|---|
| Bear | 28px |
| Sparkline | ~170px |
| Temperature, 4x2 | ~136px |
| Gaps and padding | ~28px |
| **Total** | **~362, rounded to 360** |

Height budget, which is the tight one:

| Part | Height |
|---|---|
| Strip padding, panel border, panel padding | 8px |
| Grid, two rows plus a 2px gap | 40px |
| **Per tile** | **19px** |

A collapsed tile is about 34px: 3px padding twice, 1px border twice, a 9px label and a 16.8px value. Two rows plus the gap is 72px, which is larger than the whole taskbar. **So the label comes off.** 19px holds a border, 1px of padding, and a 12px number. Position in the 4x2 grid identifies the core, which is what collapsed mode already relies on spatially, and the band colour survives untouched.

**This is the part that cannot be settled by reasoning.** A 12px tabular number in a coloured 19px tile may read fine or may be unusable. Bojan looks at it once it runs. If it fails, the fallbacks in order are: drop the sparkline and give the tiles the width, then let the strip stand a few pixels taller than the taskbar.

---

## Task 1: Three-value mode in the shell

**Files:**
- Modify: `crates/loadbear-app/src/main.rs`

**Interfaces:**
- Produces: `set_mode(window, mode: &str) -> Result<(), String>`, accepting `"expanded"`, `"collapsed"` and `"taskbar"`. Replaces `set_collapsed`. Task 2 calls it.

- [x] **Step 1: Write the failing test**

`main.rs` already has a test module covering icon sizes. Add tests that the three modes map to three distinct sizes and that an unknown mode is refused rather than defaulted.

```rust
#[test]
fn every_mode_has_its_own_shape() {
    assert_ne!(EXPANDED_SIZE, COLLAPSED_SIZE);
    assert_ne!(COLLAPSED_SIZE, TASKBAR_SIZE);
    assert_ne!(EXPANDED_SIZE, TASKBAR_SIZE);
}

#[test]
fn the_taskbar_shape_is_the_windows_11_taskbar_height() {
    // Measured on 2026-08-16: 1080 bounds against a 1032 work area at 96 DPI.
    // 48 is the Windows 11 default and 22H2 removed the small taskbar option.
    assert_eq!(TASKBAR_SIZE.1, 48.0);
}

#[test]
fn an_unknown_mode_is_refused_rather_than_defaulted() {
    assert!(size_for_mode("wobbly").is_none());
}
```

- [x] **Step 2: Add the constant and the lookup**

Add beside the existing two, with the measurement recorded in the doc comment so nobody later trims it to look tidier:

```rust
/// The taskbar strip, in logical pixels.
///
/// 48 is the Windows 11 taskbar height, measured rather than assumed: a
/// 1080 pixel display reports a 1032 pixel work area at 96 DPI. Windows 11
/// 22H2 removed the small taskbar setting, so this does not vary.
///
/// No title bar, so all 48 are content. 360 is the sum of a 28px bear, a
/// 170px sparkline and a 136px block of tiles, plus the air between them.
const TASKBAR_SIZE: (f64, f64) = (360.0, 48.0);

fn size_for_mode(mode: &str) -> Option<(f64, f64)> {
    match mode {
        "expanded" => Some(EXPANDED_SIZE),
        "collapsed" => Some(COLLAPSED_SIZE),
        "taskbar" => Some(TASKBAR_SIZE),
        _ => None,
    }
}
```

- [x] **Step 3: Replace the command**

`set_collapsed` becomes `set_mode`, because a boolean cannot carry three states and adding a second boolean would allow a shape that does not exist. Keep the existing doc comment's point that the page has already rearranged itself by the time this runs. Update the `invoke_handler` list at `main.rs:646`.

- [x] **Step 4: Verify**

`cargo test -p loadbear-app` and `cargo clippy --all-targets -- -D warnings`.

---

## Task 2: The taskbar layout

**Files:**
- Modify: `crates/loadbear-app/dist/index.html`

**Interfaces:**
- Consumes: `set_mode` from Task 1.
- Produces: `body.taskbar` styles and a `setMode(mode)` function replacing `setCollapsed(bool)`.

- [x] **Step 1: Read how Tauri v2 decides what is a drag region, before writing the drag handling**

**Do not write this from memory.** The question is whether `data-tauri-drag-region` on a container makes its children draggable, or whether Tauri tests the exact event target. If it tests the exact target, then putting the attribute on `#compact` gives a strip that only drags in the gaps between the bear, the graph and the tiles, which is not what "the whole strip drags" means.

Read the drag region handling in the Tauri v2 source or its documentation, then record here which it is and the date it was read.

**If it tests the exact target,** the answer is a transparent overlay: one absolutely positioned div covering the strip, carrying the attribute, shown only in taskbar mode, with the restore chevron sitting above it. Taskbar mode has no other interactive content, so an overlay costs nothing.

**If it walks ancestors,** the attribute on `#compact` is enough and the chevron needs the attribute removed from itself.

### Answer, read 2026-08-16

Source: `tauri-2.11.5/src/window/scripts/drag.js`, the copy in the cargo registry that this build links against, not the documentation and not a newer release.

It does both, and which one is chosen by the attribute's own value. `isDragRegion` walks `e.composedPath()` from the event target upward and returns on the first element that decides:

| Value | Meaning |
|---|---|
| bare, or `"true"` | Drags only on a direct click on that element. `el === composedPath[0]` |
| `"deep"` | Drags anywhere in the subtree |
| `"false"` | Blocks dragging, for that element and for everything above it |

So neither branch of the plan applies as written. **No overlay is needed.** One attribute, `data-tauri-drag-region="deep"` on `#compact`, gives the whole strip.

Two details the walk settles for free:

- **The chevron stays clickable without any attribute of its own.** The loop returns `false` at any clickable element carrying no attribute, and `BUTTON` is in its `CLICKABLE_TAGS` set. `#tb-restore` is a button, so it blocks the drag on itself and nowhere else.
- **SVG does not interrupt the walk.** The loop skips anything that is not an `HTMLElement`, so a click landing on `#chart` continues up to `#compact` rather than stopping at the plot.

**The attribute is set and removed by `setMode` rather than written into the markup**, because `#compact` also holds the collapsed shape, and collapsed already drags from its own title bar. Giving it a deep drag region would be a change to a mode this plan says to leave alone.

- [x] **Step 2: Add the restore control**

The title bar is hidden in taskbar mode, which takes `#tb-collapse` with it. So taskbar mode needs its own way out. Add a chevron inside `#compact`, hidden in every other mode:

```html
<button id="tb-restore" type="button" title="Expand">&#9652;</button>
```

16px wide, `var(--faint)`, no background until hover. It is the only click target in the strip.

- [x] **Step 3: Write the styles**

Mirror the `body.collapsed` block at `index.html:251`. The three columns change and the tiles lose their labels:

```css
body.taskbar #titlebar { display: none; }
body.taskbar header,
body.taskbar .cols,
body.taskbar footer { display: none; }
body.taskbar #app { grid-template-rows: 1fr; padding: 2px 4px; }
body.taskbar #compact { display: grid;
                        grid-template-columns: 28px 1fr 136px 16px;
                        gap: 6px; min-height: 0; overflow: hidden; }
body.taskbar #bear { width: 28px; height: 28px; align-self: center; }
body.taskbar #graph-panel { border: 0; background: transparent; padding: 0; }
body.taskbar #graph-panel > h2,
body.taskbar .graph .cap { display: none; }
body.taskbar #temp { align-self: stretch; padding: 0; border: 0; background: transparent; }
body.taskbar #zones { grid-template-columns: repeat(4, 1fr); gap: 2px; }
body.taskbar #zones .z { padding: 1px 0; border-radius: 3px; }
/* The label does not fit. Four columns by two rows says which core each
   number is, which is what collapsed mode already relies on. */
body.taskbar #zones .zl { display: none; }
body.taskbar #zones .zv { font-size: 12px; line-height: 1.1; }
body.taskbar #zones .zu { display: none; }
body.taskbar #enable,
body.taskbar #enable-note,
body.taskbar #temp-note { display: none !important; }
```

The degree symbol goes with the label. Eight coloured numbers in a grid beside a bear are not mistakable for anything else, and the unit costs a third of the width of the number it follows.

- [x] **Step 4: Rewrite the mode switch**

`setCollapsed(next)` becomes `setMode(next)` over three string values. Both compact modes reparent into `#compact`, so the reparenting condition becomes `next !== 'expanded'` rather than a boolean. The class toggle becomes two calls, and only one can be true.

~~The chevron in the title bar cycles expanded to collapsed to taskbar and back to expanded.~~ **Cycling was built, shown to Bojan, and rejected the same day.** A control whose meaning depends on the shape you are already in has to be clicked to find out what it does.

**One control per shape instead.** `#tb-collapse` is a plain toggle between expanded and collapsed, which is what it was before this plan touched it, and it is the only one with two states because it is the only toggle. `#tb-strip` is a new button beside it, glyph `&#9644;` at 8px, and it goes straight to the taskbar shape. `#tb-restore` goes straight back to expanded, since the person clicking it wants out of a 48px strip and not a step through it.

Neither title bar control is reachable from the strip, which hides the bar they live in.

- [x] **Step 5: Verify by running it**

`cargo build --release` then `target\release\loadbear-app.exe`, with the app not already running because Windows holds the exe open.

Bojan does the looking. He cycles to taskbar mode, drags the strip over his taskbar, and answers three things:

1. Are the 12px numbers readable at a glance, or is the tile grid a smudge?
2. Does clicking the taskbar raise it above the strip? Windows gives focus priority within the topmost band, and this is the risk that the whole approach turns on.
3. Does the strip drag from anywhere on it, including on top of the bear and the tiles?

---

## Known risks, in the order they are likely to bite

1. ~~**Clicking the taskbar may cover the strip.**~~ **It did, on the first run, exactly as predicted. Fixed 2026-08-16 and confirmed working.**

   The fix is the timer the risk named, and it needed one line of native code rather than reparenting. `presentation::raise_to_the_front` issues `SetWindowPos(HWND_TOPMOST, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE)` every 400 ms while the strip is the shape on screen, and once on entering it.

   **Two things were learned the hard way and are worth not relearning.**

   `Window::set_always_on_top(true)` does nothing when the window is already always on top. Tao's `apply_diff` computes `self ^ new` and returns early on an empty difference, so no `SetWindowPos` is issued at all. The first attempt at this fix used it, shipped, and changed nothing whatsoever. Read `tao-0.35.3/src/platform_impl/windows/window_state.rs` before assuming any window flag setter does something.

   The band question was settled by measurement rather than by argument. A raw `SetWindowPos(HWND_TOPMOST)` from PowerShell, followed by a walk of the visible z-order, put LoadBear at position 2 against the taskbar's 3. So an ordinary topmost window does beat the Windows 11 taskbar, and none of the `SetWindowBand` or `uiAccess` machinery is needed.

   **The strip stands down over anything full screen**, which the timer made necessary. Raising three times a second over a game or a shared screen is a 48px bar that cannot be dismissed. `SHQueryUserNotificationState` is the question, and it is the same one the shell asks before showing a notification.
2. **12px numbers may be unreadable.** Fallbacks are listed above under Target shape.
3. **Position is not remembered.** The strip must be dragged back after every launch. Deliberate for now.
4. **Fullscreen applications hide the taskbar and will not hide the strip.** A topmost 360x48 strip will sit over a game or a video. Not handled in this plan. If it matters, the detection is `SHQueryUserNotificationState` returning a fullscreen state, and the response is to hide.
5. **A non-96 DPI display changes nothing here**, because Tauri sizes in logical pixels and the taskbar is 48 logical pixels at every scale. Worth confirming on the Surface if it runs at 150 percent.

## Self-Review

**Scope.** One constant, one command signature, one CSS block and one function. No new crate, no native call, no persistence, no tray menu entry. The temperature grid needed no work at all because `body.collapsed` already folds eight tiles to four columns.

**The one genuine unknown is Task 2 Step 1**, and it is written as a read-first step rather than an answer, because guessing how Tauri decides a drag region produces a strip that appears to work and then does not drag where a person happens to click.

**Deviation from strict TDD.** Task 2 has no automated test. The page has no test harness, and the thing being verified is whether a number is legible and whether a window drags, neither of which a unit test can answer. Task 1 carries the tests, and Task 2 carries a verification step performed by a person.

**Not decided here.** Whether collapsed mode survives once taskbar mode exists. Both ship, and the question gets asked again after Bojan has used them for a week.
