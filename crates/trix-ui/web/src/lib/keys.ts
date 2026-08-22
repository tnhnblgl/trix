/**
 * Tags a user types into. These keep every key, arrows included — a text
 * field must never lose a keystroke to a window-level shortcut sitting
 * behind it.
 */
const TYPING_TAGS = new Set(['INPUT', 'TEXTAREA', 'SELECT']);

/**
 * `<input>` types that are not text entry at all, and so are not covered by
 * the rule above. `INPUT` is a tag that means several unrelated controls, and
 * only some of them are a place a person types.
 *
 * This exists because of the trim bar. `TrimBar.svelte`'s In and Out handles
 * are `<input type="range">`, WebView2 focuses an input the moment it is
 * clicked, and a guard keyed on the tag alone therefore went dead for the rest
 * of the visit the first time anyone touched a handle: `o` no longer marked
 * the out point, Space no longer played, Escape no longer went back, Delete no
 * longer opened the confirm strip. Only Ctrl+E survived, because `ClipPage`
 * deliberately tests it above `shouldHandleKey`. Same shape of bug as the
 * `<button>` one that `ACTIVATABLE_TAGS` below exists for, on a different tag.
 *
 * The deliberate trade, chosen rather than inherited: `ArrowLeft`/`ArrowRight`
 * now belong to the page (prev/next clip) instead of nudging the focused
 * slider, so the sliders lose keyboard adjustment. That is the right way round
 * here — `step="1"` is one millisecond, so an arrow key moved the trim point
 * by an amount too small to see while silently eating the shortcut the user
 * meant, and `i`/`o` set both points from the playhead anyway, which is the
 * documented way to place them precisely. If a control ever needs its arrows
 * back, give it a bigger step and an exception, not a return to matching on
 * the tag.
 */
const NON_TYPING_INPUT_TYPES = new Set(['range', 'checkbox', 'radio', 'button']);

/**
 * The subset of the above that answers Space (and, for `button`, Enter)
 * itself, exactly as a `<button>` element does — so those two keys stay with
 * the control while every other key falls through.
 *
 * `range` is deliberately absent: a range input does nothing with Space, so on
 * the clip page Space must reach the page and play the video.
 */
const ACTIVATABLE_INPUT_TYPES = new Set(['checkbox', 'radio', 'button']);

/** An `<input>`'s `type`, lower-cased, or `null` for anything else. */
function inputType(el: { tagName?: unknown; type?: unknown }): string | null {
  if (typeof el.tagName !== 'string' || el.tagName.toUpperCase() !== 'INPUT') return null;
  // A real `HTMLInputElement` always reports a string here (`text` when the
  // attribute is absent); the DOM-less suite passes whatever it likes.
  return typeof el.type === 'string' ? el.type.toLowerCase() : 'text';
}

/**
 * Tags that answer Space and Enter themselves, and only those two.
 *
 * `BUTTON` is the one that actually bit, and the reason this is a second,
 * narrower set rather than folded into `TYPING_TAGS`. The rail's Arm control
 * is a real `<button>` and `Grid.svelte` listens on `<svelte:window>`, so
 * while the grid was mounted every key press in the app reached it: Space hit
 * the grid's `preventDefault` before the browser could activate the focused
 * button, which left a keyboard user unable to arm or disarm at all, and
 * Enter navigated away *without* preventing the default, so the button fired
 * too and one press both toggled arm and left the screen. But a button only
 * ever consumes Space and Enter — WebView2 moves focus to it on click, and a
 * blanket guard keyed on the tag alone then ate ArrowLeft/Right/Up/Down too,
 * killing the grid's own navigation for anyone who had just clicked a card.
 */
const ACTIVATABLE_TAGS = new Set(['BUTTON']);

/**
 * True when a key press is typed text that belongs to the control it landed
 * on, not a window-level shortcut sitting behind it.
 *
 * Duck-typed rather than `instanceof HTMLElement` because the suite runs on
 * node with no DOM, and the only two things this needs off an event target are
 * its tag name and whether it is being typed into.
 */
export function isTypingTarget(target: EventTarget | null): boolean {
  if (!target) return false;
  const el = target as { tagName?: unknown; isContentEditable?: unknown; type?: unknown };
  if (el.isContentEditable === true) return true;
  // Checked before the tag, because `INPUT` is in `TYPING_TAGS` and a slider,
  // a checkbox and a radio are all `INPUT` without being a place to type.
  const type = inputType(el);
  if (type !== null && NON_TYPING_INPUT_TYPES.has(type)) return false;
  return typeof el.tagName === 'string' && TYPING_TAGS.has(el.tagName.toUpperCase());
}

/**
 * True when the target is a control that natively consumes Space and Enter
 * itself — a `<button>`, or one of the `<input>` types that behaves like one.
 * Every other key, arrows included, must fall through to whatever
 * window-level handler sits behind it.
 *
 * The input types are here for the same reason they are excluded from
 * `isTypingTarget`: once a checkbox stops being treated as text entry it stops
 * getting the free pass that kept Space with it, and Space is the one key a
 * checkbox genuinely owns. Nothing in the app hits that today (`Field.svelte`
 * holds the only checkbox and lives on the settings view, which mounts no
 * window-level key handler) — it is here so the next control that does is
 * right by default.
 */
export function isActivatableTarget(target: EventTarget | null): boolean {
  if (!target) return false;
  const el = target as { tagName?: unknown; type?: unknown };
  const type = inputType(el);
  if (type !== null) return ACTIVATABLE_INPUT_TYPES.has(type);
  return typeof el.tagName === 'string' && ACTIVATABLE_TAGS.has(el.tagName.toUpperCase());
}

/**
 * Whether a window-level keydown handler should act on this key press,
 * rather than leaving it to the control the event landed on.
 *
 * Two rules hold for every caller in the app:
 *
 * - Typed text always belongs to the field it landed in. *Typed* text: a
 *   slider, a checkbox and a radio are `<input>` too and are not covered,
 *   because nothing is typed into them and WebView2 focuses them on click —
 *   see `NON_TYPING_INPUT_TYPES` for the trim-bar bug that made this explicit
 *   and for the trade it accepts.
 * - A `<button>` owns Space and Enter itself and must keep them — WebView2
 *   focuses it on click, so a handler that also claims those keys both fires
 *   its own action *and* suppresses the button's native activation. Arrow
 *   keys are never part of what a button owns, so they always fall through.
 *
 * `ownsActivation` lets one caller declare an exception to the second rule
 * for a specific target: `Grid.svelte`'s cards are `<button>`s too, but
 * inside the grid their Space and Enter belong to the grid itself (spec
 * §6.2) — a card left to activate itself would re-select the clip that is
 * already selected instead of previewing or opening it. Every other caller
 * (`ClipPage.svelte` among them) has no such exception and leaves this at
 * its default of `false`, so every button on the page keeps its own Space
 * and Enter.
 *
 * `ownsActivation` is passed in rather than read off the target because the
 * suite runs on node with no DOM; the grid's caller owns the `contains`
 * check that decides it.
 */
export function shouldHandleKey(
  target: EventTarget | null,
  key: string,
  ownsActivation = false,
): boolean {
  if (isTypingTarget(target)) return false;
  const activates = key === ' ' || key === 'Enter';
  return !(isActivatableTarget(target) && activates && !ownsActivation);
}

/**
 * Where the selection lands after an arrow key in a grid `columns` wide.
 *
 * Clamps rather than wraps, on purpose. The grid is newest-first and `Del` acts
 * on the selection, so wrapping from the newest clip to the oldest puts the
 * clip you care least about under a destructive key.
 */
export function moveSelection(
  current: number,
  key: string,
  count: number,
  columns: number,
): number {
  if (count === 0) return 0;
  const delta =
    key === 'ArrowRight' ? 1
    : key === 'ArrowLeft' ? -1
    : key === 'ArrowDown' ? columns
    : key === 'ArrowUp' ? -columns
    : 0;
  if (delta === 0) return current;
  const next = current + delta;
  return next < 0 || next >= count ? current : next;
}
