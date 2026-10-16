import { describe, expect, it } from 'vitest';
import { gridWindow, toggleClipSelection } from './clip-grid';

describe('the clip grid rendering window', () => {
  it('mounts only nearby rows even for ten thousand thumbnails', () => {
    const view = gridWindow(10_000, 4, 200, 0, 600);
    expect(view.start).toBe(0);
    expect(view.end).toBe(20);
    expect(view.bottom).toBe(499_000);
  });

  it('can reach the last clip without retaining the rows above it', () => {
    const view = gridWindow(1205, 4, 200, 99_999, 600);
    expect(view.end).toBe(1205);
    expect(view.end - view.start).toBeLessThanOrEqual(28);
    expect(view.bottom).toBe(0);
    expect(view.top).toBeGreaterThan(0);
  });

  it('clamps stale scroll positions after a bulk deletion', () => {
    const view = gridWindow(3, 4, 200, 90_000, 600);
    expect(view).toEqual({ start: 0, end: 3, top: 0, bottom: 0 });
    expect(gridWindow(0, 4, 200, 90_000, 600)).toEqual({ start: 0, end: 0, top: 0, bottom: 0 });
  });
});

describe('multiple clip selection', () => {
  it('toggles one id without losing the other checked clips', () => {
    expect([...toggleClipSelection(new Set(['a']), ['a', 'b', 'c'], 'c', null, false)]).toEqual(['a', 'c']);
    expect([...toggleClipSelection(new Set(['a', 'c']), ['a', 'b', 'c'], 'a', null, false)]).toEqual(['c']);
  });

  it('selects a range by visible order, including unmounted cards', () => {
    const ids = Array.from({ length: 1205 }, (_, i) => String(i));
    const selected = toggleClipSelection(new Set(['2']), ids, '1204', '2', true);
    expect(selected.size).toBe(1203);
    expect(selected.has('800')).toBe(true);
    expect(selected.has('0')).toBe(false);
  });

  it('never adds a hidden clip or a stale anchor to a filtered selection', () => {
    expect([...toggleClipSelection(new Set(), ['b', 'd'], 'd', 'a', true)]).toEqual(['d']);
    expect([...toggleClipSelection(new Set(), ['b', 'd'], 'a', 'b', true)]).toEqual([]);
  });
});
