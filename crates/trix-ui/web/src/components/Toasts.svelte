<script lang="ts">
  import { app } from '../lib/state.svelte';
</script>

<div class="toasts">
  {#each app.toasts as toast (toast.id)}
    <div class="toast" class:error={toast.kind === 'error'}>{toast.text}</div>
  {/each}
</div>

<style>
  .toasts { position: fixed; right: 18px; bottom: 18px; display: grid; gap: 8px; z-index: 20; }
  .toast { padding: 10px 14px; border-radius: 8px; background: var(--panel); border: 1px solid var(--line); max-width: 46ch; font-size: 13px; }
  .toast.error { border-color: var(--danger); }
</style>
