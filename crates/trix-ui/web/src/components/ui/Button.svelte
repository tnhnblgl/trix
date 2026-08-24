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

<button type="button" class="b {variant} {size}" {disabled} {title} {onclick}>
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

  /* Every hover and press is guarded with `:not(:disabled)` rather than
     relying on a later `:disabled` rule to undo them. A browser still
     matches `:hover` on a disabled button -- disabling blocks activation,
     not pointer-over styling -- and `.b:disabled` ties on specificity with
     `.ghost:hover`, so whichever is written last wins. Guarding each one
     makes the result independent of source order. */
  .b:hover:not(:disabled) { background: var(--raised-hi); }
  .b:active:not(:disabled) { transform: translateY(1px); }
  .b:disabled { opacity: 0.4; cursor: default; }

  .primary { background: var(--accent); border-color: var(--accent); color: var(--accent-ink); font-weight: 600; }
  .primary:hover:not(:disabled) { background: var(--accent-hi); }

  .ghost { background: transparent; border-color: transparent; color: var(--dim); }
  .ghost:hover:not(:disabled) { background: var(--hover); color: var(--text); }

  .danger { background: transparent; border-color: color-mix(in srgb, var(--danger) 40%, transparent); color: var(--danger); }
  .danger:hover:not(:disabled) { background: color-mix(in srgb, var(--danger) 12%, transparent); }
</style>
