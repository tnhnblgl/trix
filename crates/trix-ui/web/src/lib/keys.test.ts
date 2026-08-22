import { describe, expect, it } from 'vitest';
import { shouldHandleKey, isActivatableTarget, isTypingTarget, moveSelection } from './keys';

/** An event target, as much of one as a DOM-less suite needs. */
const target = (props: Record<string, unknown>) => props as unknown as EventTarget;

describe('isTypingTarget', () => {
  it('keeps every key for text entry, arrows included', () => {
    expect(isTypingTarget(target({ tagName: 'INPUT' }))).toBe(true);
    expect(isTypingTarget(target({ tagName: 'TEXTAREA' }))).toBe(true);
    expect(isTypingTarget(target({ tagName: 'SELECT' }))).toBe(true);
    expect(isTypingTarget(target({ tagName: 'DIV', isContentEditable: true }))).toBe(true);
  });

  it('lets the grid keep the keys nothing else wanted', () => {
    expect(isTypingTarget(target({ tagName: 'DIV' }))).toBe(false);
    expect(isTypingTarget(target({ tagName: 'BODY', isContentEditable: false }))).toBe(false);
    // `window` itself is the target when nothing is focused, and it has no
    // tagName at all — the case every grid shortcut actually runs in.
    expect(isTypingTarget(target({}))).toBe(false);
    expect(isTypingTarget(null)).toBe(false);
    // A button is activatable, not a typing target — it must not get the
    // free pass on arrow keys that `TYPING_TAGS` grants.
    expect(isTypingTarget(target({ tagName: 'BUTTON' }))).toBe(false);
  });

  it('does not treat an input that takes no text as text entry', () => {
    // The trim bar's In and Out handles. WebView2 focuses an input on click,
    // so before this every app shortcut on the clip page died the moment a
    // handle was touched — `o` stopped marking the out point, Space stopped
    // playing, Escape stopped going back.
    expect(isTypingTarget(target({ tagName: 'INPUT', type: 'range' }))).toBe(false);
    expect(isTypingTarget(target({ tagName: 'INPUT', type: 'checkbox' }))).toBe(false);
    expect(isTypingTarget(target({ tagName: 'INPUT', type: 'radio' }))).toBe(false);
    expect(isTypingTarget(target({ tagName: 'INPUT', type: 'button' }))).toBe(false);
    // Case-folded, since the property is only normalized on a real element.
    expect(isTypingTarget(target({ tagName: 'input', type: 'RANGE' }))).toBe(false);
  });

  it('still keeps every key for the input types that do take text', () => {
    // The rename field on the clip page and the numeric settings fields. An
    // `<input>` with no type attribute is a text field.
    expect(isTypingTarget(target({ tagName: 'INPUT', type: 'text' }))).toBe(true);
    expect(isTypingTarget(target({ tagName: 'INPUT', type: 'number' }))).toBe(true);
    expect(isTypingTarget(target({ tagName: 'INPUT' }))).toBe(true);
    // `type` belongs to `<input>` alone: `<textarea>` reports 'textarea' and
    // `<select>` reports 'select-one', and neither must be read as a slider.
    expect(isTypingTarget(target({ tagName: 'TEXTAREA', type: 'textarea' }))).toBe(true);
    expect(isTypingTarget(target({ tagName: 'SELECT', type: 'select-one' }))).toBe(true);
  });
});

describe('isActivatableTarget', () => {
  it('is true only for the tag that natively consumes Space and Enter', () => {
    expect(isActivatableTarget(target({ tagName: 'BUTTON' }))).toBe(true);
    expect(isActivatableTarget(target({ tagName: 'DIV' }))).toBe(false);
    expect(isActivatableTarget(target({}))).toBe(false);
    expect(isActivatableTarget(null)).toBe(false);
  });

  it('covers the input types that behave like a button, but not the slider', () => {
    // Once a checkbox stops counting as text entry it stops getting the free
    // pass that kept Space with it, and Space is the one key it really owns.
    expect(isActivatableTarget(target({ tagName: 'INPUT', type: 'checkbox' }))).toBe(true);
    expect(isActivatableTarget(target({ tagName: 'INPUT', type: 'radio' }))).toBe(true);
    expect(isActivatableTarget(target({ tagName: 'INPUT', type: 'button' }))).toBe(true);
    // A range input does nothing with Space, so Space must reach the page and
    // play the video — the clip page's most-used key, at the control the user
    // most recently clicked.
    expect(isActivatableTarget(target({ tagName: 'INPUT', type: 'range' }))).toBe(false);
    // A text input is a typing target instead; it must not also claim to be
    // activatable, or Enter in the rename field would take a second rule.
    expect(isActivatableTarget(target({ tagName: 'INPUT', type: 'text' }))).toBe(false);
    expect(isActivatableTarget(target({ tagName: 'INPUT' }))).toBe(false);
  });
});

describe('shouldHandleKey', () => {
  // Calls the shipped function rather than restating its expression: a mirror
  // of the guard would keep passing after the guard itself was changed, which
  // is the failure mode these cases exist to catch. `Grid.svelte` and
  // `ClipPage.svelte` are both real callers of this one predicate — the grid
  // passes `ownsActivation`, the clip page leaves it at its default.
  const card = target({ tagName: 'BUTTON' });
  const arm = target({ tagName: 'BUTTON' });

  it('still lets arrow keys through a focused clip card', () => {
    // The regression this split exists to prevent. A card is a real `<button>`
    // (ClipCard.svelte) and WebView2 focuses it on click — the primary way a
    // user selects a clip — so a guard keyed on the tag alone went dead on
    // every grid key the moment anyone clicked anything.
    for (const key of ['ArrowRight', 'ArrowLeft', 'ArrowUp', 'ArrowDown']) {
      expect(shouldHandleKey(card, key, true)).toBe(true);
      expect(shouldHandleKey(arm, key, false)).toBe(true);
    }
  });

  it('keeps Space and Enter for a card, because spec §6.2 gives them to the grid', () => {
    // Left to itself the card would fire its own `onclick={onselect}` and
    // merely re-select the clip that is already selected — no preview, no
    // open. Inside the grid, the grid decides.
    expect(shouldHandleKey(card, ' ', true)).toBe(true);
    expect(shouldHandleKey(card, 'Enter', true)).toBe(true);
  });

  it('leaves Space and Enter to a button with no ownsActivation exception', () => {
    // The rail's Arm control, and every button on the clip page. Space must
    // activate the button natively rather than being claimed by a
    // window-level shortcut, and Enter there must not also navigate away.
    expect(shouldHandleKey(arm, ' ', false)).toBe(false);
    expect(shouldHandleKey(arm, 'Enter', false)).toBe(false);
  });

  it('defaults ownsActivation to false when the caller omits it', () => {
    // `ClipPage.svelte` calls this two-argument form — every button on that
    // page owns its own Space and Enter, so there is no exception to opt into.
    expect(shouldHandleKey(arm, ' ')).toBe(false);
    expect(shouldHandleKey(arm, 'Enter')).toBe(false);
    expect(shouldHandleKey(arm, 'ArrowRight')).toBe(true);
  });

  it('never takes a key from a text field, wherever it sits', () => {
    const field = target({ tagName: 'INPUT' });
    expect(shouldHandleKey(field, 'ArrowRight', false)).toBe(false);
    expect(shouldHandleKey(field, ' ', false)).toBe(false);
    // Even inside the grid: a rename box is still a rename box.
    expect(shouldHandleKey(field, 'ArrowRight', true)).toBe(false);
  });

  it('gives the clip page back every shortcut a focused trim handle used to eat', () => {
    // The whole point of the input-type split. A user drags In, so the slider
    // has focus, and then reaches for the keyboard: `o` marks the out point,
    // Space plays, Escape returns to the grid, Delete opens the confirm strip,
    // and the arrows step to the next clip rather than nudging the value they
    // just set by one millisecond.
    const slider = target({ tagName: 'INPUT', type: 'range' });
    for (const key of ['o', 'i', ' ', 'Escape', 'Delete', 'ArrowLeft', 'ArrowRight', 'e']) {
      expect(shouldHandleKey(slider, key)).toBe(true);
    }
  });

  it('leaves Space to a focused checkbox but not the rest of its keys', () => {
    const box = target({ tagName: 'INPUT', type: 'checkbox' });
    expect(shouldHandleKey(box, ' ')).toBe(false);
    expect(shouldHandleKey(box, 'Enter')).toBe(false);
    expect(shouldHandleKey(box, 'ArrowRight')).toBe(true);
  });

  it('handles everything when nothing is focused', () => {
    // `window` is the target then, and it has no tagName at all — the case
    // every window-level shortcut actually runs in.
    expect(shouldHandleKey(target({}), ' ', false)).toBe(true);
    expect(shouldHandleKey(null, 'Enter', false)).toBe(true);
  });
});

describe('moveSelection', () => {
  it('walks the grid by one and by a row', () => {
    expect(moveSelection(0, 'ArrowRight', 10, 4)).toBe(1);
    expect(moveSelection(0, 'ArrowDown', 10, 4)).toBe(4);
    expect(moveSelection(5, 'ArrowUp', 10, 4)).toBe(1);
  });

  it('stops at the ends instead of wrapping', () => {
    // Wrapping from the newest clip to the oldest is how you delete the wrong
    // thing with the Del key.
    expect(moveSelection(0, 'ArrowLeft', 10, 4)).toBe(0);
    expect(moveSelection(9, 'ArrowRight', 10, 4)).toBe(9);
    expect(moveSelection(8, 'ArrowDown', 10, 4)).toBe(8);
  });

  it('leaves the selection alone for keys it does not own', () => {
    expect(moveSelection(3, 'Enter', 10, 4)).toBe(3);
  });

  it('cannot select anything in an empty library', () => {
    expect(moveSelection(0, 'ArrowRight', 0, 4)).toBe(0);
  });
});
