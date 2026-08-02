import { describe, expect, it } from 'vitest';
import { gridShouldHandle, isActivatableTarget, isTypingTarget, moveSelection } from './keys';

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
});

describe('isActivatableTarget', () => {
  it('is true only for the tag that natively consumes Space and Enter', () => {
    expect(isActivatableTarget(target({ tagName: 'BUTTON' }))).toBe(true);
    expect(isActivatableTarget(target({ tagName: 'DIV' }))).toBe(false);
    expect(isActivatableTarget(target({}))).toBe(false);
    expect(isActivatableTarget(null)).toBe(false);
  });
});

describe('gridShouldHandle', () => {
  // Calls the shipped function rather than restating its expression: a mirror
  // of the guard would keep passing after the guard itself was changed, which
  // is the failure mode these cases exist to catch.
  const card = target({ tagName: 'BUTTON' });
  const arm = target({ tagName: 'BUTTON' });

  it('still lets arrow keys through a focused clip card', () => {
    // The regression this split exists to prevent. A card is a real `<button>`
    // (ClipCard.svelte) and WebView2 focuses it on click — the primary way a
    // user selects a clip — so a guard keyed on the tag alone went dead on
    // every grid key the moment anyone clicked anything.
    for (const key of ['ArrowRight', 'ArrowLeft', 'ArrowUp', 'ArrowDown']) {
      expect(gridShouldHandle(card, key, true)).toBe(true);
      expect(gridShouldHandle(arm, key, false)).toBe(true);
    }
  });

  it('keeps Space and Enter for a card, because spec §6.2 gives them to the grid', () => {
    // Left to itself the card would fire its own `onclick={onselect}` and
    // merely re-select the clip that is already selected — no preview, no
    // open. Inside the grid, the grid decides.
    expect(gridShouldHandle(card, ' ', true)).toBe(true);
    expect(gridShouldHandle(card, 'Enter', true)).toBe(true);
  });

  it('leaves Space and Enter to a button outside the grid', () => {
    // The rail's Arm control. Space must arm/disarm natively rather than
    // preview in place, and Enter there must not also navigate away.
    expect(gridShouldHandle(arm, ' ', false)).toBe(false);
    expect(gridShouldHandle(arm, 'Enter', false)).toBe(false);
  });

  it('never takes a key from a text field, wherever it sits', () => {
    const field = target({ tagName: 'INPUT' });
    expect(gridShouldHandle(field, 'ArrowRight', false)).toBe(false);
    expect(gridShouldHandle(field, ' ', false)).toBe(false);
    // Even inside the grid: a rename box is still a rename box.
    expect(gridShouldHandle(field, 'ArrowRight', true)).toBe(false);
  });

  it('handles everything when nothing is focused', () => {
    // `window` is the target then, and it has no tagName at all — the case
    // every grid shortcut actually runs in.
    expect(gridShouldHandle(target({}), ' ', false)).toBe(true);
    expect(gridShouldHandle(null, 'Enter', false)).toBe(true);
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
