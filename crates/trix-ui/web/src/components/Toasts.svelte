<script lang="ts">
  import { app } from '../lib/state.svelte';
  import Icon from './ui/Icon.svelte';
</script>

<div class="toasts">
  {#each app.toasts as toast (toast.id)}
    <div class="toast" class:error={toast.kind === 'error'}>
      {#if toast.kind === 'error'}<Icon name="alert" size={14} />{/if}
      <span>{toast.text}</span>
    </div>
  {/each}
</div>

<style>
  .toasts { position: fixed; right: 18px; bottom: 18px; display: grid; gap: 8px; z-index: 50; }
  .toast {
    display: flex;
    align-items: flex-start;
    gap: 9px;
    padding: 10px 14px;
    max-width: 46ch;
    border-radius: var(--r-md);
    background: var(--overlay);
    border: 1px solid var(--line-strong);
    box-shadow: var(--shadow);
    font-size: 12.5px;
    animation: slide var(--t-slow) var(--ease);
  }
  .toast.error { border-left: 3px solid var(--danger); }
  .toast.error :global(svg) { color: var(--danger); margin-top: 1px; }
  @keyframes slide { from { opacity: 0; transform: translateX(12px); } }
</style>
