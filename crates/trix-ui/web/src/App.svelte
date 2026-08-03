<script lang="ts">
  import Rail from './views/Rail.svelte';
  import DaemonDown from './views/DaemonDown.svelte';
  import Grid from './views/Grid.svelte';
  import ClipPage from './views/ClipPage.svelte';
  import Settings from './views/Settings.svelte';
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
        <Grid />
      {:else if app.view === 'clip'}
        <ClipPage />
      {:else if app.view === 'settings'}
        <Settings />
      {/if}
    </main>
  </div>
{/if}

<Toasts />

<style>
  .shell { display: flex; height: 100vh; }
  .content { flex: 1; overflow: auto; padding: 20px 24px; }
</style>
