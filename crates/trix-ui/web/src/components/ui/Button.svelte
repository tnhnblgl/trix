<script lang="ts">
  import Icon from './Icon.svelte';
  import type { IconName } from '../../lib/icons';
  import type { Snippet } from 'svelte';

  let {
    variant = 'default',
    size = 'md',
    icon,
    disabled = false,
    title,
    onclick,
    children,
  }: {
    variant?: 'primary' | 'default' | 'ghost' | 'danger';
    size?: 'sm' | 'md';
    icon?: IconName;
    disabled?: boolean;
    title?: string;
    onclick?: () => void;
    children: Snippet;
  } = $props();
</script>

<button class="b {variant} {size}" {disabled} {title} {onclick}>
  {#if icon}<Icon name={icon} size={size === 'sm' ? 13 : 14} />{/if}
  {@render children()}
</button>

<style>
  .b {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font: inherit;
    font-size: 12px;
    border-radius: var(--r);
    border: 1px solid var(--line-strong);
    background: var(--raised);
    color: var(--text);
    cursor: pointer;
    transition: background var(--t-fast) var(--ease), border-color var(--t-fast) var(--ease),
      transform var(--t-fast) var(--ease);
  }
  .md { padding: 7px 12px; }
  .sm { padding: 5px 10px; }

  .b:hover { background: #1d222b; }
  .b:active { transform: translateY(1px); }
  .b:disabled { opacity: 0.4; cursor: default; transform: none; background: var(--raised); }

  .primary { background: var(--accent); border-color: var(--accent); color: var(--accent-ink); font-weight: 600; }
  .primary:hover { background: #6ea8ff; }
  .primary:disabled { background: var(--accent); }

  .ghost { background: transparent; border-color: transparent; color: var(--dim); }
  .ghost:hover { background: rgba(255, 255, 255, 0.07); color: var(--text); }

  .danger { background: transparent; border-color: color-mix(in srgb, var(--danger) 40%, transparent); color: var(--danger); }
  .danger:hover { background: color-mix(in srgb, var(--danger) 12%, transparent); }
</style>
