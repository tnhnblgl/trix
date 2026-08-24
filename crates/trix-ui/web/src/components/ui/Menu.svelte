<script lang="ts">
  import Icon from './Icon.svelte';
  import { nextIndex } from '../../lib/ui';
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
    onpick,
    onclose,
  }: {
    items: MenuItem[];
    onpick: (id: string) => void;
    onclose: () => void;
  } = $props();

  let active = $state(0);
  let el = $state<HTMLDivElement | null>(null);

  // Focus lands on the menu itself, not on an item: one roving `active`
  // index is simpler than moving DOM focus between items, and it keeps
  // every key press arriving at one handler.
  $effect(() => {
    el?.focus();
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
    }
  }
</script>

<div
  bind:this={el}
  class="menu"
  role="menu"
  tabindex="-1"
  aria-orientation="vertical"
  {onkeydown}>
  {#each items as item, i (item.id)}
    {#if item.separatorBefore}<hr />{/if}
    <button
      type="button"
      class="item"
      class:danger={item.danger}
      class:active={i === active}
      role="menuitem"
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
    border: 1px solid rgba(255, 255, 255, 0.12);
    border-radius: var(--r-md);
    box-shadow: var(--shadow);
  }
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
