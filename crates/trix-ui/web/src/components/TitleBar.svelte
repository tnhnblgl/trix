<script lang="ts">
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { app } from '../lib/state.svelte';
  import Icon from './ui/Icon.svelte';
  import IconButton from './ui/IconButton.svelte';

  const win = getCurrentWindow();
  let maximized = $state(false);

  // The maximize button has to show a restore glyph once the window is
  // maximized, and the window can be maximized without this button -- by
  // double-clicking the drag region, by Win+Up, by dragging to the top edge.
  // So the state is read from the window, never inferred from our own clicks.
  $effect(() => {
    let alive = true;
    void win.isMaximized().then((v) => { if (alive) maximized = v; });
    const un = win.onResized(() => {
      void win.isMaximized().then((v) => { if (alive) maximized = v; });
    });
    return () => {
      alive = false;
      void un.then((fn) => fn());
    };
  });

  const pct = $derived(
    app.status && app.status.ring_seconds_total > 0
      ? Math.min(100, (app.ringUsed / app.status.ring_seconds_total) * 100)
      : 0,
  );
</script>

<header class="tb">
  <div class="logo">T</div>
  <span class="name">TRIX</span>

  <button
    class="arm"
    class:on={app.armed}
    disabled={!app.connected}
    onclick={() => app.toggleArm()}>
    <span class="dot"></span>
    {app.armed ? 'ARMED' : 'Arm'}
    {#if app.armed}
      <span class="meter"><i style="width: {pct}%"></i></span>
      <span class="n tnum">
        {app.ringUsed.toFixed(0)}/{app.status?.ring_seconds_total.toFixed(0) ?? '0'}s
      </span>
    {/if}
  </button>

  <!-- The whole remaining width drags the window. Tauri handles
       double-click-to-maximize on this attribute itself. -->
  <div class="grab" data-tauri-drag-region></div>

  <div class="wc">
    <IconButton icon="minimize" label="Minimize" size={11} onclick={() => void win.minimize()} />
    <IconButton
      icon={maximized ? 'restore' : 'maximize'}
      label={maximized ? 'Restore' : 'Maximize'}
      size={11}
      onclick={() => void win.toggleMaximize()} />
    <span class="close">
      <IconButton icon="close" label="Close" size={11} onclick={() => void win.close()} />
    </span>
  </div>
</header>

<style>
  .tb {
    display: flex;
    align-items: center;
    gap: 12px;
    height: 40px;
    padding-left: 12px;
    flex: 0 0 40px;
    background: var(--surface);
    border-bottom: 1px solid var(--line);
  }
  .logo {
    width: 20px;
    height: 20px;
    display: grid;
    place-items: center;
    border-radius: var(--r-sm);
    background: var(--accent);
    color: var(--accent-ink);
    font-size: 11px;
    font-weight: 800;
  }
  .name { font-size: 12px; font-weight: 600; letter-spacing: 0.05em; }
  .grab { flex: 1; align-self: stretch; }

  .arm {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 5px 11px;
    border-radius: var(--r-full);
    border: 1px solid var(--line-strong);
    background: transparent;
    color: var(--dim);
    font: inherit;
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.04em;
    cursor: pointer;
    transition: color var(--t) var(--ease), background var(--t) var(--ease), border-color var(--t) var(--ease);
  }
  .arm .dot { width: 6px; height: 6px; border-radius: 50%; background: var(--faint); transition: background var(--t) var(--ease); }
  .arm.on {
    background: color-mix(in srgb, var(--live) 13%, transparent);
    border-color: color-mix(in srgb, var(--live) 50%, transparent);
    color: var(--live);
  }
  .arm.on .dot { background: var(--live); box-shadow: 0 0 0 3px color-mix(in srgb, var(--live) 20%, transparent); }
  .arm:disabled { opacity: 0.4; cursor: default; }
  .meter { width: 44px; height: 3px; border-radius: 2px; background: var(--line-strong); overflow: hidden; }
  .meter i { display: block; height: 100%; background: var(--live); transition: width 200ms linear; }
  .n { opacity: 0.85; }

  .wc { display: flex; align-self: stretch; }
  /* Windows-standard 44px, and square to the bar rather than the 30px
     rounded shape IconButton uses everywhere else. */
  .wc :global(.ib) { width: 44px; height: 100%; border-radius: 0; }
  .close :global(.ib:hover) { background: var(--win-close); color: var(--text); }
</style>
