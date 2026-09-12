<script lang="ts">
  import { app } from '../lib/state.svelte';
  import { shotThumbUrl, shotUrl } from '../lib/shots';
  import { formatBytes } from '../lib/clips';
  import type { ShotMeta } from '../lib/types';
  import IconButton from './ui/IconButton.svelte';
  import Menu, { type MenuItem } from './ui/Menu.svelte';

  let { shot, selected, onopen, onselect }: {
    shot: ShotMeta;
    selected: boolean;
    onopen: () => void;
    onselect: () => void;
  } = $props();

  /** Where the card was right-clicked, while its menu is open. */
  let menuAt = $state<{ x: number; y: number } | null>(null);

  /** The buttons on the tile, plus Open. */
  const items: MenuItem[] = [
    { id: 'open', label: 'Open' },
    { id: 'copy', label: 'Copy', icon: 'copy' },
    { id: 'reveal', label: 'Show in folder', icon: 'folder' },
    { id: 'delete', label: 'Delete', icon: 'trash', danger: true, separatorBefore: true },
  ];

  function pick(id: string) {
    menuAt = null;
    if (id === 'open') onopen();
    else if (id === 'copy') app.copyShot(shot.id);
    else if (id === 'reveal') app.revealShot(shot.id);
    else if (id === 'delete') app.deleteShot(shot.id);
  }

  function oncontextmenu(e: MouseEvent) {
    e.preventDefault();
    onselect();
    menuAt = { x: e.clientX, y: e.clientY };
  }

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

<!-- svelte-ignore a11y_no_static_element_interactions -->
<!-- The right-click is a pointer shortcut to the tile's own buttons, not the
     only way to reach them, so the card needs no role for it. -->
<div class="card" class:selected {oncontextmenu}>
  <button type="button" class="hit" onclick={onopen} aria-label={`Open screenshot from ${taken}`}>
    <img {src} alt="" loading="lazy" onerror={() => { if (dir) src = shotUrl(dir, shot.id); }} />
  </button>
  <div class="meta">
    <span class="when">{taken}</span>
    <span class="size tnum">{shot.width}x{shot.height} &middot; {formatBytes(shot.bytes)}</span>
  </div>
  <!-- The card/grid control marker (see `keys.ts`'s `CARD_CONTROL_ATTR` doc
       comment): `Shots.svelte`'s grid keyboard handler walks up from the
       focused element with `closest()`, so this one mark on the wrapper
       covers all three buttons and keeps the grid from stealing Enter and
       Space off them, the way `ClipCard`'s equivalent controls are already
       covered. -->
  <div class="actions" data-card-control>
    <IconButton icon="copy" label="Copy" onclick={() => app.copyShot(shot.id)} />
    <IconButton icon="folder" label="Show in folder" onclick={() => app.revealShot(shot.id)} />
    <IconButton icon="trash" label="Delete" onclick={() => app.deleteShot(shot.id)} />
  </div>

  <!-- `fixed`, so the card's `overflow: hidden` does not clip it: only a
       transform, filter or `will-change` on an ancestor would, and neither
       this card nor anything above it sets one. -->
  {#if menuAt}
    <Menu {items} at={menuAt} onpick={pick} onclose={() => (menuAt = null)} />
  {/if}
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
  /* The buttons sit on the screenshot itself, and a screenshot can be any
     colour -- dim grey glyphs on a transparent ground vanished against a
     bright one. So they share a dark pill, the way a video player's controls
     sit over footage, and take full text colour on it. `--scrim-strong` is
     the same wash the clip card's duration badge uses over a video frame. */
  .actions {
    position: absolute;
    top: 6px;
    right: 6px;
    display: flex;
    gap: 2px;
    padding: 3px;
    border-radius: var(--r);
    background: var(--scrim-strong);
    border: 1px solid var(--line-strong);
    box-shadow: 0 2px 10px rgba(0, 0, 0, 0.4);
    opacity: 0;
    transition: opacity var(--t-fast) var(--ease);
  }
  .card:hover .actions, .card:focus-within .actions, .card.selected .actions { opacity: 1; }
  .actions :global(.ib) { width: 28px; height: 28px; color: var(--text); }
  .actions :global(.ib:hover:not(:disabled)) { background: rgba(255, 255, 255, 0.14); }
  /* Delete answers the pointer in red before it is pressed. */
  .actions :global(.ib:last-child:hover:not(:disabled)) {
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 18%, transparent);
  }
</style>
