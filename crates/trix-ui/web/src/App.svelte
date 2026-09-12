<script lang="ts">
  import Rail from './views/Rail.svelte';
  import DaemonDown from './views/DaemonDown.svelte';
  import Grid from './views/Grid.svelte';
  import ClipPage from './views/ClipPage.svelte';
  import Shots from './views/Shots.svelte';
  import Settings from './views/Settings.svelte';
  import FirstRun from './views/FirstRun.svelte';
  import Toasts from './components/Toasts.svelte';
  import TitleBar from './components/TitleBar.svelte';
  import Button from './components/ui/Button.svelte';
  import { app, updates, wireDaemon, wireUpdates } from './lib/state.svelte';
  import { isTypingTarget } from './lib/keys';

  // The same page the Rust side checks every opened URL against.
  const RELEASES_URL = 'https://github.com/tnhnblgl/trix/releases';

  wireDaemon();
  wireUpdates();

  /**
   * No webview right-click menu anywhere in the app.
   *
   * Its "Save image as", "Copy image link" and "More tools" belong to a web
   * page, not to Trix. Clip and screenshot cards open their own menu and call
   * `preventDefault` themselves; this catches everything else, so right-click
   * elsewhere does nothing. A text box keeps the native menu -- Cut, Copy and
   * Paste are what anyone right-clicking inside one is after.
   */
  function oncontextmenu(e: MouseEvent) {
    if (!isTypingTarget(e.target)) e.preventDefault();
  }
</script>

<svelte:window {oncontextmenu} />

<div class="app">
  <TitleBar />

  {#if updates.banner}
    <div class="update" class:error={updates.banner.error}>
      {#if updates.banner.error}
        <span>{updates.banner.error}</span>
        <!-- `href` stays so the target is visible on hover and can be copied, but
             the click is handled in Rust: a Tauri webview opens no new window, so
             the default action here is nothing at all. -->
        <a href={RELEASES_URL}
          onclick={(e) => { e.preventDefault(); updates.openReleasesPage(RELEASES_URL); }}
          >Download it by hand</a>
      {:else if updates.busy}
        <span>{updates.phase === 'downloading' ? `Downloading… ${updates.percent}%` : 'Installing…'}</span>
      {:else}
        <span>Trix {updates.banner.version} is available</span>
        <a href={updates.release?.notes_url}
          onclick={(e) => { e.preventDefault(); if (updates.release) updates.openReleasesPage(updates.release.notes_url); }}
          >What's new</a>
        <span class="spacer"></span>
        <Button size="sm" variant="primary" onclick={() => updates.install()}>Update</Button>
      {/if}
    </div>
  {/if}

  {#if updates.swapping}
    <!-- The swap itself is why the daemon looks gone -- DaemonDown's Start
         button would just be refused (an install already owns the pipe), so
         for this window the banner above is the entire window. -->
    <div class="filler"></div>
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
        {:else if app.view === 'shots'}
          <Shots />
        {:else if app.view === 'settings'}
          <Settings />
        {/if}
      </main>
    </div>
  {/if}
</div>

<Toasts />

<style>
  .app { display: flex; flex-direction: column; height: 100vh; }
  .shell { display: flex; flex: 1; min-height: 0; }
  .content { flex: 1; overflow: auto; padding: 18px 20px; }
  .filler { flex: 1; }

  .update {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 8px 16px;
    flex: 0 0 auto;
    background: color-mix(in srgb, var(--accent) 12%, var(--surface));
    border-bottom: 1px solid var(--line);
    font-size: 12.5px;
  }
  .update.error { background: var(--surface); color: var(--dim); }
  .update a { color: var(--accent); }
  .spacer { margin-left: auto; }
</style>
