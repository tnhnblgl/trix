<script lang="ts">
  import ClipCard from '../components/ClipCard.svelte';
  import Icon from '../components/ui/Icon.svelte';
  import Button from '../components/ui/Button.svelte';
  import Modal from '../components/ui/Modal.svelte';
  import type { ClipMeta } from '../lib/types';
  import { app } from '../lib/state.svelte';
  import { shouldHandleKey, moveSelection, CARD_CONTROL_SELECTOR } from '../lib/keys';
  import { clipUrl, formatBytes } from '../lib/clips';
  import { formatCombo } from '../lib/ui';

  /**
   * The saved hotkey as keycaps, through the same `formatCombo` `KeycapInput`
   * uses. The empty state printed the raw config string, so this page said
   * `alt+f10` while the Settings row two clicks away said `Alt` + `F10` for
   * the same setting.
   */
  const hotkeyCaps = $derived(formatCombo(app.hotkey));

  /** Kept in sync with the CSS grid below so ArrowDown moves one visual row. */
  let columns = $state(4);
  let previewing = $state<string | null>(null);
  let gridEl = $state<HTMLDivElement | null>(null);
  /**
   * The clip a card's menu or the Delete key asked to delete, while the dialog
   * asks whether to.
   *
   * Hosted here rather than in `ClipCard`: `Modal`'s scrim is
   * `position: fixed`, and a card's hover lift is a `transform`, which would
   * make the card -- not the window -- the box the scrim covers.
   */
  let deleting = $state<ClipMeta | null>(null);

  async function confirmDelete() {
    const clip = deleting;
    deleting = null;
    if (clip) await app.remove(clip.id);
  }

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
    // While the dialog is up, no grid shortcut may act behind it: an arrow
    // would move the selection and Enter would open a clip under the question.
    if (deleting) return;
    // `<svelte:window>` is global for as long as the grid is mounted, so a key
    // meant for a control anywhere in the app arrives here too. Which of them
    // are ours is `shouldHandleKey`'s decision — and it needs to know whether
    // the target is one of our own cards, because those are `<button>`s that
    // WebView2 focuses on click yet whose Space and Enter belong to the grid.
    // The grid claims Space and Enter for its cards, but not for the two
    // controls inside a card that own those keys themselves: the overflow
    // button and, while a card is being renamed, its text box. Without this,
    // Enter on a focused `⋯` would open the clip instead of its menu.
    const target = e.target instanceof Element ? e.target : null;
    const ownedByCardControl = !!target?.closest(CARD_CONTROL_SELECTOR);
    const insideGrid =
      !ownedByCardControl && !!gridEl && e.target instanceof Node && gridEl.contains(e.target);
    if (!shouldHandleKey(e.target, e.key, insideGrid)) return;

    if (e.key === 'Enter') {
      // Nothing to open. With an empty library `app.current` is null, so
      // `ClipPage`'s `{#if clip}` renders nothing at all -- no back button,
      // no Escape target's worth of UI, nothing -- and switching to `clip`
      // would strand the user on a blank page with no way out.
      if (app.visible.length === 0) return;
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
      const clip = app.visible[app.selected];
      previewing = clip && previewing !== clip.id ? clip.id : null;
      return;
    }
    if (e.key === 'Delete') {
      // The same question the card menus and the clip page ask. The rename box
      // never gets here -- `shouldHandleKey` leaves a typing target its own
      // Delete -- so this is always the selected card's.
      const clip = app.visible[app.selected];
      if (!clip) return;
      e.preventDefault();
      deleting = clip;
      return;
    }
    const next = moveSelection(app.selected, e.key, app.visible.length, columns);
    if (next !== app.selected) {
      e.preventDefault();
      app.selected = next;
      previewing = null;
    }
  }

  /**
   * The library's byte total, but only when the whole library is loaded.
   *
   * `library.list` is fetched with `limit: 200`, so on a bigger library
   * `app.clips` is a page and summing it would understate the total by
   * however much did not fit. A count is always true; a size is only true
   * when there is nothing else to count.
   */
  const librarySize = $derived(
    app.clips.length === app.total
      ? formatBytes(app.clips.reduce((sum, c) => sum + c.bytes, 0))
      : null,
  );
</script>

<svelte:window {onkeydown} />

<header class="head">
  <h1>Clips</h1>
  <span class="cnt tnum">
    {#if app.favoritesOnly}
      <!-- "of N clips" rather than "N favourites": a clip un-starred while
           filtering stays in the list until the filter is next switched on. -->
      {app.visible.length} of {app.total} {app.total === 1 ? 'clip' : 'clips'}
    {:else}
      {app.total} {app.total === 1 ? 'clip' : 'clips'}{#if librarySize} &middot; {librarySize}{/if}
    {/if}
  </span>
  {#if app.clips.length > 0}
    <button
      type="button"
      class="filter"
      class:on={app.favoritesOnly}
      aria-pressed={app.favoritesOnly}
      onclick={() => app.setFavoritesOnly(!app.favoritesOnly)}>
      <Icon name={app.favoritesOnly ? 'star-filled' : 'star'} size={13} />
      Favourites
    </button>
  {/if}
</header>

{#if app.clips.length === 0}
  <div class="empty">
    <p class="big">No clips yet.</p>
    <p>
      Arm Trix, then press
      {#if hotkeyCaps.length > 0}
        <span class="combo">
          {#each hotkeyCaps as cap, i (i)}
            {#if i > 0}<span class="plus">+</span>{/if}
            <kbd>{cap}</kbd>
          {/each}
        </span>
      {:else}
        <!-- A combo that formats to nothing would leave a hole in the middle
             of the sentence. The words the base shipped go there instead. -->
        your clip hotkey
      {/if}
      while you play.
    </p>
  </div>
{:else if app.visible.length === 0}
  <div class="empty">
    <p class="big">No favourites yet.</p>
    <p>Right-click a clip and choose Favourite, and it will show up here.</p>
  </div>
{:else}
  <div class="grid" bind:this={gridEl}>
    {#each app.visible as clip, i (clip.id)}
      {#if previewing === clip.id}
        <!-- svelte-ignore a11y_media_has_caption -->
        <video class="preview" src={clipUrl(app.clipDir, clip.id)} autoplay loop muted></video>
      {:else}
        <ClipCard
          {clip}
          clipDir={app.clipDir}
          selected={i === app.selected}
          onselect={() => (app.selected = i)}
          onopen={() => { app.selected = i; app.view = 'clip'; }}
          onfavorite={() => app.setFavorite(clip.id, !clip.favorite)}
          onrename={(title) => app.rename(clip.id, title)}
          onreveal={() => app.reveal(clip.id)}
          ondelete={() => { app.selected = i; deleting = clip; }} />
      {/if}
    {/each}
  </div>
{/if}

{#if deleting}
  <!-- The same question, in the same words, the clip page asks. -->
  <Modal title="Delete this clip?" onclose={() => (deleting = null)}>
    {#snippet children()}
      <p>Deleting <strong>{deleting?.title}</strong> removes the mp4, its metadata and its thumbnail. This cannot be undone.</p>
    {/snippet}
    {#snippet actions()}
      <Button variant="ghost" size="sm" onclick={() => (deleting = null)}>Keep</Button>
      <Button variant="danger" size="sm" icon="trash" onclick={confirmDelete}>Delete</Button>
    {/snippet}
  </Modal>
{/if}

<style>
  .head { display: flex; align-items: baseline; gap: 10px; margin-bottom: 14px; }
  .head h1 { margin: 0; font-size: 15px; font-weight: 650; }
  .cnt { font-size: 11px; color: var(--faint); }
  /* A pill, like the title bar's Arm button, and amber when on -- the colour
     every favourite star in the app already is. */
  .filter {
    margin-left: auto;
    align-self: center;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 5px 11px;
    border-radius: var(--r-full);
    border: 1px solid var(--line-strong);
    background: transparent;
    color: var(--dim);
    font: inherit;
    font-size: 11.5px;
    font-weight: 600;
    cursor: pointer;
    transition: color var(--t-fast) var(--ease), background var(--t-fast) var(--ease), border-color var(--t-fast) var(--ease);
  }
  .filter:hover:not(.on) { background: var(--hover); border-color: var(--line-hi); color: var(--text); }
  .filter.on {
    color: var(--fav);
    border-color: color-mix(in srgb, var(--fav) 45%, transparent);
    background: color-mix(in srgb, var(--fav) 12%, transparent);
  }
  .filter.on:hover { background: color-mix(in srgb, var(--fav) 18%, transparent); }
  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(230px, 1fr)); gap: 14px; }
  .preview { width: 100%; aspect-ratio: 16 / 10; border-radius: var(--r-md); background: var(--video-bg); object-fit: cover; }
  .empty { display: grid; place-content: center; height: 60vh; text-align: center; gap: 6px; color: var(--dim); }
  .empty .big { font-size: 15px; color: var(--text); margin: 0; }
  .empty p { margin: 0; font-size: 12.5px; }
  /* Same 5px gap and same faint `+` KeycapInput uses, so the two renderings
     of one hotkey read as one thing. */
  .combo { display: inline-flex; align-items: center; gap: 5px; vertical-align: middle; }
  .plus { color: var(--faint); font-size: 11px; }
  kbd {
    padding: 2px 7px;
    border-radius: var(--r-sm);
    background: var(--raised);
    border: 1px solid var(--line-strong);
    border-bottom-width: 2px;
    font: inherit;
    font-size: 11px;
    font-weight: 600;
    color: var(--text);
  }
</style>
