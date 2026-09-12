import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { ClipMeta, DaemonEvent, ShotMeta } from './types';

// `state.svelte.ts` talks to the daemon through `./ipc`; mocking it here is
// what lets these tests call the real store methods -- `remove`, `step`,
// `current` -- rather than a restatement of their logic that would keep
// passing after the real thing broke.
const callMock = vi.fn();

vi.mock('./ipc', () => ({
  call: (...args: unknown[]) => callMock(...args),
  // Resolved rather than a bare vi.fn(): wireDaemon() chains `.then()` off
  // this call unconditionally, and an unmocked vi.fn() returns `undefined`,
  // which would throw the moment a test calls wireDaemon().
  daemonConnected: vi.fn().mockResolvedValue(false),
  onConnected: vi.fn(),
  onDaemonEvent: vi.fn(),
  onDisconnected: vi.fn(),
}));

// state.svelte.ts talks to Tauri directly for the update commands (they are
// not daemon calls, so they do not go through `./ipc`'s `call`). Mocked the
// same way clips.test.ts mocks this module: a bare `invoke` would otherwise
// hit the real `@tauri-apps/api/core`, which has nothing to talk to outside
// a webview.
const invokeMock = vi.fn();

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

// Nothing here drives wireUpdates() itself, but state.svelte.ts imports
// `listen` at module scope, and the real `@tauri-apps/api/event` reaches for
// the same Tauri runtime `@tauri-apps/api/core` does.
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(),
}));

const { app, wireDaemon, UpdateStore, trimRangeError } = await import('./state.svelte');
const { onConnected, onDaemonEvent } = await import('./ipc');

/**
 * Registers the real `wireDaemon()` event listener and hands back the
 * callback it passed to `onDaemonEvent`, so tests can drive the shipped
 * handler directly instead of restating its logic.
 */
function registerDaemonEventHandler(): (event: DaemonEvent) => void {
  wireDaemon();
  const handler = vi.mocked(onDaemonEvent).mock.calls.at(-1)?.[0];
  if (!handler) throw new Error('wireDaemon() never registered a daemon event handler');
  return handler;
}

/**
 * Registers the real `wireDaemon()` connect listener and hands back the
 * callback it passed to `onConnected` -- the same capture-and-invoke shape as
 * `registerDaemonEventHandler`, but for the handler that drives `onDaemonUp`.
 */
function registerConnectedHandler(): (status: unknown) => void {
  wireDaemon();
  const handler = vi.mocked(onConnected).mock.calls.at(-1)?.[0];
  if (!handler) throw new Error('wireDaemon() never registered an onConnected handler');
  return handler;
}

/** Minimal valid `status` payload, enough for `refreshStatus` to read. */
const statusPayload = () => ({
  armed: false,
  encoder: null,
  monitor_index: 0,
  ring_seconds_used: 0,
  ring_seconds_total: 0,
  version: '0.0.0-test',
  clip_dir: 'C:\\fake\\clips',
});

/** Minimal valid `library.list` payload, enough for `loadClips` to read. */
const libraryListPayload = () => ({ clips: [], total: 0, offset: 0 });

/** Minimal valid `shots.list` payload, enough for `loadShots` to read. */
const shotsListPayload = () => ({ shots: [], total: 0, offset: 0 });

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

/** Minimal valid `ShotMeta`, mirroring `clip` above. */
const shot = (id: string): ShotMeta => ({
  id,
  created: '2026-07-26T14:30:12+03:00',
  bytes: 2048576,
  width: 1920,
  height: 1200,
});

beforeEach(() => {
  callMock.mockReset();
  invokeMock.mockReset();
  vi.mocked(onDaemonEvent).mockClear();
  vi.mocked(onConnected).mockClear();
  app.clips = [];
  app.total = 0;
  app.selected = 0;
  app.favoriteIds = null;
  app.shots = [];
  app.shotTotal = 0;
  app.shotSelected = 0;
  app.view = 'clip';
  app.toasts = [];
  app.rearmNeeded = [];
  // onDaemonUp() short-circuits when this is already true, so a test that
  // ran earlier (or wireDaemon()'s own daemonConnected() bootstrap) must not
  // leave it set for the next one.
  app.connected = false;
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

describe('AppState.deleteShot', () => {
  it('lands on the screenshot that slid into the deleted slot', async () => {
    callMock.mockResolvedValue({});
    app.shots = [shot('a'), shot('b'), shot('c')];
    app.shotTotal = 3;
    app.shotSelected = 1; // pointing at 'b'

    await app.deleteShot('b');

    expect(callMock).toHaveBeenCalledWith('shots.delete', { shot_id: 'b' });
    expect(app.shots.map((s) => s.id)).toEqual(['a', 'c']);
    // 'c' slid down into index 1, where 'b' used to be.
    expect(app.shotSelected).toBe(1);
    expect(app.shotTotal).toBe(2);
  });

  it('clamps to the new last screenshot when the deleted one was at the end', async () => {
    // The off-by-one this guards: deleting the last screenshot must not leave
    // `shotSelected` pointing one past the new end of the (now shorter) array.
    callMock.mockResolvedValue({});
    app.shots = [shot('a'), shot('b'), shot('c')];
    app.shotSelected = 2; // pointing at 'c', the last one

    await app.deleteShot('c');

    expect(app.shots.map((s) => s.id)).toEqual(['a', 'b']);
    expect(app.shotSelected).toBe(1);
  });

  it('stays put when deleting the first screenshot while selected elsewhere', async () => {
    callMock.mockResolvedValue({});
    app.shots = [shot('a'), shot('b'), shot('c')];
    app.shotSelected = 2; // pointing at 'c'

    await app.deleteShot('a');

    expect(app.shots.map((s) => s.id)).toEqual(['b', 'c']);
    // 'c' is now at index 1, but selection tracked the deleted screenshot's
    // slot (index 0), not the tile the user was actually looking at -- the
    // same documented, if surprising, contract `remove` has for clips.
    expect(app.shotSelected).toBe(0);
  });

  it('clamps to 0 once the last screenshot is gone', async () => {
    callMock.mockResolvedValue({});
    app.shots = [shot('a')];
    app.shotTotal = 1;
    app.shotSelected = 0;

    await app.deleteShot('a');

    expect(app.shots).toHaveLength(0);
    expect(app.shotTotal).toBe(0);
    expect(app.shotSelected).toBe(0);
  });

  it('leaves the screenshot list untouched and toasts when the daemon call fails', async () => {
    callMock.mockRejectedValue(new Error('pipe closed'));
    app.shots = [shot('a'), shot('b')];
    app.shotTotal = 2;
    app.shotSelected = 0;

    await app.deleteShot('a');

    expect(app.shots.map((s) => s.id)).toEqual(['a', 'b']);
    expect(app.shotTotal).toBe(2);
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

describe('AppState.keyframesFor', () => {
  it('asks the daemon for the clip keyframes and hands back the list', async () => {
    callMock.mockResolvedValue({ clip_id: 'a', keyframes: [0, 2000, 4000] });

    await expect(app.keyframesFor('a')).resolves.toEqual([0, 2000, 4000]);

    expect(callMock).toHaveBeenCalledWith('library.keyframes', { clip_id: 'a' });
  });

  // A clip whose keyframes cannot be read is still perfectly playable, so this
  // must degrade to a bar with no ticks. Toasting here would put an error in
  // front of someone who only wanted to watch the clip.
  it('degrades to an empty tick list, without a toast, when the clip cannot be indexed', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    callMock.mockRejectedValue(new Error('could not index the keyframes of clip a'));

    await expect(app.keyframesFor('a')).resolves.toEqual([]);

    expect(app.toasts).toHaveLength(0);
    warn.mockRestore();
  });
});

describe('AppState.exportTrim', () => {
  it('exportTrim refuses a range whose out point is not after its in point', async () => {
    const calls: string[] = [];
    callMock.mockImplementation((cmd: string) => {
      calls.push(cmd);
      return Promise.resolve({});
    });

    await app.exportTrim('20260822_101500', 4000, 4000);

    expect(calls).not.toContain('library.export');
    expect(app.toasts.at(-1)?.kind).toBe('error');
  });

  // `MIN_TRIM_MS` in trix-core/src/export.rs. Mirrored here so a too-short
  // range is refused with a sentence naming the floor, rather than by a
  // round trip that comes back as the daemon's own refusal.
  it('refuses a range shorter than the 200 ms floor the daemon enforces', async () => {
    const calls: string[] = [];
    callMock.mockImplementation((cmd: string) => {
      calls.push(cmd);
      return Promise.resolve({});
    });

    await app.exportTrim('20260822_101500', 4000, 4150);

    expect(calls).not.toContain('library.export');
    expect(app.toasts.at(-1)?.text).toContain('200');
  });

  it('sends whole milliseconds in fast mode', async () => {
    callMock.mockResolvedValue(clip('20260822_101500_trim'));

    await app.exportTrim('20260822_101500', 4000.4, 9000.6);

    expect(callMock).toHaveBeenCalledWith('library.export', {
      clip_id: '20260822_101500',
      start_ms: 4000,
      end_ms: 9001,
      mode: 'fast',
    });
  });

  // One export used to announce itself three times: the daemon's capture
  // chime, "Saved <title>" from the `clip_saved` handler, and a flat "Trimmed
  // clip saved." from here. The chime is gone on the daemon side and this
  // toast is gone here; `clip_saved` is the survivor because it names the
  // clip. So a plain successful export must say nothing at all from this
  // method -- the announcement is the event's job.
  it('leaves the success announcement to clip_saved rather than toasting twice', async () => {
    app.clips = [clip('20260822_101500')];
    callMock.mockResolvedValue(clip('20260822_101500_trim'));

    await app.exportTrim('20260822_101500', 4000, 9000);

    expect(app.toasts).toHaveLength(0);
  });

  // The daemon's audio fallback is deliberate and documented: any AAC
  // passthrough failure re-runs the whole export with no audio stream and
  // reports `has_audio: false`. That is a success everywhere else in this app,
  // so without this the user finds out by playing the clip back. Whether AAC
  // survives passthrough is the one thing about fast mode nobody could verify
  // up front -- on a machine where it does not, every export is silent.
  it('says so when a clip that had sound comes back without it', async () => {
    app.clips = [clip('20260822_101500')];
    callMock.mockResolvedValue({ ...clip('20260822_101500_trim'), has_audio: false });

    await app.exportTrim('20260822_101500', 4000, 9000);

    expect(app.toasts.at(-1)?.kind).toBe('error');
    expect(app.toasts.at(-1)?.text).toContain('without sound');
  });

  // The other half of the same rule: a source with no audio track produces an
  // export with no audio track, and that is not a loss to report. Warning here
  // would fire on every trim of a clip recorded with the microphone and system
  // audio both off, and a warning that is always wrong is one nobody reads.
  it('stays quiet when the source had no sound to lose', async () => {
    app.clips = [{ ...clip('20260822_101500'), has_audio: false }];
    callMock.mockResolvedValue({ ...clip('20260822_101500_trim'), has_audio: false });

    await app.exportTrim('20260822_101500', 4000, 9000);

    expect(app.toasts).toHaveLength(0);
  });

  // The new clip reaches the grid on the daemon's own `clip_saved` broadcast,
  // which `wireDaemon` already prepends on -- so an export must not also
  // reload the library, which would reset `selected` out from under whoever
  // is still looking at the source clip.
  it('does not reload the library, since clip_saved already announces the new clip', async () => {
    const calls: string[] = [];
    callMock.mockImplementation((cmd: string) => {
      calls.push(cmd);
      return Promise.resolve(clip('20260822_101500_trim'));
    });

    await app.exportTrim('20260822_101500', 0, 5000);

    expect(calls).toEqual(['library.export']);
  });

  it('toasts the daemon refusal rather than throwing at the caller', async () => {
    callMock.mockRejectedValue(new Error('this clip has no duration to trim'));

    await app.exportTrim('20260822_101500', 0, 5000);

    expect(app.toasts.at(-1)?.text).toContain('this clip has no duration to trim');
    expect(app.toasts.at(-1)?.kind).toBe('error');
  });
});

describe('trimRangeError', () => {
  // The clip page disables Export on this and prints the reason under the
  // bar, and `exportTrim` refuses on the same call -- so the two can only
  // agree because they ask one function. It is also the only part of the trim
  // UI a test can execute: the suite runs on node with no DOM, so the bar,
  // the sliders and the key bindings are read-verified instead.
  it('accepts a range at or above the 200 ms floor', () => {
    expect(trimRangeError(0, 200)).toBeNull();
    expect(trimRangeError(4000, 9000)).toBeNull();
  });

  it('names the in/out order when the range is empty or inverted', () => {
    // Crossing is what `i` at 12s after `o` at 8s produces, and what either
    // slider dragged past the other produces.
    expect(trimRangeError(12000, 8000)).toBe('Set the out point after the in point.');
    expect(trimRangeError(4000, 4000)).toBe('Set the out point after the in point.');
  });

  it('names the floor when the range is positive but too short', () => {
    expect(trimRangeError(4000, 4150)).toContain('200');
  });
});

describe('AppState.toggleArm', () => {
  // dispatch.rs's `fail` broadcasts an `error` event for a failed `arm` (spec
  // §4.4), which `wireDaemon`'s `onDaemonEvent` switch already toasts. Before
  // this fix, `toggleArm`'s own `catch` toasted the same rejection again, so a
  // failed arm from the rail showed the identical message twice.
  it('does not toast locally when arming fails, since the daemon already broadcasts an error event', async () => {
    app.status = { ...statusPayload(), armed: false };
    callMock.mockRejectedValueOnce(new Error('arm failed: no hardware encoder'));

    await app.toggleArm();

    expect(app.toasts).toHaveLength(0);
  });

  // `disarm` broadcasts nothing on failure (dispatch.rs's own comment: only
  // `arm` and `clip` do, because a refused disarm concerns only the client
  // that asked) -- so this is the only surface that ever tells the user a
  // disarm failed, and it must keep doing so.
  it('still toasts locally when disarming fails, since disarm broadcasts no error event', async () => {
    app.status = { ...statusPayload(), armed: true };
    callMock.mockRejectedValueOnce(new Error('disarm failed: engine wedged'));

    await app.toggleArm();

    expect(app.toasts.at(-1)?.text).toContain('disarm failed: engine wedged');
  });
});

describe('AppState.addRearmNeeded', () => {
  // The whole point of this finding: `rearmNeeded` used to be component-local
  // state in Settings.svelte, which App.svelte mounts only inside
  // `{#if app.view === 'settings'}` -- navigating away destroyed it. Reading
  // it back off the `app` singleton (module-scoped, not tied to any
  // component's lifetime) is what proves it now survives that trip.
  it('is readable from the shared app singleton, not scoped to a component', () => {
    app.addRearmNeeded(['mic_volume']);
    expect(app.rearmNeeded).toEqual(['mic_volume']);
  });

  it('accumulates across separate config.set responses instead of replacing', () => {
    app.addRearmNeeded(['fps']);
    app.addRearmNeeded(['mic_volume']);
    expect(app.rearmNeeded).toEqual(['fps', 'mic_volume']);
  });

  it('de-duplicates a key reported more than once', () => {
    app.addRearmNeeded(['mic_volume']);
    app.addRearmNeeded(['mic_volume', 'system_volume']);
    expect(app.rearmNeeded).toEqual(['mic_volume', 'system_volume']);
  });

  it('leaves the list untouched when nothing is pending', () => {
    app.addRearmNeeded(['fps']);
    app.addRearmNeeded([]);
    expect(app.rearmNeeded).toEqual(['fps']);
  });
});

describe('AppState.rearmNow', () => {
  it('disarms, re-arms, clears the pending list, and refreshes status', async () => {
    app.rearmNeeded = ['mic_volume', 'system_volume'];
    callMock.mockImplementation((cmd: string) => {
      if (cmd === 'status') return Promise.resolve(statusPayload());
      return Promise.resolve({});
    });

    await app.rearmNow();

    expect(callMock).toHaveBeenNthCalledWith(1, 'disarm');
    expect(callMock).toHaveBeenNthCalledWith(2, 'arm');
    expect(callMock).toHaveBeenCalledWith('status');
    expect(app.rearmNeeded).toEqual([]);
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

describe('AppState favourites filter', () => {
  const fav = (id: string): ClipMeta => ({ ...clip(id), favorite: true });

  it('shows only the clips that were favourites when it was switched on', () => {
    app.clips = [clip('a'), fav('b'), clip('c'), fav('d')];
    app.setFavoritesOnly(true);
    expect(app.favoritesOnly).toBe(true);
    expect(app.visible.map((c) => c.id)).toEqual(['b', 'd']);

    app.setFavoritesOnly(false);
    expect(app.visible.map((c) => c.id)).toEqual(['a', 'b', 'c', 'd']);
  });

  it('keeps a clip unfavourited while filtering until the filter is switched on again', async () => {
    app.clips = [fav('a'), fav('b')];
    app.setFavoritesOnly(true);
    app.selected = 1; // looking at 'b'
    callMock.mockResolvedValue(clip('b'));

    await app.setFavorite('b', false);

    // Still on screen, still selected: the clip page does not jump.
    expect(app.visible.map((c) => c.id)).toEqual(['a', 'b']);
    expect(app.current?.id).toBe('b');
    expect(app.current?.favorite).toBe(false);

    app.setFavoritesOnly(false);
    app.setFavoritesOnly(true);
    expect(app.visible.map((c) => c.id)).toEqual(['a']);
  });

  it('keeps the selection on the same clip across a switch when the new list has it', () => {
    app.clips = [clip('a'), fav('b'), fav('c')];
    app.selected = 2; // 'c'
    app.setFavoritesOnly(true);
    expect(app.current?.id).toBe('c');
    app.setFavoritesOnly(false);
    expect(app.current?.id).toBe('c');
  });

  it('falls back to the first shown clip when the selected one is filtered out', () => {
    app.clips = [clip('a'), fav('b'), clip('c')];
    app.selected = 2; // 'c', not a favourite
    app.setFavoritesOnly(true);
    expect(app.selected).toBe(0);
    expect(app.current?.id).toBe('b');
  });

  it('steps through favourites only', () => {
    app.clips = [fav('a'), clip('b'), fav('c')];
    app.setFavoritesOnly(true);
    app.step(1);
    expect(app.current?.id).toBe('c');
    app.step(1); // already the last favourite
    expect(app.current?.id).toBe('c');
  });

  it('lands a delete on the next shown clip, not the next clip in the library', async () => {
    callMock.mockResolvedValue({});
    app.clips = [fav('a'), clip('b'), fav('c'), fav('d')];
    app.total = 4;
    app.setFavoritesOnly(true);
    app.selected = 1; // 'c'

    await app.remove('c');

    expect(app.current?.id).toBe('d');
  });

  it('returns to the grid once the last shown clip is deleted, even with others in the library', async () => {
    callMock.mockResolvedValue({});
    app.clips = [clip('a'), fav('b')];
    app.total = 2;
    app.setFavoritesOnly(true);
    app.view = 'clip';

    await app.remove('b');

    expect(app.clips.map((c) => c.id)).toEqual(['a']);
    expect(app.view).toBe('grid');
  });

  it('does not shift the selection for a new clip the filter is hiding', () => {
    app.clips = [fav('a'), fav('b')];
    app.total = 2;
    app.setFavoritesOnly(true);
    app.selected = 1; // 'b'
    const handle = registerDaemonEventHandler();

    handle({ event: 'clip_saved', data: clip('new') as unknown as Record<string, unknown> });

    expect(app.clips.map((c) => c.id)).toEqual(['new', 'a', 'b']);
    expect(app.current?.id).toBe('b');
  });

  it('re-takes its snapshot when the library is reloaded', async () => {
    app.clips = [fav('a')];
    app.setFavoritesOnly(true);
    callMock.mockResolvedValue({ clips: [clip('x'), fav('y')], total: 2, offset: 0 });

    await app.loadClips();

    expect(app.favoritesOnly).toBe(true);
    expect(app.visible.map((c) => c.id)).toEqual(['y']);
  });
});

describe('wireDaemon: clip_saved', () => {
  it('prepends a genuinely new clip, bumps the total, and toasts', () => {
    app.clips = [clip('a'), clip('b')];
    app.total = 2;
    app.selected = 0;
    const handle = registerDaemonEventHandler();

    handle({ event: 'clip_saved', data: clip('new') as unknown as Record<string, unknown> });

    expect(app.clips.map((c) => c.id)).toEqual(['new', 'a', 'b']);
    expect(app.total).toBe(3);
    expect(app.toasts.at(-1)?.text).toBe('Saved clip_new');
  });

  it('shifts the selection so it still points at the clip the user had, not the slot', () => {
    app.clips = [clip('a'), clip('b')];
    app.total = 2;
    app.selected = 1; // pointing at 'b'
    const handle = registerDaemonEventHandler();

    handle({ event: 'clip_saved', data: clip('new') as unknown as Record<string, unknown> });

    // 'b' slid from index 1 to index 2 when 'new' was prepended.
    expect(app.selected).toBe(2);
    expect(app.clips[app.selected]?.id).toBe('b');
  });

  it('shifts the selection even when viewing the grid, not the clip detail', () => {
    // If a future change adds `&& app.view !== 'grid'` to the shift guard,
    // the user in Grid would see Space preview and Enter open the wrong clip
    // after a new one is saved (they would get the slot that just slid down,
    // not the clip they were actually looking at). This test catches that.
    app.clips = [clip('a'), clip('b')];
    app.total = 2;
    app.selected = 1; // pointing at 'b'
    app.view = 'grid';
    const handle = registerDaemonEventHandler();

    handle({ event: 'clip_saved', data: clip('new') as unknown as Record<string, unknown> });

    // 'b' slid from index 1 to index 2 when 'new' was prepended.
    expect(app.selected).toBe(2);
    expect(app.clips[app.selected]?.id).toBe('b');
  });

  it('does not shift the selection when the library was empty', () => {
    app.clips = [];
    app.total = 0;
    app.selected = 0;
    const handle = registerDaemonEventHandler();

    handle({ event: 'clip_saved', data: clip('new') as unknown as Record<string, unknown> });

    expect(app.clips.map((c) => c.id)).toEqual(['new']);
    expect(app.total).toBe(1);
    expect(app.selected).toBe(0);
  });

  it('does not inflate the total or move the selection for a clip already in the list', () => {
    // Mirrors a reconnect: `library.list` already loaded 'a', and its
    // `clip_saved` event arrives after. `mergeSaved` still replaces the
    // entry in place, but the id is not new, so count and selection must
    // not move, and there is nothing new to toast about.
    app.clips = [clip('a'), clip('b')];
    app.total = 2;
    app.selected = 1; // pointing at 'b'
    const handle = registerDaemonEventHandler();

    const relabeled = { ...clip('a'), title: 'renamed elsewhere' };
    handle({ event: 'clip_saved', data: relabeled as unknown as Record<string, unknown> });

    expect(app.clips.map((c) => c.id)).toEqual(['a', 'b']);
    expect(app.clips[0].title).toBe('renamed elsewhere');
    expect(app.total).toBe(2);
    expect(app.selected).toBe(1);
    expect(app.toasts).toHaveLength(0);
  });
});

describe('wireDaemon: shot_saved', () => {
  it('prepends a genuinely new screenshot, bumps the total, and toasts', () => {
    // C1's second half: prepending the tile is invisible unless the
    // Screenshots tab happens to be open, so the settled "clipboard + toast +
    // sound" scope needs a toast here the same way `clip_saved` toasts above.
    app.shots = [shot('a'), shot('b')];
    app.shotTotal = 2;
    app.shotSelected = 0;
    const handle = registerDaemonEventHandler();

    handle({ event: 'shot_saved', data: shot('new') as unknown as Record<string, unknown> });

    expect(app.shots.map((s) => s.id)).toEqual(['new', 'a', 'b']);
    expect(app.shotTotal).toBe(3);
    expect(app.toasts.at(-1)?.text).toBe('Screenshot saved');
  });

  // The bug the comment at state.svelte.ts:503 names: a genuine prepend
  // shifts every existing tile down one slot, so a selection held by index
  // would silently move to a different screenshot -- the same regression
  // clip_saved is tested against above.
  it('shifts the selection so it still points at the screenshot the user had, not the slot', () => {
    app.shots = [shot('a'), shot('b')];
    app.shotTotal = 2;
    app.shotSelected = 1; // pointing at 'b'
    const handle = registerDaemonEventHandler();

    handle({ event: 'shot_saved', data: shot('new') as unknown as Record<string, unknown> });

    // 'b' slid from index 1 to index 2 when 'new' was prepended.
    expect(app.shotSelected).toBe(2);
    expect(app.shots[app.shotSelected]?.id).toBe('b');
  });

  it('does not shift the selection when the screenshot list was empty', () => {
    app.shots = [];
    app.shotTotal = 0;
    app.shotSelected = 0;
    const handle = registerDaemonEventHandler();

    handle({ event: 'shot_saved', data: shot('new') as unknown as Record<string, unknown> });

    expect(app.shots.map((s) => s.id)).toEqual(['new']);
    expect(app.shotTotal).toBe(1);
    expect(app.shotSelected).toBe(0);
  });

  it('does not duplicate, inflate the total, move the selection, or toast for a screenshot already in the list', () => {
    // Mirrors a reconnect: `shots.list` already loaded 'a', and its
    // `shot_saved` event arrives after. Unlike `clip_saved`, there is no
    // `mergeSaved` here to fold the repeat in -- a known id is a straight
    // no-op, so the list itself must come back untouched too, and there is
    // nothing new to toast about.
    app.shots = [shot('a'), shot('b')];
    app.shotTotal = 2;
    app.shotSelected = 1; // pointing at 'b'
    const handle = registerDaemonEventHandler();

    handle({ event: 'shot_saved', data: shot('a') as unknown as Record<string, unknown> });

    expect(app.shots.map((s) => s.id)).toEqual(['a', 'b']);
    expect(app.shotTotal).toBe(2);
    expect(app.shotSelected).toBe(1);
    expect(app.toasts).toHaveLength(0);
  });
});

describe('wireDaemon: config_changed', () => {
  // The folder dialog is the daemon's -- the tray menu and this page's
  // `folder.pick` both end there -- so a moved folder never arrives as a
  // `config.set` reply this app could read. Before this event existed it told
  // an open app nothing, and the grid kept building asset: URLs against the
  // old directory. Refreshing status is what picks up the new clip_dir the
  // same way the `armed`/`disarmed` cases already do.
  it('refreshes status so a daemon-driven clip_dir change reaches app.clipDir', async () => {
    app.status = statusPayload();
    callMock.mockResolvedValue({ ...statusPayload(), clip_dir: 'D:\\new-clips' });
    const handle = registerDaemonEventHandler();

    handle({ event: 'config_changed', data: { clip_dir_resolved: 'D:\\new-clips' } });
    await vi.waitFor(() => expect(app.clipDir).toBe('D:\\new-clips'));

    expect(callMock).toHaveBeenCalledWith('status');
  });
  // Status alone is not enough once the folder holds *different* clips, which
  // is the normal case: the daemon rebuilt its library cache for the new
  // directory, so the list on screen is stale until this asks for it again.
  it('reloads the clip list when the folder actually moved', async () => {
    app.status = statusPayload();
    app.clips = [clip('from-the-old-folder')];
    app.total = 1;
    callMock.mockImplementation((cmd: string) => {
      if (cmd === 'status') {
        return Promise.resolve({ ...statusPayload(), clip_dir: 'D:\\new-clips' });
      }
      if (cmd === 'library.list') {
        return Promise.resolve({ clips: [clip('in-the-new-folder')], total: 1, offset: 0 });
      }
      return Promise.resolve({});
    });
    const handle = registerDaemonEventHandler();

    handle({ event: 'config_changed', data: { clip_dir_resolved: 'D:\\new-clips' } });

    await vi.waitFor(() => expect(app.clips.map((c) => c.id)).toEqual(['in-the-new-folder']));
  });

  // I1: the same folder dialog belongs to the daemon and can be opened from
  // the tray menu while the app sits on the Screenshots tab, so a moved
  // folder has to reload `app.shots` too, not just `app.clips` -- otherwise
  // every tile keeps building `asset:` URLs against a folder that no longer
  // holds those ids.
  it('reloads the screenshot list when the folder actually moved', async () => {
    app.status = statusPayload();
    app.shots = [shot('from-the-old-folder')];
    app.shotTotal = 1;
    callMock.mockImplementation((cmd: string) => {
      if (cmd === 'status') {
        return Promise.resolve({ ...statusPayload(), clip_dir: 'D:\\new-clips' });
      }
      if (cmd === 'shots.list') {
        return Promise.resolve({ shots: [shot('in-the-new-folder')], total: 1, offset: 0 });
      }
      return Promise.resolve({});
    });
    const handle = registerDaemonEventHandler();

    handle({ event: 'config_changed', data: { clip_dir_resolved: 'D:\\new-clips' } });

    await vi.waitFor(() => expect(app.shots.map((s) => s.id)).toEqual(['in-the-new-folder']));
  });

  // Every accepted `config.set` broadcasts this event carrying
  // `clip_dir_resolved`, whether or not `clip_dir` was among the keys -- so a
  // bitrate nudge lands here too. Reloading on those would refetch the page and
  // reset `selected` to 0, moving the selection out from under whoever is
  // sitting in settings.
  it('leaves the clip list and the selection alone when the folder did not move', async () => {
    app.status = statusPayload();
    app.clips = [clip('a'), clip('b')];
    app.total = 2;
    app.selected = 1;
    callMock.mockResolvedValue(statusPayload());
    const handle = registerDaemonEventHandler();

    handle({ event: 'config_changed', data: { clip_dir_resolved: statusPayload().clip_dir } });
    await vi.waitFor(() => expect(callMock).toHaveBeenCalledWith('status'));

    expect(callMock).not.toHaveBeenCalledWith('library.list', expect.anything());
    expect(callMock).not.toHaveBeenCalledWith('shots.list', expect.anything());
    expect(app.clips.map((c) => c.id)).toEqual(['a', 'b']);
    expect(app.selected).toBe(1);
  });
});

describe('onDaemonUp: first-run routing', () => {
  // Every command onDaemonUp can reach needs a stub, regardless of which
  // branch a given test takes -- refreshStatus, loadClips, loadShots and
  // stats.subscribe all run before or after the config.get check, so an
  // unstubbed one would leave a call resolving to `undefined` and throw
  // inside `refreshStatus`/`loadClips`/`loadShots` reading its shape.
  function stubDaemonCommands(configFileExists: boolean) {
    callMock.mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'config.get':
          return Promise.resolve({ config_file_exists: configFileExists });
        case 'status':
          return Promise.resolve(statusPayload());
        case 'library.list':
          return Promise.resolve(libraryListPayload());
        case 'shots.list':
          return Promise.resolve(shotsListPayload());
        case 'stats.subscribe':
          return Promise.resolve({});
        default:
          return Promise.resolve({});
      }
    });
  }

  it('opens the wizard when the daemon reports no config file', async () => {
    app.view = 'grid';
    stubDaemonCommands(false);
    const handle = registerConnectedHandler();

    // `onConnected(() => void onDaemonUp())` is fire-and-forget: the handler
    // itself returns nothing awaitable, so the assertion has to wait for the
    // promise chain inside onDaemonUp to settle rather than reading
    // app.view the instant handle() returns.
    handle(undefined);

    await vi.waitFor(() => expect(app.view).toBe('firstrun'));
  });

  it('leaves the grid up when a config file already exists', async () => {
    app.view = 'grid';
    stubDaemonCommands(true);
    const handle = registerConnectedHandler();

    handle(undefined);

    await vi.waitFor(() => expect(callMock).toHaveBeenCalledWith('config.get'));
    expect(app.view).toBe('grid');
  });

  // I1's other hole: a daemon restart while the Screenshots tab is open. Only
  // `Shots.svelte`'s mount loaded `app.shots` before this fix, so a reconnect
  // left it stale. onDaemonUp runs on every connect, including a reconnect
  // (see its own comment), so this is also what the resiliency-panel restart
  // path in App.svelte exercises.
  it('reloads the screenshot list on every connect, same as the clip list', async () => {
    app.view = 'grid';
    stubDaemonCommands(true);
    const handle = registerConnectedHandler();

    handle(undefined);

    await vi.waitFor(() => expect(callMock).toHaveBeenCalledWith('shots.list', expect.anything()));
  });
});

describe('update state', () => {
  it('shows nothing when the check finds nothing', () => {
    // The common case by far. A quiet result must leave the banner absent --
    // an "you are up to date" bar every launch is noise the user never asked
    // for. Applying 'available' first, so there is a release to clear, is
    // what makes this test fail if 'up-to-date' stopped clearing a prior
    // release -- a store that never held one is null regardless.
    const state = new UpdateStore();
    state.apply({
      state: 'available',
      release: { version: '0.5.0', notes_url: 'https://x', zip_url: '', sums_url: '', size: 10 },
    });
    state.apply({ state: 'up-to-date' });
    expect(state.banner).toBe(null);
  });

  it('shows the version and keeps it while downloading', () => {
    const state = new UpdateStore();
    state.apply({ state: 'available', release: { version: '0.5.0', notes_url: 'https://x', zip_url: '', sums_url: '', size: 10 } });
    expect(state.banner?.version).toBe('0.5.0');
    state.apply({ state: 'downloading', received: 5, total: 10 });
    expect(state.banner?.version).toBe('0.5.0');
    expect(state.percent).toBe(50);
  });

  // A failure only earns the banner once the user has actually asked Trix to
  // install something (resolution #1: a failed *automatic* check must not
  // show the banner). Driven through install() rather than asserted on a
  // fresh store -- see the test right after this one for the fresh-store
  // case, which is the one this store must stay quiet for.
  it('keeps the failure message visible so the user can act on it', async () => {
    const state = new UpdateStore();
    state.apply({
      state: 'available',
      release: { version: '0.5.0', notes_url: 'https://x', zip_url: '', sums_url: '', size: 10 },
    });
    invokeMock.mockRejectedValueOnce(new Error('checksum did not match'));

    await state.install();

    expect(state.banner?.error).toContain('checksum did not match');
  });

  // The bug this store exists to avoid: launching Trix offline must not
  // paint a red error bar just because the automatic check could not reach
  // GitHub. No install() call ever happened here, so the failure must be
  // dropped rather than shown.
  it('drops a failed event when no install was ever started, so an offline launch stays quiet', () => {
    const state = new UpdateStore();
    state.apply({ state: 'failed', message: 'could not reach GitHub' });
    expect(state.banner).toBe(null);
  });

  // Finding 1: the Update button has no click guard of its own and stays
  // rendered until the first channel event flips `busy` -- a real gap on a
  // slow connection. A second install() landing in that gap must not fire a
  // second `update_install` (Rust's InstallGuard would reject it, and the
  // generic catch here would paint that rejection as a real failure over an
  // install that is proceeding normally).
  it('ignores a second install() call while the first is still in flight, so a double click cannot paint a false failure', async () => {
    const state = new UpdateStore();
    state.apply({
      state: 'available',
      release: { version: '0.5.0', notes_url: 'https://x', zip_url: '', sums_url: '', size: 10 },
    });
    let resolveInstall = () => {};
    invokeMock.mockImplementationOnce(
      () =>
        new Promise<void>((resolve) => {
          resolveInstall = resolve;
        }),
    );

    const firstInstall = state.install();
    await state.install(); // the second click, before the first has resolved

    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(state.error).toBe(null);

    resolveInstall();
    await firstInstall;
  });

  // Finding I-1: `installTriggered` used to double as both the sticky
  // failed-event gate and install()'s click guard. That made a retry after
  // a genuinely retryable failure (checksum mismatch, dropped connection, a
  // momentarily full disk) impossible: the banner goes back to its offer
  // branch on the next `available` event (e.g. Settings' "Check now"), the
  // Update button reappears, but clicking it hit the still-set flag and
  // returned silently -- no invoke, no event, no progress, no error. This
  // drives exactly that sequence and asserts the second click actually
  // reaches Rust.
  it('actually invokes on a second install() after a failed install is followed by a fresh available event', async () => {
    const state = new UpdateStore();
    const release = { version: '0.5.0', notes_url: 'https://x', zip_url: '', sums_url: '', size: 10 };
    state.apply({ state: 'available', release });
    invokeMock.mockRejectedValueOnce(new Error('checksum did not match'));

    await state.install();
    expect(state.banner?.error).toContain('checksum did not match');

    // A later successful check (Settings' "Check now", or another automatic
    // check) re-offers the same release and clears the error, same as a
    // fresh launch would.
    state.apply({ state: 'available', release });
    expect(state.banner?.error).toBe(null);

    invokeMock.mockResolvedValueOnce(undefined);
    await state.install();

    expect(invokeMock).toHaveBeenCalledTimes(2);
  });
});

describe('UpdateStore.checkOnceAtLaunch', () => {
  // onDaemonUp() runs again on every daemon reconnect (see its own comment),
  // so checkOnceAtLaunch is what stops that from becoming a second automatic
  // GitHub check -- the setting this guards is named "once per launch", not
  // "once per connect".
  it('checks only once across two calls, matching a daemon reconnect', () => {
    const state = new UpdateStore();
    invokeMock.mockResolvedValue(undefined);

    state.checkOnceAtLaunch(true); // the initial connect
    state.checkOnceAtLaunch(true); // a later reconnect

    expect(invokeMock).toHaveBeenCalledTimes(1);
  });

  // If shouldCheck:false stopped consuming the once-per-launch budget, a
  // reconnect after a `check_for_updates: false` launch would run the check
  // the setting was supposed to suppress entirely.
  it('consumes the once-per-launch budget even when shouldCheck is false, so a later reconnect still does not check', () => {
    const state = new UpdateStore();
    invokeMock.mockResolvedValue(undefined);

    state.checkOnceAtLaunch(false);
    expect(invokeMock).not.toHaveBeenCalled();

    state.checkOnceAtLaunch(true); // e.g. a reconnect; must still be skipped
    expect(invokeMock).not.toHaveBeenCalled();
  });
});

describe('UpdateStore.swapping', () => {
  // installing/restarting are the two phases this update itself causes the
  // daemon to disappear for -- App.svelte relies on this to keep DaemonDown
  // (with its Start button Rust would refuse) off screen during the swap.
  it('is true while installing and while restarting', () => {
    const state = new UpdateStore();

    state.apply({ state: 'installing' });
    expect(state.swapping).toBe(true);

    state.apply({ state: 'restarting' });
    expect(state.swapping).toBe(true);
  });

  // downloading/verifying leave the daemon untouched and still running, so a
  // disconnect during either is a genuine "not running" -- `swapping` must
  // stay narrower than `busy` (true for both) or DaemonDown would wrongly
  // stay hidden during an ordinary disconnect mid-download.
  it('is false while downloading and while verifying, even though busy is true for both', () => {
    const state = new UpdateStore();

    state.apply({ state: 'downloading', received: 1, total: 10 });
    expect(state.swapping).toBe(false);
    expect(state.busy).toBe(true);

    state.apply({ state: 'verifying' });
    expect(state.swapping).toBe(false);
    expect(state.busy).toBe(true);
  });
});
