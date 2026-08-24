import { describe, expect, it } from 'vitest';
import { clamp, nextIndex, ratioToValue, snapToStep, stepBy, valueToRatio } from './ui';

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

  it('leaves a value alone when the step is 0 or 1', () => {
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
