<script lang="ts">
  import Rail from './views/Rail.svelte';
  import DaemonDown from './views/DaemonDown.svelte';
  import Toasts from './components/Toasts.svelte';
  import { app, wireDaemon } from './lib/state.svelte';

  wireDaemon();
</script>

{#if !app.connected}
  <DaemonDown />
{:else}
  <div class="shell">
    <Rail />
    <main class="content">
      {#if app.view === 'grid'}
        <p class="placeholder">Clips arrive in Task 5.</p>
      {:else if app.view === 'settings'}
        <p class="placeholder">Settings arrive in Task 9.</p>
      {/if}
    </main>
  </div>
{/if}

<Toasts />

<style>
  .shell { display: flex; height: 100vh; }
  .content { flex: 1; overflow: auto; padding: 20px 24px; }
  .placeholder { color: var(--dim); }
</style>
