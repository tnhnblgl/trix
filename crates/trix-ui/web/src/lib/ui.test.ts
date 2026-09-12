import { describe, expect, it } from 'vitest';
import { clamp, formatCombo, nextIndex, pointerRatio, ratioToValue, resolveStepperInput, snapToStep, stepBy, valueToRatio } from './ui';

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

  it('keeps a whole number whole at step 1, and passes anything through at step 0', () => {
    // Step 1 is a real grid -- the whole numbers -- so 37 comes back because
    // it is already on it, not because the function declined to snap.
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

  it('returns a whole number at the default step of 1', () => {
    // Both volume sliders run 0..100 at the default step, and the value goes
    // straight into a 44px readout and then to `config.set`. A pointer 40px
    // along a 186px track used to come back as 21.50537634408602.
    const v = ratioToValue(40 / 186, 0, 100, 1);
    expect(Number.isInteger(v)).toBe(true);
    expect(v).toBe(22);
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

describe('pointerRatio', () => {
  const box = { left: 100, bottom: 300, width: 200, height: 100 };

  it('measures a horizontal track from its left edge', () => {
    expect(pointerRatio(box, 150, 0, false)).toBe(0.25);
  });

  it('measures a vertical track from its bottom edge, so up is more', () => {
    expect(pointerRatio(box, 0, 275, true)).toBe(0.25);
    expect(pointerRatio(box, 0, 200, true)).toBe(1);
  });

  it('returns 0 for a track with no length rather than dividing by zero', () => {
    const flat = { left: 0, bottom: 0, width: 0, height: 0 };
    expect(pointerRatio(flat, 10, 10, false)).toBe(0);
    expect(pointerRatio(flat, 10, 10, true)).toBe(0);
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

describe('formatCombo', () => {
  it('title-cases each named key in a normal combo', () => {
    expect(formatCombo('ctrl+shift+f10')).toEqual(['Ctrl', 'Shift', 'F10']);
  });

  it('upper-cases a single character', () => {
    expect(formatCombo('e')).toEqual(['E']);
  });

  it('returns an empty list for an empty string', () => {
    expect(formatCombo('')).toEqual([]);
  });

  it('drops the empty segment left by a trailing +', () => {
    expect(formatCombo('ctrl+')).toEqual(['Ctrl']);
  });

  it('drops empty segments from a doubled + in the middle', () => {
    expect(formatCombo('ctrl++shift')).toEqual(['Ctrl', 'Shift']);
  });

  it('drops the empty segment left by a leading +', () => {
    expect(formatCombo('+shift')).toEqual(['Shift']);
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
