<script lang="ts">
  import ClipCard from '../components/ClipCard.svelte';
  import { app } from '../lib/state.svelte';
  import { isActivatableTarget, isTypingTarget, moveSelection } from '../lib/keys';
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
    // meant for a control anywhere in the app arrives here too. A text field
    // keeps every key it's given, arrows included — see `isTypingTarget`.
    if (isTypingTarget(e.target)) return;
    // A `<button>` (the rail's Arm control, a clip card once WebView2 has
    // focused it after a click) only ever consumes Space and Enter itself;
    // this exists because a blanket guard on the tag alone was also eating
    // the arrow keys a focused card should be passing straight through to the
    // grid's own navigation below. See `isActivatableTarget`.
    if (isActivatableTarget(e.target) && (e.key === ' ' || e.key === 'Enter')) return;

    if (e.key === 'Enter') {
      // Nothing to open. `App.svelte` renders branches for `grid` and
      // `settings` only, so switching to `clip` with no clip would paint an
      // empty <main> and unmount this grid — taking this handler with it, and
      // leaving the rail's "Clips" button as the only way back out.
      if (app.clips.length === 0) return;
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
