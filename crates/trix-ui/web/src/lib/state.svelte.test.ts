import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { ClipMeta, DaemonEvent } from './types';

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

const { app, wireDaemon, UpdateStore } = await import('./state.svelte');
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
  invokeMock.mockReset();
  vi.mocked(onDaemonEvent).mockClear();
  vi.mocked(onConnected).mockClear();
  app.clips = [];
  app.total = 0;
  app.selected = 0;
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

describe('wireDaemon: config_changed', () => {
  // The tray's "Change clips folder..." is the only reachable way to change
  // clip_dir outside this app, and it never told an open app anything before
  // this event existed -- the grid kept building asset: URLs against the old
  // directory. Refreshing status is what picks up the new clip_dir the same
  // way the `armed`/`disarmed` cases already do.
  it('refreshes status so a tray-driven clip_dir change reaches app.clipDir', async () => {
    app.status = statusPayload();
    callMock.mockResolvedValue({ ...statusPayload(), clip_dir: 'D:\\new-clips' });
    const handle = registerDaemonEventHandler();

    handle({ event: 'config_changed', data: { clip_dir_resolved: 'D:\\new-clips' } });
    await vi.waitFor(() => expect(app.clipDir).toBe('D:\\new-clips'));

    expect(callMock).toHaveBeenCalledWith('status');
  });
});

describe('onDaemonUp: first-run routing', () => {
  // Every command onDaemonUp can reach needs a stub, regardless of which
  // branch a given test takes -- refreshStatus, loadClips and
  // stats.subscribe all run before or after the config.get check, so an
  // unstubbed one would leave a call resolving to `undefined` and throw
  // inside `refreshStatus`/`loadClips` reading its shape.
  function stubDaemonCommands(configFileExists: boolean) {
    callMock.mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'config.get':
          return Promise.resolve({ config_file_exists: configFileExists });
        case 'status':
          return Promise.resolve(statusPayload());
        case 'library.list':
          return Promise.resolve(libraryListPayload());
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
