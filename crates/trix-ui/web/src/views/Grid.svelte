<script lang="ts">
  import { tick, untrack } from 'svelte';
  import ClipCard from '../components/ClipCard.svelte';
  import Icon from '../components/ui/Icon.svelte';
  import Button from '../components/ui/Button.svelte';
  import Modal from '../components/ui/Modal.svelte';
  import type { ClipMeta } from '../lib/types';
  import { app } from '../lib/state.svelte';
  import { shouldHandleKey, moveSelection, CARD_CONTROL_SELECTOR } from '../lib/keys';
  import { clipUrl, formatBytes } from '../lib/clips';
  import { gridWindow, toggleClipSelection } from '../lib/clip-grid';
  import { formatCombo } from '../lib/ui';

  const hotkeyCaps = $derived(formatCombo(app.hotkey));
  let columns = $state(4);
  let stride = $state(240);
  let scroll = $state(0);
  let viewport = $state(800);
  let gridEl = $state<HTMLDivElement | null>(null);
  let scroller: HTMLElement | null = null;
  let previewing = $state<string | null>(null);
  let deleting = $state<ClipMeta[] | null>(null);
  let deleteContext = 0;
  let selecting = $state(false);
  let checked = $state(new Set<string>());
  let anchor = $state<string | null>(null);
  const visible = $derived(app.visible);
  const windowed = $derived(gridWindow(visible.length, columns, stride, scroll, viewport));
  const mounted = $derived(visible.slice(windowed.start, windowed.end));
  const selectedClips = $derived(visible.filter(clip => checked.has(clip.id)));
  const selectedBytes = $derived(selectedClips.reduce((sum, clip) => sum + clip.bytes, 0));
  const busy = $derived(app.deleteProgress !== null);
  const canSelect = $derived(!app.clipsLoading && !app.clipsLoadError && !busy);

  function clearSelection() {
    selecting = false;
    checked = new Set();
    anchor = null;
  }

  function toggleCheck(id: string, range = false) {
    if (!canSelect) return;
    selecting = true;
    checked = toggleClipSelection(checked, visible.map(c => c.id), id, anchor, range);
    if (!range || anchor === null) anchor = id;
    app.selected = Math.max(0, visible.findIndex(c => c.id === id));
    previewing = null;
  }

  function selectAll() {
    if (!canSelect) return;
    selecting = true;
    checked = new Set(visible.map(c => c.id));
    anchor = visible[0]?.id ?? null;
    previewing = null;
  }

  function pick(clip: ClipMeta, index: number, event?: MouseEvent) {
    app.selected = index;
    if (event && (selecting || event.ctrlKey || event.metaKey || event.shiftKey)) {
      toggleCheck(clip.id, event.shiftKey);
    }
  }

  function askDelete(clips: ClipMeta[]) {
    if (busy || clips.length === 0) return;
    previewing = null;
    deleteContext = app.libraryContext;
    deleting = clips.slice();
  }

  async function confirmDelete() {
    if (!deleting || busy) return;
    const ids = deleting.map(clip => clip.id);
    const context = deleteContext;
    const failed = await app.removeMany(ids, true, context);
    deleting = null;
    if (context !== app.libraryContext) return;
    const attempted = new Set(ids);
    checked = new Set([...checked].filter(id => !attempted.has(id)).concat(failed));
    if (checked.size === 0) clearSelection();
  }

  // A folder switch must not leave a confirmation targeting ids from the old
  // folder. Scrolling and live prepends, in contrast, keep selection by id.
  $effect(() => {
    app.clipDir;
    app.libraryContext;
    untrack(() => { if (!busy) deleting = null; clearSelection(); });
  });

  $effect(() => {
    selecting;
    app.clipsLoading;
    app.clipsLoadError;
    void tick().then(measure);
  });

  function measure() {
    if (!gridEl || !scroller) return;
    const style = getComputedStyle(gridEl);
    columns = Math.max(1, style.gridTemplateColumns.split(' ').length);
    const tile = gridEl.firstElementChild;
    if (tile instanceof HTMLElement) stride = tile.offsetHeight + parseFloat(style.rowGap);
    viewport = scroller.clientHeight;
    scroll = Math.max(0, scroller.getBoundingClientRect().top - gridEl.getBoundingClientRect().top);
  }

  // The scroll container belongs to App, so subscribe to it rather than the
  // window. Only the visible rows plus two spare rows on each side are mounted.
  $effect(() => {
    if (!gridEl) return;
    scroller = gridEl.closest('main');
    if (!scroller) return;
    const host = scroller;
    let frame = 0;
    const schedule = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(measure);
    };
    host.addEventListener('scroll', schedule, { passive: true });
    const observer = new ResizeObserver(schedule);
    observer.observe(host);
    observer.observe(gridEl);
    untrack(measure);
    void tick().then(() => revealSelected());
    return () => {
      host.removeEventListener('scroll', schedule);
      observer.disconnect();
      cancelAnimationFrame(frame);
      scroller = null;
    };
  });

  async function revealSelected(focus = false) {
    if (!gridEl || !scroller) return;
    // The selected tile may not exist in the DOM yet. Scroll using its row,
    // then let Svelte mount that row before restoring keyboard focus.
    const origin = gridEl.getBoundingClientRect().top - scroller.getBoundingClientRect().top + scroller.scrollTop;
    const top = origin + Math.floor(app.selected / columns) * stride;
    const bottom = top + stride;
    if (top < scroller.scrollTop) scroller.scrollTop = Math.max(0, top - 14);
    else if (bottom > scroller.scrollTop + scroller.clientHeight) scroller.scrollTop = bottom - scroller.clientHeight;
    measure();
    await tick();
    if (focus) gridEl?.querySelector<HTMLElement>('[data-index="' + app.selected + '"] .thumb')?.focus({ preventScroll: true });
  }

  function onkeydown(event: KeyboardEvent) {
    if (deleting || busy || event.defaultPrevented) return;
    const target = event.target instanceof Element ? event.target : null;
    const ownedByCardControl = !!target?.closest(CARD_CONTROL_SELECTOR);
    const insideGrid = !ownedByCardControl && !!gridEl && event.target instanceof Node && gridEl.contains(event.target);
    if (!shouldHandleKey(event.target, event.key, insideGrid)) return;
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'a') {
      event.preventDefault();
      selectAll();
      return;
    }
    if (event.key === 'Escape') {
      previewing = null;
      clearSelection();
      return;
    }
    if (event.key === 'Enter') {
      if (visible.length === 0 || selecting) return;
      event.preventDefault();
      app.view = 'clip';
      return;
    }
    if (event.key === ' ') {
      event.preventDefault();
      const clip = visible[app.selected];
      if (selecting && clip) toggleCheck(clip.id, event.shiftKey);
      else previewing = clip && previewing !== clip.id ? clip.id : null;
      return;
    }
    if (event.key === 'Delete') {
      event.preventDefault();
      if (selecting) askDelete(selectedClips);
      else {
        const clip = visible[app.selected];
        if (clip) askDelete([clip]);
      }
      return;
    }
    const next = moveSelection(app.selected, event.key, visible.length, columns);
    if (next !== app.selected) {
      event.preventDefault();
      const previous = visible[app.selected]?.id;
      app.selected = next;
      previewing = null;
      if (event.shiftKey && canSelect) {
        anchor ??= previous ?? null;
        toggleCheck(visible[next].id, true);
      }
      void revealSelected(insideGrid);
    }
  }
</script>

<svelte:window {onkeydown} />

<header class="head">
  <h1>Clips</h1>
  <span class="cnt tnum">
    {#if app.favoritesOnly}{visible.length} of {/if}{app.total} {app.total === 1 ? 'clip' : 'clips'}
    {#if app.totalBytes !== null} &middot; {formatBytes(app.totalBytes)}{/if}
  </span>
  {#if app.clips.length > 0}
    <div class="head-actions">
      <button type="button" class="head-action selection-toggle" class:on={selecting} disabled={!canSelect}
        aria-pressed={selecting}
        onclick={() => selecting ? clearSelection() : (selecting = true)}>
        <Icon name="check" size={13} /> {selecting ? 'Done' : 'Select'}
      </button>
      <button type="button" class="head-action filter" class:on={app.favoritesOnly} disabled={busy}
        aria-pressed={app.favoritesOnly}
        onclick={() => { clearSelection(); previewing = null; app.setFavoritesOnly(!app.favoritesOnly); void revealSelected(); }}>
        <Icon name={app.favoritesOnly ? 'star-filled' : 'star'} size={13} /> Favourites
      </button>
    </div>
  {/if}
</header>

{#if selecting}
  <div class="selection-bar" role="region" aria-label="Clip selection">
    <span class="tnum" aria-live="polite">{selectedClips.length} selected &middot; {formatBytes(selectedBytes)}</span>
    <Button size="sm" variant="ghost" disabled={!canSelect || selectedClips.length === visible.length} onclick={selectAll}>Select all{app.favoritesOnly ? ' favourites' : ''}</Button>
    <Button size="sm" variant="ghost" disabled={busy || checked.size === 0} onclick={() => { checked = new Set(); anchor = null; }}>Clear</Button>
    <Button size="sm" variant="danger" icon="trash" disabled={busy || selectedClips.length === 0} onclick={() => askDelete(selectedClips)}>
      Delete selected ({selectedClips.length})
    </Button>
  </div>
{/if}

{#if app.clipsLoading}
  <p class="load-status" role="status">Loading clips…</p>
{:else if app.clipsLoadError}
  <div class="load-status" role="status">
    <span>Some clips could not be loaded. {app.clipsLoadError}</span>
    <Button size="sm" variant="ghost" onclick={() => app.loadClips()}>Retry</Button>
  </div>
{/if}

{#if app.clips.length === 0}
  {#if !app.clipsLoading && !app.clipsLoadError}
    <div class="empty">
      <p class="big">No clips yet.</p>
      <p>Arm Trix, then press
        {#if hotkeyCaps.length > 0}
          <span class="combo">
            {#each hotkeyCaps as cap, i (i)}
              {#if i > 0}<span class="plus">+</span>{/if}<kbd>{cap}</kbd>
            {/each}
          </span>
        {:else}your clip hotkey{/if}
        while you play.
      </p>
    </div>
  {/if}
{:else if visible.length === 0}
  <div class="empty">
    <p class="big">{app.clipsLoading ? 'Loading favourites…' : 'No favourites yet.'}</p>
    <p>Right-click a clip and choose Favourite, and it will show up here.</p>
  </div>
{:else}
  <div class="grid" bind:this={gridEl} style:padding-top={windowed.top + 'px'} style:padding-bottom={windowed.bottom + 'px'}>
    {#each mounted as clip, localIndex (clip.id)}
      {@const index = windowed.start + localIndex}
      <div class="tile" data-index={index}>
        <ClipCard {clip} clipDir={app.clipDir} selected={index === app.selected}
          selectable={selecting} checked={checked.has(clip.id)}
          ontoggle={(range) => toggleCheck(clip.id, range)}
          onselect={(event) => pick(clip, index, event)}
          onopen={() => { if (!selecting && !busy) { app.selected = index; app.view = 'clip'; } }}
          onfavorite={() => app.setFavorite(clip.id, !clip.favorite)}
          onrename={(title) => app.rename(clip.id, title)}
          onreveal={() => app.reveal(clip.id)}
          ondelete={() => { app.selected = index; askDelete([clip]); }} />
        {#if previewing === clip.id}
          <!-- svelte-ignore a11y_media_has_caption -->
          <video class="preview" src={clipUrl(app.clipDir, clip.id)} autoplay loop muted></video>
        {/if}
      </div>
    {/each}
  </div>
{/if}

{#if deleting}
  <Modal title={deleting.length === 1 ? 'Delete this clip?' : 'Delete ' + deleting.length + ' clips?'}
    onclose={() => { if (!busy) deleting = null; }}>
    {#snippet children()}
      <p>
        {#if deleting?.length === 1}Deleting <strong>{deleting[0].title}</strong>
        {:else}Deleting these <strong>{deleting?.length} clips</strong>{/if}
        removes the videos, their metadata and thumbnails. This cannot be undone.
      </p>
      {#if app.deleteProgress}<p role="status">Deleting {app.deleteProgress.done} of {app.deleteProgress.total}…</p>{/if}
    {/snippet}
    {#snippet actions()}
      <Button variant="ghost" size="sm" disabled={busy} onclick={() => (deleting = null)}>Keep</Button>
      <Button variant="danger" size="sm" icon="trash" disabled={busy} onclick={confirmDelete}>
        {busy ? 'Deleting…' : deleting?.length === 1 ? 'Delete' : 'Delete all ' + deleting?.length}
      </Button>
    {/snippet}
  </Modal>
{/if}

<style>
  .head { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; margin-bottom: 14px; }
  .head h1 { margin: 0; font-size: 15px; font-weight: 650; }
  .cnt { font-size: 11px; color: var(--faint); }
  .head-actions { display: flex; align-items: center; gap: 6px; margin-left: auto; }
  .head-action { display: inline-flex; align-items: center; gap: 6px; padding: 5px 11px; border-radius: var(--r-full); border: 1px solid var(--line-strong); background: transparent; color: var(--dim); font: inherit; font-size: 11.5px; font-weight: 600; cursor: pointer; }
  .head-action:hover:not(.on):not(:disabled) { background: var(--hover); border-color: var(--line-hi); color: var(--text); }
  .head-action.on { color: var(--action-color, var(--fav)); border-color: color-mix(in srgb, var(--action-color, var(--fav)) 45%, transparent); background: color-mix(in srgb, var(--action-color, var(--fav)) 12%, transparent); }
  .head-action:disabled { opacity: 0.4; cursor: default; }
  .selection-toggle { --action-color: var(--accent); }
  .selection-bar { position: sticky; top: -18px; z-index: 5; display: flex; flex-wrap: wrap; align-items: center; gap: 8px; margin-bottom: 14px; padding: 10px; border: 1px solid var(--line-strong); border-radius: var(--r-md); background: var(--surface); font-size: 12px; }
  .selection-bar > span { margin-right: auto; }
  .load-status { display: flex; align-items: center; gap: 10px; color: var(--dim); font-size: 12px; margin: 0 0 14px; }
  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(min(230px, 100%), 1fr)); gap: 14px; overflow-anchor: none; }
  .tile { position: relative; min-width: 0; }
  .tile:focus-within, .tile:hover { z-index: 1; }
  .preview { position: absolute; top: 0; left: 0; width: 100%; aspect-ratio: 16 / 10; border-radius: var(--r-md); background: var(--video-bg); object-fit: cover; pointer-events: none; }
  .empty { display: grid; place-content: center; height: 60vh; text-align: center; gap: 6px; color: var(--dim); }
  .empty .big { font-size: 15px; color: var(--text); margin: 0; }
  .empty p { margin: 0; font-size: 12.5px; }
  .combo { display: inline-flex; align-items: center; gap: 5px; vertical-align: middle; }
  .plus { color: var(--faint); font-size: 11px; }
  kbd { padding: 2px 7px; border-radius: var(--r-sm); background: var(--raised); border: 1px solid var(--line-strong); border-bottom-width: 2px; font: inherit; font-size: 11px; font-weight: 600; color: var(--text); }
</style>
