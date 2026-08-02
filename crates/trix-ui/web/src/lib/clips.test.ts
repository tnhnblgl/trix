import { describe, expect, it } from 'vitest';
import { formatBytes, formatDuration, mergeSaved, counterLabel } from './clips';
import type { ClipMeta } from './types';

const clip = (id: string, favorite = false): ClipMeta => ({
  id,
  title: `clip_${id}`,
  created: '2026-07-26T14:30:12+03:00',
  duration_ms: 20016,
  bytes: 19812352,
  width: 1920,
  height: 1200,
  fps: 60,
  encoder: 'NVENC H.264',
  has_audio: true,
  favorite,
});

describe('formatDuration', () => {
  it('reads as a clip length, not as milliseconds', () => {
    expect(formatDuration(20016)).toBe('0:20');
    expect(formatDuration(65000)).toBe('1:05');
    expect(formatDuration(0)).toBe('0:00');
  });
});

describe('formatBytes', () => {
  it('uses the unit a person would', () => {
    expect(formatBytes(19812352)).toBe('18.9 MB');
    expect(formatBytes(1024)).toBe('1.0 KB');
    expect(formatBytes(0)).toBe('0 B');
  });
});

describe('mergeSaved', () => {
  it('puts a new clip at the front, because the grid is newest-first', () => {
    const list = [clip('20260726_120000')];
    expect(mergeSaved(list, clip('20260726_143012'))[0].id).toBe('20260726_143012');
  });

  it('replaces rather than duplicates when the id is already there', () => {
    // library.list and a clip_saved event can race after a reconnect; two
    // cards for one file is the visible bug that causes.
    const list = [clip('20260726_143012'), clip('20260726_120000')];
    const merged = mergeSaved(list, clip('20260726_143012', true));
    expect(merged).toHaveLength(2);
    expect(merged[0].favorite).toBe(true);
  });
});

describe('counterLabel', () => {
  it('is one-based, the way spec §6.2 writes it', () => {
    expect(counterLabel(0, 47)).toBe('1 of 47');
    expect(counterLabel(2, 47)).toBe('3 of 47');
  });

  it('says nothing when there is nothing', () => {
    expect(counterLabel(0, 0)).toBe('');
  });
});
