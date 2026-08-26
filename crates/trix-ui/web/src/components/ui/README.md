# The drawn control library

Ten components: `Button`, `Icon`, `IconButton`, `KeycapInput`, `Menu`, `Modal`,
`Select`, `Slider`, `Stepper`, `Toggle`. Everything in here is drawn rather than
native, because the native controls WebView2 gives us cannot be made to match
the rest of the app and half of them ignore `color-scheme: dark` in one detail
or another.

Nothing in this folder calls the daemon, reads `app` state, or knows what a clip
is. A control is handed values and reports back; `state.svelte.ts` owns every
side effect. That is what lets `Field.svelte` render sixteen different settings
through six of these without any of them knowing about config keys.

This file is the conventions, written down after a whole-branch review found the
API drifting. **It describes what is there, not what would be ideal** — where a
convention is broken on purpose, or simply is not uniform yet, it says so.

---

## Naming

A control with no visible text takes a **required `label: string`** prop and
renders it as `aria-label`. That is `IconButton`, `KeycapInput`, `Select`,
`Slider`, `Stepper` and `Toggle`. It is required, not optional, because there is
nothing else in the markup that could name the control — an optional one would
ship unnamed.

`IconButton` also puts `label` in `title`, so the same string is the tooltip.

`Stepper` derives two more names from it: `"{label}: decrease"` and
`"{label}: increase"` for its `-` and `+` buttons.

The three that carry their own text do not take `label`: `Button` takes
`children`, `Menu` takes `items[].label`, `Modal` takes `title`.

`Icon` is the exception with no name at all: it is `aria-hidden` unconditionally,
because something outside it always carries the name. See its own doc comment.

## `disabled`

`Button`, `IconButton`, `Toggle` and `Slider` take `disabled`. `Select`,
`Stepper` and `KeycapInput` do not.

That is deliberate rather than an oversight to be filled in: no caller passes it.
`Field.svelte` is the only place a settings control could need greying out, and
it only ever hands `disabled` to `Button`. Add the prop to the others when
something actually needs it, and add the disabled styling in the same change —
not before.

`Slider` is a `<div role="slider">`, not a form control, so `:disabled` never
matches it. It carries a `.disabled` class and `aria-disabled` instead, and its
hover rules are guarded on `:not(.disabled)`. Its own comment explains why.

## `size` — two different shapes, on purpose left alone

- `Button`: `'sm' | 'md'`, a padding preset (`5px 10px` / `7px 12px`). It also
  picks the icon size, 13px or 14px.
- `IconButton` and `Icon`: a **number**, the pixel size of the glyph. The
  button's own 30px box does not change.

Those are two different meanings of one prop name and it is a wart. It is
documented rather than unified because unifying it is an API break across every
call site with no component test anywhere in this project to catch a missed one.
If it is ever unified, `'sm' | 'md'` is the shape to keep and the pixel number
should become an `icon` size of its own.

## Focus

`app.css` styles `:focus-visible` globally — a 2px `--accent` outline — and that
is the whole focus idiom. A component must not add its own focus ring, and must
not key one off `:focus` or `:focus-within`: both of those match a pointer click
and leave a ring sitting behind the cursor.

Where a component needs to ring a *container* around the focused element, the
selector is `:has(:focus-visible)` (see `Stepper`). Where a component genuinely
wants "something inside me has focus" for a non-ring purpose — revealing a
hidden control, say — `:focus-within` is correct; `ClipCard`'s `⋯` uses it.

## Buttons

Every `<button>` in this folder carries `type="button"`. There is no `<form>` in
this app, so nothing is submitting today; the attribute is there so that adding
one later cannot silently turn half the UI into submit buttons. The five bespoke
buttons outside this folder — the clip card thumbnail, the title bar's arm pill,
the player's play button and the rail's two nav items — carry it too.

## Callbacks

Callbacks are props, always: `onclick`, `onchange`, `oninput`, `onpick`,
`onclose`, `oncapture`, `onstart`. No `createEventDispatcher`, no bubbling
custom events. Where a control distinguishes the two, `oninput` fires
continuously and is display-only, and `onchange` fires once at the end and is
the one that may reach the daemon (`Slider` documents this).

None of these declares a bindable prop. Values go down as props and come back
through a callback, so that the caller decides whether a change is accepted —
which matters when the daemon can refuse one. (`bind:this` on a DOM node inside
a component is a different thing and several of them use it.)

## Styling

Styles are scoped to the component. A class name in here is private: nothing
outside may select it.

**One known exception:** `TitleBar.svelte` reaches into `IconButton`'s `.ib`
through `:global` to square off the window-control buttons to the Windows
standard 44px. It is left as it is rather than grown into a prop, but it is the
only one, and a second should be a prop instead.

Colour literals live in `app.css` and nowhere else in `src`. The semantic tokens
each mean exactly one thing: `--accent` "you selected this", `--live` "Trix is
live" (title bar and settings only), `--danger` destructive or broken, `--fav`
kept — `--live` is spent in `TitleBar.svelte` and `Settings.svelte` and nowhere
else, and `--fav` only in `ClipCard.svelte`. A control in this folder should not
need any of the three.

## Logic

This project has no component test framework and is not taking one, so anything
in a `.svelte` file is untested by construction. Arithmetic therefore goes to
`lib/ui.ts` and is tested there — `clamp`, `snapToStep`, `ratioToValue`,
`valueToRatio`, `stepBy`, `resolveStepperInput`, `formatCombo`, `nextIndex` all
exist because a primitive needed them. A `.svelte` file that is growing a
calculation is a file that is growing an untested branch.

The same rule sends keyboard-ownership decisions to `lib/keys.ts`. Note that the
suite runs on node with no DOM, so `lib/` duck-types event targets rather than
using `instanceof Element`.
