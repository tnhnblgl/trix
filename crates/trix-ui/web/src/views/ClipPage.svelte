<script lang="ts">
  import { app, trimRangeError } from '../lib/state.svelte';
  import { clipUrl, counterLabel, formatBytes, formatDuration } from '../lib/clips';
  import { formatClock } from '../lib/timeline';
  import { shouldHandleKey, sliderOwnsKey } from '../lib/keys';
  import Timeline from '../components/Timeline.svelte';
  import VideoPlayer from '../components/VideoPlayer.svelte';
  import Button from '../components/ui/Button.svelte';
  import IconButton from '../components/ui/IconButton.svelte';
  import Modal from '../components/ui/Modal.svelte';

  let player = $state<VideoPlayer | null>(null);
  let renaming = $state(false);
  let draftTitle = $state('');
  let confirmingDelete = $state(false);
  let keyframes = $state<number[]>([]);
  let inMs = $state(0);
  let outMs = $state(0);
  let playheadMs = $state(0);
  let exporting = $state(false);

  const clip = $derived(app.current);

  // Read off `clip` as primitives rather than tracking the object: `rename`
  // and `setFavorite` both replace the ClipMeta in `app.clips` with a fresh
  // object, so an effect that depended on `clip` itself would throw away
  // in/out points and re-fetch the ticks every time someone starred the clip
  // they were halfway through trimming. The id and the duration are what the
  // trim bar actually depends on, and neither changes under a rename.
  const clipId = $derived(clip?.id ?? null);
  const clipDurationMs = $derived(clip?.duration_ms ?? 0);

  // `library::scan` adopts a bare .mp4 with `duration_ms: 0` -- which is what a
  // clip whose sidecar write failed looks like, and what any file dropped into
  // the clips folder by hand looks like. It plays, and the bar would even draw,
  // but `clamp_range` refuses it ("this clip has no duration to trim"). So the
  // trim chrome is withheld and the band is not: the player reads a real
  // duration off the file, which is enough to seek against, while In, Out and
  // Export would only earn that refusal.
  const canTrim = $derived(clipDurationMs > 0);

  // Why the current range cannot be exported, or null when it can. Nothing
  // stops In and Out crossing -- both sliders and both of `i`/`o` can do it --
  // and the deliberate choice here is to *say so* rather than to silently drag
  // the partner handle along. `i` and `o` mean "mark the frame I am looking
  // at"; a clamp that answered `i` at 12s with an in-point of 8s would be
  // moving a point the user had just set on purpose. So the mistake is named
  // where it is made: the readout under the bar states it and the Export
  // button is disabled, instead of a toast arriving after the press.
  // Same rule as `exportTrim`'s refusal, from the same function, so the greyed
  // button and Ctrl+E can never disagree about what is exportable.
  const rangeError = $derived(canTrim ? trimRangeError(inMs, outMs) : null);

  // Guards against a stale fetch landing on the wrong clip. Stepping through
  // clips with the arrow keys is faster than a keyframe index of a long clip,
  // and the ticks of the clip you left must not appear over the one you are
  // now looking at. A plain `let`, not `$state`: nothing renders it.
  let keyframeToken = 0;

  // Re-fetch whenever the page shows a different clip. Prev/next is a UI-side
  // repoint of the same component, so without this the bar would keep the
  // previous clip's ticks and trim points.
  $effect(() => {
    const id = clipId;
    if (!id) return;
    inMs = 0;
    outMs = clipDurationMs;
    playheadMs = 0;
    // Cleared with the rest, not left standing until the new fetch lands. The
    // token below stops a *late* result appearing over the wrong clip; it does
    // nothing about the *previous* result still being displayed, and a
    // keyframe index takes longer than an arrow key. Holding the old list
    // would draw the previous clip's ticks rescaled to this clip's duration
    // and -- worse -- `snap()` would report an in-point off that list, so the
    // readout would name a cut the daemon is not going to make. No ticks for a
    // moment is honest; the wrong ticks are not.
    keyframes = [];
    const token = ++keyframeToken;
    void (async () => {
      const found = await app.keyframesFor(id);
      if (token === keyframeToken) keyframes = found;
    })();
  });

  function playPause() {
    player?.toggle();
  }

  /**
   * The only way this page opens a `Modal`, and it leaves fullscreen first.
   *
   * `VideoPlayer`'s `.stage:fullscreen` is the fullscreen element, and
   * `Modal`'s scrim and panel are mounted outside `.stage` -- so the browser
   * paints the stage over both. The dialog still mounts, still takes focus and
   * still traps Tab, all off screen: Escape is the only way back and nothing
   * says so. Refusing `Delete` while fullscreen would be worse -- a key that
   * silently does nothing -- so the video comes out of fullscreen and the
   * question is asked where it can be read.
   */
  function askToDelete() {
    if (document.fullscreenElement) void document.exitFullscreen();
    confirmingDelete = true;
  }

  async function exportTrim() {
    // `exporting` is the double-fire guard. The data-loss reason it was added
    // is gone -- clip ids come from the wall clock (YYYYMMDD_HHMMSS) and the
    // daemon used to pick one by asking whether the file existed, so two
    // exports inside the same second landed on the same id and the second
    // overwrote the first; `allocate_clip_id` now reserves the name atomically
    // and the collision resolves to `_2`. What is left is still worth keeping:
    // a double click on the button, or leaning on Ctrl+E, would otherwise put
    // two identical trims in the library and make the user delete one.
    if (!clip || exporting) return;
    // Ctrl+E has no button to grey out, so the zero-duration clip is refused
    // here in plain words rather than by silently doing nothing.
    if (!canTrim) {
      app.toast('error', 'This clip has no duration to trim.');
      return;
    }
    exporting = true;
    try {
      await app.exportTrim(clip.id, inMs, outMs);
    } finally {
      exporting = false;
    }
  }

  function onkeydown(e: KeyboardEvent) {
    if (renaming) {
      if (e.key === 'Escape') renaming = false;
      return;
    }
    // Every key is dropped here so the app-level shortcuts below (arrows,
    // space, delete) cannot act on a different clip while the dialog is up.
    // Escape is deliberately not handled: `Modal` closes itself, from a
    // handler bound to its own panel, and stops the event there -- so a
    // press inside the dialog never reaches this handler at all. This branch
    // is the guard for a key press that arrives from somewhere else entirely
    // while the dialog happens to be open.
    if (confirmingDelete) return;
    // Ahead of the switch because it carries a modifier, which the switch's
    // plain `e.key` cases cannot express, and ahead of `shouldHandleKey` so
    // that the one shortcut with no button to fall back on cannot be taken
    // away by whatever happens to hold focus. Nothing on the page takes it
    // away today: the trim controls are `Timeline`'s `role="slider"` spans
    // rather than `<input>`s, so `shouldHandleKey` has no reason to drop
    // anything aimed at them, and a button only ever claims Space and Enter.
    // So the position is insurance against a control this page grows later,
    // not a live workaround — but it is the one shortcut that would have no
    // way back if it were ever wrong, which is why it keeps the insurance.
    // The only other input on this page is the rename field, and the
    // `renaming` branch above has already returned by the time we get here.
    // `!e.altKey` is not defensive tidiness: Windows reports AltGr as
    // Ctrl+Alt, so every AltGr press arrives here already looking like Ctrl.
    // This machine runs a Turkish Q layout, where AltGr is a live typing
    // modifier (AltGr+E is the euro sign), and an AltGr combination the layout
    // does not map still delivers the base letter in `e.key` -- so a
    // `ctrlKey`-only test fires a full export and preventDefault()s the
    // keystroke, leaving a stray trimmed clip in the library from a press
    // meant to type a character. Settings.svelte's `captureHotkey` already
    // keeps ctrl and alt apart when it records a combo; this is the same
    // distinction on the reading side.
    if (e.key === 'e' && e.ctrlKey && !e.altKey) {
      e.preventDefault();
      void exportTrim();
      return;
    }
    // A focused slider owns Left and Right, and every control on `Timeline`
    // that can hold focus is one: In steps by keyframes, Out by a tenth of a
    // second, the playhead by five seconds -- one with Shift. `Timeline`
    // stops those events itself, so this is belt and braces for the case
    // where focus is on one of them but the event was retargeted; everywhere
    // else the arrows still step clips.
    if (sliderOwnsKey(e.target, e.key)) return;
    // Every button on this page owns its own Space and Enter (WebView2
    // focuses a button when it is clicked, and a button's native activation
    // must not be stolen by a window-level shortcut) — arrows are never part
    // of what a button owns, so they always fall through regardless.
    if (!shouldHandleKey(e.target, e.key)) return;
    switch (e.key) {
      case 'Escape':
        app.view = 'grid';
        break;
      case 'ArrowLeft':
        e.preventDefault();
        app.step(-1);
        break;
      case 'ArrowRight':
        e.preventDefault();
        app.step(1);
        break;
      case ' ':
        e.preventDefault();
        playPause();
        break;
      // Set from the playhead, not from where the slider happens to sit: the
      // point of `i`/`o` is to mark the frame you are looking at.
      case 'i':
        inMs = playheadMs;
        break;
      case 'o':
        outMs = playheadMs;
        break;
      case 'Delete':
        askToDelete();
        break;
    }
  }

  function startRename() {
    if (!clip) return;
    draftTitle = clip.title;
    renaming = true;
  }

  async function commitRename() {
    if (clip && draftTitle.trim()) await app.rename(clip.id, draftTitle.trim());
    renaming = false;
  }
</script>

<svelte:window {onkeydown} />

{#if clip}
  <header class="head">
    <Button variant="ghost" size="sm" icon="chevron-left" onclick={() => (app.view = 'grid')}>
      Clips
    </Button>
    <span class="spacer"></span>
    <span class="counter tnum">{counterLabel(app.selected, app.clips.length)}</span>
    <IconButton icon="chevron-left" label="Previous clip"
      disabled={app.selected === 0} onclick={() => app.step(-1)} />
    <IconButton icon="chevron-right" label="Next clip"
      disabled={app.selected >= app.clips.length - 1} onclick={() => app.step(1)} />
  </header>

  <VideoPlayer
    bind:this={player}
    src={clipUrl(app.clipDir, clip.id)}
    ontime={(ms) => (playheadMs = ms)}>
    {#snippet timeline(playerDurationMs)}
      <!-- A clip with no duration still gets the band, and still cannot be
           trimmed on it: the track and the playhead are there to seek with,
           the handles are not, because `library::scan` adopts a bare .mp4 with
           `duration_ms: 0` and `clamp_range` refuses to trim it. The player
           read a real duration off the file even though the library has none,
           so seeking has something to scale against; the daemon has not, so
           the trim chrome would be an offer it would refuse. Keyframes go with
           the handles -- they exist only to snap the In point, and there is no
           In point to snap. -->
      <Timeline
        durationMs={canTrim ? clip.duration_ms : playerDurationMs}
        trimmable={canTrim}
        {playheadMs}
        keyframes={canTrim ? keyframes : []}
        {inMs}
        {outMs}
        onchange={(i, o) => { inMs = i; outMs = o; }}
        onseek={(ms) => player?.seekTo(ms)} />
    {/snippet}
  </VideoPlayer>

  {#if canTrim}
    <div class="trimrow">
      <span class="txt tnum" class:bad={rangeError !== null}>
        In <b>{formatClock(inMs)}</b> &middot; Out <b>{formatClock(outMs)}</b>
        &middot; <b>{formatClock(outMs - inMs)}</b> selected{#if rangeError}
          &middot; {rangeError}{/if}
      </span>
      <span class="spacer"></span>
      <!-- `title` carries the reason a disabled button is disabled, and the
           readout to its left is the other half of the answer. Same rule as
           `exportTrim`'s refusal, from the same function, so the greyed
           button and Ctrl+E can never disagree about what is exportable. -->
      <Button
        variant="primary"
        icon="scissors"
        disabled={exporting || rangeError !== null}
        title={rangeError ?? ''}
        onclick={exportTrim}>
        {exporting ? 'Exporting…' : 'Export trimmed'}
      </Button>
    </div>
  {/if}

  <p class="meta tnum">
    {clip.width}x{clip.height} &middot; {clip.fps} fps &middot; {formatDuration(clip.duration_ms)} &middot;
    {formatBytes(clip.bytes)} &middot; {clip.encoder}{clip.has_audio ? ' + audio' : ''}
  </p>

  <div class="actions">
    {#if renaming}
      <input class="rn" bind:value={draftTitle}
        onkeydown={(e) => e.key === 'Enter' && commitRename()} />
      <Button variant="primary" size="sm" onclick={commitRename}>Save</Button>
      <Button variant="ghost" size="sm" onclick={() => (renaming = false)}>Cancel</Button>
    {:else}
      <!-- Worded buttons, not bare icons. Borderless icons at the text's own
           dim grey went unnoticed until someone pointed them out, and an icon
           alone still leaves a guess about what a folder or a pencil does.
           Same buttons, same words, as the screenshot viewer's bar.

           One star on this page, and it is the toggle. A second, non-clickable
           star beside the title said the same thing twice, two controls apart,
           against spec 4.1's "one glyph, one colour, one meaning". A
           favourited clip's star takes `--fav`, the colour the grid card's
           star already uses, so the one glyph keeps one colour across pages. -->
      <span class="title">{clip.title}</span>
      <span class="fav" class:on={clip.favorite}>
        <Button size="sm" icon={clip.favorite ? 'star-filled' : 'star'}
          onclick={() => app.setFavorite(clip.id, !clip.favorite)}>
          {clip.favorite ? 'Favourited' : 'Favourite'}
        </Button>
      </span>
      <Button size="sm" icon="pencil" onclick={startRename}>Rename</Button>
      <Button size="sm" icon="folder" onclick={() => app.reveal(clip.id)}>Show in folder</Button>
      <span class="sep"></span>
      <Button variant="danger" size="sm" icon="trash"
        onclick={askToDelete}>Delete</Button>
    {/if}
  </div>

  {#if confirmingDelete}
    <Modal title="Delete this clip?" onclose={() => (confirmingDelete = false)}>
      {#snippet children()}
        <p>Deleting <strong>{clip.title}</strong> removes the mp4, its metadata and its thumbnail. This cannot be undone.</p>
      {/snippet}
      {#snippet actions()}
        <Button variant="ghost" size="sm" onclick={() => (confirmingDelete = false)}>Keep</Button>
        <Button variant="danger" size="sm" icon="trash"
          onclick={async () => { confirmingDelete = false; await app.remove(clip.id); }}>Delete</Button>
      {/snippet}
    </Modal>
  {/if}
{/if}

<style>
  .head { display: flex; align-items: center; gap: 6px; margin-bottom: 12px; }
  .spacer { margin-left: auto; }
  .counter { color: var(--faint); font-size: 11px; margin-right: 4px; }

  .trimrow { display: flex; align-items: center; gap: 10px; margin-top: 12px; }
  .trimrow .txt { font-size: 11.5px; color: var(--dim); }
  .trimrow .txt b { color: var(--text); font-weight: 600; }
  /* An unexportable range reads as a sliver on the band and "0:00.0
     selected" in the numbers, neither of which says what is wrong. This does. */
  .trimrow .txt.bad, .trimrow .txt.bad b { color: var(--danger); }

  .meta { color: var(--faint); font-size: 11px; margin: 14px 0 12px; }

  .actions { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; }
  .actions .title { margin-right: auto; min-width: 0; font-weight: 650; font-size: 13.5px; }
  .fav { display: contents; }
  .fav.on :global(svg) { color: var(--fav); }
  .sep { width: 1px; height: 18px; background: var(--line); margin: 0 4px; }
  .rn {
    flex: 1;
    padding: 6px 10px;
    border-radius: var(--r);
    border: 1px solid var(--accent);
    background: var(--bg);
    color: var(--text);
    font: inherit;
  }
</style>
