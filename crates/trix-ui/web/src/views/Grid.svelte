<script lang="ts">
  import ClipCard from '../components/ClipCard.svelte';
  import { app } from '../lib/state.svelte';
  import { shouldHandleKey, moveSelection } from '../lib/keys';
  import { clipUrl } from '../lib/clips';

  /** Kept in sync with the CSS grid below so ArrowDown moves one visual row. */
  let columns = $state(4);
  let previewing = $state<string | null>(null);
  let gridEl = $state<HTMLDivElement | null>(null);

  $effect(() => {
    if (!gridEl) return;
    const observer = new ResizeObserver(() => {
      const style = getComputedStyle(gridEl!);
      columns = Math.max(1, style.gridTemplateColumns.split(' ').length);
    });
    observer.observe(gridEl);
    return () => observer.disconnect();
  });

  function onkeydown(e: KeyboardEvent) {
    // `<svelte:window>` is global for as long as the grid is mounted, so a key
    // meant for a control anywhere in the app arrives here too. Which of them
    // are ours is `shouldHandleKey`'s decision — and it needs to know whether
    // the target is one of our own cards, because those are `<button>`s that
    // WebView2 focuses on click yet whose Space and Enter belong to the grid.
    const insideGrid = !!gridEl && e.target instanceof Node && gridEl.contains(e.target);
    if (!shouldHandleKey(e.target, e.key, insideGrid)) return;

    if (e.key === 'Enter') {
      // Nothing to open. With an empty library `app.current` is null, so
      // `ClipPage`'s `{#if clip}` renders nothing at all -- no back button,
      // no Escape target's worth of UI, nothing -- and switching to `clip`
      // would strand the user on a blank page with no way out.
      if (app.clips.length === 0) return;
      // Without this a focused card would also fire its own `click` — Chromium
      // activates a button on Enter's keydown — and re-select the clip we are
      // in the middle of leaving.
      e.preventDefault();
      app.view = 'clip';
      return;
    }
    if (e.key === ' ') {
      // Spec §6.2: in the grid, Space previews in place. The "did it save?"
      // glance must not cost a page load.
      e.preventDefault();
      const clip = app.clips[app.selected];
      previewing = clip && previewing !== clip.id ? clip.id : null;
      return;
    }
    const next = moveSelection(app.selected, e.key, app.clips.length, columns);
    if (next !== app.selected) {
      e.preventDefault();
      app.selected = next;
      previewing = null;
    }
  }
</script>

<svelte:window {onkeydown} />

{#if app.clips.length === 0}
  <div class="empty">
    <p>No clips yet.</p>
    <p class="hint">Arm Trix, then press your clip hotkey while you play.</p>
  </div>
{:else}
  <div class="grid" bind:this={gridEl}>
    {#each app.clips as clip, i (clip.id)}
      {#if previewing === clip.id}
        <!-- svelte-ignore a11y_media_has_caption -->
        <video class="preview" src={clipUrl(app.clipDir, clip.id)} autoplay loop muted></video>
      {:else}
        <ClipCard
          {clip}
          clipDir={app.clipDir}
          selected={i === app.selected}
          onselect={() => (app.selected = i)}
          onopen={() => {
            app.selected = i;
            app.view = 'clip';
          }}
        />
      {/if}
    {/each}
  </div>
{/if}

<style>
  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(230px, 1fr)); gap: 16px; }
  .preview { width: 100%; aspect-ratio: 16 / 10; border-radius: 8px; background: #000; object-fit: cover; }
  .empty { display: grid; place-content: center; height: 60vh; text-align: center; gap: 6px; }
  .hint { color: var(--dim); }
</style>
