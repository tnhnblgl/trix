# Trix UI/UX Overhaul Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace every native browser control in the Trix desktop UI with a drawn one, give the app a frameless window and a coherent visual language, and merge the video scrubber with the trim track into one timeline — without changing what Trix does, says, or how it is driven from the keyboard.

**Architecture:** A token layer in `app.css` feeds a set of presentational primitives in `web/src/components/ui/`. Every primitive that has arithmetic or keyboard logic pushes it into a pure module in `web/src/lib/` (`ui.ts`, `icons.ts`, `timeline.ts`) so it is unit-testable without a DOM. Screens then consume the primitives. No daemon call moves; no primitive makes one.

**Tech Stack:** Svelte 5 (runes), TypeScript, Vite, Vitest, Tauri v2. No new dependency of any kind.

**Spec:** `docs/superpowers/specs/2026-08-24-trix-ui-overhaul-design.md` (`d0fe8f8`)

## Global Constraints

Every task's requirements implicitly include all of these.

- **No new npm dependency**, runtime or dev. Trix targets low-end PCs.
- **No implementer touches a file outside `crates/trix-ui/`.** Nothing in
  `trix-core`, `trix-daemon` or `trix-proto`. The one exception is not an
  implementer's to make: when review finds that this plan's own prescribed
  code was wrong, the controller amends **this document** in a separate
  `docs:` commit so the plan and the shipped code do not drift. Those commits
  are expected, and are not a breach of this rule.
- **No new, removed or renamed config key. No new daemon command.** Every `call(...)` used already exists.
- **No user-facing sentence changes** except units moving out of help text onto a control (Task 7) and the empty-state hotkey (Task 9). Adding a sentence where there was none is not a *change*: new help text on a row this overhaul restructures is allowed, and Task 7's Settings lede ("Changes take effect on the next clip...") and the Version row's help line ship as written. Rewriting or deleting a sentence that already ships is still forbidden,
  with one ruled exception: the arm control's label (`Armed` -> `ARMED`) and
  its buffer readout (`12s / 60s` -> `12/60s`), reworded in Task 8 when the
  control moved from the rail into the much narrower title bar. Both carve-outs
  above are the project owner's rulings, made on 2026-08-25 after review found
  the brief mandating copy that this line, as first written, forbade.
- **Testing rule, deliberate:** this project has **zero component tests** and no DOM test library, and adding one would break the dependency rule. All 90 existing vitest cases test `lib/*.ts`. Therefore: **logic goes in `lib/`, and is tested there; `.svelte` files carry markup and styling only.** A reviewer must not treat "no test for this component" as a defect — they must treat "testable logic left inside a component" as one.
- **Dark only.** `color-scheme: dark` stays; no light theme.
- **Every task ends green:** `npm run check` at 0 errors / 0 warnings, `npx vitest run` all passing, `npm run build` succeeding. Run from `crates/trix-ui/web`.
- **Commits are authored by `tnhnblgl <tnhnblgl@gmail.com>`.** No `Co-Authored-By` trailer, no "Generated with" line, no `--author` flag, no mention of Claude or Anthropic anywhere in a commit message.
- **Never push.** The branch stays local until the user asks.
- Colour, radius, spacing, type and motion values are **copied verbatim** from spec §4. No new value may be invented; if something seems missing, use the nearest listed token.

## The colour contract

Every task obeys this. It is the whole point of the palette.

| Token | Means | Appears on |
| --- | --- | --- |
| `--accent` `#5b9dff` | **you selected this** | nav pill, selected clip, slider fill, primary button, focus ring, trim range |
| `--live` `#3ddc97` | **Trix is live** | arm control, its dot, buffer meter, re-arm notice. Nowhere else |
| `--danger` `#ff5c5c` | destructive or broken | Delete, error toasts, refused trim range |
| `--fav` `#f5c451` | kept | the favourite star, and nothing else |

---

## File structure

| File | Responsibility |
| --- | --- |
| `web/src/app.css` | Tokens, reset, global focus ring, reduced motion. **No control styling.** |
| `web/src/lib/ui.ts` | Pure control maths: clamp, step, ratio↔value, list index movement |
| `web/src/lib/icons.ts` | Icon path data, keyed by name |
| `web/src/lib/timeline.ts` | Pure timeline maths: ms↔ratio, keyframe snap, keyframe stepping |
| `web/src/components/ui/Icon.svelte` | Renders one entry from `icons.ts` |
| `web/src/components/ui/Button.svelte` | Text button, four variants |
| `web/src/components/ui/IconButton.svelte` | Square icon-only button, `aria-label` required |
| `web/src/components/ui/Toggle.svelte` | `role="switch"` — replaces `<input type=checkbox>` |
| `web/src/components/ui/Slider.svelte` | `role="slider"` — replaces `<input type=range>` |
| `web/src/components/ui/Stepper.svelte` | −/value/+ with unit — replaces `<input type=number>` |
| `web/src/components/ui/Select.svelte` | Trigger + listbox popover — replaces `<select>` |
| `web/src/components/ui/Menu.svelte` | `role="menu"` popover for the card overflow |
| `web/src/components/ui/Modal.svelte` | Focus-trapped dialog — replaces the inline delete strip |
| `web/src/components/ui/KeycapInput.svelte` | Hotkey capture rendered as `<kbd>` chips |
| `web/src/components/TitleBar.svelte` | Frameless chrome: brand, arm pill, drag region, window buttons |
| `web/src/components/VideoPlayer.svelte` | `<video controls={false}>` plus the transport row |
| `web/src/components/Timeline.svelte` | The unified seek + trim band |
| `web/src/components/ClipCard.svelte` | Restructured: thumb button + meta row + overflow menu |
| `web/src/components/Field.svelte` | Rewritten to label-left / control-right |
| `web/src/components/TrimBar.svelte` | **Deleted** in Task 10 |

---

## Task 1: Design tokens and control maths

**Files:**
- Modify: `crates/trix-ui/web/src/app.css` (whole file, 60 lines)
- Create: `crates/trix-ui/web/src/lib/ui.ts`
- Create: `crates/trix-ui/web/src/lib/ui.test.ts`

**Interfaces:**
- Consumes: nothing.
- Produces: the CSS custom properties every later task uses, and
  `clamp(value, min, max)`, `snapToStep(value, min, step)`,
  `ratioToValue(ratio, min, max, step)`, `valueToRatio(value, min, max)`,
  `stepBy(value, delta, min, max, step)`, `nextIndex(current, delta, count)` —
  all `(…numbers) => number`.

- [ ] **Step 1: Write the failing test**

Create `crates/trix-ui/web/src/lib/ui.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { clamp, nextIndex, ratioToValue, snapToStep, stepBy, valueToRatio } from './ui';

describe('clamp', () => {
  it('passes a value already inside the range through', () => {
    expect(clamp(5, 0, 10)).toBe(5);
  });

  it('pins a value outside the range to the nearer end', () => {
    expect(clamp(-3, 0, 10)).toBe(0);
    expect(clamp(99, 0, 10)).toBe(10);
  });
});

describe('snapToStep', () => {
  it('rounds to the nearest step measured from min, not from zero', () => {
    // A slider running 20..80 in steps of 15 can only sit on 20, 35, 50, 65, 80.
    expect(snapToStep(38, 20, 15)).toBe(35);
    expect(snapToStep(44, 20, 15)).toBe(50);
  });

  it('leaves a value alone when the step is 0 or 1', () => {
    expect(snapToStep(37, 0, 1)).toBe(37);
    expect(snapToStep(37, 0, 0)).toBe(37);
  });
});

describe('ratioToValue', () => {
  it('maps the two ends exactly', () => {
    expect(ratioToValue(0, 0, 100, 1)).toBe(0);
    expect(ratioToValue(1, 0, 100, 1)).toBe(100);
  });

  it('snaps the result to the step', () => {
    expect(ratioToValue(0.5, 0, 100, 10)).toBe(50);
    expect(ratioToValue(0.54, 0, 100, 10)).toBe(50);
    expect(ratioToValue(0.56, 0, 100, 10)).toBe(60);
  });

  it('clamps a pointer dragged outside the track', () => {
    expect(ratioToValue(-0.4, 0, 100, 1)).toBe(0);
    expect(ratioToValue(1.8, 0, 100, 1)).toBe(100);
  });
});

describe('valueToRatio', () => {
  it('is the inverse of ratioToValue at the ends and the middle', () => {
    expect(valueToRatio(0, 0, 100)).toBe(0);
    expect(valueToRatio(50, 0, 100)).toBe(0.5);
    expect(valueToRatio(100, 0, 100)).toBe(1);
  });

  it('returns 0 for a zero-width range rather than dividing by zero', () => {
    expect(valueToRatio(7, 7, 7)).toBe(0);
  });
});

describe('stepBy', () => {
  it('moves one step per call and stops at the ends', () => {
    expect(stepBy(50, 1, 0, 100, 5)).toBe(55);
    expect(stepBy(50, -1, 0, 100, 5)).toBe(45);
    expect(stepBy(100, 1, 0, 100, 5)).toBe(100);
    expect(stepBy(0, -1, 0, 100, 5)).toBe(0);
  });

  it('takes a multiple for PageUp and PageDown', () => {
    expect(stepBy(50, 10, 0, 100, 1)).toBe(60);
    expect(stepBy(50, -10, 0, 100, 1)).toBe(40);
  });

  it('lands on the step grid even from an off-grid start', () => {
    // The daemon can hand back a value the UI's own step would never produce.
    expect(stepBy(37, 1, 0, 100, 10)).toBe(40);
  });
});

describe('nextIndex', () => {
  it('moves within the list and clamps at both ends', () => {
    expect(nextIndex(0, 1, 3)).toBe(1);
    expect(nextIndex(2, 1, 3)).toBe(2);
    expect(nextIndex(0, -1, 3)).toBe(0);
  });

  it('returns -1 for an empty list so a caller cannot index into nothing', () => {
    expect(nextIndex(0, 1, 0)).toBe(-1);
  });
});
```

- [ ] **Step 2: Run the test to verify it fails**

Run from `crates/trix-ui/web`: `npx vitest run src/lib/ui.test.ts`
Expected: FAIL — `Failed to resolve import "./ui"`.

- [ ] **Step 3: Write the implementation**

Create `crates/trix-ui/web/src/lib/ui.ts`:

```ts
/**
 * Control maths for the drawn primitives in `components/ui/`.
 *
 * Everything here is pure and DOM-free on purpose. The project has no
 * component test framework and adding one would mean a new dependency, so a
 * primitive's correctness lives in whatever part of it can be pulled out to
 * here and tested. A `.svelte` file that grows arithmetic is a file with an
 * untested branch in it.
 */

/** `value`, pinned into `[min, max]`. */
export function clamp(value: number, min: number, max: number): number {
  return value < min ? min : value > max ? max : value;
}

/**
 * `value` rounded onto the grid of `step`s starting at `min`.
 *
 * Measured from `min` rather than from zero: a control running 20..80 in
 * fifteens can sit on 20, 35, 50, 65 and 80, and rounding from zero would
 * offer 30, 45 and 60 instead -- positions the control cannot actually hold.
 */
export function snapToStep(value: number, min: number, step: number): number {
  if (step <= 1) return value;
  return min + Math.round((value - min) / step) * step;
}

/** Where a pointer at `ratio` along a track lands, snapped and clamped. */
export function ratioToValue(ratio: number, min: number, max: number, step: number): number {
  const raw = min + clamp(ratio, 0, 1) * (max - min);
  return clamp(snapToStep(raw, min, step), min, max);
}

/** How far along its track `value` sits, as 0..1. */
export function valueToRatio(value: number, min: number, max: number): number {
  // A zero-width range is a real state -- a clip of no length, a bound the
  // daemon collapsed -- and dividing by it yields NaN, which reaches CSS as
  // `left: NaN%` and drops the thumb out of the control entirely.
  if (max <= min) return 0;
  return clamp((value - min) / (max - min), 0, 1);
}

/**
 * `value` moved `delta` steps.
 *
 * Snapped after moving, so a value the daemon handed back that is off the
 * UI's own grid is pulled onto it by the first arrow key rather than carrying
 * its offset for the rest of the drag.
 *
 * Rounded in the direction of travel rather than to the nearest step, which
 * is what makes that pull-onto-grid land where the user aimed: from 37 on a
 * grid of tens, Up must reach 40 and Down must reach 30. Nearest-rounding
 * sends Up to 50 -- past the grid point the user was reaching for.
 */
export function stepBy(value: number, delta: number, min: number, max: number, step: number): number {
  const s = step <= 0 ? 1 : step;
  const moved = value + delta * s;
  const idx = delta >= 0 ? Math.floor((moved - min) / s) : Math.ceil((moved - min) / s);
  return clamp(min + idx * s, min, max);
}

/**
 * The next index in a list of `count`, clamped rather than wrapped.
 *
 * Same rule as `moveSelection` in `keys.ts`, and for a related reason: a menu
 * whose last item is Delete must not put Delete under the cursor because
 * someone pressed Down once too often.
 */
export function nextIndex(current: number, delta: number, count: number): number {
  if (count <= 0) return -1;
  return clamp(current + delta, 0, count - 1);
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `npx vitest run src/lib/ui.test.ts`
Expected: PASS — 6 suites, 14 tests.

- [ ] **Step 5: Replace `app.css` entirely**

Overwrite `crates/trix-ui/web/src/app.css`. The `input`/`select`/`button` element rules at the bottom of the current file are **deleted, not adapted** — every control gets a component from Task 3 onward, and leaving element rules behind would silently style the ones not yet converted into something between two designs.

```css
:root {
  /* Surfaces */
  --bg: #0b0d10;
  --surface: #101319;
  --raised: #161a21;
  --overlay: #151920;

  /* Lines. Two weights: `--line` edges a panel against the page,
     `--line-soft` separates rows inside one panel and must not compete
     with it. */
  --line: rgba(255, 255, 255, 0.07);
  --line-soft: rgba(255, 255, 255, 0.05);
  --line-strong: rgba(255, 255, 255, 0.11);
  /* --line-strong, brightened under the pointer. The two-step is what makes
     an outlined control feel like a control: a border that does not move on
     hover reads as decoration. */
  --line-hi: rgba(255, 255, 255, 0.18);

  /* Interaction. One wash for every transparent control's hover, and the
     lifted form of the two surfaces that have one. Tokens rather than
     literals because these appear across a dozen components: the first
     draft of this design used 0.05, 0.07 and 0.08 in different files for
     the same gesture, which is exactly the incoherence the overhaul is
     meant to remove. */
  --hover: rgba(255, 255, 255, 0.07);
  --raised-hi: #1d222b;
  --accent-hi: #6ea8ff;

  /* The wash behind a modal. Dark rather than tinted, because it sits over
     video as often as over a page. */
  --scrim: rgba(0, 0, 0, 0.55);

  /* Two more weights of the same wash, each with one job. --scrim-soft dims
     the trimmed-away film on the timeline and must stay readable through it;
     --scrim-strong backs 10px text on a clip card over an unpredictable video
     frame, where 0.55 does not reliably carry it. Consumed by Tasks 9 and 10. */
  --scrim-soft: rgba(0, 0, 0, 0.45);
  --scrim-strong: rgba(0, 0, 0, 0.75);

  /* Platform constants, deliberately outside the theme. --win-close is the
     Windows close-button hover red, which users expect on a title bar; it is
     NOT --danger, because closing a window is not destructive and --danger
     means exactly one thing. --video-bg is true black, used only behind video,
     where the UI's slightly blue --bg would read as a seam. */
  --win-close: #e81123;
  --video-bg: #000;

  /* Text */
  --text: #e4e8ee;
  --dim: #8b94a3;
  --faint: #5d6675;

  /* Semantics. Each of these means exactly one thing -- see the plan's
     colour contract. --accent is "you selected this", --live is "Trix is
     live", --danger is destructive or broken, --fav is kept. */
  --accent: #5b9dff;
  --accent-ink: #04122e;
  --live: #3ddc97;
  --danger: #ff5c5c;
  --fav: #f5c451;

  /* Radii */
  --r-sm: 6px;
  --r: 7px;
  --r-md: 9px;
  --r-lg: 10px;
  --r-full: 999px;

  /* Motion. Nothing in this app animates longer than --t-slow. */
  --t-fast: 120ms;
  --t: 160ms;
  --t-slow: 220ms;
  --ease: cubic-bezier(0.2, 0.8, 0.3, 1);

  /* Floating surfaces only: menus, dropdowns, modals, toasts. Elevation is
     otherwise carried by surface lightness, never by shadow. */
  --shadow: 0 14px 34px rgba(0, 0, 0, 0.6);

  color-scheme: dark;
}

* { box-sizing: border-box; }

body {
  margin: 0;
  background: var(--bg);
  color: var(--text);
  font: 13px/1.45 "Segoe UI Variable Text", "Segoe UI", system-ui, sans-serif;
  user-select: none;
  overflow: hidden;
}

/* Every readout with digits in it. Durations, byte counts, percentages,
   buffer seconds, trim points, clip counts -- digits must not jitter as
   they count. */
.tnum { font-variant-numeric: tabular-nums; }

/* The settings row: label and help on the left, control on the right.
   Global rather than scoped to a component because two components render
   this same row -- `Field.svelte` for every config-backed setting, and
   `Settings.svelte` inline for the Version row, which has no config key
   behind it to render through `Field`. Svelte scopes a component's styles to
   its own markup, so a copy in each would be these six rules maintained in
   two places. Layout, not control styling -- the controls themselves are
   still components. */
.row { display: flex; align-items: center; gap: 20px; padding: 13px 0; }
.row + .row { border-top: 1px solid var(--line-soft); }
.row .lt { flex: 1; min-width: 0; }
.row .lt b { display: block; font-weight: 500; font-size: 13px; }
.row .lt span { display: block; color: var(--dim); font-size: 11.5px; margin-top: 2px; }
.row .rt { flex: 0 0 auto; display: flex; align-items: center; gap: 8px; }

/* The app had no focus styling at all before this. `:focus-visible` rather
   than `:focus`, so clicking a control does not leave a ring behind it. */
:focus-visible {
  outline: 2px solid var(--accent);
  outline-offset: 2px;
  border-radius: inherit;
}

/* WebView2 draws a scrollbar that belongs to no part of this design. */
::-webkit-scrollbar { width: 10px; height: 10px; }
::-webkit-scrollbar-track { background: transparent; }
::-webkit-scrollbar-thumb {
  background: var(--line-strong);
  border-radius: var(--r-full);
  border: 2px solid var(--bg);
}
::-webkit-scrollbar-thumb:hover { background: var(--line-hi); }

@media (prefers-reduced-motion: reduce) {
  :root { --t-fast: 0.01ms; --t: 0.01ms; --t-slow: 0.01ms; }
  *, *::before, *::after {
    animation-duration: 0.01ms !important;
    animation-iteration-count: 1 !important;
    transition-duration: 0.01ms !important;
  }
}
```

- [ ] **Step 6: Confirm the app still builds with every control unstyled**

Run from `crates/trix-ui/web`: `npm run check && npx vitest run && npm run build`
Expected: check 0 errors / 0 warnings; vitest 104 passing (90 existing + 14 new); build succeeds.

The running app now shows native Windows controls with no styling. That is correct for this point in the branch — Tasks 3 through 11 replace them.

- [ ] **Step 7: Commit**

```bash
git add crates/trix-ui/web/src/app.css crates/trix-ui/web/src/lib/ui.ts crates/trix-ui/web/src/lib/ui.test.ts
git commit -m "feat(ui): design tokens and control maths"
```

---

## Task 2: The icon set

**Files:**
- Create: `crates/trix-ui/web/src/lib/icons.ts`
- Create: `crates/trix-ui/web/src/lib/icons.test.ts`
- Create: `crates/trix-ui/web/src/components/ui/Icon.svelte`

**Interfaces:**
- Consumes: nothing.
- Produces: `type IconName`, `ICONS: Record<IconName, IconDef>` where
  `IconDef = { d: string; filled?: true }`, and
  `Icon.svelte` with props `{ name: IconName; size?: number }`.

The project has no icon set at all today — the favourite marker is the literal character `*` and prev/next are `&lsaquo;` / `&rsaquo;` entities.

- [ ] **Step 1: Write the failing test**

Create `crates/trix-ui/web/src/lib/icons.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { ICONS, type IconName } from './icons';

/**
 * Every name the app asks for by string somewhere. A missing entry is a
 * blank square in the UI and nothing in the console, so it is checked here
 * rather than discovered on screen.
 */
const REQUIRED: IconName[] = [
  'play', 'pause', 'clips', 'settings', 'star', 'star-filled', 'pencil',
  'trash', 'folder', 'scissors', 'volume', 'volume-mute', 'fullscreen',
  'fullscreen-exit', 'chevron-left', 'chevron-right', 'chevron-down', 'dots',
  'check', 'rearm', 'minimize', 'maximize', 'restore', 'close', 'alert',
];

describe('ICONS', () => {
  it('has every icon the app names', () => {
    for (const name of REQUIRED) {
      expect(ICONS[name], `missing icon: ${name}`).toBeDefined();
    }
  });

  it('gives every icon a non-empty path', () => {
    for (const [name, def] of Object.entries(ICONS)) {
      expect(def.d.trim().length, `empty path: ${name}`).toBeGreaterThan(0);
    }
  });

  it('carries no icon the app does not ask for', () => {
    // YAGNI, enforced. An unused icon is dead weight in every bundle.
    expect(Object.keys(ICONS).sort()).toEqual([...REQUIRED].sort());
  });
});
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `npx vitest run src/lib/icons.test.ts`
Expected: FAIL — `Failed to resolve import "./icons"`.

- [ ] **Step 3: Write the implementation**

Create `crates/trix-ui/web/src/lib/icons.ts`. Every path is drawn on a
`0 0 16 16` grid, stroked, so one `stroke-width` governs the whole set.

```ts
/**
 * Icon path data, on a 16x16 grid.
 *
 * Data rather than markup so the set is testable without a DOM -- see the
 * testing rule in the plan header. `Icon.svelte` is the only consumer.
 *
 * Stroked by default; `filled: true` marks the three that are solid shapes
 * (a play triangle and a favourited star read wrong as outlines, and the
 * overflow dots have no outline to draw).
 */
export type IconDef = { d: string; filled?: true };

export type IconName =
  | 'play' | 'pause' | 'clips' | 'settings' | 'star' | 'star-filled'
  | 'pencil' | 'trash' | 'folder' | 'scissors' | 'volume' | 'volume-mute'
  | 'fullscreen' | 'fullscreen-exit' | 'chevron-left' | 'chevron-right'
  | 'chevron-down' | 'dots' | 'check' | 'rearm' | 'minimize' | 'maximize'
  | 'restore' | 'close' | 'alert';

/** Shared by the outline and filled stars, which are one shape drawn twice. */
const STAR = 'M8 2l1.8 3.9 4.2.5-3.1 2.9.8 4.2L8 11.5 4.3 13.5l.8-4.2L2 6.4l4.2-.5z';

export const ICONS: Record<IconName, IconDef> = {
  play: { d: 'M4 2.5l9 5.5-9 5.5z', filled: true },
  pause: { d: 'M5 3v10M11 3v10' },
  // Inset to x 1.5..14.5, not 0..16. A path that reaches the edge of the
  // viewBox has half its 1.35 stroke clipped off, so the frame's left and
  // right sides render visibly thinner than its top and bottom -- and this
  // is the Clips nav icon, on screen at all times.
  clips: { d: 'M3 3h10a1.5 1.5 0 011.5 1.5v7A1.5 1.5 0 0113 13H3a1.5 1.5 0 01-1.5-1.5v-7A1.5 1.5 0 013 3zM6.5 6.2v3.6l3.2-1.8z' },
  // A cog, not a sun. The first draft of this set drew rays and it read as
  // weather rather than settings.
  settings: {
    d: 'M8 5.7a2.3 2.3 0 100 4.6 2.3 2.3 0 000-4.6z'
      + 'M12.9 9.7a1 1 0 00.2 1.1l.1.1a1.15 1.15 0 11-1.6 1.6l-.1-.1a1 1 0 00-1.1-.2 1 1 0 00-.6.9v.2a1.15 1.15 0 11-2.3 0v-.1a1 1 0 00-.7-.9 1 1 0 00-1.1.2l-.1.1a1.15 1.15 0 11-1.6-1.6l.1-.1a1 1 0 00.2-1.1 1 1 0 00-.9-.6h-.2a1.15 1.15 0 110-2.3h.1a1 1 0 00.9-.7 1 1 0 00-.2-1.1l-.1-.1a1.15 1.15 0 111.6-1.6l.1.1a1 1 0 001.1.2h.1a1 1 0 00.6-.9v-.2a1.15 1.15 0 112.3 0v.1a1 1 0 00.6.9 1 1 0 001.1-.2l.1-.1a1.15 1.15 0 111.6 1.6l-.1.1a1 1 0 00-.2 1.1v.1a1 1 0 00.9.6h.2a1.15 1.15 0 110 2.3h-.1a1 1 0 00-.9.6z',
  },
  star: { d: STAR },
  'star-filled': { d: STAR, filled: true },
  pencil: { d: 'M11 2.8l2.2 2.2L6 12.2 3.2 13l.8-2.8z' },
  trash: { d: 'M3 4.5h10M6.5 4.5V3h3v1.5M4.5 4.5l.6 8.2h5.8l.6-8.2' },
  folder: { d: 'M1.8 4.2h4.4l1.2 1.4h6.8v7.2H1.8z' },
  scissors: {
    d: 'M6 4a2 2 0 11-4 0 2 2 0 014 0zM6 12a2 2 0 11-4 0 2 2 0 014 0z'
      + 'M13.3 2.7L5.4 10.6M9.65 9.65L13.3 13.3M5.4 5.4L8 8',
  },
  volume: { d: 'M3 6h2.2L8.5 3.2v9.6L5.2 10H3zM11 6.2a2.6 2.6 0 010 3.6' },
  'volume-mute': { d: 'M3 6h2.2L8.5 3.2v9.6L5.2 10H3zM11 6.5l3 3M14 6.5l-3 3' },
  fullscreen: { d: 'M2.5 5.8V2.5h3.3M13.5 5.8V2.5h-3.3M2.5 10.2v3.3h3.3M13.5 10.2v3.3h-3.3' },
  'fullscreen-exit': { d: 'M5.8 2.5v3.3H2.5M10.2 2.5v3.3h3.3M5.8 13.5v-3.3H2.5M10.2 13.5v-3.3h3.3' },
  'chevron-left': { d: 'M10 3.5L5.5 8l4.5 4.5' },
  'chevron-right': { d: 'M6 3.5L10.5 8 6 12.5' },
  'chevron-down': { d: 'M3.5 6L8 10.5 12.5 6' },
  dots: {
    d: 'M4.35 8a1.15 1.15 0 11-2.3 0 1.15 1.15 0 012.3 0z'
      + 'M9.15 8a1.15 1.15 0 11-2.3 0 1.15 1.15 0 012.3 0z'
      + 'M13.95 8a1.15 1.15 0 11-2.3 0 1.15 1.15 0 012.3 0z',
    filled: true,
  },
  check: { d: 'M3 8.4l3 3 7-7' },
  rearm: { d: 'M13.5 8a5.5 5.5 0 11-1.6-3.9M13.5 2v3h-3' },
  minimize: { d: 'M3 8h10' },
  maximize: { d: 'M3.5 3.5h9v9h-9z' },
  restore: { d: 'M5 5V3.5h7.5V11H11M3.5 5H11v7.5H3.5z' },
  close: { d: 'M3.5 3.5l9 9M12.5 3.5l-9 9' },
  alert: { d: 'M8 2.8l5.7 10H2.3zM8 6.6v3M8 11.4v.6' },
};
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `npx vitest run src/lib/icons.test.ts`
Expected: PASS — 3 tests.

- [ ] **Step 5: Create the component**

Create `crates/trix-ui/web/src/components/ui/Icon.svelte`:

```svelte
<script lang="ts">
  import { ICONS, type IconName } from '../../lib/icons';

  /**
   * Every icon in this app is decorative: it sits beside a text label, or
   * inside a control that carries its own `aria-label`. So this is
   * `aria-hidden` unconditionally and takes no title -- an icon that
   * announced itself would double up every button in the app.
   */
  let { name, size = 16 }: { name: IconName; size?: number } = $props();

  const def = $derived(ICONS[name]);
</script>

<svg
  width={size}
  height={size}
  viewBox="0 0 16 16"
  fill={def.filled ? 'currentColor' : 'none'}
  stroke={def.filled ? 'none' : 'currentColor'}
  stroke-width="1.35"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
  focusable="false">
  <path d={def.d} />
</svg>

<style>
  svg { display: block; flex: 0 0 auto; }
</style>
```

- [ ] **Step 6: Verify the gates**

Run: `npm run check && npx vitest run`
Expected: check 0 errors / 0 warnings; vitest 107 passing.

- [ ] **Step 7: Commit**

```bash
git add crates/trix-ui/web/src/lib/icons.ts crates/trix-ui/web/src/lib/icons.test.ts crates/trix-ui/web/src/components/ui/Icon.svelte
git commit -m "feat(ui): hand-drawn icon set"
```

---

## Task 3: Button, IconButton and Toggle

**Files:**
- Create: `crates/trix-ui/web/src/components/ui/Button.svelte`
- Create: `crates/trix-ui/web/src/components/ui/IconButton.svelte`
- Create: `crates/trix-ui/web/src/components/ui/Toggle.svelte`

**Interfaces:**
- Consumes: `Icon.svelte` (`{ name, size }`), tokens from Task 1.
- Produces:
  - `Button` — `{ variant?: 'primary'|'default'|'ghost'|'danger'; size?: 'sm'|'md'; icon?: IconName; disabled?: boolean; title?: string; onclick?: () => void; children }`
  - `IconButton` — `{ icon: IconName; label: string; active?: boolean; disabled?: boolean; size?: number; onclick?: () => void }` — `label` is **required**
  - `Toggle` — `{ checked: boolean; disabled?: boolean; label: string; onchange: (next: boolean) => void }`

All three render real `<button>` elements. This matters: `keys.ts`'s
`ACTIVATABLE_TAGS` already knows `BUTTON` owns Space and Enter, so nothing in
the keyboard layer needs to learn about them.

There is no unit test in this task. Per the plan header's testing rule these
files are markup and styling with no arithmetic, and the project has no DOM
test framework. `npm run check` is the gate.

- [ ] **Step 1: Create `Button.svelte`**

```svelte
<script lang="ts">
  import Icon from './Icon.svelte';
  import type { IconName } from '../../lib/icons';
  import type { Snippet } from 'svelte';

  let {
    variant = 'default',
    size = 'md',
    icon,
    disabled = false,
    title,
    onclick,
    children,
  }: {
    variant?: 'primary' | 'default' | 'ghost' | 'danger';
    size?: 'sm' | 'md';
    icon?: IconName;
    disabled?: boolean;
    title?: string;
    onclick?: () => void;
    children: Snippet;
  } = $props();
</script>

<button type="button" class="b {variant} {size}" {disabled} {title} {onclick}>
  {#if icon}<Icon name={icon} size={size === 'sm' ? 13 : 14} />{/if}
  {@render children()}
</button>

<style>
  .b {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font: inherit;
    font-size: 12px;
    border-radius: var(--r);
    border: 1px solid var(--line-strong);
    background: var(--raised);
    color: var(--text);
    cursor: pointer;
    transition: background var(--t-fast) var(--ease), border-color var(--t-fast) var(--ease),
      transform var(--t-fast) var(--ease);
  }
  .md { padding: 7px 12px; }
  .sm { padding: 5px 10px; }

  /* Every hover and press is guarded with `:not(:disabled)` rather than
     relying on a later `:disabled` rule to undo them. A browser still
     matches `:hover` on a disabled button -- disabling blocks activation,
     not pointer-over styling -- and `.b:disabled` ties on specificity with
     `.ghost:hover`, so whichever is written last wins. Guarding each one
     makes the result independent of source order. */
  .b:hover:not(:disabled) { background: var(--raised-hi); }
  .b:active:not(:disabled) { transform: translateY(1px); }
  .b:disabled { opacity: 0.4; cursor: default; }

  .primary { background: var(--accent); border-color: var(--accent); color: var(--accent-ink); font-weight: 600; }
  .primary:hover:not(:disabled) { background: var(--accent-hi); }

  .ghost { background: transparent; border-color: transparent; color: var(--dim); }
  .ghost:hover:not(:disabled) { background: var(--hover); color: var(--text); }

  .danger { background: transparent; border-color: color-mix(in srgb, var(--danger) 40%, transparent); color: var(--danger); }
  .danger:hover:not(:disabled) { background: color-mix(in srgb, var(--danger) 12%, transparent); }
</style>
```

- [ ] **Step 2: Create `IconButton.svelte`**

`label` is a required prop, not an optional one, so TypeScript refuses an
icon-only button with no accessible name at build time.

```svelte
<script lang="ts">
  import Icon from './Icon.svelte';
  import type { IconName } from '../../lib/icons';

  let {
    icon,
    label,
    active = false,
    disabled = false,
    size = 14,
    onclick,
  }: {
    icon: IconName;
    /** Required: this button has no text, so this is its only name. */
    label: string;
    active?: boolean;
    disabled?: boolean;
    size?: number;
    onclick?: () => void;
  } = $props();
</script>

<button type="button" class="ib" class:active aria-label={label} title={label} {disabled} {onclick}>
  <Icon name={icon} {size} />
</button>

<style>
  .ib {
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    padding: 0;
    border-radius: var(--r);
    border: 1px solid transparent;
    background: transparent;
    color: var(--dim);
    cursor: pointer;
    transition: background var(--t-fast) var(--ease), color var(--t-fast) var(--ease);
  }
  .ib:hover:not(:disabled) { background: var(--hover); color: var(--text); }
  .ib:active:not(:disabled) { transform: translateY(1px); }
  .ib.active { background: color-mix(in srgb, var(--accent) 15%, transparent); color: var(--text); }
  .ib:disabled { opacity: 0.35; cursor: default; }
</style>
```

- [ ] **Step 3: Create `Toggle.svelte`**

Replaces `<input type="checkbox">`. `role="switch"` on a real `<button>`, so
Space and Enter are native and no key handling is written here at all.

```svelte
<script lang="ts">
  let {
    checked,
    disabled = false,
    label,
    onchange,
  }: {
    checked: boolean;
    disabled?: boolean;
    label: string;
    onchange: (next: boolean) => void;
  } = $props();
</script>

<button
  type="button"
  class="tg"
  class:on={checked}
  role="switch"
  aria-checked={checked}
  aria-label={label}
  {disabled}
  onclick={() => onchange(!checked)}>
  <span class="knob"></span>
</button>

<style>
  .tg {
    position: relative;
    width: 38px;
    height: 21px;
    flex: 0 0 auto;
    padding: 0;
    border-radius: var(--r-full);
    border: 1px solid var(--line-strong);
    background: var(--line-strong);
    cursor: pointer;
    transition: background var(--t) var(--ease), border-color var(--t) var(--ease);
  }
  .knob {
    position: absolute;
    top: 2px;
    left: 2px;
    width: 15px;
    height: 15px;
    border-radius: 50%;
    background: var(--dim);
    transition: left var(--t) var(--ease), background var(--t) var(--ease);
  }
  .tg.on { background: var(--accent); border-color: var(--accent); }
  .tg.on .knob { left: 20px; background: var(--text); }
  .tg:disabled { opacity: 0.4; cursor: default; }
</style>
```

- [ ] **Step 4: Verify the gates**

Run: `npm run check && npx vitest run && npm run build`
Expected: check 0 errors / 0 warnings; vitest 107 passing; build succeeds.

- [ ] **Step 5: Commit**

```bash
git add crates/trix-ui/web/src/components/ui/Button.svelte crates/trix-ui/web/src/components/ui/IconButton.svelte crates/trix-ui/web/src/components/ui/Toggle.svelte
git commit -m "feat(ui): button, icon button and toggle primitives"
```

---

## Task 4: Menu and Modal

**Files:**
- Create: `crates/trix-ui/web/src/components/ui/Menu.svelte`
- Create: `crates/trix-ui/web/src/components/ui/Modal.svelte`

**Interfaces:**
- Consumes: `Icon.svelte`, `nextIndex` from `lib/ui.ts`, tokens.
- Produces:
  - `Menu` — `{ items: MenuItem[]; onpick: (id: string) => void; onclose: () => void }`
    where `MenuItem = { id: string; label: string; icon?: IconName; danger?: true; separatorBefore?: true }`.
    The caller owns whether the menu is mounted; this component only draws it
    and reports.
  - `Modal` — `{ title: string; onclose: () => void; children: Snippet; actions: Snippet }`

- [ ] **Step 1: Create `Menu.svelte`**

```svelte
<script module lang="ts">
  /** Instance counter -- see `uid` below. */
  let nextMenuId = 0;
</script>

<script lang="ts">
  import Icon from './Icon.svelte';
  import { nextIndex } from '../../lib/ui';
  import type { IconName } from '../../lib/icons';

  export type MenuItem = {
    id: string;
    label: string;
    icon?: IconName;
    danger?: true;
    separatorBefore?: true;
  };



  let {
    items,
    onpick,
    onclose,
  }: {
    items: MenuItem[];
    onpick: (id: string) => void;
    onclose: () => void;
  } = $props();

  let active = $state(0);
  let el = $state<HTMLDivElement | null>(null);

  /**
   * A prefix unique to this menu instance, for the item ids that
   * `aria-activedescendant` points at.
   *
   * Module-scoped counter rather than a random id: it is deterministic, costs
   * nothing, and two menus can be mounted at once during the frame where one
   * card's menu is closing as another opens. Colliding ids there would leave
   * `aria-activedescendant` naming an element in the wrong menu.
   */
  const uid = `menu-${nextMenuId++}`;

  // Focus lands on the menu itself, not on an item: one roving `active`
  // index is simpler than moving DOM focus between items, and it keeps
  // every key press arriving at one handler.
  $effect(() => {
    el?.focus();
  });

  // Pointerdown, not click: a click that started inside the menu and ended
  // outside it would otherwise close the menu before the item fired.
  $effect(() => {
    const away = (e: PointerEvent) => {
      if (el && e.target instanceof Node && !el.contains(e.target)) onclose();
    };
    document.addEventListener('pointerdown', away, true);
    return () => document.removeEventListener('pointerdown', away, true);
  });

  function onkeydown(e: KeyboardEvent) {
    // Every key this menu understands is stopped here. Without this, Escape
    // would also reach `ClipPage`'s window handler and navigate to the grid
    // while merely closing the menu, and the arrows would move the grid
    // selection underneath the open menu.
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      onclose();
    } else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      e.stopPropagation();
      active = nextIndex(active, e.key === 'ArrowDown' ? 1 : -1, items.length);
    } else if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      e.stopPropagation();
      const item = items[active];
      if (item) onpick(item.id);
    } else if (e.key === 'Tab') {
      // Not prevented: Tab should carry on to the next real control. But the
      // items are `tabindex="-1"`, so focus leaves this subtree entirely --
      // and the key handler lives on `el`, so once focus is gone Escape can
      // no longer reach it. A menu left open behind a focus that has moved on
      // would be undismissable from the keyboard.
      onclose();
    }
  }
</script>

<div
  bind:this={el}
  class="menu"
  role="menu"
  tabindex="-1"
  aria-orientation="vertical"
  aria-activedescendant={items[active] ? `${uid}-${items[active].id}` : undefined}
  {onkeydown}>
  {#each items as item, i (item.id)}
    {#if item.separatorBefore}<hr />{/if}
    <!-- Focus stays on the container and `active` is a visual index, so
         without `aria-activedescendant` naming this id a screen reader would
         announce nothing as the arrows move. `role="menu"` promises one or
         the other; this is the half that does not fight the pointer. -->
    <button
      type="button"
      id="{uid}-{item.id}"
      class="item"
      class:danger={item.danger}
      class:active={i === active}
      role="menuitem"
      tabindex="-1"
      onpointerenter={() => (active = i)}
      onclick={() => onpick(item.id)}>
      {#if item.icon}<Icon name={item.icon} size={14} />{/if}
      {item.label}
    </button>
  {/each}
</div>

<style>
  .menu {
    position: absolute;
    right: 0;
    top: calc(100% + 4px);
    z-index: 30;
    min-width: 158px;
    padding: 5px;
    display: grid;
    gap: 1px;
    background: var(--overlay);
    border: 1px solid var(--line-strong);
    border-radius: var(--r-md);
    box-shadow: var(--shadow);
  }
  .menu:focus-visible { outline: none; }
  .item {
    display: flex;
    align-items: center;
    gap: 9px;
    width: 100%;
    padding: 7px 9px;
    border: 0;
    border-radius: var(--r-sm);
    background: transparent;
    color: var(--text);
    font: inherit;
    font-size: 12.5px;
    text-align: left;
    cursor: pointer;
  }
  .item.active { background: var(--hover); }
  .item.danger { color: var(--danger); }
  .item.danger.active { background: color-mix(in srgb, var(--danger) 13%, transparent); }
  hr { border: 0; border-top: 1px solid var(--line); margin: 4px 2px; }
</style>
```

- [ ] **Step 2: Create `Modal.svelte`**

```svelte
<script module lang="ts">
  /** Instance counter -- see `uid` below. */
  let nextModalId = 0;
</script>

<script lang="ts">
  import type { Snippet } from 'svelte';

  let {
    title,
    onclose,
    children,
    actions,
  }: {
    title: string;
    onclose: () => void;
    children: Snippet;
    actions: Snippet;
  } = $props();

  let panel = $state<HTMLDivElement | null>(null);
  /** Whatever had focus before the dialog opened, so it can be given back. */
  let opener: Element | null = null;
  /** Instance-unique, so `aria-labelledby` names this dialog's own heading. */
  const uid = `modal-${nextModalId++}`;

  $effect(() => {
    opener = document.activeElement;
    panel?.focus();
    return () => {
      if (opener instanceof HTMLElement) opener.focus();
    };
  });

  /**
   * Focus trap. The old inline confirm strip achieved the same thing by
   * putting `disabled={confirmingDelete}` on every other button on the page;
   * a trap enforces it structurally instead, so a control added later cannot
   * forget to opt in.
   */
  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      onclose();
      return;
    }
    if (e.key !== 'Tab' || !panel) return;
    // `select` and `textarea` are in the list even though today's only
    // consumer is a pair of buttons. Leaving them out does not merely strand
    // focus: the wrap is triggered by comparing `activeElement` against the
    // first and last of *this* list, so focus sitting on an unlisted control
    // matches neither, `preventDefault` never runs, and the browser's own Tab
    // walks straight out of the dialog into the page behind it.
    const focusable = panel.querySelectorAll<HTMLElement>(
      'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]),'
        + ' textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
    );
    if (focusable.length === 0) return;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (e.shiftKey && document.activeElement === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      first.focus();
    }
  }
</script>

<!--
  The keydown handler is bound to the panel, NOT to `<svelte:window>`.

  `ClipPage` and `Grid` each already mount their own `<svelte:window
  onkeydown>`. Two listeners on the *same* window are siblings, and
  `stopPropagation` does not stop a sibling on the same node -- only
  `stopImmediatePropagation` does. So a window-bound modal handler would
  double-fire with the page underneath it, and Escape would both close the
  dialog and run the page's shortcut. Bound to the panel, the event stops
  where it is handled and never reaches window at all -- which is exactly why
  `Menu` binds to its own element too. Focus is trapped inside the panel, so
  there is no keydown outside it to miss.
-->
<div class="scrim">
  <div
    bind:this={panel}
    class="panel"
    role="dialog"
    aria-modal="true"
    aria-labelledby="{uid}-title"
    tabindex="-1"
    {onkeydown}>
    <h2 id="{uid}-title">{title}</h2>
    <div class="body">{@render children()}</div>
    <div class="actions">{@render actions()}</div>
  </div>
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    z-index: 40;
    display: grid;
    place-items: center;
    background: var(--scrim);
    animation: fade var(--t-slow) var(--ease);
  }
  .panel {
    width: min(420px, calc(100vw - 48px));
    padding: 18px 20px 16px;
    background: var(--overlay);
    border: 1px solid var(--line-strong);
    border-radius: var(--r-lg);
    box-shadow: var(--shadow);
    animation: rise var(--t-slow) var(--ease);
  }
  .panel:focus-visible { outline: none; }
  h2 { margin: 0 0 8px; font-size: 15px; font-weight: 650; }
  .body { color: var(--dim); font-size: 12.5px; }
  .actions { display: flex; justify-content: flex-end; gap: 8px; margin-top: 18px; }
  @keyframes fade { from { opacity: 0; } }
  @keyframes rise { from { opacity: 0; transform: translateY(8px); } }
</style>
```

- [ ] **Step 3: Verify the gates**

Run: `npm run check && npx vitest run && npm run build`
Expected: check 0 errors / 0 warnings; vitest 107 passing; build succeeds.

- [ ] **Step 4: Commit**

```bash
git add crates/trix-ui/web/src/components/ui/Menu.svelte crates/trix-ui/web/src/components/ui/Modal.svelte
git commit -m "feat(ui): menu popover and focus-trapped modal"
```

---

## Task 5: Slider and Stepper

**Files:**
- Create: `crates/trix-ui/web/src/components/ui/Slider.svelte`
- Create: `crates/trix-ui/web/src/components/ui/Stepper.svelte`

**Interfaces:**
- Consumes: `clamp`, `ratioToValue`, `valueToRatio`, `stepBy` from `lib/ui.ts`. No icons: the stepper's buttons are the typographic `&minus;` and `+`, which are symbols rather than the arrow glyphs this overhaul is removing.
- Produces:
  - `Slider` — `{ value: number; min: number; max: number; step?: number; label: string; disabled?: boolean; oninput?: (v: number) => void; onchange: (v: number) => void }`
  - `Stepper` — `{ value: number; min: number; max: number; step?: number; unit?: string; label: string; onchange: (v: number) => void }`
  - `resolveStepperInput(raw, current, min, max)` in `lib/ui.ts`, added by Step 2. Task 6's `Select` does not need it; nothing else does yet.

**The `Slider` event contract is load-bearing and must be exactly this:**
`oninput` fires continuously while dragging or on every arrow key; `onchange`
fires **once**, on pointer release or on the key press that ended the change.
`Field.svelte` today tells the daemon on `change` and not on `input`, so a
drag across the track is one round trip rather than eighty, and shows the
in-flight value from its own local `dragging` state meanwhile. Task 7 keeps
that mechanism, and it only works if these two events mean what they say here.

- [ ] **Step 1: Create `Slider.svelte`**

```svelte
<script lang="ts">
  import { clamp, ratioToValue, stepBy, valueToRatio } from '../../lib/ui';

  let {
    value,
    min,
    max,
    step = 1,
    label,
    disabled = false,
    oninput,
    onchange,
  }: {
    value: number;
    min: number;
    max: number;
    step?: number;
    label: string;
    disabled?: boolean;
    /** Fires continuously during a drag. Display only -- do not call the daemon here. */
    oninput?: (v: number) => void;
    /** Fires once, when the change is finished. This is the one to act on. */
    onchange: (v: number) => void;
  } = $props();

  let track = $state<HTMLDivElement | null>(null);
  let dragging = $state(false);

  const ratio = $derived(valueToRatio(value, min, max));
  const pct = $derived(`${ratio * 100}%`);

  function valueAt(clientX: number): number {
    if (!track) return value;
    const box = track.getBoundingClientRect();
    return ratioToValue((clientX - box.left) / box.width, min, max, step);
  }

  function onpointerdown(e: PointerEvent) {
    if (disabled) return;
    // Capture on the track, so a drag that leaves the element -- which is
    // most of them, the control is 4px tall -- keeps delivering moves here
    // instead of to whatever is underneath the pointer.
    e.preventDefault();
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    dragging = true;
    oninput?.(valueAt(e.clientX));
  }

  function onpointermove(e: PointerEvent) {
    if (!dragging) return;
    oninput?.(valueAt(e.clientX));
  }

  function onpointerup(e: PointerEvent) {
    if (!dragging) return;
    dragging = false;
    (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    onchange(valueAt(e.clientX));
  }

  function onkeydown(e: KeyboardEvent) {
    if (disabled) return;
    const delta =
      e.key === 'ArrowRight' || e.key === 'ArrowUp' ? 1
      : e.key === 'ArrowLeft' || e.key === 'ArrowDown' ? -1
      : e.key === 'PageUp' ? 10
      : e.key === 'PageDown' ? -10
      : 0;
    let next: number;
    if (delta !== 0) next = stepBy(value, delta, min, max, step);
    else if (e.key === 'Home') next = min;
    else if (e.key === 'End') next = max;
    else return;
    e.preventDefault();
    // A key press is a whole change on its own -- there is no release to
    // wait for -- so both events fire, in the order a drag would produce.
    oninput?.(next);
    onchange(next);
  }
</script>

<div
  bind:this={track}
  class="sl"
  class:disabled
  role="slider"
  tabindex={disabled ? -1 : 0}
  aria-label={label}
  aria-valuemin={min}
  aria-valuemax={max}
  aria-valuenow={clamp(value, min, max)}
  aria-disabled={disabled}
  {onpointerdown}
  {onpointermove}
  {onpointerup}
  {onkeydown}>
  <span class="track"></span>
  <span class="fill" style="width: {pct}"></span>
  <span class="thumb" style="left: {pct}"></span>
</div>

<style>
  .sl {
    position: relative;
    width: 186px;
    height: 18px;
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    cursor: pointer;
    touch-action: none;
  }
  .sl.disabled { opacity: 0.4; cursor: default; }
  .track {
    position: absolute;
    left: 0;
    right: 0;
    height: 4px;
    border-radius: 2px;
    background: var(--line-strong);
  }
  .fill { position: absolute; left: 0; height: 4px; border-radius: 2px; background: var(--accent); }
  .thumb {
    position: absolute;
    width: 15px;
    height: 15px;
    border-radius: 50%;
    background: var(--accent);
    border: 2px solid var(--text);
    transform: translateX(-7.5px);
    transition: box-shadow var(--t-fast) var(--ease);
  }
  .sl:hover .thumb { box-shadow: 0 0 0 4px color-mix(in srgb, var(--accent) 20%, transparent); }
</style>
```

- [ ] **Step 2: Add `resolveStepperInput` to `lib/ui.ts`, test first**

The Stepper's text box has to decide what a committed string means, and that
decision is arithmetic, not event wiring -- so it lives in `lib/ui.ts` where
vitest can reach it. The trap it exists to close: `Number('')` is `0`, not
`NaN`, so a naive `Number.isFinite` test reads a cleared box as "the user
typed zero" and silently commits 0 instead of putting the previous value
back. `Number('   ')` does the same.

Add to `crates/trix-ui/web/src/lib/ui.test.ts`:

```ts
describe('resolveStepperInput', () => {
  it('returns the current value for an empty box rather than treating it as zero', () => {
    // Number('') is 0, not NaN -- an empty box must be caught before that
    // coercion runs, or clearing the box would commit zero.
    expect(resolveStepperInput('', 42, 0, 100)).toBe(42);
  });

  it('returns the current value for a whitespace-only box', () => {
    // Number('   ') is also 0, for the same reason.
    expect(resolveStepperInput('   ', 42, 0, 100)).toBe(42);
  });

  it('returns the current value for unparseable text', () => {
    expect(resolveStepperInput('abc', 42, 0, 100)).toBe(42);
  });

  it('returns a plain in-range number as-is', () => {
    expect(resolveStepperInput('55', 42, 0, 100)).toBe(55);
  });

  it('clamps a number above max down to max', () => {
    expect(resolveStepperInput('150', 42, 0, 100)).toBe(100);
  });

  it('clamps a number below min up to min', () => {
    expect(resolveStepperInput('-10', 42, 0, 100)).toBe(0);
  });
});
```

Run them and confirm they fail because the function does not exist yet. Then
add to `crates/trix-ui/web/src/lib/ui.ts`:

```ts
/**
 * What a Stepper's text box should commit as: `raw` parsed and clamped, or
 * `current` if `raw` cannot be read as a number.
 *
 * The empty string gets its own check before `Number()` ever runs. `Number('')`
 * -- and `Number('   ')` -- coerce to `0`, not `NaN`, so a naive
 * `Number.isFinite` test would read a cleared box as "the user typed zero"
 * instead of "the user typed nothing," silently committing 0 rather than
 * putting the previous value back.
 */
export function resolveStepperInput(raw: string, current: number, min: number, max: number): number {
  const trimmed = raw.trim();
  if (trimmed === '') return current;
  const parsed = Number(trimmed);
  return Number.isFinite(parsed) ? clamp(parsed, min, max) : current;
}
```

Run them again and confirm all six pass. Clamping only -- no step snapping
here; the arrow keys and the +/- buttons own that, through `stepBy`.

- [ ] **Step 3: Create `Stepper.svelte`**

Replaces `<input type="number">` and its native spinner arrows. Typing is
still allowed; the unit is rendered after the value so it no longer lives
only in the help text.

```svelte
<script lang="ts">
  import { resolveStepperInput, stepBy } from '../../lib/ui';

  let {
    value,
    min,
    max,
    step = 1,
    unit,
    label,
    onchange,
  }: {
    value: number;
    min: number;
    max: number;
    step?: number;
    unit?: string;
    label: string;
    onchange: (v: number) => void;
  } = $props();

  function commit(el: HTMLInputElement) {
    const resolved = resolveStepperInput(el.value, value, min, max);
    // The box takes `value` as a one-way attribute, so if the resolved value
    // matches what's already in effect, Svelte has nothing to re-render and
    // the box would otherwise keep showing whatever the user typed -- most
    // visibly a rejected (unparseable) edit that fell back to the old value.
    // Writing it back onto the element directly makes the box always show
    // what actually took effect.
    el.value = String(resolved);
    // An unresolved or unchanged edit doesn't reach the daemon at all.
    if (resolved !== value) onchange(resolved);
  }

  function onkeydown(e: KeyboardEvent) {
    const delta = e.key === 'ArrowUp' ? 1 : e.key === 'ArrowDown' ? -1 : 0;
    if (delta !== 0) {
      e.preventDefault();
      onchange(stepBy(value, delta, min, max, step));
    } else if (e.key === 'Home') {
      e.preventDefault();
      onchange(min);
    } else if (e.key === 'End') {
      e.preventDefault();
      onchange(max);
    } else if (e.key === 'Enter') {
      commit(e.currentTarget as HTMLInputElement);
    }
  }
</script>

<div class="st">
  <button
    class="pm"
    aria-label="{label}: decrease"
    disabled={value <= min}
    onclick={() => onchange(stepBy(value, -1, min, max, step))}>&minus;</button>
  <input
    class="v tnum"
    type="text"
    inputmode="numeric"
    aria-label={label}
    {value}
    onchange={(e) => commit(e.currentTarget)}
    {onkeydown} />
  {#if unit}<span class="u">{unit}</span>{/if}
  <button
    class="pm"
    aria-label="{label}: increase"
    disabled={value >= max}
    onclick={() => onchange(stepBy(value, 1, min, max, step))}>+</button>
</div>

<style>
  .st {
    display: flex;
    align-items: center;
    background: var(--bg);
    border: 1px solid var(--line-strong);
    border-radius: var(--r);
    overflow: hidden;
  }
  .pm {
    width: 28px;
    height: 30px;
    flex: 0 0 auto;
    border: 0;
    background: transparent;
    color: var(--dim);
    font: inherit;
    font-size: 14px;
    cursor: pointer;
  }
  .pm:hover { background: var(--hover); color: var(--text); }
  .pm:disabled { opacity: 0.3; cursor: default; background: transparent; }
  .v {
    width: 58px;
    padding: 0;
    border: 0;
    background: transparent;
    color: var(--text);
    font: inherit;
    font-size: 12.5px;
    text-align: center;
  }
  .v:focus-visible { outline: none; }
  .st:focus-within { border-color: var(--accent); box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 18%, transparent); }
  .u { color: var(--faint); font-size: 11px; padding-right: 8px; }
</style>
```

- [ ] **Step 4: Verify the gates**

Run: `npm run check && npx vitest run && npm run build`
Expected: check 0 errors / 0 warnings; vitest 113 passing (107 + 6 new); build succeeds.

- [ ] **Step 5: Commit**

```bash
git add crates/trix-ui/web/src/components/ui/Slider.svelte crates/trix-ui/web/src/components/ui/Stepper.svelte
git commit -m "feat(ui): slider and stepper primitives"
```

---

## Task 6: Select

**Files:**
- Create: `crates/trix-ui/web/src/components/ui/Select.svelte`

**Interfaces:**
- Consumes: `nextIndex` from `lib/ui.ts`, `Icon.svelte`.
- Produces: `Select` — `{ value: string; options: { value: string; label: string }[]; label: string; onchange: (value: string) => void }`

Replaces `<select>`, whose native Windows dropdown is the single most
off-theme widget in Settings.

- [ ] **Step 1: Create `Select.svelte`**

```svelte
<script module lang="ts">
  /** Instance counter -- see `uid` below, same reasoning as Menu.svelte. */
  let nextSelectId = 0;
</script>

<script lang="ts">
  import Icon from './Icon.svelte';
  import { nextIndex } from '../../lib/ui';

  let {
    value,
    options,
    label,
    onchange,
  }: {
    value: string;
    options: { value: string; label: string }[];
    label: string;
    onchange: (value: string) => void;
  } = $props();

  let open = $state(false);
  let active = $state(0);
  let root = $state<HTMLDivElement | null>(null);
  let trigger = $state<HTMLButtonElement | null>(null);

  /**
   * A prefix unique to this instance, for the option ids `aria-activedescendant`
   * points at. Index-based rather than keyed on `option.value` -- a config
   * value is not guaranteed to be a valid id token, and the index is unique
   * regardless. Module-scoped counter for the same reason as Menu.svelte's
   * `uid`: deterministic, free, and safe if two selects are ever mounted in
   * the same frame.
   */
  const uid = `select-${nextSelectId++}`;

  const selected = $derived(options.find((o) => o.value === value));

  function show() {
    // Open on the current value, not on the top of the list, so the first
    // arrow key moves away from where the user already is.
    active = Math.max(0, options.findIndex((o) => o.value === value));
    open = true;
  }

  /** Close and give focus back to the trigger -- a Tab from a closed
      dropdown must continue from the control, not from the top of the page. */
  function dismiss() {
    open = false;
    trigger?.focus();
  }

  function pick(index: number) {
    const option = options[index];
    // Re-picking the option already showing is not a change, and every
    // consumer's `onchange` here is a daemon round trip that rewrites
    // config.toml. Guarded in the primitive rather than in each caller, so
    // `Stepper` and `Select` agree about what a no-op means.
    if (option && option.value !== value) onchange(option.value);
    dismiss();
  }

  $effect(() => {
    if (!open) return;
    const away = (e: PointerEvent) => {
      if (root && e.target instanceof Node && !root.contains(e.target)) open = false;
    };
    document.addEventListener('pointerdown', away, true);
    return () => document.removeEventListener('pointerdown', away, true);
  });

  function onkeydown(e: KeyboardEvent) {
    if (!open) {
      if (e.key === 'Enter' || e.key === ' ' || e.key === 'ArrowDown') {
        e.preventDefault();
        show();
      }
      return;
    }
    // Stopped as well as prevented: Settings mounts no window-level key
    // handler today, but a dropdown that let Escape through would close
    // itself and navigate on any page that adds one later.
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      dismiss();
    } else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      e.stopPropagation();
      active = nextIndex(active, e.key === 'ArrowDown' ? 1 : -1, options.length);
    } else if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      e.stopPropagation();
      pick(active);
    } else if (e.key === 'Home' || e.key === 'End') {
      e.preventDefault();
      e.stopPropagation();
      active = e.key === 'Home' ? 0 : options.length - 1;
    } else if (e.key === 'Tab') {
      // Tab moves on rather than being trapped, but the list must not be
      // left hanging open over the control the user just moved to.
      open = false;
    }
  }
</script>

<div bind:this={root} class="wrap">
  <button
    bind:this={trigger}
    type="button"
    class="trigger"
    class:open
    role="combobox"
    aria-expanded={open}
    aria-haspopup="listbox"
    aria-controls={open ? `${uid}-listbox` : undefined}
    aria-activedescendant={open && options[active] ? `${uid}-${active}` : undefined}
    aria-label={label}
    onclick={() => (open ? dismiss() : show())}
    {onkeydown}>
    <span class="txt">{selected?.label ?? ''}</span>
    <Icon name="chevron-down" size={11} />
  </button>

  {#if open}
    <div id="{uid}-listbox" class="pop" role="listbox" aria-label={label}>
      {#each options as option, i (option.value)}
        <button
          type="button"
          id="{uid}-{i}"
          class="opt"
          class:active={i === active}
          class:on={option.value === value}
          role="option"
          tabindex="-1"
          aria-selected={option.value === value}
          onpointerenter={() => (active = i)}
          onclick={() => pick(i)}>
          <span class="txt">{option.label}</span>
          {#if option.value === value}<Icon name="check" size={12} />{/if}
        </button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .wrap { position: relative; flex: 0 0 auto; }
  .trigger {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    width: 100%;
    min-width: 212px;
    padding: 7px 10px;
    background: var(--bg);
    border: 1px solid var(--line-strong);
    border-radius: var(--r);
    color: var(--text);
    font: inherit;
    font-size: 12.5px;
    text-align: left;
    cursor: pointer;
    transition: border-color var(--t-fast) var(--ease);
  }
  .trigger:hover { border-color: var(--line-hi); }
  .trigger.open { border-color: var(--accent); box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 18%, transparent); }
  .trigger :global(svg) { color: var(--faint); }
  .txt { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .pop {
    position: absolute;
    right: 0;
    top: calc(100% + 4px);
    z-index: 30;
    width: 100%;
    padding: 5px;
    display: grid;
    gap: 1px;
    background: var(--overlay);
    border: 1px solid var(--line-strong);
    border-radius: var(--r-md);
    box-shadow: var(--shadow);
  }
  .opt {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 7px 9px;
    border: 0;
    border-radius: var(--r-sm);
    background: transparent;
    color: var(--text);
    font: inherit;
    font-size: 12.5px;
    text-align: left;
    cursor: pointer;
  }
  .opt .txt { flex: 1; }
  .opt.active { background: var(--hover); }
  .opt.on { color: var(--text); }
  .opt.on :global(svg) { color: var(--accent); }
</style>
```

- [ ] **Step 2: Verify the gates**

Run: `npm run check && npx vitest run && npm run build`
Expected: check 0 errors / 0 warnings; vitest 113 passing; build succeeds.

- [ ] **Step 3: Commit**

```bash
git add crates/trix-ui/web/src/components/ui/Select.svelte
git commit -m "feat(ui): drawn select with keyboard listbox"
```

---

## Task 7: Settings

**Files:**
- Create: `crates/trix-ui/web/src/components/ui/KeycapInput.svelte`
- Modify: `crates/trix-ui/web/src/components/Field.svelte` (rewrite, 143 lines)
- Modify: `crates/trix-ui/web/src/views/Settings.svelte` (markup and styles; the `<script>` is preserved)

**Interfaces:**
- Consumes: `Toggle`, `Slider`, `Stepper`, `Select`, `Button`, `Icon`.
- Produces: nothing later tasks depend on.

**Preserve verbatim — do not rewrite, re-derive or "tidy":**

1. `Settings.svelte`'s entire `<script>` block: `load`, `set`, `pickFolder`,
   `pickSound`, `testSound`, `saveHotkey`, `captureHotkey`, `checkNow`, the
   `onDaemonEvent` subscription and its `onDestroy` unlisten. `captureHotkey`'s
   separation of `ctrlKey` from `altKey` exists because Windows reports AltGr
   as Ctrl+Alt on this machine's Turkish Q layout.
2. `Field.svelte`'s `dragging` state, its `shown` derivation, and the
   `$effect` that clears `dragging` when the authoritative value lands. The
   daemon is told on `change`, never on `input`.
3. Every `help` string, and the seven `field.kind` branches. Kinds are
   `bool | select | number | slider | folder | sound | hotkey` — `text` is
   declared in the type but no field uses it, so it needs no branch, exactly as
   today.
4. The `clip_dir_resolved` reading in the `folder` branch and the
   `clip_sound`-gated `disabled` bindings in the `sound` branch.

- [ ] **Step 1: Create `KeycapInput.svelte`**

```svelte
<script lang="ts">
  let {
    combo,
    capturing,
    oncapture,
    onstart,
  }: {
    /** The combination to show, e.g. `alt+f10`. */
    combo: string;
    capturing: boolean;
    oncapture: (e: KeyboardEvent) => void;
    onstart: () => void;
  } = $props();

  // `alt+f10` -> ['Alt', 'F10']. Single characters upper-case ('e' -> 'E');
  // named keys title-case ('f10' -> 'F10', 'ctrl' -> 'Ctrl').
  const caps = $derived(
    combo
      .split('+')
      .filter((p) => p.length > 0)
      .map((p) => (p.length === 1 ? p.toUpperCase() : p[0].toUpperCase() + p.slice(1))),
  );
</script>

<button
  class="hk"
  class:capturing
  aria-label="Clip hotkey"
  onclick={onstart}
  onkeydown={(e) => { if (capturing) oncapture(e); }}>
  {#if caps.length === 0}
    <span class="ask">click, then press a combination</span>
  {:else}
    {#each caps as cap, i (i)}
      {#if i > 0}<span class="plus">+</span>{/if}
      <kbd>{cap}</kbd>
    {/each}
  {/if}
</button>

<style>
  .hk {
    display: flex;
    align-items: center;
    gap: 5px;
    min-width: 172px;
    padding: 6px 10px;
    background: var(--bg);
    border: 1px solid var(--line-strong);
    border-radius: var(--r);
    color: var(--text);
    font: inherit;
    cursor: pointer;
  }
  .hk.capturing { border-color: var(--accent); box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 18%, transparent); }
  kbd {
    padding: 2px 7px;
    border-radius: var(--r-sm);
    background: var(--raised);
    border: 1px solid var(--line-strong);
    border-bottom-width: 2px;
    font: inherit;
    font-size: 11px;
    font-weight: 600;
  }
  .plus { color: var(--faint); font-size: 11px; }
  .ask { color: var(--faint); font-size: 12px; }
</style>
```

- [ ] **Step 2: Rewrite `Field.svelte`**

The row goes from a `180px 1fr` grid with help under the control, to a flex
row with label and help on the left and the control right-aligned. Keep the
file's existing doc comments about `dragging` — they explain a real
round-trip decision.

```svelte
<script lang="ts">
  import type { Field } from '../lib/settings';
  import type { Monitor } from '../lib/types';
  import Button from './ui/Button.svelte';
  import KeycapInput from './ui/KeycapInput.svelte';
  import Select from './ui/Select.svelte';
  import Slider from './ui/Slider.svelte';
  import Stepper from './ui/Stepper.svelte';
  import Toggle from './ui/Toggle.svelte';

  /**
   * One row of the settings page: a label with its help text on the left, and
   * the control for `field.kind` on the right.
   *
   * Owns one piece of state, `dragging`, documented below. `capture`/
   * `listening`/`heard` live in `Settings.svelte` because the daemon's
   * `hotkey_pressed`/`hotkey_rebound` events (wired up there, not here) write
   * to them directly; this component only renders what they say and reports
   * user actions back up through the `on*` callbacks.
   */
  let {
    field, config, monitors, capture, listening, heard,
    onset, oncapture, onstartcapture, onsavehotkey, ontogglelisten,
    onpickfolder, onpicksound, ontestsound,
  }: {
    field: Field;
    config: Record<string, unknown>;
    monitors: Monitor[];
    capture: string | null;
    listening: boolean;
    heard: boolean;
    onset: (key: string, value: unknown) => void;
    oncapture: (e: KeyboardEvent) => void;
    onstartcapture: () => void;
    onsavehotkey: () => void;
    ontogglelisten: () => void;
    onpickfolder: () => void;
    onpicksound: () => void;
    ontestsound: () => void;
  } = $props();

  /**
   * The value under the user's thumb, shown while dragging.
   *
   * Display-only: the daemon is told on `change` (thumb released), not on
   * `input`, so a drag across the track is one round trip rather than eighty.
   * Without it the percentage beside the slider would sit at the old value for
   * the whole drag, which reads as a broken control.
   */
  let dragging = $state<number | null>(null);
  const shown = $derived(dragging ?? Number(config[field.key] ?? 0));

  // Clear the drag override whenever the authoritative value lands -- whether
  // that is the daemon accepting the change or the parent reloading the config
  // after refusing it. Without this a refused change would leave the slider
  // showing a value the daemon rejected.
  $effect(() => {
    void config[field.key];
    dragging = null;
  });

  const monitorOptions = $derived(
    monitors.map((m) => ({
      value: String(m.index),
      label: `${m.name} - ${m.width}x${m.height} (${m.adapter})`,
    })),
  );

  /** The unit shown inside the stepper, so it is no longer only in the help text. */
  const UNITS: Record<string, string> = {
    fps: 'fps',
    replay_seconds: 's',
    bitrate_kbps: 'kbps',
    max_bitrate_kbps: 'kbps',
    max_library_gb: 'GB',
    stats_seconds: 's',
  };
</script>

<div class="row">
  <div class="lt">
    <b>{field.label}</b>
    <span>{field.help}</span>
  </div>

  <div class="rt">
    {#if field.kind === 'bool'}
      <Toggle
        checked={Boolean(config[field.key])}
        label={field.label}
        onchange={(next) => onset(field.key, next)} />

    {:else if field.kind === 'select' && field.dynamic === 'monitors'}
      <Select
        value={String(config[field.key] ?? 0)}
        options={monitorOptions}
        label={field.label}
        onchange={(v) => onset(field.key, Number(v))} />

    {:else if field.kind === 'select'}
      <Select
        value={String(config[field.key] ?? '')}
        options={field.options ?? []}
        label={field.label}
        onchange={(v) => onset(field.key, v)} />

    {:else if field.kind === 'number'}
      <Stepper
        value={Number(config[field.key] ?? 0)}
        min={field.min ?? 0}
        max={field.max ?? 0}
        unit={UNITS[field.key]}
        label={field.label}
        onchange={(v) => onset(field.key, v)} />

    {:else if field.kind === 'slider'}
      <Slider
        value={shown}
        min={field.min ?? 0}
        max={field.max ?? 100}
        label={field.label}
        oninput={(v) => (dragging = v)}
        onchange={(v) => onset(field.key, v)} />
      <span class="pct tnum">{shown}%</span>

    {:else if field.kind === 'folder'}
      <!-- `clip_dir_resolved`, not `clip_dir`: the config key is empty by
           default and an empty box tells the user nothing about where their
           clips actually are. Read-only rather than editable because the
           daemon owns the picker, and a typed path that does not exist is a
           refusal the user has to decode. Reset writes `clip_dir` itself (the
           empty string), because "back to the default" is a value the picker
           cannot express. -->
      <span class="path">{String(config['clip_dir_resolved'] ?? '')}</span>
      <Button size="sm" onclick={onpickfolder}>Choose...</Button>
      <Button size="sm" variant="ghost" disabled={!config[field.key]}
        onclick={() => onset(field.key, '')}>Reset</Button>

    {:else if field.kind === 'sound'}
      <span class="path" class:off={!config['clip_sound']}>
        {String(config[field.key] ?? '') || "Trix's built-in sound"}
      </span>
      <Button size="sm" disabled={!config['clip_sound']} onclick={onpicksound}>Choose...</Button>
      <Button size="sm" variant="ghost" disabled={!config['clip_sound']} onclick={ontestsound}>Test</Button>
      <Button size="sm" variant="ghost" disabled={!config['clip_sound'] || !config[field.key]}
        onclick={() => onset(field.key, '')}>Reset</Button>

    {:else if field.kind === 'hotkey'}
      <KeycapInput
        combo={capture ?? String(config[field.key] ?? '')}
        capturing={capture !== null}
        oncapture={oncapture}
        onstart={onstartcapture} />
      {#if capture && capture !== config[field.key]}
        <Button size="sm" variant="primary" onclick={onsavehotkey}>Save</Button>
      {/if}
      <Button size="sm" variant="ghost" onclick={ontogglelisten}>{listening ? 'Stop test' : 'Test'}</Button>
      {#if listening}
        <span class="hint" class:ok={heard}>{heard ? 'Trix received it.' : 'Press the hotkey now...'}</span>
      {/if}
    {/if}
  </div>
</div>

<style>
  /* `.row`, `.lt` and `.rt` are global, in app.css: `Settings.svelte`
     renders the same row shape inline for its Version entry, and a scoped
     copy here would mean maintaining both. Only what is unique to a field's
     control lives here. */
  .pct { min-width: 44px; text-align: right; font-size: 12.5px; color: var(--dim); }
  .path {
    display: block;
    max-width: 280px;
    padding: 7px 10px;
    background: var(--bg);
    border: 1px solid var(--line-strong);
    border-radius: var(--r);
    color: var(--dim);
    font-size: 12px;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .path.off { opacity: 0.45; }
  .hint { font-size: 11.5px; color: var(--dim); }
  .hint.ok { color: var(--live); }
</style>
```

`onstartcapture` is a separate prop from `oncapture` on purpose. Arming the
field and recording a combination are different events, and routing "the user
clicked the field" through `captureHotkey` would mean synthesising a
`KeyboardEvent` for it — which `captureHotkey` would dutifully turn into the
combination `unidentified` and save.

- [ ] **Step 3: Add `startCapture` to `Settings.svelte`'s script**

Insert directly above the existing `captureHotkey` function, and leave
`captureHotkey` itself untouched:

```ts
  /**
   * Arms the hotkey field. `capture` doubles as "we are listening" for
   * `KeycapInput`, so it starts as the saved combination rather than as an
   * empty string -- an empty field would blank the keycaps the moment it was
   * clicked, before the user had pressed anything.
   */
  function startCapture() {
    capture = String(config['clip_hotkey'] ?? '');
  }
```

Then pass it through in the `<Field ... />` call: add
`onstartcapture={startCapture}`.

- [ ] **Step 4: Replace `Settings.svelte`'s markup and styles**

The `<script>` block keeps everything from Step 3 and before. Replace only
from `<h1>Settings</h1>` to the end of the file:

```svelte
<h1>Settings</h1>
<p class="lede">Changes take effect on the next clip. A few need a re-arm, and Trix says which.</p>

{#if app.rearmNeeded.length > 0}
  <!-- `--live`, not `--accent`: this is about the running capture, which is
       what green means everywhere else in the app. -->
  <div class="notice">
    <Icon name="rearm" size={15} />
    Re-arm to apply: {app.rearmNeeded.join(', ')}
    <span class="spacer"></span>
    <Button size="sm" onclick={() => app.rearmNow()}>Re-arm now</Button>
  </div>
{/if}

{#each SECTIONS as section (section)}
  <section>
    <h2>{section}</h2>
    <div class="panel">
      {#each FIELDS.filter((f) => f.section === section) as field (field.key)}
        <Field
          {field} {config} {monitors} {capture} {listening} {heard}
          onset={set}
          oncapture={captureHotkey}
          onstartcapture={startCapture}
          onsavehotkey={saveHotkey}
          onpickfolder={pickFolder}
          onpicksound={pickSound}
          ontestsound={testSound}
          ontogglelisten={() => { listening = !listening; heard = false; }} />
      {/each}
      {#if section === 'Updates'}
        <div class="row">
          <div class="lt">
            <b>Version</b>
            <span>The build you are running.</span>
            {#if checked}<span class="checked">{checked}</span>{/if}
          </div>
          <div class="rt">
            <span class="ver tnum">{version}</span>
            <Button size="sm" disabled={checking || updates.busy} onclick={checkNow}>
              {checking ? 'Checking…' : 'Check now'}
            </Button>
          </div>
        </div>
      {/if}
    </div>
  </section>
{/each}

{#if extras.length > 0}
  <p class="extras">
    This daemon has settings this app does not render yet: {extras.join(', ')}. Edit them in config.toml.
  </p>
{/if}

<style>
  h1 { font-size: 19px; font-weight: 650; margin: 0 0 4px; }
  .lede { color: var(--dim); font-size: 12px; margin: 0 0 18px; }
  section { margin-bottom: 20px; }
  h2 {
    font-size: 10.5px;
    text-transform: uppercase;
    letter-spacing: 0.11em;
    color: var(--faint);
    font-weight: 600;
    margin: 0 0 8px;
  }
  .panel {
    background: var(--surface);
    border: 1px solid var(--line);
    border-radius: var(--r-lg);
    padding: 2px 15px;
  }
  .notice {
    display: flex;
    align-items: center;
    gap: 11px;
    padding: 10px 13px;
    margin-bottom: 18px;
    border-radius: var(--r-md);
    font-size: 12.5px;
    background: color-mix(in srgb, var(--live) 10%, transparent);
    border: 1px solid color-mix(in srgb, var(--live) 40%, transparent);
  }
  .notice :global(svg) { color: var(--live); }
  .spacer { margin-left: auto; }
  /* `.row`, `.lt` and `.rt` come from app.css, shared with `Field.svelte` --
     the Version row is this same shape and must line up with the generated
     rows above it. Only what is unique to this row lives here. */
  .checked { color: var(--text) !important; }
  .ver { color: var(--dim); font-size: 12.5px; }
  .extras { font-size: 11.5px; color: var(--dim); margin: 0; }
</style>
```

Add to the imports at the top of the `<script>`:

```ts
  import Button from '../components/ui/Button.svelte';
  import Icon from '../components/ui/Icon.svelte';
```

- [ ] **Step 5: Verify the gates**

Run: `npm run check && npx vitest run && npm run build`
Expected: check 0 errors / 0 warnings; vitest 113 passing; build succeeds.

`settings.test.ts` must pass **without being edited**. It tests `FIELDS`,
`unknownKeys` and `validate`, none of which this task touches. An edit to it is
a signal that behaviour moved when it should not have.

- [ ] **Step 6: Commit**

```bash
git add crates/trix-ui/web/src/components/ui/KeycapInput.svelte crates/trix-ui/web/src/components/Field.svelte crates/trix-ui/web/src/views/Settings.svelte
git commit -m "feat(ui): panelled settings with drawn controls"
```

---

## Task 8: Frameless window, title bar and shell

**Files:**
- Modify: `crates/trix-ui/tauri.conf.json`
- Modify: `crates/trix-ui/capabilities/default.json`
- Create: `crates/trix-ui/web/src/components/TitleBar.svelte`
- Modify: `crates/trix-ui/web/src/App.svelte` (rewrite, 92 lines)
- Modify: `crates/trix-ui/web/src/views/Rail.svelte` (rewrite, 45 lines)
- Modify: `crates/trix-ui/web/src/lib/state.svelte.ts` (add `hotkey`, ~6 lines)

**Interfaces:**
- Consumes: `Button`, `IconButton`, `Icon`.
- Produces: `app.hotkey: string` — the saved clip hotkey, for Task 9's empty state.

- [ ] **Step 1: Turn decorations off**

In `crates/trix-ui/tauri.conf.json`, add `"decorations": false` to the `main`
window object, leaving every other key as it is:

```json
      {
        "label": "main",
        "title": "Trix",
        "width": 1180,
        "height": 760,
        "minWidth": 880,
        "minHeight": 560,
        "resizable": true,
        "visible": true,
        "decorations": false
      }
```

- [ ] **Step 2: Grant the window-control permissions**

`core:default` covers neither dragging nor the window buttons. Replace the
`permissions` array in `crates/trix-ui/capabilities/default.json`:

```json
  "permissions": [
    "core:default",
    "core:window:allow-start-dragging",
    "core:window:allow-minimize",
    "core:window:allow-maximize",
    "core:window:allow-unmaximize",
    "core:window:allow-toggle-maximize",
    "core:window:allow-is-maximized",
    "core:window:allow-close"
  ]
```

**A missing window permission fails at runtime, not at compile time.** Both
`npm run check` and `cargo build` will pass with a title bar whose buttons do
nothing, so this is confirmed by clicking them in Step 8.

`allow-start-dragging` is the one this plan originally got wrong, and it is
worth understanding why rather than just copying the line. `core:window:default`
grants 28 permissions; its list includes `allow-internal-toggle-maximize` and
omits `allow-start-dragging`. So with `core:default` alone,
`data-tauri-drag-region` still maximizes on double-click while dragging the
same element does nothing at all -- two behaviours of one attribute, split by
a table nothing in the API surface reveals. `data-tauri-drag-region` is not a
webview-level behaviour that sidesteps permissions; it is sugar over
`startDragging()`, an IPC call gated like any other.

This is now covered by `src/lib/capabilities.test.ts`, which reads the shipped
`TitleBar.svelte` and the shipped capability file and fails when a window call
has no permission behind it. Extend `WINDOW_CALLS` in `src/lib/capabilities.ts`
when a task starts calling a window API this table does not list yet.

- [ ] **Step 3: Add the hotkey to app state**

In `crates/trix-ui/web/src/lib/state.svelte.ts`, add to `class AppState`,
directly under the `ringUsed` declaration:

```ts
  /**
   * The saved clip hotkey, for the empty grid's "press X while you play".
   * Free to keep: `onDaemonUp` already fetches the whole config to answer the
   * first-run question, so this is one more field off a call already made.
   */
  hotkey = $state('alt+f10');
```

In `onDaemonUp`, inside the existing `try` block, directly after the
`const config = await call<...>('config.get');` line:

```ts
    app.hotkey = String(config['clip_hotkey'] ?? 'alt+f10');
```

And in the `wireDaemon` event handler, add a `config_changed` case so the
grid's hint follows a rebind — find the existing `onDaemonEvent((event) => {`
block and add:

```ts
    if (event.event === 'config_changed' && typeof event.data['clip_hotkey'] === 'string') {
      app.hotkey = event.data['clip_hotkey'];
    }
```

- [ ] **Step 4: Create `TitleBar.svelte`**

```svelte
<script lang="ts">
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { app } from '../lib/state.svelte';
  import IconButton from './ui/IconButton.svelte';

  const win = getCurrentWindow();
  let maximized = $state(false);

  // The maximize button has to show a restore glyph once the window is
  // maximized, and the window can be maximized without this button -- by
  // double-clicking the drag region, by Win+Up, by dragging to the top edge.
  // So the state is read from the window, never inferred from our own clicks.
  $effect(() => {
    let alive = true;
    void win.isMaximized().then((v) => { if (alive) maximized = v; });
    const un = win.onResized(() => {
      void win.isMaximized().then((v) => { if (alive) maximized = v; });
    });
    return () => {
      alive = false;
      void un.then((fn) => fn());
    };
  });

  const pct = $derived(
    app.status && app.status.ring_seconds_total > 0
      ? Math.min(100, (app.ringUsed / app.status.ring_seconds_total) * 100)
      : 0,
  );
</script>

<header class="tb">
  <div class="logo">T</div>
  <span class="name">TRIX</span>

  <button
    class="arm"
    class:on={app.armed}
    disabled={!app.connected}
    onclick={() => app.toggleArm()}>
    <span class="dot"></span>
    {app.armed ? 'ARMED' : 'Arm'}
    {#if app.armed}
      <span class="meter"><i style="width: {pct}%"></i></span>
      <span class="n tnum">
        {app.ringUsed.toFixed(0)}/{app.status?.ring_seconds_total.toFixed(0) ?? '0'}s
      </span>
    {/if}
  </button>

  <!-- The whole remaining width drags the window. Tauri handles
       double-click-to-maximize on this attribute itself. -->
  <div class="grab" data-tauri-drag-region></div>

  <div class="wc">
    <IconButton icon="minimize" label="Minimize" size={11} onclick={() => void win.minimize()} />
    <IconButton
      icon={maximized ? 'restore' : 'maximize'}
      label={maximized ? 'Restore' : 'Maximize'}
      size={11}
      onclick={() => void win.toggleMaximize()} />
    <span class="close">
      <IconButton icon="close" label="Close" size={11} onclick={() => void win.close()} />
    </span>
  </div>
</header>

<style>
  .tb {
    display: flex;
    align-items: center;
    gap: 12px;
    height: 40px;
    padding-left: 12px;
    flex: 0 0 40px;
    background: var(--surface);
    border-bottom: 1px solid var(--line);
  }
  .logo {
    width: 20px;
    height: 20px;
    display: grid;
    place-items: center;
    border-radius: var(--r-sm);
    background: var(--accent);
    color: var(--accent-ink);
    font-size: 11px;
    font-weight: 800;
  }
  .name { font-size: 12px; font-weight: 600; letter-spacing: 0.05em; }
  .grab { flex: 1; align-self: stretch; }

  .arm {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 5px 11px;
    border-radius: var(--r-full);
    border: 1px solid var(--line-strong);
    background: transparent;
    color: var(--dim);
    font: inherit;
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.04em;
    cursor: pointer;
    transition: color var(--t) var(--ease), background var(--t) var(--ease), border-color var(--t) var(--ease);
  }
  .arm .dot { width: 6px; height: 6px; border-radius: 50%; background: var(--faint); transition: background var(--t) var(--ease); }
  /* The pill is the most-pressed control in the app, so it answers the
     pointer in both of its states. Scoped off `.on` because the armed rule
     below is only (0,2,0) specificity and a bare `.arm:hover` would outrank
     it -- washing the green out from under the cursor. */
  .arm:hover:not(:disabled):not(.on) {
    background: var(--hover);
    border-color: var(--line-hi);
    color: var(--text);
  }
  .arm.on {
    background: color-mix(in srgb, var(--live) 13%, transparent);
    border-color: color-mix(in srgb, var(--live) 50%, transparent);
    color: var(--live);
  }
  .arm.on .dot { background: var(--live); box-shadow: 0 0 0 3px color-mix(in srgb, var(--live) 20%, transparent); }
  .arm.on:hover:not(:disabled) { background: color-mix(in srgb, var(--live) 20%, transparent); }
  .arm:disabled { opacity: 0.4; cursor: default; }
  .meter { width: 44px; height: 3px; border-radius: 2px; background: var(--line-strong); overflow: hidden; }
  .meter i { display: block; height: 100%; background: var(--live); transition: width 200ms linear; }
  .n { opacity: 0.85; }

  .wc { display: flex; align-self: stretch; }
  /* Windows-standard 44px, and square to the bar rather than the 30px
     rounded shape IconButton uses everywhere else. */
  .wc :global(.ib) { width: 44px; height: 100%; border-radius: 0; }
  .close :global(.ib:hover) { background: var(--win-close); color: var(--text); }
</style>
```

- [ ] **Step 5: Rewrite `App.svelte`**

The title bar renders in **every** branch, including `DaemonDown`,
`updates.swapping` and `FirstRun` — without it those states would give the
user a window they cannot move, resize or close.

```svelte
<script lang="ts">
  import Rail from './views/Rail.svelte';
  import DaemonDown from './views/DaemonDown.svelte';
  import Grid from './views/Grid.svelte';
  import ClipPage from './views/ClipPage.svelte';
  import Settings from './views/Settings.svelte';
  import FirstRun from './views/FirstRun.svelte';
  import Toasts from './components/Toasts.svelte';
  import TitleBar from './components/TitleBar.svelte';
  import Button from './components/ui/Button.svelte';
  import { app, updates, wireDaemon, wireUpdates } from './lib/state.svelte';

  // The same page the Rust side checks every opened URL against.
  const RELEASES_URL = 'https://github.com/tnhnblgl/trix/releases';

  wireDaemon();
  wireUpdates();
</script>

<div class="app">
  <TitleBar />

  {#if updates.banner}
    <div class="update" class:error={updates.banner.error}>
      {#if updates.banner.error}
        <span>{updates.banner.error}</span>
        <!-- `href` stays so the target is visible on hover and can be copied, but
             the click is handled in Rust: a Tauri webview opens no new window, so
             the default action here is nothing at all. -->
        <a href={RELEASES_URL}
          onclick={(e) => { e.preventDefault(); updates.openReleasesPage(RELEASES_URL); }}
          >Download it by hand</a>
      {:else if updates.busy}
        <span>{updates.phase === 'downloading' ? `Downloading… ${updates.percent}%` : 'Installing…'}</span>
      {:else}
        <span>Trix {updates.banner.version} is available</span>
        <a href={updates.release?.notes_url}
          onclick={(e) => { e.preventDefault(); if (updates.release) updates.openReleasesPage(updates.release.notes_url); }}
          >What's new</a>
        <span class="spacer"></span>
        <Button size="sm" variant="primary" onclick={() => updates.install()}>Update</Button>
      {/if}
    </div>
  {/if}

  {#if updates.swapping}
    <!-- The swap itself is why the daemon looks gone -- DaemonDown's Start
         button would just be refused (an install already owns the pipe), so
         for this window the banner above is the entire window. -->
    <div class="filler"></div>
  {:else if !app.connected}
    <DaemonDown />
  {:else if app.view === 'firstrun'}
    <FirstRun />
  {:else}
    <div class="shell">
      <Rail />
      <main class="content">
        {#if app.view === 'grid'}
          <Grid />
        {:else if app.view === 'clip'}
          <ClipPage />
        {:else if app.view === 'settings'}
          <Settings />
        {/if}
      </main>
    </div>
  {/if}
</div>

<Toasts />

<style>
  .app { display: flex; flex-direction: column; height: 100vh; }
  .shell { display: flex; flex: 1; min-height: 0; }
  .content { flex: 1; overflow: auto; padding: 18px 20px; }
  .filler { flex: 1; }

  .update {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 8px 16px;
    flex: 0 0 auto;
    background: color-mix(in srgb, var(--accent) 12%, var(--surface));
    border-bottom: 1px solid var(--line);
    font-size: 12.5px;
  }
  .update.error { background: var(--surface); color: var(--dim); }
  .update a { color: var(--accent); }
  .spacer { margin-left: auto; }
</style>
```

- [ ] **Step 6: Rewrite `Rail.svelte`**

The arm control and the buffer meter have moved to the title bar, so the rail
is navigation only and narrows from 200px to 150px.

```svelte
<script lang="ts">
  import { app } from '../lib/state.svelte';
  import Icon from '../components/ui/Icon.svelte';
</script>

<nav class="rail">
  <button class="nav" class:on={app.view === 'grid'} onclick={() => (app.view = 'grid')}>
    <Icon name="clips" size={15} />Clips
  </button>
  <button class="nav" class:on={app.view === 'settings'} onclick={() => (app.view = 'settings')}>
    <Icon name="settings" size={15} />Settings
  </button>
  <div class="ver tnum">{app.status?.version ?? ''}</div>
</nav>

<style>
  .rail {
    width: 150px;
    flex: 0 0 150px;
    padding: 12px 10px;
    display: flex;
    flex-direction: column;
    gap: 4px;
    background: var(--surface);
    border-right: 1px solid var(--line);
  }
  .nav {
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 8px 10px;
    border: 0;
    border-radius: var(--r);
    background: transparent;
    color: var(--dim);
    font: inherit;
    font-size: 12.5px;
    text-align: left;
    cursor: pointer;
    transition: background var(--t-fast) var(--ease), color var(--t-fast) var(--ease);
  }
  .nav:hover { background: var(--hover); color: var(--text); }
  .nav.on { background: color-mix(in srgb, var(--accent) 15%, transparent); color: var(--text); font-weight: 600; }
  .ver { margin-top: auto; font-size: 10px; color: var(--faint); padding: 0 10px; }
</style>
```

- [ ] **Step 7: Verify the gates**

Run: `npm run check && npx vitest run && npm run build`
Expected: check 0 errors / 0 warnings; vitest 131 passing (119 + 12 from the drag-permission gate); build succeeds.

- [ ] **Step 8: Hand-check the window (no script covers this)**

Build and run the real app:

```bash
cargo build --release --workspace
cd crates/trix-ui && cargo tauri build --no-bundle
```

Then confirm, in order:

- [ ] The window drags from the empty area of the title bar.
- [ ] Double-clicking that area maximizes, and again restores.
- [ ] Minimize, maximize and close all work. **If any does nothing, a
      capability from Step 2 is missing or misnamed** — that is the only
      failure mode that reaches this point silently.
- [ ] The maximize glyph changes to the restore glyph when maximized, including
      when maximized by Win+Up rather than by the button.
- [ ] The window still resizes from all four edges and all four corners.
- [ ] A maximized window does not overflow the screen edges.
- [ ] Arm and disarm from the title bar; the meter fills green while armed.

- [ ] **Step 9: Commit**

```bash
git add crates/trix-ui/tauri.conf.json crates/trix-ui/capabilities/default.json crates/trix-ui/web/src/components/TitleBar.svelte crates/trix-ui/web/src/App.svelte crates/trix-ui/web/src/views/Rail.svelte crates/trix-ui/web/src/lib/state.svelte.ts
git commit -m "feat(ui): frameless window with the arm control in the title bar"
```

---

## Task 9: Clip card and library grid

**Files:**
- Modify: `crates/trix-ui/web/src/components/ClipCard.svelte` (rewrite, 42 lines)
- Modify: `crates/trix-ui/web/src/views/Grid.svelte` (markup and styles)

**Interfaces:**
- Consumes: `Menu` (and its `MenuItem` type), `IconButton`, `Icon`, `app.hotkey` from Task 8.
- Produces: nothing later tasks depend on.

**The structural problem this task solves:** `ClipCard` is a single
`<button>` today. It now needs an overflow menu button inside it, and an
interactive element nested inside a button is invalid HTML and breaks
keyboard operation in every browser. The card becomes a `<div>` whose
*thumbnail* is the button.

**That change interacts with `keys.ts`.** `Grid.svelte` passes
`insideGrid` as `shouldHandleKey`'s `ownsActivation` argument, so that Space
and Enter on a focused card belong to the grid rather than to the card's own
`<button>`. The thumbnail is still a `<button>` and still inside `gridEl`, so
`insideGrid` still computes correctly — **but the new `⋯` button is also
inside `gridEl`**, and its Enter must open its menu rather than being taken by
the grid. Step 3 handles this.

- [ ] **Step 1: Rewrite `ClipCard.svelte`**

```svelte
<script lang="ts">
  import { formatBytes, formatDuration, thumbUrl } from '../lib/clips';
  import type { ClipMeta } from '../lib/types';
  import Icon from './ui/Icon.svelte';
  import IconButton from './ui/IconButton.svelte';
  import Menu from './ui/Menu.svelte';

  let {
    clip, clipDir, selected = false, onopen, onselect, onfavorite, onrename, ondelete,
  }: {
    clip: ClipMeta;
    clipDir: string;
    selected?: boolean;
    onopen: () => void;
    onselect: () => void;
    onfavorite: () => void;
    onrename: (title: string) => void;
    ondelete: () => void;
  } = $props();

  let menuOpen = $state(false);
  let renaming = $state(false);
  let draft = $state('');

  function pick(id: string) {
    menuOpen = false;
    if (id === 'favorite') onfavorite();
    else if (id === 'rename') {
      draft = clip.title;
      renaming = true;
    } else if (id === 'delete') ondelete();
  }

  function commit() {
    if (!renaming) return;
    renaming = false;
    const next = draft.trim();
    if (next && next !== clip.title) onrename(next);
  }
</script>

<div class="card" class:selected class:menuOpen>
  <button class="thumb" onclick={onselect} ondblclick={onopen}>
    <img src={thumbUrl(clipDir, clip.id)} alt="" loading="lazy" />
    <span class="len tnum">{formatDuration(clip.duration_ms)}</span>
  </button>

  <div class="meta">
    <div class="line">
      <!-- The star lives here, not on the thumbnail. Over a bright frame a
           bare glyph is invisible; on the app's own surface it never is. It
           also sits on the same line as the menu item that toggles it. -->
      {#if clip.favorite}
        <span class="star"><Icon name="star-filled" size={13} /></span>
      {/if}

      {#if renaming}
        <!-- svelte-ignore a11y_autofocus -->
        <input
          class="rn"
          bind:value={draft}
          autofocus
          onblur={commit}
          onkeydown={(e) => {
            // Stopped as well as handled: `Grid.svelte` listens on
            // `<svelte:window>`, and without this every keystroke typed here
            // would also reach the grid's arrow/Enter/Space handling.
            e.stopPropagation();
            if (e.key === 'Enter') commit();
            else if (e.key === 'Escape') renaming = false;
          }} />
      {:else}
        <span class="title">{clip.title}</span>
        <span class="dots">
          <IconButton icon="dots" label="More actions for {clip.title}" size={14}
            onclick={() => (menuOpen = !menuOpen)} />
          {#if menuOpen}
            <Menu
              items={[
                { id: 'favorite', label: clip.favorite ? 'Unfavourite' : 'Favourite', icon: clip.favorite ? 'star-filled' : 'star' },
                { id: 'rename', label: 'Rename', icon: 'pencil' },
                { id: 'delete', label: 'Delete', icon: 'trash', danger: true, separatorBefore: true },
              ]}
              onpick={pick}
              onclose={() => (menuOpen = false)} />
          {/if}
        </span>
      {/if}
    </div>
    <!-- The separators are markup, not string content. An `&middot;` written
         inside a `{...}` expression renders as the six literal characters
         `&middot;` -- entities are only decoded in markup. -->
    <span class="sub tnum">
      {formatBytes(clip.bytes)} &middot; {clip.width}x{clip.height}{#if clip.fps} &middot; {clip.fps} fps{/if}
    </span>
  </div>
</div>

<style>
  .card { display: grid; gap: 8px; transition: transform var(--t-fast) var(--ease); }
  .card:hover { transform: translateY(-2px); }

  .thumb {
    position: relative;
    aspect-ratio: 16 / 10;
    padding: 0;
    border: 0;
    border-radius: var(--r-md);
    overflow: hidden;
    background: var(--surface);
    box-shadow: inset 0 0 0 1px var(--line-soft);
    cursor: pointer;
    transition: box-shadow var(--t-fast) var(--ease);
  }
  .thumb img { width: 100%; height: 100%; object-fit: cover; display: block; }
  .card:hover .thumb { box-shadow: inset 0 0 0 1px var(--line-strong); }
  .card.selected .thumb {
    box-shadow: inset 0 0 0 2px var(--accent), 0 0 0 3px color-mix(in srgb, var(--accent) 22%, transparent);
  }
  .len {
    position: absolute;
    right: 6px;
    bottom: 6px;
    padding: 1px 6px;
    border-radius: var(--r-sm);
    background: var(--scrim-strong);
    color: var(--text);
    font-size: 10px;
  }

  .meta { display: grid; gap: 1px; padding: 0 2px 2px; }
  .line { display: flex; align-items: center; gap: 6px; min-width: 0; }
  .star { color: var(--fav); display: flex; flex: 0 0 auto; }
  .title { flex: 1; min-width: 0; font-size: 12.5px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .sub { font-size: 10.5px; color: var(--dim); }

  /* Hidden at rest, shown whenever the card is in play: hovered, focused
     within, selected, or with its own menu open. Never a control the user has
     to guess is there. */
  .dots { position: relative; flex: 0 0 auto; opacity: 0; transition: opacity var(--t-fast) var(--ease); }
  .card:hover .dots,
  .card:focus-within .dots,
  .card.selected .dots,
  .card.menuOpen .dots { opacity: 1; }

  .rn {
    flex: 1;
    min-width: 0;
    padding: 3px 7px;
    border-radius: var(--r-sm);
    border: 1px solid var(--accent);
    background: var(--bg);
    color: var(--text);
    font: inherit;
    font-size: 12.5px;
  }
</style>
```

`onclose` here closes the menu and does nothing else. **It must not move focus
back to the `⋯` button**, however natural that looks. `Menu` fires this same
callback from three places -- an outside pointerdown, Escape, and Tab -- and
gives the consumer no way to tell them apart. It runs synchronously inside the
keydown handler, before the browser performs Tab's own focus move, so a
`.focus()` here would yank focus back onto the trigger and undo the Tab. That
would rebuild the focus trap `Menu`'s Tab branch exists to prevent. If a future
screen genuinely needs focus restored on Escape but not on Tab, the fix is to
give `onclose` a reason argument in `Menu` -- not to restore focus here and
hope.

- [ ] **Step 2: Update `Grid.svelte`'s markup and styles**

Keep the entire `<script>` block — the `ResizeObserver` column count, the
`onkeydown` handler and every comment in it — and change only the imports and
everything from `{#if app.clips.length === 0}` down.

`Grid.svelte` already imports from `../lib/clips`. **Extend that existing
import line rather than adding a second one from the same module:**

```ts
  import { clipUrl, formatBytes } from '../lib/clips';
```

Add above the markup, at the end of the `<script>`:

```ts
  /**
   * The library's byte total, but only when the whole library is loaded.
   *
   * `library.list` is fetched with `limit: 200`, so on a bigger library
   * `app.clips` is a page and summing it would understate the total by
   * however much did not fit. A count is always true; a size is only true
   * when there is nothing else to count.
   */
  const librarySize = $derived(
    app.clips.length === app.total
      ? formatBytes(app.clips.reduce((sum, c) => sum + c.bytes, 0))
      : null,
  );
</script>
```

Then the markup and styles:

```svelte
<svelte:window {onkeydown} />

<header class="head">
  <h1>Clips</h1>
  <span class="cnt tnum">
    {app.total} {app.total === 1 ? 'clip' : 'clips'}{#if librarySize} &middot; {librarySize}{/if}
  </span>
</header>

{#if app.clips.length === 0}
  <div class="empty">
    <p class="big">No clips yet.</p>
    <p>Arm Trix, then press <kbd>{app.hotkey}</kbd> while you play.</p>
  </div>
{:else}
  <div class="grid" bind:this={gridEl}>
    {#each app.clips as clip, i (clip.id)}
      {#if previewing === clip.id}
        <!-- svelte-ignore a11y_media_has_caption -->
        <video class="preview" src={clipUrl(app.clipDir, clip.id)} autoplay loop muted></video>
      {:else}
        <ClipCard
          {clip}
          clipDir={app.clipDir}
          selected={i === app.selected}
          onselect={() => (app.selected = i)}
          onopen={() => { app.selected = i; app.view = 'clip'; }}
          onfavorite={() => app.setFavorite(clip.id, !clip.favorite)}
          onrename={(title) => app.rename(clip.id, title)}
          ondelete={() => { app.selected = i; void app.remove(clip.id); }} />
      {/if}
    {/each}
  </div>
{/if}

<style>
  .head { display: flex; align-items: baseline; gap: 10px; margin-bottom: 14px; }
  .head h1 { margin: 0; font-size: 15px; font-weight: 650; }
  .cnt { font-size: 11px; color: var(--faint); }
  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(230px, 1fr)); gap: 14px; }
  .preview { width: 100%; aspect-ratio: 16 / 10; border-radius: var(--r-md); background: var(--video-bg); object-fit: cover; }
  .empty { display: grid; place-content: center; height: 60vh; text-align: center; gap: 6px; color: var(--dim); }
  .empty .big { font-size: 15px; color: var(--text); margin: 0; }
  .empty p { margin: 0; font-size: 12.5px; }
  kbd {
    padding: 2px 7px;
    border-radius: var(--r-sm);
    background: var(--raised);
    border: 1px solid var(--line-strong);
    border-bottom-width: 2px;
    font: inherit;
    font-size: 11px;
    font-weight: 600;
    color: var(--text);
  }
</style>
```

- [ ] **Step 3: Add the `⋯` exception to the grid's key handler**

The grid claims Space and Enter for every target inside `gridEl`. The overflow
button is now inside `gridEl` too, and its Enter must open its own menu.

In `Grid.svelte`'s `onkeydown`, replace the `insideGrid` line with:

```ts
    // The grid claims Space and Enter for its cards, but not for the two
    // controls inside a card that own those keys themselves: the overflow
    // button and, while a card is being renamed, its text box. Without this,
    // Enter on a focused `⋯` would open the clip instead of its menu.
    const target = e.target instanceof Element ? e.target : null;
    const ownedByCardControl = !!target?.closest(CARD_CONTROL_SELECTOR);
    const insideGrid =
      !ownedByCardControl && !!gridEl && e.target instanceof Node && gridEl.contains(e.target);
```

- [ ] **Step 4: Extend `keys.test.ts`**

**Do not test `shouldHandleKey` again here.** Both obvious cases -- a
`BUTTON` with `ownsActivation` false, and one with it true -- are already
pinned by the existing `card` and `arm` fixtures, which use the same
`target({ tagName: 'BUTTON' })` input. A test written against them restates
passing behaviour and cannot fail.

The part that can actually break is the marker, because it lives in markup
and `closest()` takes a string with no type behind it. Add to
`crates/trix-ui/web/src/lib/keys.test.ts`, at the end of the file:

```ts
describe('the card/grid control marker', () => {
  it('is carried by both of the card controls the grid must not claim', () => {
    const marks = clipCardSource.split(CARD_CONTROL_ATTR).length - 1;
    // The overflow button and the rename box. If a third control is ever added
    // to a card, this number is a decision to make deliberately -- does the
    // grid have to withhold Space and Enter from it too? -- not a nuisance to
    // bump past.
    expect(marks).toBe(2);
  });

  it('is what the grid actually searches for', () => {
    // Guards against someone re-inlining a literal selector here. The shared
    // constant would go unused, and this project sets no `noUnusedLocals`, so
    // nothing else would say a word.
    expect(gridSource).toContain('CARD_CONTROL_SELECTOR');
  });
});
```

with `clipCardSource` and `gridSource` imported through Vite's `?raw` at the
top of the file -- not `node:fs`, which would need `@types/node`.

- [ ] **Step 5: Verify the gates**

Run: `npm run check && npx vitest run && npm run build`
Expected: check 0 errors / 0 warnings; vitest 133 passing; build succeeds.

- [ ] **Step 6: Commit**

```bash
git add crates/trix-ui/web/src/components/ClipCard.svelte crates/trix-ui/web/src/views/Grid.svelte crates/trix-ui/web/src/lib/keys.test.ts
git commit -m "feat(ui): clip cards with an overflow menu and a legible favourite"
```

---

## Task 10: The unified timeline

**Files:**
- Create: `crates/trix-ui/web/src/lib/timeline.ts`
- Create: `crates/trix-ui/web/src/lib/timeline.test.ts`
- Create: `crates/trix-ui/web/src/components/Timeline.svelte`
- Create: `crates/trix-ui/web/src/components/VideoPlayer.svelte`
- Delete: `crates/trix-ui/web/src/components/TrimBar.svelte`

**Interfaces:**
- Consumes: `clamp` from `lib/ui.ts`, `IconButton`, `Icon`.
- Produces:
  - `snapStart(ms, keyframes)`, `keyframeStep(ms, keyframes, dir)`,
    `msFromRatio(ratio, durationMs)`, `ratioFromMs(ms, durationMs)`,
    `formatClock(ms)` — all in `lib/timeline.ts`
  - `Timeline` — `{ durationMs, playheadMs, keyframes, inMs, outMs, onchange: (inMs, outMs) => void, onseek: (ms) => void }`
  - `VideoPlayer` — props `{ src: string; ontime: (ms: number) => void; timeline: Snippet }`,
    plus two instance methods reachable through `bind:this`: `toggle(): void`
    and `seekTo(ms: number): void`

This is the largest change in the overhaul: Chromium's player bar disappears
and its scrubber merges with the trim track into one band.

**`snapStart` must behave exactly as `TrimBar.svelte`'s `snap()` does today**,
including its two non-obvious cases: an empty keyframe list returns the input
unchanged (the ticks are still loading, or the clip could not be indexed —
pinning to 0 would make the control look broken), and a list whose every entry
is later than `ms` returns 0.

- [ ] **Step 1: Write the failing test**

Create `crates/trix-ui/web/src/lib/timeline.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { formatClock, keyframeStep, msFromRatio, ratioFromMs, snapStart } from './timeline';

describe('snapStart', () => {
  it('moves back to the latest keyframe at or before the point', () => {
    expect(snapStart(2500, [0, 1000, 2000, 3000])).toBe(2000);
    expect(snapStart(2000, [0, 1000, 2000, 3000])).toBe(2000);
  });

  it('returns the point untouched when there are no keyframes', () => {
    // Not "snap to zero". An empty list means the ticks are still loading, or
    // the clip could not be indexed at all -- and pinning the in-point to 0
    // in either case makes the control look broken. The daemon snaps it for
    // real regardless.
    expect(snapStart(2500, [])).toBe(2500);
  });

  it('returns 0 when every keyframe is later than the point', () => {
    expect(snapStart(500, [1000, 2000])).toBe(0);
  });
});

describe('keyframeStep', () => {
  it('moves to the neighbouring keyframe', () => {
    expect(keyframeStep(2000, [0, 1000, 2000, 3000], 1)).toBe(3000);
    expect(keyframeStep(2000, [0, 1000, 2000, 3000], -1)).toBe(1000);
  });

  it('moves off an off-grid point to the neighbour in that direction', () => {
    expect(keyframeStep(2400, [0, 1000, 2000, 3000], 1)).toBe(3000);
    expect(keyframeStep(2400, [0, 1000, 2000, 3000], -1)).toBe(2000);
  });

  it('stays put at either end of the list', () => {
    expect(keyframeStep(3000, [0, 1000, 2000, 3000], 1)).toBe(3000);
    expect(keyframeStep(0, [0, 1000, 2000, 3000], -1)).toBe(0);
  });

  it('falls back to one second when there are no keyframes', () => {
    // Same honesty as snapStart: with nothing to step between, a whole
    // second is a defensible unit and pretending otherwise is not.
    expect(keyframeStep(4000, [], 1)).toBe(5000);
    expect(keyframeStep(4000, [], -1)).toBe(3000);
    expect(keyframeStep(300, [], -1)).toBe(0);
  });
});

describe('msFromRatio / ratioFromMs', () => {
  it('maps both ends and clamps a pointer dragged past them', () => {
    expect(msFromRatio(0, 15000)).toBe(0);
    expect(msFromRatio(1, 15000)).toBe(15000);
    expect(msFromRatio(-0.5, 15000)).toBe(0);
    expect(msFromRatio(2, 15000)).toBe(15000);
  });

  it('rounds to a whole millisecond', () => {
    expect(msFromRatio(1 / 3, 10000)).toBe(3333);
  });

  it('gives 0 for a clip with no duration rather than NaN', () => {
    // `library::scan` adopts a bare .mp4 as `duration_ms: 0`, and `NaN%`
    // in CSS drops the playhead out of the band entirely.
    expect(ratioFromMs(500, 0)).toBe(0);
    expect(msFromRatio(0.5, 0)).toBe(0);
  });
});

describe('formatClock', () => {
  it('shows minutes, seconds and a tenth', () => {
    expect(formatClock(0)).toBe('0:00.0');
    expect(formatClock(5700)).toBe('0:05.7');
    expect(formatClock(65400)).toBe('1:05.4');
  });

  it('never shows a negative time', () => {
    expect(formatClock(-20)).toBe('0:00.0');
  });
});
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `npx vitest run src/lib/timeline.test.ts`
Expected: FAIL — `Failed to resolve import "./timeline"`.

- [ ] **Step 3: Write the implementation**

Create `crates/trix-ui/web/src/lib/timeline.ts`:

```ts
import { clamp } from './ui';

/**
 * Geometry and snapping for the unified seek-and-trim band.
 *
 * Pulled out of the component so it can be tested without a DOM. `snapStart`
 * is a move of `TrimBar.svelte`'s `snap()`, unchanged in behaviour -- it is
 * the same rule the daemon applies in `trix-core/src/export.rs`'s
 * `snap_start`, so the handle sits where the export will actually cut.
 */

/** Milliseconds a step covers when a clip has no keyframe index at all. */
const NO_KEYFRAME_STEP_MS = 1000;

/**
 * Latest keyframe at or before `ms`.
 *
 * An empty list is not "snap to zero": the ticks are still loading, or the
 * clip could not be indexed and `keyframesFor` deliberately degraded to a bar
 * with no ticks. Pinning the in-point to 0 in either case would make the
 * control look broken. With nothing to snap against, the raw position is the
 * honest answer and the daemon snaps it for real anyway.
 */
export function snapStart(ms: number, keyframes: number[]): number {
  if (keyframes.length === 0) return ms;
  let best = 0;
  for (const k of keyframes) if (k <= ms) best = k;
  return best;
}

/**
 * The neighbouring keyframe in `dir`, or `ms` itself at either end.
 *
 * Strictly past `ms`, so a handle already sitting on a keyframe moves off it
 * rather than snapping back to where it already is.
 */
export function keyframeStep(ms: number, keyframes: number[], dir: 1 | -1): number {
  if (keyframes.length === 0) return Math.max(0, ms + dir * NO_KEYFRAME_STEP_MS);
  if (dir === 1) {
    for (const k of keyframes) if (k > ms) return k;
    return ms;
  }
  let best = ms;
  for (const k of keyframes) if (k < ms) best = k;
  return best === ms ? ms : best;
}

/** Where a pointer at `ratio` along the band lands, in whole milliseconds. */
export function msFromRatio(ratio: number, durationMs: number): number {
  if (durationMs <= 0) return 0;
  return Math.round(clamp(ratio, 0, 1) * durationMs);
}

/** How far along the band `ms` sits, as 0..1. */
export function ratioFromMs(ms: number, durationMs: number): number {
  if (durationMs <= 0) return 0;
  return clamp(ms / durationMs, 0, 1);
}

/**
 * `m:ss.t`.
 *
 * A tenth, unlike `formatDuration`'s whole seconds: that is the right grain
 * for a clip's length in a metadata line and the wrong one for a playhead,
 * where a person is picking a moment.
 */
export function formatClock(ms: number): string {
  const safe = Math.max(0, ms);
  const tenths = Math.floor(safe / 100) % 10;
  const totalSeconds = Math.floor(safe / 1000);
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  return `${minutes}:${String(seconds).padStart(2, '0')}.${tenths}`;
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `npx vitest run src/lib/timeline.test.ts`
Expected: PASS — 4 suites, 12 tests.

- [ ] **Step 5: Create `Timeline.svelte`**

```svelte
<script lang="ts">
  import { keyframeStep, msFromRatio, ratioFromMs, snapStart } from '../lib/timeline';
  import { clamp } from '../lib/ui';

  /**
   * The unified seek-and-trim band -- one timeline where there used to be
   * two, Chromium's scrubber above the trim track.
   *
   * Presentational on purpose: it never calls the daemon. It is given a
   * duration, a playhead, a keyframe list and the current in/out, and reports
   * back where the user dragged. Every daemon call stays in state.svelte.ts.
   *
   * A band with tick marks rather than the filmstrip spec §6.2 sketches:
   * thumbnails along the range would need a video decode path, and the daemon
   * has none -- it writes clips, it does not read frames back out of them.
   * The ticks carry the part that actually matters, which is where a fast-mode
   * cut can land.
   */
  let {
    durationMs, playheadMs, keyframes, inMs, outMs, onchange, onseek,
  }: {
    durationMs: number;
    playheadMs: number;
    keyframes: number[];
    inMs: number;
    outMs: number;
    onchange: (inMs: number, outMs: number) => void;
    onseek: (ms: number) => void;
  } = $props();

  let band = $state<HTMLDivElement | null>(null);
  /** Which handle a pointer is currently dragging, or `seek` for the track. */
  let drag = $state<'in' | 'out' | 'seek' | null>(null);

  const pct = (ms: number) => `${ratioFromMs(ms, durationMs) * 100}%`;

  function msAt(clientX: number): number {
    if (!band) return 0;
    const box = band.getBoundingClientRect();
    return msFromRatio((clientX - box.left) / box.width, durationMs);
  }

  function grab(which: 'in' | 'out' | 'seek', e: PointerEvent) {
    e.preventDefault();
    e.stopPropagation();
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    drag = which;
    move(e);
  }

  function move(e: PointerEvent) {
    if (!drag) return;
    const ms = msAt(e.clientX);
    if (drag === 'seek') onseek(ms);
    // Snapped here, at the moment of the drag, for the same reason the old
    // trim bar wrote the snapped value back into its input: the handle must
    // sit where the export will really cut, not where the pointer was.
    else if (drag === 'in') onchange(snapStart(ms, keyframes), outMs);
    // Out is reported raw: fast mode cuts the end where it is asked to, so
    // there is no snap to diverge from.
    else onchange(inMs, ms);
  }

  function drop(e: PointerEvent) {
    if (!drag) return;
    (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    drag = null;
  }

  /**
   * Arrow keys on a focused handle.
   *
   * In moves by whole keyframes, which is the only unit an in-point has --
   * `keys.ts` records that the old `<input type="range">` handles used a 1ms
   * step, which moved the point by an invisible amount and cost the page its
   * prev/next shortcut for nothing. Out moves by a tenth of a second, or a
   * whole one with Shift.
   */
  function onHandleKey(which: 'in' | 'out', e: KeyboardEvent) {
    const dir = e.key === 'ArrowRight' ? 1 : e.key === 'ArrowLeft' ? -1 : 0;
    let next: number;
    if (dir !== 0) {
      if (which === 'in') next = keyframeStep(inMs, keyframes, dir);
      else next = clamp(outMs + dir * (e.shiftKey ? 1000 : 100), 0, durationMs);
    } else if (e.key === 'Home') next = 0;
    else if (e.key === 'End') next = durationMs;
    else return;
    // Stopped as well as prevented: `ClipPage` listens on
    // `<svelte:window>` and would otherwise also step to the next clip.
    e.preventDefault();
    e.stopPropagation();
    if (which === 'in') onchange(clamp(next, 0, durationMs), outMs);
    else onchange(inMs, clamp(next, 0, durationMs));
  }
</script>

<div
  bind:this={band}
  class="band"
  onpointerdown={(e) => grab('seek', e)}
  onpointermove={move}
  onpointerup={drop}>
  {#each keyframes as k (k)}
    <span class="tick" style="left: {pct(k)}"></span>
  {/each}

  <span class="scrim" style="left: 0; width: {pct(inMs)}"></span>
  <span class="scrim" style="left: {pct(outMs)}; right: 0"></span>
  <span class="range" style="left: {pct(inMs)}; width: {ratioFromMs(Math.max(0, outMs - inMs), durationMs) * 100}%"></span>
  <span class="playhead" style="left: {pct(playheadMs)}"></span>

  <span
    class="handle"
    class:dragging={drag === 'in'}
    style="left: {pct(inMs)}"
    role="slider"
    tabindex="0"
    aria-label="Trim start"
    aria-valuemin={0}
    aria-valuemax={durationMs}
    aria-valuenow={inMs}
    onpointerdown={(e) => grab('in', e)}
    onpointermove={move}
    onpointerup={drop}
    onkeydown={(e) => onHandleKey('in', e)}></span>

  <span
    class="handle"
    class:dragging={drag === 'out'}
    style="left: {pct(outMs)}"
    role="slider"
    tabindex="0"
    aria-label="Trim end"
    aria-valuemin={0}
    aria-valuemax={durationMs}
    aria-valuenow={outMs}
    onpointerdown={(e) => grab('out', e)}
    onpointermove={move}
    onpointerup={drop}
    onkeydown={(e) => onHandleKey('out', e)}></span>
</div>

<style>
  .band {
    position: relative;
    height: 38px;
    border-radius: var(--r);
    background: var(--raised);
    border: 1px solid var(--line);
    cursor: pointer;
    touch-action: none;
  }
  .tick { position: absolute; top: 0; bottom: 0; width: 1px; background: var(--line-strong); }
  .scrim { position: absolute; top: 0; bottom: 0; background: var(--scrim-soft); pointer-events: none; }
  .range {
    position: absolute;
    top: 0;
    bottom: 0;
    background: color-mix(in srgb, var(--accent) 20%, transparent);
    border-left: 2px solid var(--accent);
    border-right: 2px solid var(--accent);
    pointer-events: none;
  }
  .playhead { position: absolute; top: 0; bottom: 0; width: 2px; background: var(--text); pointer-events: none; }
  .playhead::before {
    content: '';
    position: absolute;
    top: -1px;
    left: -3px;
    width: 8px;
    height: 5px;
    background: var(--text);
    border-radius: 0 0 2px 2px;
  }
  .handle {
    position: absolute;
    top: 50%;
    width: 15px;
    height: 15px;
    border-radius: 50%;
    background: var(--accent);
    border: 2px solid var(--text);
    transform: translate(-50%, -50%);
    cursor: ew-resize;
    transition: box-shadow var(--t-fast) var(--ease);
  }
  .handle:hover,
  .handle.dragging { box-shadow: 0 0 0 5px color-mix(in srgb, var(--accent) 22%, transparent); }
</style>
```

- [ ] **Step 6: Create `VideoPlayer.svelte`**

```svelte
<script lang="ts">
  import type { Snippet } from 'svelte';
  import { formatClock } from '../lib/timeline';
  import Icon from './ui/Icon.svelte';
  import IconButton from './ui/IconButton.svelte';

  let {
    src,
    ontime,
    timeline,
  }: {
    src: string;
    ontime: (ms: number) => void;
    /** The unified band, rendered inside the transport row. */
    timeline: Snippet;
  } = $props();

  let video = $state<HTMLVideoElement | null>(null);
  /** The wrapper, not the video: fullscreen on the `<video>` alone would take
      the transport row -- which is its sibling -- off screen. */
  let stage = $state<HTMLDivElement | null>(null);
  let playing = $state(false);
  let muted = $state(false);
  let full = $state(false);
  let positionMs = $state(0);
  let durationMs = $state(0);

  export function toggle() {
    if (!video) return;
    if (video.paused) void video.play();
    else video.pause();
  }

  export function seekTo(ms: number) {
    if (video) video.currentTime = ms / 1000;
  }

  $effect(() => {
    const onchange = () => (full = document.fullscreenElement === stage);
    document.addEventListener('fullscreenchange', onchange);
    return () => document.removeEventListener('fullscreenchange', onchange);
  });

  function toggleFull() {
    if (document.fullscreenElement) void document.exitFullscreen();
    else void stage?.requestFullscreen();
  }
</script>

<div bind:this={stage} class="stage">
  <!-- svelte-ignore a11y_media_has_caption -->
  <video
    bind:this={video}
    {src}
    autoplay
    onplay={() => (playing = true)}
    onpause={() => (playing = false)}
    onloadedmetadata={() => { if (video) durationMs = video.duration * 1000; }}
    ontimeupdate={() => {
      if (!video) return;
      positionMs = video.currentTime * 1000;
      ontime(positionMs);
    }}
    onclick={toggle}></video>

  <div class="transport">
    <button class="play" aria-label={playing ? 'Pause' : 'Play'} onclick={toggle}>
      <Icon name={playing ? 'pause' : 'play'} size={13} />
    </button>

    <div class="tl">{@render timeline()}</div>

    <span class="clock tnum">{formatClock(positionMs)} / {formatClock(durationMs)}</span>

    <IconButton
      icon={muted ? 'volume-mute' : 'volume'}
      label={muted ? 'Unmute' : 'Mute'}
      onclick={() => { muted = !muted; if (video) video.muted = muted; }} />
    <IconButton
      icon={full ? 'fullscreen-exit' : 'fullscreen'}
      label={full ? 'Leave fullscreen' : 'Fullscreen'}
      onclick={toggleFull} />
  </div>
</div>

<style>
  .stage { display: grid; gap: 12px; }
  .stage:fullscreen { background: var(--video-bg); align-content: center; padding: 0 24px 24px; }
  video {
    width: 100%;
    max-height: 58vh;
    background: var(--video-bg);
    border-radius: var(--r-lg);
    display: block;
    cursor: pointer;
  }
  .stage:fullscreen video { max-height: calc(100vh - 100px); }
  .transport { display: flex; align-items: center; gap: 13px; }
  .play {
    width: 36px;
    height: 36px;
    flex: 0 0 36px;
    display: grid;
    place-items: center;
    padding: 0;
    border: 0;
    border-radius: 50%;
    background: var(--accent);
    color: var(--accent-ink);
    cursor: pointer;
    transition: background var(--t-fast) var(--ease);
  }
  .play:hover { background: var(--accent-hi); }
  .tl { flex: 1; min-width: 0; }
  .clock { font-size: 11px; color: var(--faint); flex: 0 0 auto; }
</style>
```

- [ ] **Step 7: Delete the old trim bar**

```bash
git rm crates/trix-ui/web/src/components/TrimBar.svelte
```

`npm run check` will now fail on `ClipPage.svelte`'s import of it. That is
expected and Task 11 fixes it — **this task's gate is the unit tests only.**

- [ ] **Step 8: Verify the unit gate**

Run: `npx vitest run`
Expected: 145 passing (133 + 12 new).

- [ ] **Step 9: Commit**

```bash
git add crates/trix-ui/web/src/lib/timeline.ts crates/trix-ui/web/src/lib/timeline.test.ts crates/trix-ui/web/src/components/Timeline.svelte crates/trix-ui/web/src/components/VideoPlayer.svelte
git commit -m "feat(ui): one timeline for seeking and trimming"
```

---

## Task 11: The clip page

**Files:**
- Modify: `crates/trix-ui/web/src/views/ClipPage.svelte` (markup and styles; most of the `<script>` is preserved)

**Interfaces:**
- Consumes: `Timeline`, `VideoPlayer`, `Modal`, `Button`, `IconButton`, `Icon`, `formatClock`.
- Produces: nothing.

**Preserve verbatim — every one of these is a fix for a real bug:**

1. The `$effect` that re-fetches keyframes on `clipId` change, its
   `keyframeToken` guard, and the `keyframes = []` reset with its comment.
2. `clipId` / `clipDurationMs` being read off `clip` as primitives rather than
   tracking the object.
3. `canTrim`, `rangeError` and the `trimRangeError` import.
4. `exportTrim` in full, including the `exporting` double-fire guard.
5. The `Ctrl+E` branch **with its `!e.altKey` test and its entire comment.**
   Windows reports AltGr as Ctrl+Alt and this machine runs a Turkish Q layout,
   where AltGr+E is the euro sign — a `ctrlKey`-only test leaves a stray
   trimmed clip in the library from a press meant to type a character.
6. The `renaming` early-return branch in `onkeydown`, unchanged.
7. The `i` / `o` cases setting from `playheadMs`, not from a handle position.
8. `commitRename` and `startRename`.

**One preserved branch does change, and only in one line.** The
`confirmingDelete` branch must keep swallowing every key — window-level
shortcuts still must not act on another clip while the dialog is up — but it
must stop closing the dialog itself, because `Modal` owns Escape now.
Replace the branch with:

```ts
    // Every key is dropped here so the app-level shortcuts below (arrows,
    // space, delete) cannot act on a different clip while the dialog is up.
    // Escape is deliberately not handled: `Modal` closes itself, from a
    // handler bound to its own panel, and stops the event there -- so a
    // press inside the dialog never reaches this handler at all. This branch
    // is the guard for a key press that arrives from somewhere else entirely
    // while the dialog happens to be open.
    if (confirmingDelete) return;
```

- [ ] **Step 1: Replace the imports**

```ts
  import { app, trimRangeError } from '../lib/state.svelte';
  import { clipUrl, counterLabel, formatBytes, formatDuration } from '../lib/clips';
  import { formatClock } from '../lib/timeline';
  import { shouldHandleKey } from '../lib/keys';
  import Timeline from '../components/Timeline.svelte';
  import VideoPlayer from '../components/VideoPlayer.svelte';
  import Button from '../components/ui/Button.svelte';
  import IconButton from '../components/ui/IconButton.svelte';
  import Icon from '../components/ui/Icon.svelte';
  import Modal from '../components/ui/Modal.svelte';
```

- [ ] **Step 2: Swap the video handle for the player component**

Replace `let video = $state<HTMLVideoElement | null>(null);` with:

```ts
  let player = $state<VideoPlayer | null>(null);
```

Replace the whole `playPause` function with:

```ts
  function playPause() {
    player?.toggle();
  }
```

- [ ] **Step 3: Let a focused trim handle keep its arrows**

In `onkeydown`, immediately **above** the `if (!shouldHandleKey(...))` line:

```ts
    // A focused trim handle owns Left and Right -- In steps by keyframes,
    // Out by a tenth of a second. `Timeline` stops those events itself, so
    // this is belt and braces for the case where focus is on a handle but
    // the event was retargeted; everywhere else the arrows still step clips.
    if (
      (e.key === 'ArrowLeft' || e.key === 'ArrowRight') &&
      e.target instanceof Element &&
      e.target.getAttribute('role') === 'slider'
    ) {
      return;
    }
```

- [ ] **Step 4: Replace the markup and styles**

Everything from `{#if clip}` to the end of the file:

```svelte
{#if clip}
  <header class="head">
    <Button variant="ghost" size="sm" icon="chevron-left" onclick={() => (app.view = 'grid')}>
      Clips
    </Button>
    <span class="spacer"></span>
    <span class="counter tnum">{counterLabel(app.selected, app.clips.length)}</span>
    <IconButton icon="chevron-left" label="Previous clip"
      disabled={app.selected === 0} onclick={() => app.step(-1)} />
    <IconButton icon="chevron-right" label="Next clip"
      disabled={app.selected >= app.clips.length - 1} onclick={() => app.step(1)} />
  </header>

  <VideoPlayer
    bind:this={player}
    src={clipUrl(app.clipDir, clip.id)}
    ontime={(ms) => (playheadMs = ms)}>
    {#snippet timeline()}
      <!-- Hidden for a clip with no duration, which is the one case the
           daemon will refuse: `library::scan` adopts a bare .mp4 with
           `duration_ms: 0`, and `clamp_range` refuses it. Better not to offer
           the control than to hand back a refusal. -->
      {#if canTrim}
        <Timeline
          durationMs={clip.duration_ms}
          {playheadMs}
          {keyframes}
          {inMs}
          {outMs}
          onchange={(i, o) => { inMs = i; outMs = o; }}
          onseek={(ms) => player?.seekTo(ms)} />
      {/if}
    {/snippet}
  </VideoPlayer>

  {#if canTrim}
    <div class="trimrow">
      <span class="txt tnum" class:bad={rangeError !== null}>
        In <b>{formatClock(inMs)}</b> &middot; Out <b>{formatClock(outMs)}</b>
        &middot; <b>{formatClock(Math.max(0, outMs - inMs))}</b> selected{#if rangeError}
          &middot; {rangeError}{/if}
      </span>
      <span class="spacer"></span>
      <!-- `title` carries the reason a disabled button is disabled, and the
           readout to its left is the other half of the answer. Same rule as
           `exportTrim`'s refusal, from the same function, so the greyed
           button and Ctrl+E can never disagree about what is exportable. -->
      <Button
        variant="primary"
        icon="scissors"
        disabled={exporting || rangeError !== null}
        title={rangeError ?? ''}
        onclick={exportTrim}>
        {exporting ? 'Exporting…' : 'Export trimmed'}
      </Button>
    </div>
  {/if}

  <p class="meta tnum">
    {clip.width}x{clip.height} &middot; {clip.fps} fps &middot; {formatDuration(clip.duration_ms)} &middot;
    {formatBytes(clip.bytes)} &middot; {clip.encoder}{clip.has_audio ? ' + audio' : ''}
  </p>

  <div class="actions">
    {#if renaming}
      <input class="rn" bind:value={draftTitle}
        onkeydown={(e) => e.key === 'Enter' && commitRename()} />
      <Button variant="primary" size="sm" onclick={commitRename}>Save</Button>
      <Button variant="ghost" size="sm" onclick={() => (renaming = false)}>Cancel</Button>
    {:else}
      <!-- Star and title in one box: two siblings each carrying
           `margin-right: auto` would both claim the free space and push
           twice. -->
      <span class="titlebox">
        {#if clip.favorite}<span class="star"><Icon name="star-filled" size={14} /></span>{/if}
        <span class="title">{clip.title}</span>
      </span>
      <IconButton
        icon={clip.favorite ? 'star-filled' : 'star'}
        label={clip.favorite ? 'Unfavourite' : 'Favourite'}
        active={clip.favorite}
        onclick={() => app.setFavorite(clip.id, !clip.favorite)} />
      <IconButton icon="pencil" label="Rename" onclick={startRename} />
      <IconButton icon="folder" label="Show in Explorer" onclick={() => app.reveal(clip.id)} />
      <span class="sep"></span>
      <Button variant="danger" size="sm" icon="trash"
        onclick={() => (confirmingDelete = true)}>Delete</Button>
    {/if}
  </div>

  {#if confirmingDelete}
    <Modal title="Delete this clip?" onclose={() => (confirmingDelete = false)}>
      {#snippet children()}
        <p>Deleting <strong>{clip.title}</strong> removes the mp4, its metadata and its thumbnail. This cannot be undone.</p>
      {/snippet}
      {#snippet actions()}
        <Button variant="ghost" size="sm" onclick={() => (confirmingDelete = false)}>Keep</Button>
        <Button variant="danger" size="sm" icon="trash"
          onclick={async () => { confirmingDelete = false; await app.remove(clip.id); }}>Delete</Button>
      {/snippet}
    </Modal>
  {/if}
{/if}

<style>
  .head { display: flex; align-items: center; gap: 6px; margin-bottom: 12px; }
  .spacer { margin-left: auto; }
  .counter { color: var(--faint); font-size: 11px; margin-right: 4px; }

  .trimrow { display: flex; align-items: center; gap: 10px; margin-top: 12px; }
  .trimrow .txt { font-size: 11.5px; color: var(--dim); }
  .trimrow .txt b { color: var(--text); font-weight: 600; }
  /* An unexportable range reads as a sliver on the band and "0:00.0
     selected" in the numbers, neither of which says what is wrong. This does. */
  .trimrow .txt.bad, .trimrow .txt.bad b { color: var(--danger); }

  .meta { color: var(--faint); font-size: 11px; margin: 14px 0 12px; }

  .actions { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; }
  .titlebox { display: flex; align-items: center; gap: 6px; margin-right: auto; min-width: 0; }
  .actions .title { font-weight: 650; font-size: 13.5px; }
  .actions .star { color: var(--fav); display: flex; }
  .sep { width: 1px; height: 18px; background: var(--line); margin: 0 4px; }
  .rn {
    flex: 1;
    padding: 6px 10px;
    border-radius: var(--r);
    border: 1px solid var(--accent);
    background: var(--bg);
    color: var(--text);
    font: inherit;
  }
</style>
```

- [ ] **Step 5: Verify the gates**

Run: `npm run check && npx vitest run && npm run build`
Expected: check 0 errors / 0 warnings; vitest 145 passing; build succeeds.

The `TrimBar.svelte` import error from Task 10 is resolved by this task.

- [ ] **Step 6: Commit**

```bash
git add crates/trix-ui/web/src/views/ClipPage.svelte
git commit -m "feat(ui): clip page on the unified player"
```

---

## Task 12: Remaining surfaces and the keyboard sweep

**Files:**
- Modify: `crates/trix-ui/web/src/components/Toasts.svelte`
- Modify: `crates/trix-ui/web/src/views/DaemonDown.svelte`
- Modify: `crates/trix-ui/web/src/views/FirstRun.svelte`

**Interfaces:**
- Consumes: `Button`, `Icon`, `Select`, `KeycapInput`.
- Produces: nothing.

- [ ] **Step 1: Restyle `Toasts.svelte`**

```svelte
<script lang="ts">
  import { app } from '../lib/state.svelte';
  import Icon from './ui/Icon.svelte';
</script>

<div class="toasts">
  {#each app.toasts as toast (toast.id)}
    <div class="toast" class:error={toast.kind === 'error'}>
      {#if toast.kind === 'error'}<Icon name="alert" size={14} />{/if}
      <span>{toast.text}</span>
    </div>
  {/each}
</div>

<style>
  .toasts { position: fixed; right: 18px; bottom: 18px; display: grid; gap: 8px; z-index: 50; }
  .toast {
    display: flex;
    align-items: flex-start;
    gap: 9px;
    padding: 10px 14px;
    max-width: 46ch;
    border-radius: var(--r-md);
    background: var(--overlay);
    border: 1px solid var(--line-strong);
    box-shadow: var(--shadow);
    font-size: 12.5px;
    animation: slide var(--t-slow) var(--ease);
  }
  .toast.error { border-left: 3px solid var(--danger); }
  .toast.error :global(svg) { color: var(--danger); margin-top: 1px; }
  @keyframes slide { from { opacity: 0; transform: translateX(12px); } }
</style>
```

- [ ] **Step 2: Restyle `DaemonDown.svelte`**

Keep the entire `<script>` block. Replace only the markup and styles:

```svelte
<div class="down">
  <h2>Trix isn't running</h2>
  <p>The background service owns the replay buffer and the clip hotkey. Nothing is being captured right now.</p>
  <Button variant="primary" disabled={starting} onclick={start}>
    {starting ? 'Starting...' : 'Start Trix'}
  </Button>
</div>

<style>
  .down {
    display: grid;
    place-content: center;
    justify-items: center;
    gap: 12px;
    flex: 1;
    text-align: center;
    padding: 40px;
  }
  h2 { margin: 0; font-size: 18px; font-weight: 650; }
  p { margin: 0 0 6px; max-width: 42ch; color: var(--dim); font-size: 12.5px; }
</style>
```

Add to its `<script>` imports:

```ts
  import Button from '../components/ui/Button.svelte';
```

Note the `height: 100vh` became `flex: 1` — `App.svelte` now renders this
inside a column flex layout under the title bar, and a full-viewport height
here would push the window's own chrome off screen.

- [ ] **Step 3: Restyle `FirstRun.svelte`**

Keep the entire `<script>` block unchanged — every daemon call, the
`onDaemonEvent` subscription, the `onDestroy` unlisten, and `finish` with its
one-call `config.set` and its arm-failure comment.

Replace the monitor `<button>` list with `Select`, the readonly hotkey box
with `KeycapInput` in a non-capturing state, the path `<input>` with the same
`.path` span `Field.svelte` uses, and every `<button>` with `Button`:

```svelte
<div class="wizard">
  <h1>Set up Trix</h1>
  <p class="step tnum">Step {step} of 3</p>

  {#if step === 1}
    <h2>Which screen do you play on?</h2>
    <Select
      value={String(monitorIndex)}
      options={monitors.map((m) => ({
        value: String(m.index),
        label: `${m.name} - ${m.width}x${m.height} (${m.adapter})`,
      }))}
      label="Monitor"
      onchange={(v) => (monitorIndex = Number(v))} />
    <Button variant="primary" onclick={() => (step = 2)}>Next</Button>
  {:else if step === 2}
    <h2>Confirm your clip hotkey</h2>
    <!--
      Press-it-now is a best-effort check, not a guarantee: `heard` only ever
      flips if `hotkey_pressed` arrives, and that only fires when Windows
      actually gave the daemon this combination at startup. If another app
      already owns the combination (a capture overlay is a common culprit),
      this sits on "Waiting for a press..." forever. `Next` still advances
      regardless, so that is not a dead end -- but the wizard itself has no
      way to change the hotkey; that is Settings' job, after setup.
    -->
    <p class="hint">Press it now. Some overlays quietly take a hotkey inside games, so this checks Trix really gets it.</p>
    <KeycapInput combo={hotkey} capturing={false} oncapture={() => {}} onstart={() => {}} />
    <p class="hint" class:ok={heard}>{heard ? 'Trix received it.' : 'Waiting for a press...'}</p>
    <Button variant="primary" onclick={() => (step = 3)}>Next</Button>
  {:else}
    <h2>Where should clips go?</h2>
    <!-- Read-only for the same reason the Settings row is: the daemon owns
         the picker, and a typed path that does not exist is a refusal the
         user has to decode. There is no Reset here because the box already
         shows the default -- nothing has been changed away from yet. -->
    <div class="pathrow">
      <span class="path">{clipDir}</span>
      <Button onclick={pickFolder}>Choose...</Button>
    </div>
    <p class="hint">You can change this any time in Settings, or from the Trix tray icon.</p>
    <Button variant="primary" onclick={finish}>Finish and arm</Button>
  {/if}
</div>

<style>
  .wizard { width: min(560px, 100%); margin: 8vh auto; padding: 0 24px; display: grid; gap: 10px; justify-items: start; }
  h1 { font-size: 21px; font-weight: 650; margin: 0; }
  h2 { font-size: 15px; font-weight: 650; margin: 14px 0 4px; }
  .step { color: var(--faint); margin: 0; font-size: 11px; }
  .hint { color: var(--dim); font-size: 11.5px; margin: 2px 0; }
  .hint.ok { color: var(--live); }
  .pathrow { display: flex; align-items: center; gap: 8px; width: 100%; }
  .path {
    flex: 1;
    min-width: 0;
    padding: 7px 10px;
    background: var(--bg);
    border: 1px solid var(--line-strong);
    border-radius: var(--r);
    color: var(--dim);
    font-size: 12px;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
</style>
```

Add to its `<script>` imports:

```ts
  import Button from '../components/ui/Button.svelte';
  import KeycapInput from '../components/ui/KeycapInput.svelte';
  import Select from '../components/ui/Select.svelte';
```

- [ ] **Step 4: Verify every gate**

From `crates/trix-ui/web`:

```bash
npm run check && npx vitest run && npm run build
```

Expected: check 0 errors / 0 warnings; vitest 145 passing; build succeeds.

From the repo root:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt --check
```

Expected: 321 passed / 0 failed / 14 ignored; clippy no worse than the 10
warnings already on `master`; fmt clean.

- [ ] **Step 5: Confirm no stray native control survives**

```bash
grep -rn "type=[\"']range[\"']\|type=[\"']checkbox[\"']\|type=[\"']number[\"']\|<select\|<video[^>]*controls" crates/trix-ui/web/src --include=*.svelte
```

Expected: **no output.** Any hit is a control that missed the overhaul.

Then check that no screen still reaches for a token Task 1 removed:

```bash
grep -rn "var(--panel)" crates/trix-ui/web/src --include=*.svelte
```

Expected: **no output.** Task 1 replaced `--panel` with `--surface` /
`--raised` / `--overlay`, and every file that referenced it is rewritten by
Task 8, 9, 11 or 12 or deleted by Task 10. A hit means one of those rewrites
missed a rule, and it renders as a transparent background rather than as an
error — nothing else in the toolchain will catch it.

Then check that no literal colour survives outside `app.css`:

```bash
grep -rnE '#[0-9a-fA-F]{3,8}\b|rgba?\(|hsla?\(' crates/trix-ui/web/src --include=*.svelte
```

Expected: **no output.** `app.css` is the one file allowed to hold literal
colour values, because that is where the tokens are defined; every other file
reaches them through `var(--...)`, which is what `--include=*.svelte` above
encodes. This gate exists because the Task 4 sweep grepped only for `rgba(`
and two hex literals walked straight through it. Nothing else in the
toolchain objects to a hard-coded colour: it renders, and only looks wrong.

Then check that nothing sits above the first tag in any component:

```bash
for f in $(find crates/trix-ui/web/src -name "*.svelte"); do
  case "$(head -1 "$f")" in "<"*|"{"*|"") ;; *) echo "STRAY: $f -> $(head -1 "$f")";; esac
done
awk '/^```svelte\r?$/{getline; if ($0 !~ /^[<{]/) print FILENAME": "NR": "$0}' \
  docs/superpowers/plans/2026-08-24-trix-ui-overhaul.md
```

Expected: **no output.** Anything before the first `<script>` or `<style>` tag
in a `.svelte` file is template markup, so a stray line there renders on
screen. During Task 6 a bad patch left `e.stopPropagation();` as the first
line of `Select.svelte` and **all three gates passed**: it is valid markup to
`svelte-check`, this project has no component tests, and `vite build` has
nothing to object to. Only reading the file caught it. This is the one class
of defect the toolchain here is structurally blind to, so it gets its own
check.

- [ ] **Step 6: Full hand-verification (a person must do this)**

Build the real app:

```bash
cargo build --release --workspace
cd crates/trix-ui && cargo tauri build --no-bundle
```

With at least one clip of 5+ seconds in the library, confirm each:

- [ ] Window drags, double-click maximizes, all three window buttons work, and it resizes from every edge and corner.
- [ ] Maximized, the window does not overflow the screen.
- [ ] Arm and disarm from the title bar; the buffer meter fills green while armed and greys when not.
- [ ] Grid: arrows move the selection one visual row and column, Enter opens, Space previews in place.
- [ ] Card `⋯` opens on hover; Favourite, Rename and Delete all work; the amber star shows in the name row and is legible over a bright thumbnail.
- [ ] Renaming from a card: Enter commits, Escape cancels, and no keystroke leaks to the grid's shortcuts.
- [ ] Clip page: click the band to seek, drag both handles to trim, keyframe ticks visible.
- [ ] With a trim handle focused, Left/Right move that handle; with anything else focused, Left/Right step clips.
- [ ] `i` and `o` set the points from the playhead; `Space` plays and pauses; `Escape` returns to the grid.
- [ ] `Ctrl+E` exports. **`AltGr+E` types a character and does not export.**
- [ ] Fullscreen shows Trix's own controls, not Chromium's, and leaves cleanly.
- [ ] Delete raises the centred modal; Escape closes it; focus returns to the Delete button; while it is open no shortcut acts on another clip.
- [ ] Settings: every control is operable with the keyboard alone, Tab order is sane, focus rings appear on Tab but not on click.
- [ ] Dragging a volume slider produces **one** daemon round trip, not one per pixel. Watch the daemon log, or confirm the value only settles on release.
- [ ] The hotkey field captures a new combination and its Test says "Trix received it."
- [ ] The whole app works with the daemon stopped: the title bar renders and the window closes.

- [ ] **Step 7: Commit**

```bash
git add crates/trix-ui/web/src/components/Toasts.svelte crates/trix-ui/web/src/views/DaemonDown.svelte crates/trix-ui/web/src/views/FirstRun.svelte
git commit -m "feat(ui): toasts, daemon-down and first-run on the new primitives"
```

---

## Self-Review

**Spec coverage.** §4.1 tokens → Task 1. §4.2 radius/spacing/elevation → Task 1.
§4.3 typography → Task 1 (`body` font, `.tnum`) and applied per screen.
§4.4 motion and reduced-motion → Task 1. §4.5 focus → Task 1.
§5 icons → Task 2. §6.1–6.3 → Task 3. §6.7–6.8 → Task 4. §6.4, §6.6 → Task 5.
§6.5 → Task 6. §6.9 → Task 7. §7 window chrome and capabilities → Task 8.
§7.1 snap layouts → accepted, no task, restated in Risks below.
§8.1 title bar → Task 8. §8.2 rail → Task 8. §8.3 library → Task 9.
§8.4 clip page → Tasks 10 and 11. §8.5 settings → Task 7. §8.6 → Tasks 8 and 12.
§9 keyboard contract → enforced across Tasks 9, 10, 11 and swept in Task 12.
§10 file structure → the table above. §11 testing → each task's gate plus Task 12.

**Known soft spots, stated rather than hidden:**

- **Task 7 spans two files for one control.** `Field.svelte` declares an
  `onstartcapture` prop (Step 2) that `Settings.svelte` only supplies in Step 3.
  Between the two steps `npm run check` fails on a missing required prop. That
  is expected; the task's gate is Step 5, not each step.
- **Task 10 deliberately ends red.** It deletes `TrimBar.svelte` while
  `ClipPage.svelte` still imports it, so `npm run check` fails until Task 11.
  Task 10's gate is `npx vitest run` alone, and its step list says so.
- **No component has a unit test**, by the rule in the Global Constraints. If a
  reviewer wants component coverage they are asking for a new dependency, which
  the spec forbids. The compensating controls are `svelte-check` and Task 12's
  16-item hand list.
- **Task 8 cannot be fully verified by any command.** A missing Tauri capability
  produces a title bar whose buttons silently do nothing, and every automated
  gate passes. Step 8 exists for exactly that.
- **Test counts in the "Expected" lines assume 90 existing vitest cases**, the
  figure verified on `master` at v0.7.0. If the real baseline differs, the deltas
  (+15, +3, +2, +13) are what matter, not the totals.
