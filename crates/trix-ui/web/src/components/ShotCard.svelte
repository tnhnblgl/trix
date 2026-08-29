<script lang="ts">
  import { app } from '../lib/state.svelte';
  import { shotThumbUrl, shotUrl } from '../lib/shots';
  import { formatBytes } from '../lib/clips';
  import type { ShotMeta } from '../lib/types';
  import IconButton from './ui/IconButton.svelte';

  let { shot, selected, onopen }: {
    shot: ShotMeta;
    selected: boolean;
    onopen: () => void;
  } = $props();

  const dir = $derived(app.status?.clip_dir ?? '');
  const taken = $derived(new Date(shot.created).toLocaleString());

  /**
   * The thumbnail, falling back to the full image. A screenshot whose
   * thumbnail encode failed is still a perfectly good screenshot, and the spec
   * is explicit that the tile falls back rather than showing a broken image.
   */
  let src = $state('');
  $effect(() => {
    src = dir ? shotThumbUrl(dir, shot.id) : '';
  });
</script>

<div class="card" class:selected>
  <button type="button" class="hit" onclick={onopen} aria-label={`Open screenshot from ${taken}`}>
    <img {src} alt="" loading="lazy" onerror={() => { if (dir) src = shotUrl(dir, shot.id); }} />
  </button>
  <div class="meta">
    <span class="when">{taken}</span>
    <span class="size tnum">{shot.width}x{shot.height} &middot; {formatBytes(shot.bytes)}</span>
  </div>
  <div class="actions">
    <IconButton icon="copy" label="Copy" onclick={() => app.copyShot(shot.id)} />
    <IconButton icon="folder" label="Show in folder" onclick={() => app.revealShot(shot.id)} />
    <IconButton icon="trash" label="Delete" onclick={() => app.deleteShot(shot.id)} />
  </div>
</div>

<style>
  .card {
    position: relative;
    border: 1px solid var(--line);
    border-radius: var(--r);
    overflow: hidden;
    background: var(--surface);
  }
  .card.selected { border-color: var(--accent); }
  .hit { display: block; width: 100%; padding: 0; border: 0; background: none; cursor: pointer; }
  .hit img { display: block; width: 100%; aspect-ratio: 16 / 10; object-fit: cover; }
  .meta { display: flex; flex-direction: column; gap: 2px; padding: 8px 10px; }
  .when { font-size: 12px; }
  .size { font-size: 10.5px; color: var(--faint); }
  .actions {
    position: absolute;
    top: 6px;
    right: 6px;
    display: flex;
    gap: 4px;
    opacity: 0;
    transition: opacity var(--t-fast) var(--ease);
  }
  .card:hover .actions, .card:focus-within .actions { opacity: 1; }
</style>
