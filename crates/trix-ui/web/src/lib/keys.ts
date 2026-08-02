/**
 * Tags a user types into. These keep every key, arrows included — a text
 * field must never lose a keystroke to a window-level shortcut sitting
 * behind it.
 */
const TYPING_TAGS = new Set(['INPUT', 'TEXTAREA', 'SELECT']);

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
  const el = target as { tagName?: unknown; isContentEditable?: unknown };
  if (el.isContentEditable === true) return true;
  return typeof el.tagName === 'string' && TYPING_TAGS.has(el.tagName.toUpperCase());
}

/**
 * True when the target is a control that natively consumes Space and Enter
 * itself — a `<button>`. Every other key, arrows included, must fall through
 * to whatever window-level handler sits behind it.
 */
export function isActivatableTarget(target: EventTarget | null): boolean {
  if (!target) return false;
  const el = target as { tagName?: unknown };
  return typeof el.tagName === 'string' && ACTIVATABLE_TAGS.has(el.tagName.toUpperCase());
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
