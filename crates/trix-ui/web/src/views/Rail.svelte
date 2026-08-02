<script lang="ts">
  import { app } from '../lib/state.svelte';

  const pct = $derived(
    app.status && app.status.ring_seconds_total > 0
      ? Math.min(100, (app.ringUsed / app.status.ring_seconds_total) * 100)
      : 0,
  );
</script>

<nav class="rail">
  <div class="brand">Trix</div>

  <button class="arm" class:armed={app.armed} disabled={!app.connected} onclick={() => app.toggleArm()}>
    {app.armed ? 'Armed' : 'Arm'}
  </button>

  <div class="ring" title="Replay buffer">
    <div class="bar"><div class="fill" style="width: {pct}%"></div></div>
    <span>{app.ringUsed.toFixed(0)}s / {app.status?.ring_seconds_total.toFixed(0) ?? '0'}s</span>
  </div>

  <div class="nav">
    <button class:active={app.view === 'grid'} onclick={() => (app.view = 'grid')}>Clips</button>
    <button class:active={app.view === 'settings'} onclick={() => (app.view = 'settings')}>Settings</button>
  </div>

  <div class="version">{app.status?.version ?? ''}</div>
</nav>

<style>
  .rail {
    width: 200px;
    flex: 0 0 200px;
    height: 100vh;
    padding: 18px 14px;
    background: var(--panel);
    border-right: 1px solid var(--line);
    display: flex;
    flex-direction: column;
    gap: 18px;
  }
  .brand { font-weight: 600; letter-spacing: 0.04em; }
  .arm {
    padding: 10px;
    border-radius: 8px;
    border: 1px solid var(--line);
    background: transparent;
    color: var(--text);
    font: inherit;
    cursor: pointer;
  }
  .arm.armed { background: var(--accent); border-color: var(--accent); color: #06121f; font-weight: 600; }
  .arm:disabled { opacity: 0.4; cursor: default; }
  .ring { display: grid; gap: 6px; font-size: 12px; color: var(--dim); }
  .bar { height: 4px; background: var(--line); border-radius: 2px; overflow: hidden; }
  .fill { height: 100%; background: var(--accent); transition: width 200ms linear; }
  .nav { display: grid; gap: 4px; }
  .nav button {
    text-align: left;
    padding: 8px 10px;
    border: 0;
    border-radius: 6px;
    background: transparent;
    color: var(--dim);
    font: inherit;
    cursor: pointer;
  }
  .nav button.active { background: var(--line); color: var(--text); }
  .version { margin-top: auto; font-size: 11px; color: var(--dim); }
</style>
