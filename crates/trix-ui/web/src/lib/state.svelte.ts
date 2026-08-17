import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { call, daemonConnected, onConnected, onDaemonEvent, onDisconnected } from './ipc';
import { mergeSaved } from './clips';
import type { ClipMeta, Status } from './types';

export type View = 'grid' | 'clip' | 'settings' | 'firstrun';
export type Toast = { id: number; kind: 'error' | 'info'; text: string };

let nextToastId = 1;

class AppState {
  connected = $state(false);
  status = $state<Status | null>(null);
  view = $state<View>('grid');
  clips = $state<ClipMeta[]>([]);
  total = $state(0);
  /** Index into `clips` of the clip the clip page is showing. */
  selected = $state(0);
  toasts = $state<Toast[]>([]);
  /** Live ring seconds while armed, from `stats`; falls back to `status`. */
  ringUsed = $state(0);
  /**
   * Config keys accepted while armed whose new value only takes effect at
   * the next arm -- the "Re-arm to apply" banner's contents. Lives on `app`
   * rather than as component-local state in Settings.svelte: that page is
   * mounted only inside `{#if app.view === 'settings'}` (App.svelte), so
   * component-local state does not survive a trip to Clips and back. Before
   * this branch that only cost a stale frame rate; the two keys this branch
   * added are the ones whose unapplied state is a lit taskbar microphone
   * indicator or a missing voice track, so losing the banner on navigation
   * is a privacy and data-loss bug, not a cosmetic one. Cleared only by an
   * actual re-arm (`rearmNow`), never on a timer.
   */
  rearmNeeded = $state<string[]>([]);

  get armed() {
    return this.status?.armed ?? false;
  }

  get clipDir() {
    return this.status?.clip_dir ?? '';
  }

  get current(): ClipMeta | null {
    return this.clips[this.selected] ?? null;
  }

  step(delta: number) {
    const next = this.selected + delta;
    if (next >= 0 && next < this.clips.length) this.selected = next;
  }

  toast(kind: Toast['kind'], text: string) {
    const toast = { id: nextToastId++, kind, text };
    this.toasts = [...this.toasts, toast];
    setTimeout(() => {
      this.toasts = this.toasts.filter((t) => t.id !== toast.id);
    }, 6000);
  }

  async refreshStatus() {
    try {
      this.status = await call<Status>('status');
      this.ringUsed = this.status.ring_seconds_used;
    } catch (e) {
      this.toast('error', String(e));
    }
  }

  async toggleArm() {
    const wasArmed = this.armed;
    try {
      const next = await call<Status>(wasArmed ? 'disarm' : 'arm');
      // `disarm` answers {} rather than a status, so re-read rather than
      // trusting the shape of the reply.
      this.status = 'armed' in next ? next : await call<Status>('status');
    } catch (e) {
      // A failed `arm` also broadcasts an `error` event (dispatch.rs's `fail`,
      // spec §4.4), which the `onDaemonEvent` switch below already toasts --
      // toasting again here would show the same failure twice for a
      // rail-initiated arm. `disarm` broadcasts nothing on failure (by
      // design: dispatch.rs's own comment says only `arm` and `clip` do, since
      // a refused disarm concerns only the client that asked), so that path,
      // and the `status` re-read after it, still need this catch -- it is the
      // only thing that ever tells the user those failed.
      if (wasArmed) this.toast('error', String(e));
    }
  }

  /** Merges newly reported pending-rearm keys into the banner's list, de-duplicated. */
  addRearmNeeded(keys: string[]) {
    if (keys.length === 0) return;
    this.rearmNeeded = [...new Set([...this.rearmNeeded, ...keys])];
  }

  /**
   * The Settings page's "Re-arm now" button: cycle the engine so every
   * pending change takes effect, then clear the banner. This is the only
   * thing that clears `rearmNeeded` -- there is no timer, because a change
   * genuinely has not applied until this runs.
   */
  async rearmNow() {
    await call('disarm');
    await call('arm');
    this.rearmNeeded = [];
    await this.refreshStatus();
  }

  async loadClips() {
    try {
      const page = await call<{ clips: ClipMeta[]; total: number; offset: number }>('library.list', {
        offset: 0,
        limit: 200,
      });
      this.clips = page.clips;
      this.total = page.total;
      this.selected = 0;
    } catch (e) {
      this.toast('error', String(e));
    }
  }

  async rename(id: string, title: string) {
    try {
      const updated = await call<ClipMeta>('library.rename', { clip_id: id, title });
      this.clips = mergeSaved(this.clips, updated);
    } catch (e) {
      this.toast('error', String(e));
    }
  }

  async setFavorite(id: string, favorite: boolean) {
    try {
      const updated = await call<ClipMeta>('library.favorite', { clip_id: id, favorite });
      this.clips = mergeSaved(this.clips, updated);
    } catch (e) {
      this.toast('error', String(e));
    }
  }

  async reveal(id: string) {
    try {
      await call('library.reveal', { clip_id: id });
    } catch (e) {
      this.toast('error', String(e));
    }
  }

  async remove(id: string) {
    try {
      await call('library.delete', { clip_id: id });
      const index = this.clips.findIndex((c) => c.id === id);
      this.clips = this.clips.filter((c) => c.id !== id);
      this.total = Math.max(0, this.total - 1);
      // Keep the selection on a real clip: the one that slid into this slot,
      // or the new last one if the deleted clip was at the end.
      //
      // Recovering by index like this is only correct because `remove`'s one
      // caller (the clip page's delete button) always deletes the clip that
      // is currently selected -- `index` is therefore the selected clip's own
      // old slot. A grid-level delete (deleting a clip the user has not
      // selected) would need to recompute `selected` relative to the clip
      // still being looked at, not to the one just removed; this line would
      // silently move the selection to the wrong clip instead.
      this.selected = Math.min(index < 0 ? 0 : index, Math.max(0, this.clips.length - 1));
      if (this.clips.length === 0) this.view = 'grid';
    } catch (e) {
      this.toast('error', String(e));
    }
  }
}

export const app = new AppState();

/**
 * Everything the app does the moment it has a daemon to talk to.
 *
 * Guarded here rather than at the call sites: the startup poll and the
 * `trix-connected` event are ordered by nothing, so either can arrive first
 * and both will fire for the same connect. `onDisconnected` clears the flag,
 * so a genuine reconnect still runs this.
 */
async function onDaemonUp() {
  if (app.connected) return;
  app.connected = true;
  await app.refreshStatus();
  try {
    // Spec §7.4: no config file means first run. Only the daemon can tell —
    // it knows whether its own `config_path` points at a real file, which a
    // UI has no way to check for itself. Checked right after `refreshStatus`,
    // ahead of `loadClips` and `stats.subscribe`, so the wizard can appear as
    // soon as this resolves instead of waiting on a library scan and a stats
    // subscription an unconfigured install has no use for yet. `app.connected`
    // is already set true above, though, so a brief grid frame before the
    // wizard mounts is still possible -- this narrows that window, it does
    // not close it.
    const config = await call<Record<string, unknown>>('config.get');
    if (config['config_file_exists'] === false) app.view = 'firstrun';
    // Reached only once config.get has actually resolved, which is what
    // makes "skip the automatic check when config.get fails" automatic --
    // the catch below never reaches this line at all. checkOnceAtLaunch
    // itself is what makes this once per launch rather than once per
    // connect, since a reconnect runs onDaemonUp again.
    updates.checkOnceAtLaunch(config['check_for_updates'] !== false);
  } catch {
    // A config.get that fails is not a reason to force a wizard on someone
    // who may have a perfectly good config; the grid is the safer default.
    // The automatic update check is skipped for the same reason: something
    // is already wrong, and a background check is the least important thing
    // on screen.
  }
  await app.loadClips();
  // Stats drive the ring meter; per spec §4.4 the daemon measures nothing
  // until a client asks, so nobody pays for this while no UI is open.
  try {
    await call('stats.subscribe', { enabled: true });
  } catch {
    // A daemon that will not subscribe is still a usable daemon; the meter
    // just falls back to the value `status` reported.
  }
}

/** Subscribes the store to the daemon. Call once, from App.svelte. */
export function wireDaemon() {
  onConnected(() => void onDaemonUp());

  onDisconnected(() => {
    app.connected = false;
    app.status = null;
  });

  // The supervisor connects from Tauri's setup hook and usually wins the race
  // against the webview booting, and Tauri replays nothing to a listener that
  // registered late. Without asking once at startup the app would sit on
  // "Trix isn't running" whenever the daemon was already up -- which, for a
  // daemon that lives in the tray, is the normal way it gets opened.
  void daemonConnected()
    .then((up) => {
      if (up) void onDaemonUp();
    })
    .catch(() => {
      // Nothing to tell the user: failing to ask only means falling back on
      // the event, which is the behaviour they would have had anyway.
    });

  onDaemonEvent((event) => {
    const data = event.data as Record<string, unknown>;
    switch (event.event) {
      case 'armed':
      case 'disarmed':
      // The tray's "Change clips folder..." is the only reachable way to
      // change `clip_dir` outside this app, and it never told an open app
      // anything -- the grid kept building `asset:` URLs against the old
      // directory, so every thumbnail broke and every clip stopped playing
      // until the daemon restarted. `config_changed` (state.rs's
      // `set_config`, broadcast from `dispatch.rs`) fires for every accepted
      // `config.set` regardless of who sent it; refreshing `status` here
      // picks up the new `clip_dir` the same way `armed`/`disarmed` do. The
      // Rust side grants the new directory to the asset scope off this same
      // event (see `daemon.rs`), so the two together are what makes a
      // tray-driven folder change work in an already-open window.
      case 'config_changed':
        void app.refreshStatus();
        break;
      case 'stats':
        app.ringUsed = Number(data['ring_seconds_used'] ?? 0);
        break;
      case 'clip_saved': {
        const saved = event.data as unknown as ClipMeta;
        const wasEmpty = app.clips.length === 0;
        // Decide "is this genuinely new" before merging: `mergeSaved` replaces
        // in place when the id is already present (a reconnect's `library.list`
        // racing this same event), and that path must not move the count or
        // the selection -- there is nothing new for either to react to.
        const isNew = !app.clips.some((c) => c.id === saved.id);
        app.clips = mergeSaved(app.clips, saved);
        if (isNew) {
          app.total += 1;
          // `selected` is an index into `app.clips`, and a genuine prepend
          // shifts every existing clip down one slot in every view: Grid's
          // Space previews `app.clips[app.selected]` and Enter opens it, so
          // an unshifted index means the user previews or opens a clip they
          // never picked. Skip the shift only when the list was empty before
          // this clip arrived -- it then lands at index 0, which is where
          // `selected` already points.
          if (!wasEmpty) app.selected += 1;
          app.toast('info', `Saved ${saved.title}`);
        }
        break;
      }
      case 'error':
        app.toast('error', String(data['error'] ?? 'the daemon reported an error'));
        break;
    }
  });
}

export type Release = {
  version: string;
  notes_url: string;
  zip_url: string;
  sums_url: string;
  size: number;
};

export type UpdateEvent =
  | { state: 'checking' }
  | { state: 'up-to-date' }
  | { state: 'available'; release: Release }
  | { state: 'downloading'; received: number; total: number }
  | { state: 'verifying' }
  | { state: 'installing' }
  | { state: 'restarting' }
  | { state: 'failed'; message: string };

/**
 * The update banner's whole state.
 *
 * Separate from AppState because its lifetime is different: an update is
 * offered once and then either taken or dismissed, while AppState tracks the
 * daemon for the life of the window.
 */
export class UpdateStore {
  release = $state<Release | null>(null);
  phase = $state<UpdateEvent['state']>('up-to-date');
  received = $state(0);
  total = $state(0);
  error = $state<string | null>(null);

  /**
   * Set the moment `install()` is called, and never cleared afterwards.
   *
   * Gates `apply`'s handling of a `failed` event: a failure only earns the
   * banner once the user has actually asked Trix to install something.
   * Without this, a `failed` event from the automatic launch check -- the
   * ordinary shape of "Trix started while offline" -- would paint a red
   * error bar on every single launch, which is the bug this flag exists to
   * avoid.
   */
  installTriggered = false;

  /**
   * Whether the once-per-launch automatic check has already run, or been
   * skipped. Read and set only by `checkOnceAtLaunch`, which is the sole
   * caller allowed to arm it -- see the note there.
   */
  autoCheckDone = false;

  /** `null` means render nothing at all -- see the note on the quiet case. */
  get banner(): { version: string; error: string | null } | null {
    if (this.error) return { version: this.release?.version ?? '', error: this.error };
    if (!this.release) return null;
    return { version: this.release.version, error: null };
  }

  get percent(): number {
    return this.total > 0 ? Math.round((this.received / this.total) * 100) : 0;
  }

  get busy(): boolean {
    return ['downloading', 'verifying', 'installing', 'restarting'].includes(this.phase);
  }

  /**
   * True only for the two phases where this update itself is the reason the
   * daemon looks gone: between `install()` stopping the recorder and the
   * restarted app reconnecting to it.
   *
   * Deliberately narrower than `busy`. `downloading` and `verifying` happen
   * with the daemon untouched and still running, so a disconnect during
   * either of those is a genuine "not running" and `DaemonDown` is the right
   * thing to show. Only `installing` and `restarting` are phases this
   * update itself caused the daemon to disappear for.
   */
  get swapping(): boolean {
    return this.phase === 'installing' || this.phase === 'restarting';
  }

  apply(event: UpdateEvent) {
    if (event.state === 'failed' && !this.installTriggered) {
      // Dropped, not shown -- see the note on `installTriggered`. `check()`'s
      // own catch below already puts an automatic check's own failure on the
      // console; this is the same failure arriving the other way, over the
      // event channel, and there is nowhere better for it to go than nowhere
      // at all.
      return;
    }
    this.phase = event.state;
    if (event.state === 'failed') {
      this.error = event.message;
      return;
    }
    this.error = null;
    if (event.state === 'available') this.release = event.release;
    if (event.state === 'up-to-date') this.release = null;
    if (event.state === 'downloading') {
      this.received = event.received;
      this.total = event.total;
    }
  }

  async check() {
    try {
      await invoke('update_check');
    } catch (e) {
      // An automatic check that fails is a console warning and no more. The
      // user asked to open a clip recorder, not to check for updates;
      // interrupting them because a background request failed is not an
      // acceptable trade.
      console.warn('update check failed', e);
    }
  }

  /**
   * Runs the automatic launch check, but only once per launch and only when
   * `shouldCheck` is true.
   *
   * Called from `onDaemonUp` above, after `config.get` has already resolved
   * -- `checkForUpdates !== false` is computed there, off the same `config`
   * read `onDaemonUp` already does, so this method does not need to know
   * about config keys at all. The guard here, rather than at the call site,
   * is what makes this once per *launch* rather than once per *connect*: the
   * supervisor can reconnect, `onDaemonUp` runs again for it, and a second
   * automatic check on top of the first is exactly what `autoCheckDone`
   * exists to refuse.
   */
  checkOnceAtLaunch(shouldCheck: boolean) {
    if (this.autoCheckDone) return;
    this.autoCheckDone = true;
    if (!shouldCheck) return;
    void this.check();
  }

  async install() {
    if (!this.release) return;
    this.installTriggered = true;
    try {
      await invoke('update_install', { release: $state.snapshot(this.release) });
    } catch (e) {
      this.apply({ state: 'failed', message: String(e) });
    }
  }
}

export const updates = new UpdateStore();

/**
 * Subscribes the store to install events. Call once, from App.svelte,
 * alongside `wireDaemon()`.
 *
 * Only the subscription lives here -- the automatic check itself fires from
 * `onDaemonUp`, after `config.get`. Registering the listener unconditionally
 * at wire time, while gating the automatic check on a setting that is only
 * known once `config.get` returns, is deliberate: install events (always
 * something the user chose to trigger) must never be missed, regardless of
 * `check_for_updates`.
 */
export function wireUpdates() {
  listen<UpdateEvent>('trix-update', (event) => updates.apply(event.payload));
}
