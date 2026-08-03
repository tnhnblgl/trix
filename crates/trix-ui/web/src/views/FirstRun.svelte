<script lang="ts">
  import { onDestroy } from 'svelte';
  import { call, onDaemonEvent } from '../lib/ipc';
  import { app } from '../lib/state.svelte';
  import type { Monitor } from '../lib/types';

  let step = $state(1);
  let monitors = $state<Monitor[]>([]);
  let monitorIndex = $state(0);
  let hotkey = $state('alt+f10');
  let heard = $state(false);
  let clipDir = $state('');

  $effect(() => {
    void (async () => {
      try {
        const list = await call<{ monitors: Monitor[] }>('monitors.list');
        monitors = list.monitors;
        const config = await call<Record<string, unknown>>('config.get');
        hotkey = String(config['clip_hotkey'] ?? 'alt+f10');
        clipDir = String(config['clip_dir_resolved'] ?? '');
      } catch (e) {
        app.toast('error', String(e));
      }
    })();
  });

  // The wizard finishes by setting `app.view = 'grid'`, which destroys this
  // component. `onDaemonEvent` lives on Tauri's event bus, not on the
  // component, so a handler registered without teardown would keep firing
  // into a dead instance's `heard` for the rest of the app's life. Same fix
  // as `Settings.svelte`: capture the unlisten and run it in `onDestroy`.
  const unlisten = onDaemonEvent((event) => {
    if (event.event === 'hotkey_pressed') heard = true;
  });
  onDestroy(() => {
    void unlisten.then((fn) => fn());
  });

  async function finish() {
    try {
      // One call, because config.set is all-or-nothing: a wizard that wrote
      // three keys in three calls could leave a half-configured install behind
      // if the second failed.
      await call('config.set', { monitor_index: monitorIndex, clip_hotkey: hotkey });
      await call('arm');
      await app.refreshStatus();
      app.view = 'grid';
    } catch (e) {
      app.toast('error', String(e));
    }
  }
</script>

<div class="wizard">
  <h1>Set up Trix</h1>
  <p class="step">Step {step} of 3</p>

  {#if step === 1}
    <h2>Which screen do you play on?</h2>
    <div class="choices">
      {#each monitors as monitor (monitor.index)}
        <button class:chosen={monitorIndex === monitor.index} onclick={() => (monitorIndex = monitor.index)}>
          <strong>{monitor.name}</strong>
          <span>{monitor.width}x{monitor.height} - {monitor.adapter}</span>
        </button>
      {/each}
    </div>
    <button class="next" onclick={() => (step = 2)}>Next</button>
  {:else if step === 2}
    <h2>Confirm your clip hotkey</h2>
    <!--
      Press-it-now is a best-effort check, not a guarantee: `heard` only ever
      flips if `hotkey_pressed` arrives, and that only fires when Windows
      actually gave the daemon this combination at startup. If another app
      already owns the combination (a capture overlay is a common culprit),
      this sits on "Waiting for a press..." forever. `Next` still advances
      regardless, so that is not a dead end -- but the wizard itself has no
      way to change the hotkey; that is Settings' job, after setup.
    -->
    <p class="hint">Press it now. Some overlays quietly take a hotkey inside games, so this checks Trix really gets it.</p>
    <input readonly value={hotkey} />
    <p class="hint" class:ok={heard}>{heard ? 'Trix received it.' : 'Waiting for a press...'}</p>
    <button class="next" onclick={() => (step = 3)}>Next</button>
  {:else}
    <h2>Where should clips go?</h2>
    <input readonly value={clipDir} />
    <p class="hint">You can change this any time from the Trix tray icon.</p>
    <button class="next" onclick={finish}>Finish and arm</button>
  {/if}
</div>

<style>
  .wizard { max-width: 560px; margin: 8vh auto; display: grid; gap: 10px; }
  h1 { font-size: 22px; margin: 0; }
  h2 { font-size: 16px; margin: 14px 0 4px; }
  .step { color: var(--dim); margin: 0; font-size: 12px; }
  .choices { display: grid; gap: 8px; }
  .choices button { display: grid; gap: 2px; text-align: left; padding: 10px 12px; border-radius: 8px; border: 1px solid var(--line); background: var(--panel); color: var(--text); font: inherit; cursor: pointer; }
  .choices button.chosen { border-color: var(--accent); }
  .choices span { color: var(--dim); font-size: 12px; }
  .hint { color: var(--dim); font-size: 12px; margin: 2px 0; }
  .hint.ok { color: var(--accent); }
  input { padding: 8px 12px; border-radius: 6px; border: 1px solid var(--line); background: var(--bg); color: var(--dim); font: inherit; }
  .next { justify-self: start; margin-top: 12px; padding: 9px 20px; border-radius: 8px; border: 1px solid var(--accent); background: var(--accent); color: #06121f; font: inherit; font-weight: 600; cursor: pointer; }
</style>
