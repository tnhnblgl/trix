<script lang="ts">
  import { app } from '../lib/state.svelte';
  import { clipUrl, counterLabel, formatBytes, formatDuration } from '../lib/clips';
  import { shouldHandleKey } from '../lib/keys';
  import TrimBar from '../components/TrimBar.svelte';

  let video = $state<HTMLVideoElement | null>(null);
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
  // but `clamp_range` refuses it ("this clip has no duration to trim"). Better
  // not to offer the control at all than to hand back a refusal.
  const canTrim = $derived(clipDurationMs > 0);

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
    const token = ++keyframeToken;
    void (async () => {
      const found = await app.keyframesFor(id);
      if (token === keyframeToken) keyframes = found;
    })();
  });

  function playPause() {
    if (!video) return;
    if (video.paused) void video.play();
    else video.pause();
  }

  async function exportTrim() {
    // `exporting` is the double-fire guard, and it is not cosmetic: the daemon
    // allocates a clip id from the wall clock (YYYYMMDD_HHMMSS) by testing
    // whether the file exists, without reserving the name. Two exports started
    // inside the same second land on the same id and the second overwrites the
    // first, so the second one must not be startable -- by a double click on
    // the button, or by leaning on Ctrl+E.
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
    // Deleting is unrecoverable, so this is a safety gate: every button
    // outside the strip (back, both steppers, Rename, Favorite, Show in
    // Explorer, and the action-row Delete) carries disabled={confirmingDelete}
    // in the markup below, so clicking or keyboard-activating any of them is
    // already inert at the DOM level while the strip is open — a disabled
    // button can't be focused or receive a click, native or synthetic.
    // Escape has no button of its own, so this branch still owns it; every
    // other key is dropped here too, since the app-level shortcuts below
    // (arrows, space, delete) must not act on a different clip while the
    // dialog is up.
    if (confirmingDelete) {
      if (e.key === 'Escape') confirmingDelete = false;
      return;
    }
    // Ahead of `shouldHandleKey`, and ahead of the switch, for two reasons.
    // It carries a modifier, which the switch's plain `e.key` cases cannot
    // express; and it has to survive being pressed while a trim slider has
    // focus, which is exactly where the pointer just was. `shouldHandleKey`
    // drops every key aimed at an <input>, the sliders included, so a Ctrl+E
    // placed below it would be swallowed by the control that set the range.
    // The only other input on this page is the rename field, and the
    // `renaming` branch above has already returned by the time we get here.
    if (e.key === 'e' && e.ctrlKey) {
      e.preventDefault();
      void exportTrim();
      return;
    }
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
        confirmingDelete = true;
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
    <button class="back" onclick={() => (app.view = 'grid')} disabled={confirmingDelete}>&lsaquo; Clips</button>
    <span class="counter">{counterLabel(app.selected, app.clips.length)}</span>
  </header>

  <div class="stage">
    <button class="step" onclick={() => app.step(-1)} disabled={confirmingDelete || app.selected === 0}>&lsaquo;</button>
    <!-- svelte-ignore a11y_media_has_caption -->
    <video bind:this={video} src={clipUrl(app.clipDir, clip.id)} controls autoplay
      ontimeupdate={() => { if (video) playheadMs = video.currentTime * 1000; }}></video>
    <button class="step" onclick={() => app.step(1)} disabled={confirmingDelete || app.selected >= app.clips.length - 1}>&rsaquo;</button>
  </div>

  <!--
    Spec §6.2's slot, full width between the player and the metadata. A plain
    bar with keyframe ticks rather than the filmstrip the spec sketches --
    thumbnails would need a decode path the daemon does not have. Hidden for a
    clip with no duration, which is the one case the daemon will refuse.
  -->
  {#if canTrim}
    <TrimBar
      durationMs={clip.duration_ms}
      {playheadMs}
      {keyframes}
      {inMs}
      {outMs}
      onchange={(i, o) => { inMs = i; outMs = o; }}
      onseek={(ms) => { if (video) video.currentTime = ms / 1000; }} />
  {/if}

  <p class="meta">
    {clip.width}x{clip.height} &middot; {clip.fps} fps &middot; {formatDuration(clip.duration_ms)} &middot;
    {formatBytes(clip.bytes)} &middot; {clip.encoder}{clip.has_audio ? ' + audio' : ''}
  </p>

  <div class="actions">
    {#if renaming}
      <input bind:value={draftTitle} onkeydown={(e) => e.key === 'Enter' && commitRename()} />
      <button onclick={commitRename}>Save</button>
      <button onclick={() => (renaming = false)}>Cancel</button>
    {:else}
      <span class="title">{clip.title}</span>
      <button onclick={startRename} disabled={confirmingDelete}>Rename</button>
      {#if canTrim}
        <button onclick={exportTrim} disabled={confirmingDelete || exporting}>
          {exporting ? 'Exporting…' : 'Export trimmed'}
        </button>
      {/if}
      <button onclick={() => app.setFavorite(clip.id, !clip.favorite)} disabled={confirmingDelete}>
        {clip.favorite ? 'Unfavorite' : 'Favorite'}
      </button>
      <button onclick={() => app.reveal(clip.id)} disabled={confirmingDelete}>Show in Explorer</button>
      <button class="danger" onclick={() => (confirmingDelete = true)} disabled={confirmingDelete}>Delete</button>
    {/if}
  </div>

  {#if confirmingDelete}
    <div class="confirm">
      <p>Delete <strong>{clip.title}</strong>? The mp4, its metadata, and its thumbnail all go.</p>
      <button
        class="danger"
        onclick={async () => {
          confirmingDelete = false;
          await app.remove(clip.id);
        }}>Delete</button
      >
      <button onclick={() => (confirmingDelete = false)}>Keep</button>
    </div>
  {/if}
{/if}

<style>
  .head { display: flex; align-items: center; justify-content: space-between; margin-bottom: 12px; }
  .back { background: none; border: 0; color: var(--dim); font: inherit; cursor: pointer; padding: 0; }
  .back:disabled { opacity: 0.35; cursor: default; }
  .counter { color: var(--dim); font-size: 13px; }
  .stage { display: flex; align-items: center; gap: 10px; }
  .stage video { flex: 1; width: 100%; max-height: 62vh; background: #000; border-radius: 10px; }
  .step { background: none; border: 0; color: var(--text); font-size: 26px; cursor: pointer; padding: 0 6px; }
  .step:disabled { opacity: 0.25; cursor: default; }
  .meta { color: var(--dim); font-size: 12px; margin: 12px 0; }
  .actions { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .actions .title { margin-right: auto; font-weight: 600; }
  .actions button, .confirm button { padding: 6px 12px; border-radius: 6px; border: 1px solid var(--line); background: var(--panel); color: var(--text); font: inherit; cursor: pointer; }
  .actions button:disabled { opacity: 0.4; cursor: default; }
  .danger { border-color: var(--danger) !important; color: var(--danger) !important; }
  .confirm { margin-top: 14px; padding: 12px 14px; border: 1px solid var(--danger); border-radius: 8px; display: flex; align-items: center; gap: 10px; }
  .confirm p { margin: 0 auto 0 0; }
  input { padding: 6px 10px; border-radius: 6px; border: 1px solid var(--line); background: var(--bg); color: var(--text); font: inherit; }
</style>
