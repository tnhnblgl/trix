import { describe, expect, it } from 'vitest';
import { moveSelection } from './keys';

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
