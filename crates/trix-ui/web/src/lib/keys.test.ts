import { describe, expect, it } from 'vitest';
import { isInteractiveTarget, moveSelection } from './keys';

/** An event target, as much of one as a DOM-less suite needs. */
const target = (props: Record<string, unknown>) => props as unknown as EventTarget;

describe('isInteractiveTarget', () => {
  it('leaves Space and Enter to the control they landed on', () => {
    // The rail's Arm button. Grid.svelte listens on <svelte:window>, so
    // without this a keyboard user could not arm at all (Space was
    // preventDefault'ed out from under the button) and Enter both toggled arm
    // and navigated away.
    expect(isInteractiveTarget(target({ tagName: 'BUTTON' }))).toBe(true);
    expect(isInteractiveTarget(target({ tagName: 'INPUT' }))).toBe(true);
    expect(isInteractiveTarget(target({ tagName: 'TEXTAREA' }))).toBe(true);
    expect(isInteractiveTarget(target({ tagName: 'SELECT' }))).toBe(true);
    expect(isInteractiveTarget(target({ tagName: 'DIV', isContentEditable: true }))).toBe(true);
  });

  it('lets the grid keep the keys nothing else wanted', () => {
    expect(isInteractiveTarget(target({ tagName: 'DIV' }))).toBe(false);
    expect(isInteractiveTarget(target({ tagName: 'BODY', isContentEditable: false }))).toBe(false);
    // `window` itself is the target when nothing is focused, and it has no
    // tagName at all — the case every grid shortcut actually runs in.
    expect(isInteractiveTarget(target({}))).toBe(false);
    expect(isInteractiveTarget(null)).toBe(false);
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
