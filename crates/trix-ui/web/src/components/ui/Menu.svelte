<script module lang="ts">
  /** Instance counter -- see `uid` below. */
  let nextMenuId = 0;
</script>

<script lang="ts">
  import Icon from './Icon.svelte';
  import { nextIndex, placeMenu } from '../../lib/ui';
  import type { IconName } from '../../lib/icons';

  export type MenuItem = {
    id: string;
    label: string;
    icon?: IconName;
    danger?: true;
    separatorBefore?: true;
  };

  let {
    items,
    at,
    onpick,
    onclose,
  }: {
    items: MenuItem[];
    /**
     * A right-click menu: open at this viewport point instead of dropping
     * down from the parent. Positioned `fixed`, so no ancestor may set a
     * `transform`, `filter` or `will-change` -- any of those would become the
     * box `fixed` is measured from, and the menu would land offset by that
     * ancestor's position. `ClipCard` switches its hover lift off while one is
     * open for exactly this reason.
     */
    at?: { x: number; y: number };
    onpick: (id: string) => void;
    onclose: () => void;
  } = $props();

  let active = $state(0);
  let el = $state<HTMLDivElement | null>(null);
  /** Where a right-click menu settled once its size was known. */
  let placed = $state<{ left: number; top: number } | null>(null);

  // Measured before it is shown: whether it flips up or left depends on its
  // own size, which is not known until it has rendered. Until then it is laid
  // out but invisible, so it never flashes at the unflipped position.
  $effect(() => {
    if (!el || !at) return;
    placed = placeMenu(at.x, at.y, el.offsetWidth, el.offsetHeight, window.innerWidth, window.innerHeight);
  });

  // A right-click menu belongs to the spot it was opened on. Scrolling moves
  // the card out from under it and resizing moves the edges it was fitted
  // to, so either closes it -- as a native context menu does.
  $effect(() => {
    if (!at) return;
    const close = () => onclose();
    document.addEventListener('scroll', close, true);
    window.addEventListener('resize', close);
    window.addEventListener('blur', close);
    return () => {
      document.removeEventListener('scroll', close, true);
      window.removeEventListener('resize', close);
      window.removeEventListener('blur', close);
    };
  });

  /**
   * A prefix unique to this menu instance, for the item ids that
   * `aria-activedescendant` points at.
   *
   * Module-scoped counter rather than a random id: it is deterministic, costs
   * nothing, and two menus can be mounted at once during the frame where one
   * card's menu is closing as another opens. Colliding ids there would leave
   * `aria-activedescendant` naming an element in the wrong menu.
   */
  const uid = `menu-${nextMenuId++}`;

  // Focus lands on the menu itself, not on an item: one roving `active`
  // index is simpler than moving DOM focus between items, and it keeps
  // every key press arriving at one handler.
  //
  // A right-click menu waits until it is placed: while it measures itself it
  // is `visibility: hidden`, and a hidden element refuses focus -- Escape and
  // the arrows would then never reach it. Placed, it is `fixed` and already
  // on screen, so scrolling to it could only move the page out from under it.
  $effect(() => {
    if (at && !placed) return;
    el?.focus(at ? { preventScroll: true } : undefined);
  });

  // Pointerdown, not click: a click that started inside the menu and ended
  // outside it would otherwise close the menu before the item fired.
  $effect(() => {
    const away = (e: PointerEvent) => {
      if (el && e.target instanceof Node && !el.contains(e.target)) onclose();
    };
    document.addEventListener('pointerdown', away, true);
    return () => document.removeEventListener('pointerdown', away, true);
  });

  function onkeydown(e: KeyboardEvent) {
    // Every key this menu understands is stopped here. Without this, Escape
    // would also reach `ClipPage`'s window handler and navigate to the grid
    // while merely closing the menu, and the arrows would move the grid
    // selection underneath the open menu.
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      onclose();
    } else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      e.stopPropagation();
      active = nextIndex(active, e.key === 'ArrowDown' ? 1 : -1, items.length);
    } else if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      e.stopPropagation();
      const item = items[active];
      if (item) onpick(item.id);
    } else if (e.key === 'Tab') {
      // Not prevented: Tab should carry on to the next real control. But the
      // items are `tabindex="-1"`, so focus leaves this subtree entirely --
      // and the key handler lives on `el`, so once focus is gone Escape can
      // no longer reach it. A menu left open behind a focus that has moved on
      // would be undismissable from the keyboard.
      onclose();
    }
  }
</script>

<div
  bind:this={el}
  class="menu"
  class:at={!!at}
  style={at ? (placed ? `left: ${placed.left}px; top: ${placed.top}px` : 'visibility: hidden') : undefined}
  role="menu"
  tabindex="-1"
  aria-orientation="vertical"
  aria-activedescendant={items[active] ? `${uid}-${items[active].id}` : undefined}
  {onkeydown}>
  {#each items as item, i (item.id)}
    {#if item.separatorBefore}<hr />{/if}
    <!-- Focus stays on the container and `active` is a visual index, so
         without `aria-activedescendant` naming this id a screen reader would
         announce nothing as the arrows move. `role="menu"` promises one or
         the other; this is the half that does not fight the pointer. -->
    <button
      type="button"
      id="{uid}-{item.id}"
      class="item"
      class:danger={item.danger}
      class:active={i === active}
      role="menuitem"
      tabindex="-1"
      onpointerenter={() => (active = i)}
      onclick={() => onpick(item.id)}>
      {#if item.icon}<Icon name={item.icon} size={14} />{/if}
      {item.label}
    </button>
  {/each}
</div>

<style>
  .menu {
    position: absolute;
    right: 0;
    top: calc(100% + 4px);
    z-index: 30;
    min-width: 158px;
    padding: 5px;
    display: grid;
    gap: 1px;
    background: var(--overlay);
    border: 1px solid var(--line-strong);
    border-radius: var(--r-md);
    box-shadow: var(--shadow);
  }
  .menu.at { position: fixed; right: auto; top: 0; left: 0; }
  .menu:focus-visible { outline: none; }
  .item {
    display: flex;
    align-items: center;
    gap: 9px;
    width: 100%;
    padding: 7px 9px;
    border: 0;
    border-radius: var(--r-sm);
    background: transparent;
    color: var(--text);
    font: inherit;
    font-size: 12.5px;
    text-align: left;
    cursor: pointer;
  }
  .item.active { background: var(--hover); }
  .item.danger { color: var(--danger); }
  .item.danger.active { background: color-mix(in srgb, var(--danger) 13%, transparent); }
  hr { border: 0; border-top: 1px solid var(--line); margin: 4px 2px; }
</style>
