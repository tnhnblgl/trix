<script lang="ts">
  import Icon from './Icon.svelte';
  import type { IconName } from '../../lib/icons';

  let {
    icon,
    label,
    active = false,
    disabled = false,
    size = 14,
    onclick,
  }: {
    icon: IconName;
    /** Required: this button has no text, so this is its only name. */
    label: string;
    active?: boolean;
    disabled?: boolean;
    size?: number;
    onclick?: () => void;
  } = $props();
</script>

<button type="button" class="ib" class:active aria-label={label} title={label} {disabled} {onclick}>
  <Icon name={icon} {size} />
</button>

<style>
  .ib {
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    padding: 0;
    border-radius: var(--r);
    border: 1px solid transparent;
    background: transparent;
    color: var(--dim);
    cursor: pointer;
    transition: background var(--t-fast) var(--ease), color var(--t-fast) var(--ease);
  }
  .ib:hover:not(:disabled) { background: var(--hover); color: var(--text); }
  .ib:active:not(:disabled) { transform: translateY(1px); }
  .ib.active { background: color-mix(in srgb, var(--accent) 15%, transparent); color: #eaf1fb; }
  .ib:disabled { opacity: 0.35; cursor: default; }
</style>
