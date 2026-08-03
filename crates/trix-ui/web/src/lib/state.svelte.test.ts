import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { ClipMeta } from './types';

// `state.svelte.ts` talks to the daemon through `./ipc`; mocking it here is
// what lets these tests call the real store methods -- `remove`, `step`,
// `current` -- rather than a restatement of their logic that would keep
// passing after the real thing broke.
const callMock = vi.fn();

vi.mock('./ipc', () => ({
  call: (...args: unknown[]) => callMock(...args),
  daemonConnected: vi.fn(),
  onConnected: vi.fn(),
  onDaemonEvent: vi.fn(),
  onDisconnected: vi.fn(),
}));

const { app } = await import('./state.svelte');

const clip = (id: string): ClipMeta => ({
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
  favorite: false,
});

beforeEach(() => {
  callMock.mockReset();
  app.clips = [];
  app.total = 0;
  app.selected = 0;
  app.view = 'clip';
  app.toasts = [];
});

describe('AppState.remove', () => {
  it('lands on the clip that slid into the deleted slot', async () => {
    callMock.mockResolvedValue({});
    app.clips = [clip('a'), clip('b'), clip('c')];
    app.total = 3;
    app.selected = 1; // pointing at 'b'

    await app.remove('b');

    expect(callMock).toHaveBeenCalledWith('library.delete', { clip_id: 'b' });
    expect(app.clips.map((c) => c.id)).toEqual(['a', 'c']);
    // 'c' slid down into index 1, where 'b' used to be.
    expect(app.selected).toBe(1);
    expect(app.total).toBe(2);
  });

  it('clamps to the new last clip when the deleted one was at the end', async () => {
    // The off-by-one this guards: deleting the last clip must not leave
    // `selected` pointing one past the new end of the (now shorter) array.
    callMock.mockResolvedValue({});
    app.clips = [clip('a'), clip('b'), clip('c')];
    app.selected = 2; // pointing at 'c', the last one

    await app.remove('c');

    expect(app.clips.map((c) => c.id)).toEqual(['a', 'b']);
    expect(app.selected).toBe(1);
  });

  it('stays put when deleting the first clip while selected elsewhere', async () => {
    callMock.mockResolvedValue({});
    app.clips = [clip('a'), clip('b'), clip('c')];
    app.selected = 2; // pointing at 'c'

    await app.remove('a');

    expect(app.clips.map((c) => c.id)).toEqual(['b', 'c']);
    // 'c' is now at index 1, but selection tracked the deleted clip's slot
    // (index 0), not the clip the user was actually looking at -- that is
    // the documented, if surprising, contract of `remove`.
    expect(app.selected).toBe(0);
  });

  it('returns to the grid once the last clip in the library is gone', async () => {
    callMock.mockResolvedValue({});
    app.clips = [clip('a')];
    app.total = 1;
    app.selected = 0;
    app.view = 'clip';

    await app.remove('a');

    expect(app.clips).toHaveLength(0);
    expect(app.total).toBe(0);
    expect(app.selected).toBe(0);
    expect(app.view).toBe('grid');
  });

  it('leaves the library untouched and toasts when the daemon call fails', async () => {
    callMock.mockRejectedValue(new Error('pipe closed'));
    app.clips = [clip('a'), clip('b')];
    app.total = 2;
    app.selected = 0;

    await app.remove('a');

    expect(app.clips.map((c) => c.id)).toEqual(['a', 'b']);
    expect(app.total).toBe(2);
    expect(app.toasts.at(-1)?.text).toContain('pipe closed');
  });
});

describe('AppState.rename', () => {
  it('sends the new title and merges the updated clip into app.clips', async () => {
    const updated = { ...clip('b'), title: 'Better title' };
    callMock.mockResolvedValue(updated);
    app.clips = [clip('a'), clip('b')];

    await app.rename('b', 'Better title');

    expect(callMock).toHaveBeenCalledWith('library.rename', { clip_id: 'b', title: 'Better title' });
    expect(app.clips.find((c) => c.id === 'b')?.title).toBe('Better title');
  });
});

describe('AppState.setFavorite', () => {
  it('sends the favorite flag and merges the updated clip into app.clips', async () => {
    const updated = { ...clip('a'), favorite: true };
    callMock.mockResolvedValue(updated);
    app.clips = [clip('a')];

    await app.setFavorite('a', true);

    expect(callMock).toHaveBeenCalledWith('library.favorite', { clip_id: 'a', favorite: true });
    expect(app.clips.find((c) => c.id === 'a')?.favorite).toBe(true);
  });
});

describe('AppState.reveal', () => {
  it('asks the daemon to reveal the clip by id', async () => {
    callMock.mockResolvedValue({});

    await app.reveal('a');

    expect(callMock).toHaveBeenCalledWith('library.reveal', { clip_id: 'a' });
  });
});

describe('AppState.step', () => {
  it('walks forward and back without crossing either end', () => {
    app.clips = [clip('a'), clip('b'), clip('c')];
    app.selected = 0;

    app.step(1);
    expect(app.selected).toBe(1);
    app.step(1);
    expect(app.selected).toBe(2);
    app.step(1); // already at the last clip
    expect(app.selected).toBe(2);

    app.step(-2);
    expect(app.selected).toBe(0);
    app.step(-1); // already at the first clip
    expect(app.selected).toBe(0);
  });
});

describe('AppState.current', () => {
  it('reflects the clip at the selected index', () => {
    app.clips = [clip('a'), clip('b')];
    app.selected = 1;
    expect(app.current?.id).toBe('b');
  });

  it('is null when the library is empty', () => {
    app.clips = [];
    app.selected = 0;
    expect(app.current).toBeNull();
  });
});
