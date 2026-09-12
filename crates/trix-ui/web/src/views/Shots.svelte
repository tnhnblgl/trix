<script lang="ts">
  import ShotCard from '../components/ShotCard.svelte';
  import ShotViewer from '../components/ShotViewer.svelte';
  import Button from '../components/ui/Button.svelte';
  import Modal from '../components/ui/Modal.svelte';
  import type { ShotMeta } from '../lib/types';
  import { untrack } from 'svelte';
  import { app } from '../lib/state.svelte';
  import { shouldHandleKey, moveSelection, CARD_CONTROL_SELECTOR } from '../lib/keys';
  import { formatCombo } from '../lib/ui';

  /**
   * The saved combination as keycaps, through the same `formatCombo` the
   * settings row uses. The clip grid's empty state once printed the raw config
   * string while Settings two clicks away rendered keycaps for the same
   * setting; this page renders the array the same way `Grid.svelte` does
   * rather than interpolating it as a string, so the two pages cannot drift
   * apart the same way again.
   */
  const hotkeyCaps = $derived(formatCombo(app.shotHotkey));

  let viewing = $state(false);
  let columns = $state(4);
  let gridEl = $state<HTMLDivElement | null>(null);
  /**
   * The screenshot waiting on the delete dialog. Every way of deleting one --
   * the tile's button, its right-click menu, the Delete key -- lands here, so
   * none of them removes a file without asking.
   */
  let deleting = $state<ShotMeta | null>(null);

  function confirmDelete() {
    const shot = deleting;
    deleting = null;
    if (shot) void app.deleteShot(shot.id);
  }

  app.loadShots();

  /**
   * Scrolls the selected tile fully into view, and no further: a tile already
   * on screen does not move, one cut off at an edge scrolls just enough to show
   * it whole. The grid's children are its tiles, one each, in order.
   *
   * Called where a key moves the selection, not from an effect on
   * `app.shotSelected`: that index also shifts when a new screenshot is saved,
   * and scrolling then would yank the page back to a tile the user had
   * deliberately scrolled away from.
   */
  function revealSelected() {
    const tile = gridEl?.children[app.shotSelected];
    if (tile instanceof HTMLElement) tile.scrollIntoView({ block: 'nearest' });
  }

  // Once on arrival too: the selection outlives a trip to another tab, and the
  // selected tile can be far below the top this page opens at.
  $effect(() => {
    if (gridEl) untrack(revealSelected);
  });

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
    // The viewer owns every key while it is open, including the arrows, and
    // the delete dialog owns them while it asks: nothing may act behind it.
    if (viewing || deleting) return;
    const target = e.target instanceof Element ? e.target : null;
    const ownedByCardControl = !!target?.closest(CARD_CONTROL_SELECTOR);
    const insideGrid =
      !ownedByCardControl && !!gridEl && e.target instanceof Node && gridEl.contains(e.target);
    if (!shouldHandleKey(e.target, e.key, insideGrid)) return;

    if (e.key === 'Enter') {
      if (app.shots.length === 0) return;
      e.preventDefault();
      viewing = true;
      return;
    }
    if (e.key === 'Delete') {
      const shot = app.shots[app.shotSelected];
      if (!shot) return;
      e.preventDefault();
      deleting = shot;
      return;
    }
    const next = moveSelection(app.shotSelected, e.key, app.shots.length, columns);
    if (next !== app.shotSelected) {
      e.preventDefault();
      app.shotSelected = next;
      revealSelected();
    }
  }
</script>

<svelte:window on:keydown={onkeydown} />

<div class="page">
  {#if app.shots.length === 0}
    <div class="empty">
      <p>No screenshots yet.</p>
      <p class="hint">
        Press
        {#if hotkeyCaps.length > 0}
          <span class="combo">
            {#each hotkeyCaps as cap, i (i)}
              {#if i > 0}<span class="plus">+</span>{/if}
              <kbd>{cap}</kbd>
            {/each}
          </span>
        {:else}
          your screenshot hotkey
        {/if}
        while Trix is armed.
      </p>
    </div>
  {:else}
    <div class="grid" bind:this={gridEl}>
      {#each app.shots as shot, i (shot.id)}
        <ShotCard
          {shot}
          selected={i === app.shotSelected}
          onselect={() => (app.shotSelected = i)}
          ondelete={() => (deleting = shot)}
          onopen={() => { app.shotSelected = i; viewing = true; }} />
      {/each}
    </div>
  {/if}

  {#if viewing}
    <ShotViewer onclose={() => (viewing = false)} />
  {/if}

  {#if deleting}
    <Modal title="Delete this screenshot?" onclose={() => (deleting = null)}>
      {#snippet children()}
        <p>Deleting the screenshot from <strong>{deleting ? new Date(deleting.created).toLocaleString() : ''}</strong> removes the image and its thumbnail. This cannot be undone.</p>
      {/snippet}
      {#snippet actions()}
        <Button variant="ghost" size="sm" onclick={() => (deleting = null)}>Keep</Button>
        <Button variant="danger" size="sm" icon="trash" onclick={confirmDelete}>Delete</Button>
      {/snippet}
    </Modal>
  {/if}
</div>

<style>
  .page { position: relative; min-height: 100%; }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(240px, 1fr));
    gap: 14px;
  }
  /* Room between a revealed tile and the pane's edge, matching the grid gap. */
  .grid > :global(*) { scroll-margin: 14px 0; }
  .empty {
    display: flex;
    flex-direction: column;
    gap: 6px;
    align-items: center;
    padding: 64px 0;
    color: var(--dim);
  }
  .hint { font-size: 12.5px; color: var(--faint); }
  /* Same 5px gap and same faint `+` `KeycapInput` and `Grid.svelte` use, so
     every rendering of a hotkey in this app reads as one thing. */
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
