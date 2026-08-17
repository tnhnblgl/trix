<script lang="ts">
  import Rail from './views/Rail.svelte';
  import DaemonDown from './views/DaemonDown.svelte';
  import Grid from './views/Grid.svelte';
  import ClipPage from './views/ClipPage.svelte';
  import Settings from './views/Settings.svelte';
  import FirstRun from './views/FirstRun.svelte';
  import Toasts from './components/Toasts.svelte';
  import { app, updates, wireDaemon, wireUpdates } from './lib/state.svelte';

  wireDaemon();
  wireUpdates();
</script>

{#if updates.banner}
  <div class="update" class:error={updates.banner.error}>
    {#if updates.banner.error}
      <span>{updates.banner.error}</span>
      <a href="https://github.com/tnhnblgl/trix/releases" target="_blank" rel="noreferrer">Download it by hand</a>
    {:else if updates.busy}
      <span>{updates.phase === 'downloading' ? `Downloading… ${updates.percent}%` : 'Installing…'}</span>
    {:else}
      <span>Trix {updates.banner.version} is available</span>
      <a href={updates.release?.notes_url} target="_blank" rel="noreferrer">What's new</a>
      <button onclick={() => updates.install()}>Update</button>
    {/if}
  </div>
{/if}

{#if updates.swapping}
  <!-- The swap itself is why the daemon looks gone -- DaemonDown's Start
       button would just be refused (an install already owns the pipe), so
       for this window the banner above is the entire window. -->
{:else if !app.connected}
  <DaemonDown />
{:else if app.view === 'firstrun'}
  <FirstRun />
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

  .update { display: flex; align-items: center; gap: 12px; padding: 8px 16px; background: var(--panel); border-bottom: 1px solid var(--line); font-size: 13px; }
  .update.error { color: var(--dim); }
  .update button { margin-left: auto; padding: 5px 12px; border-radius: 6px; border: 1px solid var(--line); background: var(--bg); color: var(--text); font: inherit; cursor: pointer; }
</style>
