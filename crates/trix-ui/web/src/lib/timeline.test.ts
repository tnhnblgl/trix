import { describe, expect, it } from 'vitest';
import { formatClock, keyframeStep, msFromRatio, ratioFromMs, snapStart } from './timeline';

describe('snapStart', () => {
  it('moves back to the latest keyframe at or before the point', () => {
    expect(snapStart(2500, [0, 1000, 2000, 3000])).toBe(2000);
    expect(snapStart(2000, [0, 1000, 2000, 3000])).toBe(2000);
  });

  it('returns the point untouched when there are no keyframes', () => {
    // Not "snap to zero". An empty list means the ticks are still loading, or
    // the clip could not be indexed at all -- and pinning the in-point to 0
    // in either case makes the control look broken. The daemon snaps it for
    // real regardless.
    expect(snapStart(2500, [])).toBe(2500);
  });

  it('returns 0 when every keyframe is later than the point', () => {
    expect(snapStart(500, [1000, 2000])).toBe(0);
  });
});

describe('keyframeStep', () => {
  it('moves to the neighbouring keyframe', () => {
    expect(keyframeStep(2000, [0, 1000, 2000, 3000], 1)).toBe(3000);
    expect(keyframeStep(2000, [0, 1000, 2000, 3000], -1)).toBe(1000);
  });

  it('moves off an off-grid point to the neighbour in that direction', () => {
    expect(keyframeStep(2400, [0, 1000, 2000, 3000], 1)).toBe(3000);
    expect(keyframeStep(2400, [0, 1000, 2000, 3000], -1)).toBe(2000);
  });

  it('stays put at either end of the list', () => {
    expect(keyframeStep(3000, [0, 1000, 2000, 3000], 1)).toBe(3000);
    expect(keyframeStep(0, [0, 1000, 2000, 3000], -1)).toBe(0);
  });

  it('falls back to one second when there are no keyframes', () => {
    // Same honesty as snapStart: with nothing to step between, a whole
    // second is a defensible unit and pretending otherwise is not.
    expect(keyframeStep(4000, [], 1)).toBe(5000);
    expect(keyframeStep(4000, [], -1)).toBe(3000);
    expect(keyframeStep(300, [], -1)).toBe(0);
  });
});

describe('msFromRatio / ratioFromMs', () => {
  it('maps both ends and clamps a pointer dragged past them', () => {
    expect(msFromRatio(0, 15000)).toBe(0);
    expect(msFromRatio(1, 15000)).toBe(15000);
    expect(msFromRatio(-0.5, 15000)).toBe(0);
    expect(msFromRatio(2, 15000)).toBe(15000);
  });

  it('rounds to a whole millisecond', () => {
    expect(msFromRatio(1 / 3, 10000)).toBe(3333);
  });

  it('gives 0 for a clip with no duration rather than NaN', () => {
    // `library::scan` adopts a bare .mp4 as `duration_ms: 0`, and `NaN%`
    // in CSS drops the playhead out of the band entirely.
    expect(ratioFromMs(500, 0)).toBe(0);
    expect(msFromRatio(0.5, 0)).toBe(0);
  });
});

describe('formatClock', () => {
  it('shows minutes, seconds and a tenth', () => {
    expect(formatClock(0)).toBe('0:00.0');
    expect(formatClock(5700)).toBe('0:05.7');
    expect(formatClock(65400)).toBe('1:05.4');
  });

  it('never shows a negative time', () => {
    expect(formatClock(-20)).toBe('0:00.0');
  });
});
