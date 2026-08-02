import { describe, expect, it } from 'vitest';
import { isActivatableTarget, isTypingTarget, moveSelection } from './keys';

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

describe('Grid.svelte key guard', () => {
  // The regression this whole split exists to prevent: a clip card is a real
  // `<button>` (ClipCard.svelte), and WebView2 moves focus to it on click —
  // the primary way a user selects a clip. A blanket guard keyed on the tag
  // alone swallowed every key for a focused card, arrows included, so the
  // grid's own navigation went dead the moment a clip was clicked. This
  // asserts the actual guard expression `Grid.svelte` runs, not just the two
  // predicates in isolation.
  function guardConsumes(target: EventTarget | null, key: string): boolean {
    if (isTypingTarget(target)) return true;
    return isActivatableTarget(target) && (key === ' ' || key === 'Enter');
  }

  it('still lets ArrowRight through a focused clip-card button', () => {
    expect(guardConsumes(target({ tagName: 'BUTTON' }), 'ArrowRight')).toBe(false);
  });

  it('leaves Space and Enter to the button the rail Arm control is', () => {
    // Space with the Arm button focused must still arm/disarm natively
    // rather than also previewing in place, and Enter there must not also
    // navigate to the clip view.
    expect(guardConsumes(target({ tagName: 'BUTTON' }), ' ')).toBe(true);
    expect(guardConsumes(target({ tagName: 'BUTTON' }), 'Enter')).toBe(true);
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
