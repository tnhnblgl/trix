# Hotkey capture modes — feasibility note

**Status: built, 2026-09-07, on `feat/hotkey-modes`.** Written 2026-08-26 at
v0.8.0, in answer to "some games need a low-level `SetWindowsHookEx` keybind,
but some games wrongly understand it — could that be a setting, and how hard is
it?". It was a feasibility note, never a spec; the section below records how
each of its open decisions was actually settled, and the rest of the file is
left as written so the reasoning that led here is still readable.

## How it shipped

One `hotkey_mode` config key covering **both** hotkeys — clip and screenshot —
because the choice is about the mechanism, not about a key, and two settings
would only invite the half-configured state where the answer is yes for clips
and no for screenshots. `"standard"` (`RegisterHotKey`, the shipped behaviour)
is the default; `"low_level"` (`WH_KEYBOARD_LL`) is opt-in. It applies live,
with no re-arm, exactly like a rebind of either combination.

The estimate held: the pump thread was already there, so the work was a
`hook.rs`, two accessors on `Hotkey`, one config key, one Settings row, and one
shared registration path in `window.rs` replacing the two that had drifted
apart. Both `wnd_proc` hotkey arms collapsed into one, so the two mechanisms
cannot diverge in what a press *does*.

The open decisions, as settled:

1. **Did step 1 (visibility) happen first?** No — the mode landed first, and
   step 1 followed immediately after in the same branch, so both shipped
   together. It is now built end to end:
   - `window.rs` records every bind outcome in a **tri-state** — untried,
     bound, taken. Three states rather than a bool because `spawn` returns
     before the pump binds, and a `status` answered in that window has
     genuinely not been told yet; reported as `false` it would flash "your
     hotkey is taken" at every launch.
   - `status` carries `clip_hotkey_bound` and `screenshot_hotkey_bound`, each
     `true`/`false`/`null`. Adding keys to that published set is safe;
     renaming or dropping one is not, and the wire-shape test still guards it.
   - The `hotkey_rebound` event gained `key`, so a page with two hotkey rows
     knows which one an outcome is about instead of comparing spec strings.
   - Settings prints the reason under the offending row, in `--danger`, from
     `hotkeyProblem` in `settings.ts` — a pure function so the sentence a
     stuck user reads is pinned by tests rather than buried in markup. The
     advice inverts with the mode: standard mode's failure means another
     program owns the key, low-level mode's means the hook would not install,
     and telling the second user to "try Low level" would be telling them to
     do what they already did.
   - `clip_hotkey`'s help now ends "if the test never lights up, try Hotkey
     detection below", which is what gets a stuck user to the dropdown at all.
2. **Are the real-world failures elevated games?** Still unconfirmed, and the
   note's warning stands: a hook does not bypass UIPI. The Settings copy says
   so in the user's own words ("Neither mode helps if the game runs as
   administrator and Trix does not"), and a test asserts that sentence stays
   there — so if elevation *is* the real cause, the user finds that out from
   the setting instead of from a mode that quietly does nothing.
3. **Pass through or swallow?** **Pass through**, decided by the user. The hook
   calls `CallNextHookEx` on every event without exception, including the ones
   it acts on. The reasoning: this mode is an escape hatch someone reaches for
   when their key is broken, so it must only ever *add* behaviour — never
   silently take a shortcut away from another program or from the game. The
   accepted cost is double-firing on a contested combination (NVIDIA's replay
   *and* a Trix clip on Alt+F10), which is visible and explicable in a way a
   broken overlay shortcut would not be.
4. **Default and wording.** Off by default. `Config::hotkey_mode`'s fallback
   direction is a safety property, not a convenience: a typo in config.toml
   must never be what installs a keyboard hook. The Settings help states the
   anti-cheat caveat plainly, and `settings.test.ts` fails the build if the
   words "better", "recommended" or "improved" ever appear in it.

Two things deliberately **not** built. There is no liveness check for a hook
Windows removed for overrunning `LowLevelHooksTimeout`: the callback does a
few integer comparisons and one `PostMessageW`, and `window.rs`'s standing
rule that the pump never blocks is what keeps it that way — if silent death is
ever observed, that is the moment to add one, not before. And `trix replay`
(`control::start_hotkey`) is still `RegisterHotKey`-only; it runs its own
thread and pump, and the daemon is what users actually run.

## What was asked for

A Settings choice between two ways of catching the clip key:

| Mode | Mechanism |
|---|---|
| Current | `RegisterHotKey` — the system hotkey table |
| "Hardware level" | `WH_KEYBOARD_LL` — a global low-level keyboard hook |

The stated motivation is games where the current mode does not fire, balanced
against anti-cheat software misreading a keyboard hook.

## Verified facts about this codebase (checked 2026-08-26, v0.8.0)

- **One backend, and it is `RegisterHotKey`.** `Hotkey::register`
  (`crates/trix-core/src/control.rs:178`) calls it with
  `modifiers | MOD_NOREPEAT`, bound to the daemon's message-only window.
  `HOTKEY_ID` is the constant `1` (`crates/trix-daemon/src/window.rs:126`).
- **`WM_HOTKEY` lands in `wnd_proc`** (`window.rs:350`), which broadcasts
  `hotkey_pressed` and queues the clip. Startup registration is at
  `window.rs:548`; rebinding goes `rebind_hotkey` (`window.rs:243`) →
  `WM_TRIX_REHOTKEY` → the arm at `window.rs:384`.
- **`Hotkey`'s fields are private** (`control.rs:109`): `modifiers` is a
  `HOT_KEY_MODIFIERS` bitfield, `vk` a virtual-key code, `pretty` the display
  string. Nothing outside can read them, because nothing has needed to — the
  only consumer hands them straight to `RegisterHotKey`.
- **`Hotkey::parse`** (`control.rs:119`) accepts `[ctrl+][alt+][shift+][win+]key`
  where key is `a`–`z`, `0`–`9` or `f1`–`f24`. Letters and digits require at
  least one modifier; **function keys do not**, which is why a bare `f10` is a
  legal and currently-shipping value.
- **There is no hook or raw-input code anywhere in the tree.**
  `SetWindowsHookEx`, `WH_KEYBOARD`, `RegisterRawInputDevices` and `RIDEV_*`
  all appear zero times.
- **The daemon already runs a message pump** (`window.rs`, the message-only
  window that owns the tray icon and the hotkey). This matters more than
  anything else below — see "Why it is cheap".
- **A failed registration reaches nobody.** `window.rs:553` logs one
  `tracing::warn!` — "clip hotkey unavailable — clip from the tray, or rebind
  clip_hotkey" — and that is the entire response. There is no event, no status
  field, no UI surface: grepping for `hotkey_unavailable`, `hotkey_failed`,
  `hotkey_registered` and friends across `crates/` returns nothing.
- **The only feedback that exists is manual and opt-in.** `Settings.svelte:32`
  listens for `hotkey_pressed` while the keycap field is listening, so a user
  who goes to Settings and presses the combination sees it confirmed. It cannot
  distinguish "another program owns this" from "you did not press it properly",
  and it says nothing at all until someone goes looking.
- `settings.ts`'s help for `clip_hotkey` already warns "Overlays can silently
  take a hotkey inside games" — so the hazard was known and accepted, not
  overlooked.

## Why it is cheap

A `WH_KEYBOARD_LL` hook must be installed from a thread that pumps messages,
and the callback is delivered on that thread. **The daemon already has exactly
that thread.** In most projects standing up that pump is the bulk of the work;
here it is already paid for, tested, and carrying the tray icon.

What is actually left:

- A `hook.rs` in `trix-daemon`, roughly 150–200 lines: install, callback,
  uninstall, and a match against the configured combination.
- A small addition to `Hotkey` exposing `vk` and the modifier set for
  comparison. The hook cannot reuse `modifiers` directly — `MOD_CONTROL` and
  friends are hotkey-table flags, not virtual-key codes, so the callback has to
  test the live modifier state (`GetAsyncKeyState` on
  `VK_CONTROL`/`VK_MENU`/`VK_SHIFT`/`VK_LWIN`/`VK_RWIN`) itself.
- A `clip_hotkey_mode` config key, a Settings select row, and swapping the two
  registration sites (`window.rs:548` and the `WM_TRIX_REHOTKEY` arm).
- Tests: "does this key event match this combination" is pure and unit-testable.
  Installing a real hook is not, and would not be tested.

**Estimate: about a day**, comparable to the static Discord presence, with more
risk surface because it sits in the input path.

## The costs that are not effort

### 1. It does not fix elevated games

A low-level hook does **not** bypass UIPI. If the foreground window belongs to
a higher-integrity process and the daemon is not elevated, the hook is as blind
as `RegisterHotKey` is. This is why AutoHotkey — which uses exactly this
mechanism — tells users to run it as administrator to work with elevated
windows.

So if the real-world failures are elevated games, **the answer is elevating the
daemon, not changing the hook mode**, and that is a different and larger
decision (a UAC prompt at every launch, or a scheduled task, and a change to
what autostart means). This should be established before building the mode,
because it decides whether the mode solves the actual problem.

*This is a reading of documented Windows behaviour, not something measured
against this codebase. It is the single most important thing in this note to
confirm before building.*

### 2. What it genuinely does fix

**Another program already owning the combination.** `RegisterHotKey` fails
outright in that case; a low-level hook still sees the key. This is the
NVIDIA App / `alt+f10` collision recorded in the project's own history, and it
is a real case rather than a hypothetical one.

### 3. Blast radius

A bug in a low-level keyboard callback makes the whole machine's keyboard laggy
or eats keystrokes system-wide. Nothing Trix ships today can misbehave that far
outside its own process, and the product's pitch is that it does not cost you
anything — system-wide input latency is a brand problem, not only a bug.

### 4. Silent death

Windows removes a hook whose callback overruns `LowLevelHooksTimeout`
(300 ms by default). If the pump ever stalls, the hotkey stops working with no
error anywhere — the same invisibility problem as today, arrived at differently.
A liveness check and reinstall would be part of the work.

### 5. Suppression changes behaviour

`RegisterHotKey` swallows the combination from the foreground application. A
hook can pass through or swallow, and one has to be chosen. Passing through
means the game also receives the key — which for a bare `f10`, a common in-game
binding, is a visible difference either way it goes.

### 6. Anti-cheat

A plain low-level keyboard hook appears to be broadly tolerated: OBS, Discord,
Steam, MSI Afterburner and Medal all ship one. That is precedent, not a
guarantee, and it is not a claim about any specific anti-cheat.

The asymmetry is what matters: **a bug we can fix in a patch; a ban we cannot
undo.** The risk lands on the user, not on us. That argues for opt-in, off by
default, and wording that presents a tradeoff rather than implying a "better"
mode.

### A third option, considered

Raw Input with `RIDEV_INPUTSINK` receives keys without a hook and carries no
keylogger signature. It is also UIPI-bound, cannot suppress the key, and needs
its own window plumbing — so it trades the anti-cheat question for a different
set of limits without solving the elevation one either. Not obviously better;
worth a second look only if the anti-cheat risk turns out to be the blocker.

## Assessment: worth building, but not first

The recommendation is an **ordering**, not a yes or a no.

**The larger defect today is the silence, not the missing mode.** When
registration fails, the user's experience is that the key does nothing, forever,
with no explanation available anywhere they would look. A mode toggle does not
help someone who cannot tell why their key is dead — and if they do find the
dropdown, they are flipping a switch with real risk attached, on a guess.

1. **Make the failure visible.** A few hours. The daemon reports whether the
   hotkey actually bound; Settings says "F10 is taken by another program".
   No new attack surface, and it turns a silent dead key into a fixable one.
   Most users are then one rebind away from working, at zero risk.
2. **Then the hook mode.** Once someone is told the combination is taken, the
   next question is "but I do not want to change my key" — which is precisely
   what the hook answers, and it is then a deliberate choice rather than a
   guess.

The safest fix for a stolen combination is picking a free one: it costs nothing
and cannot get anybody banned. The hook earns its place only for the user who
insists on a combination another program owns. That user is real, but it is the
narrower case, and it is the one carrying the system-wide latency risk and the
anti-cheat ambiguity.

## Open decisions (settle these before writing a spec)

1. **Does step 1 happen first?** If the mode ships without it, the users who
   need it will not know to reach for it.
2. **Are the real-world failures elevated games?** If they are, this feature
   does not fix them and the elevation question is the one to answer instead.
   Worth confirming against an actual failing game before building anything.
3. **Pass through or swallow in hook mode?** They are different products for a
   bare function key.
4. **Default and wording.** The recommendation is off by default, worded as a
   tradeoff, never as a "better" mode.

## Files a build would touch

Step 1 (visibility):

- Modify: `crates/trix-daemon/src/window.rs` (report the registration outcome
  rather than only logging it)
- Modify: `crates/trix-daemon/src/state.rs` + `dispatch.rs` (carry it in
  `status`, or broadcast it)
- Modify: `crates/trix-ui/web/src/views/Settings.svelte` (say so)

Step 2 (the mode):

- Create: `crates/trix-daemon/src/hook.rs` (~150–200 lines), registered in
  `crates/trix-daemon/src/lib.rs`
- Modify: `crates/trix-core/src/control.rs` (expose `vk` and the modifier set
  on `Hotkey`)
- Modify: `crates/trix-daemon/src/window.rs` (both registration sites)
- Modify: `crates/trix-core/src/config.rs` (`clip_hotkey_mode` + default)
- Modify: `crates/trix-ui/web/src/lib/settings.ts` (a select row, `Trix`
  section)
