import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { call, daemonConnected, onConnected, onDaemonEvent, onDisconnected } from './ipc';
import { mergeSaved } from './clips';
import type { ClipMeta, ShotMeta, Status } from './types';

export type View = 'grid' | 'clip' | 'shots' | 'settings' | 'firstrun';
export type Toast = { id: number; kind: 'error' | 'info'; text: string };

let nextToastId = 1;

/**
 * The shortest trim the daemon will accept, mirroring `MIN_TRIM_MS` in
 * `trix-core/src/export.rs`.
 *
 * Duplicated rather than asked for: the daemon exposes no command that
 * reports it, and a range this short is worth refusing with a sentence that
 * names the floor instead of a round trip that comes back as a raw refusal.
 * The daemon still enforces it -- this only decides who explains it.
 */
const MIN_TRIM_MS = 200;

/**
 * Why `[startMs, endMs)` cannot be exported, or null when it can.
 *
 * Pulled out of `exportTrim` so the clip page can ask the same question
 * *before* the user commits: In and Out can be crossed by either slider and
 * by either of `i`/`o`, and until this existed the only thing that said so
 * was a toast fired after the export button had already been pressed. One
 * function rather than two copies of the rule -- the button's disabled state
 * and the refusal that Ctrl+E still needs have to agree, or the button greys
 * out for a range the keyboard would have accepted.
 *
 * Pure and exported for the same reason it is here rather than in the
 * component: this is the one piece of the trim UI a test can execute, since
 * the suite runs on node with no DOM.
 */
export function trimRangeError(startMs: number, endMs: number): string | null {
  if (endMs <= startMs) return 'Set the out point after the in point.';
  if (endMs - startMs < MIN_TRIM_MS) return `A trim has to be at least ${MIN_TRIM_MS} ms long.`;
  return null;
}

/** The ids of the favourited clips in `clips`: the favourites filter's snapshot. */
function favoriteIdsOf(clips: ClipMeta[]): Set<string> {
  return new Set(clips.filter((c) => c.favorite).map((c) => c.id));
}

class AppState {
  connected = $state(false);
  status = $state<Status | null>(null);
  view = $state<View>('grid');
  clips = $state<ClipMeta[]>([]);
  total = $state(0);
  shots = $state<ShotMeta[]>([]);
  shotTotal = $state(0);
  /** Index into `shots` of the tile the grid has selected. */
  shotSelected = $state(0);
  /**
   * Index into `visible` -- not `clips` -- of the clip the grid has selected
   * and the clip page is showing. The two lists are the same list unless the
   * favourites filter is on.
   */
  selected = $state(0);
  /**
   * The favourites filter: the ids of the clips it shows, or null for all.
   *
   * A snapshot taken when the filter is switched on, not a live test of each
   * clip's `favorite`. Unfavouriting a clip while the filter is on leaves it
   * in place until the filter is next switched on: nothing vanishes from
   * under the pointer, a misclicked star can be put straight back, and the
   * clip page is never yanked to a different clip for un-starring the one on
   * screen -- which a live filter would do, because `selected` is an index and
   * the list would shrink in front of it.
   *
   * Not persisted. Every launch opens on the whole library, so nobody opens
   * Trix to find most of their clips apparently gone.
   */
  favoriteIds = $state<Set<string> | null>(null);
  toasts = $state<Toast[]>([]);
  /** Live ring seconds while armed, from `stats`; falls back to `status`. */
  ringUsed = $state(0);
  /**
   * The saved clip hotkey, for the empty grid's "press X while you play".
   * Free to keep: `onDaemonUp` already fetches the whole config to answer the
   * first-run question, so this is one more field off a call already made.
   */
  hotkey = $state('alt+f10');
  /** The saved screenshot hotkey, for the empty tab's "press X while you play". */
  shotHotkey = $state('alt+f8');
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

  get favoritesOnly(): boolean {
    return this.favoriteIds !== null;
  }

  /** The clips the grid shows and the clip page steps through, newest first. */
  get visible(): ClipMeta[] {
    const ids = this.favoriteIds;
    return ids ? this.clips.filter((c) => ids.has(c.id)) : this.clips;
  }

  get current(): ClipMeta | null {
    return this.visible[this.selected] ?? null;
  }

  step(delta: number) {
    const next = this.selected + delta;
    if (next >= 0 && next < this.visible.length) this.selected = next;
  }

  /**
   * Switches the favourites filter, keeping the selection on the same clip
   * when the new list still has it and on the first clip when it does not.
   */
  setFavoritesOnly(on: boolean) {
    const keep = this.current?.id;
    this.favoriteIds = on ? favoriteIdsOf(this.clips) : null;
    const index = this.visible.findIndex((c) => c.id === keep);
    this.selected = index < 0 ? 0 : index;
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
      // A reload after a folder move brings different clips, whose ids the old
      // snapshot cannot know: re-taken, or the filter would show nothing.
      if (this.favoriteIds) this.favoriteIds = favoriteIdsOf(this.clips);
      this.selected = 0;
    } catch (e) {
      this.toast('error', String(e));
    }
  }

  async loadShots() {
    try {
      const page = await call<{ shots: ShotMeta[]; total: number; offset: number }>('shots.list', {
        offset: 0,
        limit: 200,
      });
      this.shots = page.shots;
      this.shotTotal = page.total;
      this.shotSelected = 0;
    } catch (e) {
      this.toast('error', String(e));
    }
  }

  async deleteShot(id: string) {
    try {
      await call('shots.delete', { shot_id: id });
      const index = this.shots.findIndex((s) => s.id === id);
      this.shots = this.shots.filter((s) => s.id !== id);
      this.shotTotal = Math.max(0, this.shotTotal - 1);
      // Keep the selection on the tile that took the deleted one's place, so
      // holding Delete walks the grid instead of jumping back to the start.
      this.shotSelected = Math.min(index < 0 ? 0 : index, Math.max(0, this.shots.length - 1));
    } catch (e) {
      this.toast('error', String(e));
    }
  }

  async revealShot(id: string) {
    try {
      await call('shots.reveal', { shot_id: id });
    } catch (e) {
      this.toast('error', String(e));
    }
  }

  async copyShot(id: string) {
    try {
      await call('shots.copy', { shot_id: id });
      // A clipboard write is otherwise entirely invisible: nothing on screen
      // changes, so without this the button looks broken when it worked.
      this.toast('info', 'Screenshot copied');
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

  /**
   * The clip's keyframe positions, in milliseconds -- the trim bar's ticks.
   *
   * Fast-mode export cuts on keyframes and snaps an in-point backwards to the
   * nearest one, so drawing them is what makes the bar honest: the handle
   * lands where the export will actually cut, not where the pointer was let
   * go.
   */
  async keyframesFor(id: string): Promise<number[]> {
    try {
      const data = await call<{ keyframes: number[] }>('library.keyframes', { clip_id: id });
      return data.keyframes ?? [];
    } catch (e) {
      // A clip whose keyframes cannot be read is still playable, so this
      // degrades to a bar with no ticks rather than an error the user must
      // dismiss before they can watch anything.
      console.warn('keyframes unavailable', e);
      return [];
    }
  }

  /**
   * Exports `[startMs, endMs)` of a clip as a new clip in the library.
   *
   * Both range rules are checked here as well as in the daemon. That is not
   * belt-and-braces for its own sake: the daemon's refusals arrive as bare
   * strings meant for any client, while these two are the mistakes a person
   * actually makes with two sliders, and they deserve a sentence that says
   * what to do about it.
   */
  async exportTrim(id: string, startMs: number, endMs: number) {
    // Kept here even though the clip page now disables the button for these
    // two: Ctrl+E has no button to grey out, and this is also the entry point
    // any future caller reaches.
    const problem = trimRangeError(startMs, endMs);
    if (problem) {
      this.toast('error', problem);
      return;
    }
    // Read before the call, because the response is what replaces it: the
    // question below is "did this clip have sound before I trimmed it", and
    // after `clip_saved` lands `this.clips` also holds the export.
    const sourceHadAudio = this.clips.find((c) => c.id === id)?.has_audio ?? false;
    try {
      // The daemon allocates the new clip's id and path, so there is no
      // destination to send -- and it broadcasts `clip_saved` for the result,
      // which `wireDaemon` already prepends on. Nothing here reloads the grid;
      // doing so would reset `selected` out from under someone still looking
      // at the clip they trimmed. `fast` is the only mode the daemon accepts
      // (it refuses `precise` by name), so it is sent as a constant rather
      // than offered as a choice.
      const saved = await call<ClipMeta>('library.export', {
        clip_id: id,
        start_ms: Math.round(startMs),
        end_ms: Math.round(endMs),
        mode: 'fast',
      });
      // No success toast here. `clip_saved` arrives from the same daemon call
      // and the handler in `wireDaemon` already toasts "Saved <title>" --
      // which for an export names the clip ("... (trimmed)") rather than just
      // saying that something happened, so it is the more useful of the two.
      // One export used to announce itself three times: that toast, a flat
      // "Trimmed clip saved." from here, and the capture chime, which the
      // daemon no longer plays for an export.
      //
      // What is left is the one thing no other message can say. The daemon's
      // audio policy is a documented fallback: if anything about AAC
      // passthrough fails, the whole export is re-run with no audio stream and
      // comes back `has_audio: false` -- a success as far as every other part
      // of this app is concerned. Whether AAC survives passthrough is the one
      // thing about fast mode that could not be verified up front, so on the
      // machine where it does not, *every* export is silent, and without this
      // the only record is a warning in the daemon's log and the only way to
      // find out is to play the clip back.
      if (sourceHadAudio && !saved.has_audio) {
        this.toast(
          'error',
          "The trim was saved without sound: this clip's audio could not be copied.",
        );
      }
    } catch (e) {
      this.toast('error', String(e));
    }
  }

  async remove(id: string) {
    try {
      await call('library.delete', { clip_id: id });
      const index = this.visible.findIndex((c) => c.id === id);
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
      this.selected = Math.min(index < 0 ? 0 : index, Math.max(0, this.visible.length - 1));
      // `visible`, not `clips`: deleting the last favourite while filtering
      // leaves a clip page with nothing to show, library or no library.
      if (this.visible.length === 0) this.view = 'grid';
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
    app.hotkey = String(config['clip_hotkey'] ?? 'alt+f10');
    app.shotHotkey = String(config['screenshot_hotkey'] ?? 'alt+f8');
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
  // A daemon restart while the Screenshots tab is open is the same stale-list
  // hole `loadClips` closes above: `Shots.svelte` only loads on mount, so
  // without this a reconnect would leave the grid showing whatever it had
  // before the daemon went away.
  await app.loadShots();
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
    if (event.event === 'config_changed' && typeof event.data['clip_hotkey'] === 'string') {
      app.hotkey = event.data['clip_hotkey'];
    }
    if (event.event === 'config_changed' && typeof event.data['screenshot_hotkey'] === 'string') {
      app.shotHotkey = event.data['screenshot_hotkey'];
    }
    switch (event.event) {
      case 'armed':
      case 'disarmed':
      // `clip_dir` changes without this app ever seeing a `config.set`
      // reply: the folder dialog belongs to the daemon, whether the tray menu
      // or this page's `folder.pick` opened it. Before this event that told an
      // open app nothing -- the grid kept building `asset:` URLs against the
      // old directory, so every thumbnail broke and every clip stopped playing
      // until the daemon restarted. `config_changed` (state.rs's `set_config`,
      // broadcast from `dispatch.rs`) fires for every accepted `config.set`
      // regardless of who sent it; refreshing `status` here picks up the new
      // `clip_dir` the same way `armed`/`disarmed` do. The Rust side grants
      // the new directory to the asset scope off this same event (see
      // `daemon.rs`), so the two together are what makes a folder change work
      // in an already-open window.
      case 'config_changed': {
        // Refreshing status is enough for a folder that moved *underneath* the
        // same clips; it is not enough for a folder that moved to different
        // ones. `library.list` answers from the daemon's cache, which the move
        // rebuilt (state.rs's `set_config`), so the app has to ask again or it
        // keeps rendering the old folder's clips against the new folder's
        // asset scope -- every thumbnail broken, every clip unplayable.
        // Screenshots hit the identical failure: `Screenshots\` lives under
        // the same `clip_dir`, and the folder dialog this event reacts to can
        // be opened from the tray menu while the app sits on the Screenshots
        // tab, so `loadShots` needs the same after-a-move refresh `loadClips`
        // already gets.
        //
        // Conditional on the folder actually changing, not run on every
        // `config_changed`: `clip_dir_resolved` rides along on all of them
        // (dispatch.rs sends it whether or not `clip_dir` was among the keys),
        // and `loadClips`/`loadShots` reset `selected`/`shotSelected` to 0.
        // Reloading on every bitrate nudge would move the selection out from
        // under whoever is in settings. `status.clip_dir` is the same resolved
        // path, which is what makes this a fair comparison -- and it is read
        // before `refreshStatus` replaces it.
        const moved = String(data['clip_dir_resolved'] ?? '') !== app.clipDir;
        void app.refreshStatus();
        if (moved) {
          void app.loadClips();
          void app.loadShots();
        }
        break;
      }
      case 'stats':
        app.ringUsed = Number(data['ring_seconds_used'] ?? 0);
        break;
      case 'clip_saved': {
        const saved = event.data as unknown as ClipMeta;
        const wasEmpty = app.visible.length === 0;
        // Decide "is this genuinely new" before merging: `mergeSaved` replaces
        // in place when the id is already present (a reconnect's `library.list`
        // racing this same event), and that path must not move the count or
        // the selection -- there is nothing new for either to react to.
        const isNew = !app.clips.some((c) => c.id === saved.id);
        app.clips = mergeSaved(app.clips, saved);
        if (isNew) {
          app.total += 1;
          // `selected` is an index into `app.visible`, and a genuine prepend
          // shifts every existing clip down one slot in every view: Grid's
          // Space previews `app.visible[app.selected]` and Enter opens it, so
          // an unshifted index means the user previews or opens a clip they
          // never picked. Skip the shift when the list was empty before this
          // clip arrived -- it then lands at index 0, which is where
          // `selected` already points -- and when the favourites filter is
          // hiding it, since then nothing in the shown list moved.
          if (!wasEmpty && app.visible.some((c) => c.id === saved.id)) app.selected += 1;
          app.toast('info', `Saved ${saved.title}`);
        }
        break;
      }
      case 'shot_saved': {
        const saved = event.data as unknown as ShotMeta;
        // Prepend: `shot::scan` is newest-first and so is this list.
        if (!app.shots.some((s) => s.id === saved.id)) {
          app.shots = [saved, ...app.shots];
          app.shotTotal += 1;
          // Every existing tile shifts down one slot, so a selection held by
          // index would silently move to a different screenshot -- the same
          // bug `clip_saved` documents for the clip grid.
          if (app.shots.length > 1) app.shotSelected += 1;
          // Prepending the tile is invisible unless the Screenshots tab
          // happens to be open -- the settled scope for this feature is
          // "clipboard + toast + its own sound" on every press, matching
          // `clip_saved`'s toast above. `ShotMeta` has no title (screenshots
          // are never renamed), so this names the thing instead of quoting it.
          app.toast('info', 'Screenshot saved');
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
   * Set the first time `install()` is called, and never cleared afterwards.
   *
   * Gates `apply`'s handling of a `failed` event: a failure only earns the
   * banner once the user has actually asked Trix to install something.
   * Without this, a `failed` event from the automatic launch check -- the
   * ordinary shape of "Trix started while offline" -- would paint a red
   * error bar on every single launch, which is the bug this flag exists to
   * avoid.
   *
   * Deliberately sticky, and deliberately *not* also the re-entrancy guard
   * for `install()` -- that is `installInFlight` below. This one must stay
   * true forever once the user has engaged with an update even once, so
   * that a `failed` event from a *later* automatic check still reaches the
   * banner instead of being dropped as if it were the first, unsolicited,
   * launch-time failure. Using it as the click guard too was the bug: a
   * failed install followed by a fresh `available` event (e.g. Settings'
   * "Check now") put the banner back in its offer branch, but the button
   * then did nothing at all, because this flag was still set from the first
   * attempt and never gets cleared.
   */
  installTriggered = false;

  /**
   * `install()`'s own re-entrancy guard: true from the moment it is called
   * until that attempt resolves or rejects, then cleared either way. See
   * `install()` for what this prevents. Unlike `installTriggered`, this one
   * must reset -- a failed or completed attempt has to let a later `install()`
   * (a genuine retry, or a second update entirely) actually invoke again.
   */
  installInFlight = false;

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
    // Re-entrancy guard: the button stays on screen from click until the
    // first channel event flips `busy` true, a window that spans the
    // synchronous Rust preflight plus the wait for the first HTTP bytes --
    // easily over a second on a slow connection. A second click in that
    // window would send a second `update_install`, which Rust's
    // `InstallGuard` rejects synchronously with a bare string it deliberately
    // does *not* route through `reported()` (a `Failed` event there would
    // paint a false failure over the first install's healthy download). This
    // generic `catch` cannot tell that rejection apart from a real one,
    // though, so without this guard the second call would still show the red
    // failure bar over an install proceeding normally.
    //
    // `installInFlight` resets in `finally`, unlike `installTriggered` --
    // a retry after a genuine failure (checksum mismatch, dropped
    // connection, a disk that was full a moment ago) must actually invoke
    // again. Rust makes that safe: `install()` in `update/mod.rs` clears the
    // staging directory and re-downloads from scratch, so a retry never
    // reuses a consumed payload, and `InstallGuard` still serialises
    // concurrent installs regardless.
    if (this.installInFlight) return;
    this.installInFlight = true;
    this.installTriggered = true;
    try {
      await invoke('update_install', { release: $state.snapshot(this.release) });
    } catch (e) {
      this.apply({ state: 'failed', message: String(e) });
    } finally {
      this.installInFlight = false;
    }
  }

  /**
   * Opens a releases page in the user's browser.
   *
   * A plain `<a target="_blank">` does nothing in a Tauri window -- the
   * webview will not open a second window and the app grants no opener
   * permission -- so both links in the update banner were silently dead. The
   * one that says "Download it by hand" is the escape hatch for an update
   * that failed, which made it the worst possible link to have broken.
   *
   * Errors go to `console.error` and nowhere else. This is already the
   * failure path: painting a second red bar over the first, saying the link
   * did not open, tells the user nothing they can act on that the URL beside
   * them does not.
   */
  async openReleasesPage(url: string) {
    try {
      await invoke('update_open_releases_page', { url });
    } catch (e) {
      console.error('could not open the releases page:', e);
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
