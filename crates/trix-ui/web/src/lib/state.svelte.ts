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
  } catch {
    // A config.get that fails is not a reason to force a wizard on someone
    // who may have a perfectly good config; the grid is the safer default.
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
