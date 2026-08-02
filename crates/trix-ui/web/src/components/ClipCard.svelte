<script lang="ts">
  import { formatBytes, formatDuration, thumbUrl } from '../lib/clips';
  import type { ClipMeta } from '../lib/types';

  let {
    clip,
    clipDir,
    selected = false,
    onopen,
    onselect,
  }: {
    clip: ClipMeta;
    clipDir: string;
    selected?: boolean;
    onopen: () => void;
    onselect: () => void;
  } = $props();
</script>

<button class="card" class:selected onclick={onselect} ondblclick={onopen}>
  <div class="thumb">
    <img src={thumbUrl(clipDir, clip.id)} alt="" loading="lazy" />
    <span class="len">{formatDuration(clip.duration_ms)}</span>
    {#if clip.favorite}<span class="fav" title="Favorite">*</span>{/if}
  </div>
  <div class="meta">
    <span class="title">{clip.title}</span>
    <span class="sub">{formatBytes(clip.bytes)}</span>
  </div>
</button>

<style>
  .card { display: grid; gap: 8px; padding: 0; border: 2px solid transparent; border-radius: 10px; background: transparent; color: inherit; font: inherit; text-align: left; cursor: pointer; }
  .card.selected { border-color: var(--accent); }
  .thumb { position: relative; aspect-ratio: 16 / 10; border-radius: 8px; overflow: hidden; background: var(--panel); }
  .thumb img { width: 100%; height: 100%; object-fit: cover; display: block; }
  .len { position: absolute; right: 6px; bottom: 6px; padding: 1px 5px; border-radius: 4px; background: rgba(0, 0, 0, 0.7); font-size: 11px; }
  .fav { position: absolute; left: 6px; top: 6px; color: var(--accent); font-size: 16px; }
  .meta { display: grid; padding: 0 2px 4px; }
  .title { font-size: 13px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .sub { font-size: 11px; color: var(--dim); }
</style>
