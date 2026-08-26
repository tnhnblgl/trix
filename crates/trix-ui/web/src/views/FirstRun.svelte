<script lang="ts">
  import { onDestroy } from 'svelte';
  import { call, onDaemonEvent } from '../lib/ipc';
  import { app } from '../lib/state.svelte';
  import type { Monitor } from '../lib/types';
  import Button from '../components/ui/Button.svelte';
  import KeycapInput from '../components/ui/KeycapInput.svelte';
  import Select from '../components/ui/Select.svelte';

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
    if (event.event === 'config_changed') {
      // The picker is the daemon's dialog, so the chosen folder never comes
      // back through this component's own call -- it arrives here, the same
      // way `Settings.svelte` learns about it. Without this the box would
      // keep showing the old path after the user had already picked.
      const resolved = event.data['clip_dir_resolved'];
      if (typeof resolved === 'string') clipDir = resolved;
    }
  });
  onDestroy(() => {
    void unlisten.then((fn) => fn());
  });

  async function pickFolder() {
    try {
      // Answers as soon as the dialog is open, not when it closes; the result
      // arrives as `config_changed` above. The daemon reports a folder it
      // cannot use in its own message box, so there is nothing to toast here
      // beyond the dialog failing to open at all.
      await call('folder.pick');
    } catch (e) {
      app.toast('error', String(e));
    }
  }

  async function finish() {
    try {
      // One call, because config.set is all-or-nothing: a wizard that wrote
      // three keys in three calls could leave a half-configured install behind
      // if the second failed.
      await call('config.set', { monitor_index: monitorIndex, clip_hotkey: hotkey });
    } catch (e) {
      // Setup itself did not happen -- staying on this step, whose only
      // control retries the very call that just failed, is the correct
      // response.
      app.toast('error', String(e));
      return;
    }

    // config.set succeeded, so setup is done: leave the wizard for the grid
    // no matter what happens next. Arming is a separate, retryable action the
    // rail already exposes once the grid is showing, so a failed `arm` here
    // must not strand the user in a wizard whose job is already finished --
    // same "safer default" reasoning as the config.get call site in
    // onDaemonUp.
    try {
      await call('arm');
    } catch (e) {
      app.toast('error', String(e));
    }
    await app.refreshStatus();
    app.view = 'grid';
  }
</script>

<div class="wizard">
  <h1>Set up Trix</h1>
  <p class="step tnum">Step {step} of 3</p>

  {#if step === 1}
    <h2>Which screen do you play on?</h2>
    <Select
      value={String(monitorIndex)}
      options={monitors.map((m) => ({
        value: String(m.index),
        label: `${m.name} - ${m.width}x${m.height} (${m.adapter})`,
      }))}
      label="Monitor"
      onchange={(v) => (monitorIndex = Number(v))} />
    <Button variant="primary" onclick={() => (step = 2)}>Next</Button>
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
    <KeycapInput combo={hotkey} label="Clip hotkey" readonly />
    <p class="hint" class:ok={heard}>{heard ? 'Trix received it.' : 'Waiting for a press...'}</p>
    <Button variant="primary" onclick={() => (step = 3)}>Next</Button>
  {:else}
    <h2>Where should clips go?</h2>
    <!-- Read-only for the same reason the Settings row is: the daemon owns
         the picker, and a typed path that does not exist is a refusal the
         user has to decode. There is no Reset here because the box already
         shows the default -- nothing has been changed away from yet. -->
    <div class="pathrow">
      <span class="path">{clipDir}</span>
      <Button onclick={pickFolder}>Choose...</Button>
    </div>
    <p class="hint">You can change this any time in Settings, or from the Trix tray icon.</p>
    <Button variant="primary" onclick={finish}>Finish and arm</Button>
  {/if}
</div>

<style>
  .wizard { width: min(560px, 100%); margin: 8vh auto; padding: 0 24px; display: grid; gap: 10px; justify-items: start; }
  h1 { font-size: 21px; font-weight: 650; margin: 0; }
  h2 { font-size: 15px; font-weight: 650; margin: 14px 0 4px; }
  .step { color: var(--faint); margin: 0; font-size: 11px; }
  .hint { color: var(--dim); font-size: 11.5px; margin: 2px 0; }
  /* --accent, not --live. This confirms the hotkey reached Trix past any
     overlay, which is not Trix being live -- and the contract keeps --live
     to the arm control, its dot, the buffer meter and the re-arm notice, so
     that green means one thing wherever it appears. */
  .hint.ok { color: var(--accent); }
  .pathrow { display: flex; align-items: center; gap: 8px; width: 100%; }
  .path {
    flex: 1;
    min-width: 0;
    padding: 7px 10px;
    background: var(--bg);
    border: 1px solid var(--line-strong);
    border-radius: var(--r);
    color: var(--dim);
    font-size: 12px;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
</style>
