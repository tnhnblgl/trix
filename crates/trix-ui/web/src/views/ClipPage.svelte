<script lang="ts">
  import { app } from '../lib/state.svelte';
  import { clipUrl, counterLabel, formatBytes, formatDuration } from '../lib/clips';
  import { shouldHandleKey } from '../lib/keys';

  let video = $state<HTMLVideoElement | null>(null);
  let renaming = $state(false);
  let draftTitle = $state('');
  let confirmingDelete = $state(false);

  const clip = $derived(app.current);

  function playPause() {
    if (!video) return;
    if (video.paused) void video.play();
    else video.pause();
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
    <video bind:this={video} src={clipUrl(app.clipDir, clip.id)} controls autoplay></video>
    <button class="step" onclick={() => app.step(1)} disabled={confirmingDelete || app.selected >= app.clips.length - 1}>&rsaquo;</button>
  </div>

  <!--
    Spec §6.2 puts the filmstrip trim bar here, full width, between the player
    and the metadata. It is deliberately absent: trim needs `library.export`,
    which does not exist yet and is the next plan. The slot is left rather than
    the layout redrawn, so adding it is one component and no reshuffle.
  -->

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
