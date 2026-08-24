# Trix UI/UX Overhaul Design

**Date:** 2026-08-24
**Branch:** `feat/ui-overhaul`
**Baseline:** v0.7.0 (`a788e20`)

---

## 1. Goal

Replace every native browser control in the Trix desktop UI with a drawn one, give
the app a coherent visual language and a frameless window, and merge the video
player's scrubber with the trim track into a single timeline — without changing
what Trix does, what it says, or how it is driven from the keyboard.

## 2. The diagnosis

"Looks amateurish next to Medal" resolves to eight specific things, all verifiable
in the v0.7.0 source:

| Where | What |
| --- | --- |
| `components/Field.svelte` | Native checkbox, `<select>`, number spinner and range slider across all 16 settings rows |
| `components/TrimBar.svelte:97,111` | In/Out are two raw `<input type="range">` under a hand-drawn track |
| `views/ClipPage.svelte:222` | `<video controls>` — Chromium's player bar, stacked directly above the trim track. Two timelines on one page |
| `app.css:41-48` | `button` has no `:hover`, no `:active`, no `:focus-visible` anywhere in the app. Nothing answers the pointer |
| `components/ClipCard.svelte:24` | The favourite marker is the literal character `*`; there is no icon set in the project |
| `views/ClipPage.svelte:245-262` | Six visually identical buttons in one row — no hierarchy between Export and Delete |
| `views/Settings.svelte` | 16 rows, each with a bottom rule, in one scroll. Reads as a config dump |
| `tauri.conf.json` | Stock Windows title bar |

Only the eighth is cosmetic in isolation. The others are the app telling the user
it is a web page.

## 3. What does not change

This is a re-skin and a re-layout. It is **not** a feature release.

- No new settings, no removed settings, no renamed config keys.
- No new daemon commands. No changes to `trix-proto`, `trix-daemon` or `trix-core`.
- No changes to any user-facing sentence except where a control's unit moves out of
  help text and onto the control (§8.5).
- No search, no filtering, no sorting, no tags, no folders in the library.
- No new npm dependency. Hand-written CSS on the existing custom-property system,
  hand-drawn inline SVG icons. Trix targets low-end PCs; a component framework or
  icon package would contradict the product.
- The first-run wizard stays as it is, restyled only.

## 4. Design language

Direction **A3 — "Instrument"**, chosen from three mocked alternatives: near-black,
hairline-thin, precise; two semantic colours with jobs that never overlap.

### 4.1 Colour tokens

Declared on `:root` in `app.css`, replacing the current seven variables.

```css
--bg:            #0b0d10;   /* app background */
--surface:       #101319;   /* title bar, rail, settings panels */
--raised:        #161a21;   /* default buttons, steppers, inputs on a panel */
--overlay:       #151920;   /* menus, dropdowns, modals, toasts */

--line:          rgba(255, 255, 255, 0.07);   /* dividers, panel edges */
--line-strong:   rgba(255, 255, 255, 0.11);   /* input and control borders */

--text:          #e4e8ee;
--dim:           #8b94a3;   /* help text, secondary metadata */
--faint:         #5d6675;   /* micro-labels, disabled, version string */

--accent:        #5b9dff;   /* SELECTED / interactive / primary action */
--accent-ink:    #04122e;   /* text on a filled accent surface */
--live:          #3ddc97;   /* ARMED / capture is running / re-arm notice */
--danger:        #ff5c5c;   /* destructive and error */
--fav:           #f5c451;   /* favourited, and nothing else */
```

**The colour contract, which every screen obeys:**

- **Blue means "you selected this"** — nav pill, selected clip, slider fill, primary
  button, focus ring, trim range.
- **Green means "Trix is live"** — arm control, its dot, the buffer meter, the re-arm
  notice. Green appears nowhere else.
- **Red means destructive or broken** — Delete, error toasts, refused trim ranges.
- **Amber means kept** — the favourite star. One glyph, one colour, one meaning.

Red was the first proposal for ARMED and was rejected during design: Trix already
spends red on Delete and on error toasts, so putting the alarm colour on the healthy,
desired state means the app's loudest signal never means "something is wrong."

Tinted fills use `color-mix(in srgb, var(--x) N%, transparent)`. This is already in
use at `TrimBar.svelte:129` and therefore proven on this project's WebView2.

### 4.2 Radius, spacing, elevation

```css
--r-sm:   6px;    /* badges, menu items, small chips */
--r:      7px;    /* buttons, inputs, nav pills */
--r-md:   9px;    /* thumbnails, cards, popovers */
--r-lg:  10px;    /* panels, modals, the video stage */
--r-full: 999px;  /* the arm pill, toggle tracks */
```

Spacing steps: 4, 6, 8, 10, 12, 14, 16, 20, 24 px. No other values.

Elevation is carried by surface lightness, not by shadow, with two exceptions that
float above the page: menus and modals get `0 14px 34px rgba(0,0,0,0.6)`. There is no
glow anywhere — glow was the defining trait of the rejected "Arena" direction.

### 4.3 Typography

Stack unchanged: `"Segoe UI Variable Text", "Segoe UI", system-ui, sans-serif`. No
web font is downloaded.

| Role | Size | Weight |
| --- | --- | --- |
| Page title (Settings) | 19px | 650 |
| View title (Clips) | 15px | 650 |
| Emphasis (clip title) | 13.5px | 650 |
| Body / setting label | 13px | 500 |
| Control text, menu items | 12.5px | 400 |
| Buttons, clip title | 12px | 400 (600 on primary) |
| Help text | 11.5px | 400 |
| Caption, metadata | 11px | 400 |
| Micro-label (section headers) | 10.5px | 600, uppercase, `letter-spacing: .11em` |

Every numeric readout carries `font-variant-numeric: tabular-nums` — durations, byte
counts, percentages, buffer seconds, trim points, clip counts. Digits must not jitter
as they count.

### 4.4 Motion

```css
--t-fast: 120ms;   /* hover, focus, press */
--t:      160ms;   /* toggles, menus, popovers */
--t-slow: 220ms;   /* modal and toast entry */
--ease:   cubic-bezier(.2, .8, .3, 1);
```

Nothing animates longer than 220ms. The buffer meter keeps its existing
`transition: width 200ms linear`. A global
`@media (prefers-reduced-motion: reduce)` block sets every duration to `0.01ms`.

### 4.5 Focus

The app currently has **no focus styling at all**. Every interactive element gets:

```css
:focus-visible {
  outline: 2px solid var(--accent);
  outline-offset: 2px;
  border-radius: inherit;
}
```

`:focus-visible`, not `:focus`, so a mouse click does not leave a ring behind.

## 5. Icons

One `components/ui/Icon.svelte` taking `name` and optional `size` (default 16),
switching over inline `<svg>` paths. `viewBox="0 0 16 16"`, `fill="none"`,
`stroke="currentColor"`, `stroke-width="1.35"`, `stroke-linecap="round"`,
`stroke-linejoin="round"`. Filled exceptions: `play`, `star-filled`, `dots`.

Required names: `play`, `pause`, `clips`, `settings`, `star`, `star-filled`,
`pencil`, `trash`, `folder`, `scissors`, `volume`, `volume-mute`, `fullscreen`,
`fullscreen-exit`, `chevron-left`, `chevron-right`, `chevron-down`, `dots`, `check`,
`rearm`, `minimize`, `maximize`, `restore`, `close`, `alert`.

Two corrections settled during design: **Settings is a cog, not a sun**, and
**Export trimmed is a pair of scissors**, not a download arrow.

Every icon is decorative and sits beside a text label or inside a control with an
`aria-label`; icons carry `aria-hidden="true"` and never a `<title>`.

## 6. Control primitives

New directory `web/src/components/ui/`. Each primitive is presentational, owns no
daemon call, and takes its value in and reports changes out — the same rule
`TrimBar.svelte` already follows.

Every one of these replaces a native element that gave keyboard operation for free.
**The keyboard contract below is a requirement, not a nicety.** It is the single
largest regression risk in this work.

### 6.1 `Button.svelte`

Props: `variant` (`primary` | `default` | `ghost` | `danger`), `size` (`sm` | `md`),
`icon`, `disabled`, `title`, `onclick`. Renders a real `<button>`, so Space and Enter
stay native and `keys.ts`'s existing `ACTIVATABLE_TAGS` rule keeps working unchanged.

`:hover` lightens the surface one step, `:active` translates 1px down, `:disabled`
drops to `opacity: .4; cursor: default`.

### 6.2 `IconButton.svelte`

Square `<button>`, icon only, `aria-label` **required** (a build-time TypeScript
requirement, not optional). Used for window controls, player controls, the card
overflow trigger, and the clip page's secondary actions.

### 6.3 `Toggle.svelte`

Replaces `<input type="checkbox">`. A `<button role="switch" aria-checked>`. Space
and Enter toggle. 38x21 track, 15px circular knob, `--accent` when on.

### 6.4 `Stepper.svelte`

Replaces `<input type="number">`. A text input flanked by `−` and `+` buttons, with
the unit rendered after the value (`60 fps`, `15 s`, `20 GB`). Props: `value`, `min`,
`max`, `step`, `unit`. Typing is still allowed. `ArrowUp`/`ArrowDown` step by `step`;
`Home`/`End` go to `min`/`max`. Commits on `change`, on blur and on Enter — never on
every keystroke.

### 6.5 `Select.svelte`

Replaces `<select>`. A `<button>` trigger plus a popover `role="listbox"` of
`role="option"` items. Keyboard: `Enter` / `Space` / `ArrowDown` opens, `ArrowUp` /
`ArrowDown` move the active option, `Enter` selects, `Escape` closes, `Tab` closes and
moves on. Closes on outside pointerdown. **Focus returns to the trigger on close.**
The selected option is marked with a `check` icon.

### 6.6 `Slider.svelte`

Replaces `<input type="range">`. A `<div role="slider">` with `tabindex="0"`,
`aria-valuemin`, `aria-valuemax`, `aria-valuenow`, `aria-valuetext` and `aria-label`.
4px track, **15px circular thumb** with a 2px `#dceaff` ring.

- Pointer: press anywhere on the track jumps the thumb there and begins a drag;
  `setPointerCapture` keeps the drag alive outside the element.
- Keyboard: `ArrowLeft`/`ArrowRight` ±`step`, `PageUp`/`PageDown` ±`10 × step`,
  `Home`/`End` to the ends.
- Events: fires `oninput` continuously during a drag and `onchange` once on release.

That last point preserves an existing behaviour exactly. `Field.svelte` today tells
the daemon on `change`, not on `input`, so a drag across the track is one round trip
rather than eighty, and its local `dragging` state shows the value under the thumb
meanwhile. **That mechanism, including the `$effect` that clears `dragging` when the
authoritative value lands, must survive unchanged.**

### 6.7 `Menu.svelte`

Popover for the clip card's overflow. `role="menu"` with `role="menuitem"` children.
`ArrowUp`/`ArrowDown` move a roving focus, `Enter` activates, `Escape` closes and
returns focus to the trigger, outside pointerdown closes. Positioned right-aligned
under its trigger, and flipped upward when it would overflow the viewport bottom.

### 6.8 `Modal.svelte`

Replaces the inline delete-confirmation strip at `ClipPage.svelte:265-277`. Centred
`role="dialog" aria-modal="true"` over a `rgba(0,0,0,.55)` backdrop. Focus moves to
the dialog on open and is trapped inside it; `Escape` closes; focus returns to the
element that opened it.

The existing safety design is kept and simplified: today every other button on the
clip page carries `disabled={confirmingDelete}` so it cannot be clicked while the
strip is open. A modal with a focus trap enforces that structurally, so those
`disabled` bindings can go — but the `confirmingDelete` early-return in the page's
`onkeydown` stays, because window-level shortcuts still must not act on another clip
while the dialog is up.

### 6.9 `KeycapInput.svelte`

The hotkey capture control. Renders the current combination as `<kbd>` chips
(`Alt` + `F10`) rather than as text in a readonly box. Click to arm capture, then the
existing `captureHotkey` logic in `Settings.svelte` is reused verbatim — including its
separation of `ctrlKey` from `altKey`, which exists because Windows reports AltGr as
Ctrl+Alt and this machine runs a Turkish Q layout.

## 7. Window chrome

`tauri.conf.json` gains `"decorations": false` on the `main` window. Size, minimum
size and resizability are unchanged.

The title bar is `components/TitleBar.svelte`, 40px tall, `--surface`, with a
`--line` bottom border:

`[T logo] TRIX   [arm pill + buffer meter]   ←— drag region —→   [− ▢ ✕]`

- The drag region carries `data-tauri-drag-region`. Double-clicking it toggles
  maximize, which Tauri handles natively.
- Window controls call `minimize()`, `toggleMaximize()` and `close()` from
  `@tauri-apps/api/window`. Widths are 44px each, Windows-standard; close hovers
  `#e81123`.
- **Capabilities:** `capabilities/default.json` currently grants only `core:default`.
  Dragging works under that set, but the window-control calls very likely do not.
  Verify which of `core:window:allow-minimize`, `allow-maximize`, `allow-unmaximize`,
  `allow-toggle-maximize` and `allow-close` are already granted, and add the rest
  explicitly. **A missing window permission fails at runtime, not at compile time** —
  `npm run check` and `cargo build` will both pass with a title bar whose buttons do
  nothing, so this is confirmed by clicking them in a real build (hand-check 1).
- The maximize icon swaps to a "restore" glyph while maximized, tracked through
  `getCurrentWindow().onResized`.

### 7.1 Accepted deviation: Windows 11 snap layouts

Hovering the maximize button on Windows 11 normally opens the snap-layouts flyout.
That is driven by `WM_NCHITTEST` returning `HTMAXBUTTON`, which a frameless Tauri
window does not do. Recovering it means a Win32 hit-test hook in Rust.

**Decision: accept the loss.** Win+Arrow and drag-to-edge snapping both still work;
only the hover affordance on that one button is gone. The Win32 hook is
disproportionate to a hover menu, and it is recoverable later without touching any of
this design.

## 8. Screens

### 8.1 Title bar (new)

The arm control moves out of the rail and into the title bar. Armed is a global
state, so it belongs somewhere no page can scroll it away.

- **Disarmed:** an outlined pill reading `Arm`, `--faint` dot, `--dim` text.
- **Armed:** `--live` text on a `color-mix(--live 13%)` fill with a
  `color-mix(--live 50%)` border; a `--live` dot with a 3px halo; a 44px buffer meter
  filling `--live`; and the reading `10.2/15s` in tabular numerals.
- Disabled when `!app.connected`, as the rail's button is today.

### 8.2 Rail

Narrows from 200px to 150px — the arm control and buffer meter have left. It now
holds only the two nav items and the version string. Nav items become filled pills
(`color-mix(--accent 15%)` with `#eaf1fb` text), each with its icon: a film frame for
Clips, a cog for Settings.

### 8.3 Library

Header gains `{app.total} clips`. A library byte total is shown **only when
`app.clips.length === app.total`** — `library.list` is fetched with `limit: 200`, so
summing the loaded page would understate a larger library. Honest count always,
size only when it is the whole truth.

**`ClipCard.svelte` is restructured.** It is a single `<button>` today; it now needs
an overflow menu inside it, and an interactive element nested in a button is invalid
and breaks keyboard operation. New shape:

```
<div class="card">
  <button class="thumb">   ← select on click, open on dblclick
     img, duration badge
  </button>
  <div class="meta">
     [amber star if favourite] title            [⋯ IconButton]
     size · resolution
  </div>
</div>
```

- The `⋯` trigger is hidden at rest and revealed on card hover, on card focus, while
  the selected card is selected, and while its own menu is open — it is never a
  control the user has to guess at.
- Menu items, in this order: **Favourite**, **Rename**, separator, **Delete** in
  `--danger`.
- **Rename** from the menu turns the card's title into a text input in place, committing
  on `Enter` and on blur and cancelling on `Escape`, through the same `app.rename(id,
  title)` call the clip page already uses. It does not navigate to the clip page.
- **The favourite star moves off the thumbnail** into the meta row, left of the title,
  in `--fav`. On the thumbnail a bare glyph is invisible over a bright frame; in the
  meta row it is always on the app's own dark surface, and it sits on the same line as
  the menu item that toggles it.
- Hover raises the card 2px and lightens the thumbnail's inset ring; the selected card
  keeps a 2px `--accent` ring plus a 3px tinted halo.
- Empty state gains the user's actual hotkey rather than the phrase "your clip
  hotkey".

Because the card is no longer a single `<button>`, `Grid.svelte`'s `insideGrid`
calculation and its interaction with `keys.ts` must be re-verified: the grid's Space
and Enter exception (`ownsActivation`) is written against cards being buttons.

### 8.4 Clip page

**One timeline replaces two.** This is the largest change in the overhaul.

`<video controls>` becomes `<video>` with `controls={false}`, wrapped in
`components/VideoPlayer.svelte`. `TrimBar.svelte` is replaced by
`components/Timeline.svelte`. The player's transport row is:

`[▶ 36px circle]  [ ————— unified timeline ————— ]  0:05.7 / 0:15  [volume]  [fullscreen]`

`Timeline.svelte` is a 38px band, `--raised` on a `--line` border, containing:

1. Keyframe ticks — 1px `rgba(255,255,255,.13)` verticals, from the same
   `app.keyframesFor(id)` data the trim bar uses today.
2. Two `rgba(0,0,0,.45)` scrims over the regions outside the selected range.
3. The selected range: `color-mix(--accent 20%)` fill with 2px `--accent` edges.
4. A 2px white playhead with a small tab at the top.
5. Two 15px circular `--accent` handles with `#dceaff` rings, at the In and Out
   positions.

Interactions:

- Pressing the band anywhere that is not a handle seeks, and dragging continues to
  seek. This is what the track already does at `TrimBar.svelte:88`.
- Dragging the In handle sets the in-point, **snapped backwards to the previous
  keyframe** by the existing `snap()` function, moved across unchanged — including its
  documented rule that an empty keyframe list means no snapping rather than snapping
  to zero.
- Dragging the Out handle sets the out-point raw. Fast mode cuts the end where it is
  asked to, so there is nothing to snap to.
- Both handles are `role="slider"` with `tabindex="0"` and an `aria-label` of
  "Trim start" / "Trim end". **In:** `ArrowLeft`/`ArrowRight` move to the previous /
  next keyframe — the only unit that means anything for an in-point. **Out:** ±100ms,
  and ±1s with Shift. `Home`/`End` on either goes to 0 / duration.

  This is a deliberate improvement on today. `keys.ts:31-40` records that the current
  `<input type="range">` handles were given a 1ms step, which made arrow keys move the
  trim point by an invisible amount, and that the page therefore took the arrows back
  for prev/next clip. Keyframe-and-100ms steps make arrows worth having again — so
  `keys.ts` and `ClipPage.svelte` must give `ArrowLeft`/`ArrowRight` to a focused
  handle and keep them for prev/next everywhere else.

The In/Out readout and the **Export trimmed** button (scissors icon) sit on one row
directly under the timeline, Export right-aligned. Export keeps its current disabled
logic exactly: `rangeError !== null` disables it and supplies the `title`, from the
same `trimRangeError` the Ctrl+E path uses, so the greyed button and the shortcut can
never disagree.

Below that, the metadata line unchanged, then the action row with a hierarchy:
title (flex, left) · favourite, rename, Show in Explorer as `IconButton`s ·
a `--line` separator · **Delete** in `--danger` with its trash icon.

Prev/next stepper buttons move from flanking the video to the header row beside the
`counterLabel` counter, as `chevron-left` / `chevron-right` `IconButton`s.

**Fullscreen must be requested on the stage wrapper, not on the `<video>`.** The
controls are siblings of the video element; requesting fullscreen on the video alone
would take them off screen.

### 8.5 Settings

Same 16 fields, same six sections, same help sentences. Three structural changes:

1. **Sections become panels.** Each section is a `--surface` panel with `--r-lg`
   corners under a micro-label header, and rows inside are separated by a
   `rgba(255,255,255,.05)` hairline instead of every row carrying a bottom rule.
2. **Label and help move left; the control moves right.** Today the grid is
   `180px 1fr` with the help text *under the control*, so sentences wrap early and no
   two controls line up. Help now sits under its label and runs the full width, and
   every control aligns on one right edge.
3. **Units move onto the control** — `60 fps`, `15 s`, `20 GB`, `8000 kbps`. They exist
   only inside help text today.

The re-arm notice becomes `--live`, not `--accent`: it is about live capture, which is
what green already means.

The "settings this app does not render yet" fallback and the inline Version /
Check now row both stay, restyled.

### 8.6 Everything else

- **Toasts** — `--overlay` with the modal shadow, `--danger` left edge and an `alert`
  icon on errors, sliding in over `--t-slow`.
- **Update banner** — spans under the title bar, `--accent` for an available update
  and `--dim` for the error form. Its Update button becomes a primary `Button`.
- **`DaemonDown`** — restyled; its Start button becomes a primary `Button`. It renders
  without the title bar today because it replaces the whole shell; it must now render
  **with** the title bar, so the window is still draggable and closable when the daemon
  is down. Same for the `updates.swapping` branch and `FirstRun`.
- **`FirstRun`** — restyled with the new primitives. No structural change.

## 9. Keyboard and accessibility contract

Every behaviour below exists in v0.7.0 and must still work identically afterwards.
This list is the acceptance criteria for the overhaul.

**Grid:** arrows move the selection one visual row/column (column count from the
existing `ResizeObserver`), clamping rather than wrapping; `Enter` opens; `Space`
previews in place; click selects; double-click opens.

**Clip page:** `Escape` returns to the grid; `ArrowLeft`/`ArrowRight` step clips
(except when a trim handle has focus, per §8.4); `Space` plays and pauses; `i` and `o`
set the in and out points **from the playhead**, not from a handle position; `Delete`
opens the confirmation; `Ctrl+E` exports.

**`Ctrl+E` must keep its `!e.altKey` guard verbatim.** Windows reports AltGr as
Ctrl+Alt, this machine runs a Turkish Q layout where AltGr is a live typing modifier,
and a `ctrlKey`-only test fires a full export from a keystroke meant to type a
character.

**Rename:** `Enter` commits, `Escape` cancels, and while renaming no other shortcut
fires.

**Delete confirmation:** while it is open, no window-level shortcut acts on any clip;
`Escape` closes it.

**Settings:** the hotkey capture field swallows the keys it is capturing; a `Tab` pass
reaches every control in visual order.

`keys.ts` and its test suite must be updated as part of this work, not after it. The
new focusable elements are `role="slider"` divs, `role="switch"` buttons and
`role="menuitem"` elements, none of which its current `TYPING_TAGS` /
`NON_TYPING_INPUT_TYPES` / `ACTIVATABLE_TAGS` sets know about.

## 10. File structure

```
crates/trix-ui/tauri.conf.json            decorations: false
crates/trix-ui/capabilities/default.json  window control permissions

web/src/app.css                           tokens, reset, focus; element-level
                                          control styling deleted entirely

web/src/components/ui/Icon.svelte         new
web/src/components/ui/Button.svelte       new
web/src/components/ui/IconButton.svelte   new
web/src/components/ui/Toggle.svelte       new
web/src/components/ui/Stepper.svelte      new
web/src/components/ui/Select.svelte       new
web/src/components/ui/Slider.svelte       new
web/src/components/ui/Menu.svelte         new
web/src/components/ui/Modal.svelte        new
web/src/components/ui/KeycapInput.svelte  new

web/src/components/TitleBar.svelte        new
web/src/components/VideoPlayer.svelte     new
web/src/components/Timeline.svelte        new — replaces TrimBar.svelte
web/src/components/ClipCard.svelte        restructured (§8.3)
web/src/components/Field.svelte           rewritten to the new row layout
web/src/components/Toasts.svelte          restyled
web/src/components/TrimBar.svelte         deleted

web/src/App.svelte                        title bar in every branch
web/src/views/{Rail,Grid,ClipPage,Settings,DaemonDown,FirstRun}.svelte  restyled
web/src/lib/keys.ts                       updated for the new focusables
```

No file in `trix-core`, `trix-daemon` or `trix-proto` is touched.

## 11. Testing

**Must still pass unchanged in intent:** `keys.test.ts`, `clips.test.ts`,
`settings.test.ts`, `state.svelte.test.ts` — 90 vitest cases today. `keys.test.ts`
gains cases; the others should need no edit, and an edit to them is a signal that
behaviour moved when it should not have.

**New unit tests**, all pure functions, no DOM required:

- Timeline geometry: pixel↔millisecond mapping at both ends and past both ends;
  keyframe snapping including the empty-list case; the previous/next-keyframe arrow
  steps.
- Slider value math: step rounding, clamping, `PageUp`/`PageDown`, `Home`/`End`.
- `Select` keyboard navigation: open, move, wrap-or-clamp, select, escape.
- `keys.ts`: the new roles, and that a focused trim handle keeps its arrows while
  every other focus target lets them through.

**Gates:** `npm run check` at 0 errors / 0 warnings, `vitest run` green,
`vite build` succeeds, `cargo clippy --workspace --all-targets` no worse than the
10 warnings that exist on `master` today, `cargo fmt --check` clean.

**Hand-verification is required before merge** — no script covers any of this:

1. The window drags from the title bar, double-click maximizes, and all three window
   buttons work.
2. The window still resizes from every edge and corner with `decorations: false`.
3. A maximized window does not overflow the screen edges.
4. Every settings control is operable by keyboard alone, `Tab` order is sane, and
   focus rings appear on `Tab` but not on click.
5. Dragging a volume or microphone slider produces **one** daemon round trip, not one
   per pixel.
6. On the clip page: seek by clicking the band, trim by dragging both handles, `i`/`o`
   from the playhead, `Ctrl+E` exports, and the exported clip is correct.
7. AltGr+E on the Turkish Q layout types a character and does **not** export.
8. Fullscreen shows the Trix controls, not Chromium's.
9. The card overflow menu opens, all three items work, and Delete raises the modal.
10. Arm and disarm from the title bar; the buffer meter fills green and empties.
11. The whole app is usable while the daemon is down — the title bar renders and the
    window closes.

## 12. Risks

| Risk | Severity | Mitigation |
| --- | --- | --- |
| **Keyboard regressions.** Native controls gave operation for free; hand-rolled ones do not. | **High** | §9 is written as acceptance criteria; `keys.ts` tests extended; hand-check 4 and 6 |
| **Frameless window basics** — resize edges, maximize overflow, rounded corners | Medium | Hand-checks 1-3; `decorations: false` is reversible in one line if it goes badly |
| Missing Tauri window capability fails only at runtime | Medium | §7 names the exact permissions; hand-check 1 |
| Fullscreen taking the controls off screen | Low | §8.4 requires fullscreen on the stage wrapper; hand-check 8 |
| `ClipCard` restructuring breaking the grid's Space/Enter exception | Medium | §8.3 flags it; `keys.test.ts` covers it |
| `color-mix()` unsupported on an old WebView2 | Low | Already shipping in `TrimBar.svelte:129` since 0.7.0 |

## 13. Out of scope, deliberately

- Windows 11 snap-layouts hover flyout (§7.1) — accepted loss, recoverable later.
- Light theme. Trix is dark-only and `color-scheme: dark` stays.
- Filmstrip thumbnails on the timeline — still needs a decode path the daemon does not
  have, which is why `TrimBar` drew ticks in the first place.
- Library search, filter, sort, and any new metadata.
- Window icon resources for `trix-daemon.exe` / `trix.exe` — a long-standing separate
  item, unrelated to the web UI.
