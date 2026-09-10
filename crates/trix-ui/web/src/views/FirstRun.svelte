<script lang="ts">
  import { onDestroy } from 'svelte';
  import { call, onDaemonEvent } from '../lib/ipc';
  import { app } from '../lib/state.svelte';
  import { clipHotkeyProblem, comboFromEvent } from '../lib/settings';
  import type { Monitor } from '../lib/types';
  import Button from '../components/ui/Button.svelte';
  import Icon from '../components/ui/Icon.svelte';
  import KeycapInput from '../components/ui/KeycapInput.svelte';
  import Select from '../components/ui/Select.svelte';

  let step = $state(1);
  let monitors = $state<Monitor[]>([]);
  let monitorIndex = $state(0);
  /**
   * The whole config record, not just the hotkey string it used to be.
   *
   * `clipHotkeyProblem` reads `hotkey_mode` as well as `clip_hotkey` to decide
   * which of the two opposite failures to explain, and the row now saves
   * through `config.set` the way a settings row does -- so this page keeps
   * what the daemon last told it, as `Settings.svelte` does.
   */
  let config = $state<Record<string, unknown>>({});
  /** The combination being recorded, or null while the row shows the saved one. */
  let capture = $state<string | null>(null);
  let heard = $state(false);
  let clipDir = $state('');

  const hotkey = $derived(String(config['clip_hotkey'] ?? 'alt+f10'));

  /**
   * Why the clip hotkey is dead, or null while it works or nobody knows yet.
   *
   * This is what the step was missing. The daemon has known since startup that
   * `RegisterHotKey` was refused -- it puts the answer on `status` as
   * `clip_hotkey_bound`, and Settings has rendered this exact sentence for a
   * release already. This page read only `config.get`, so a user whose hotkey
   * was taken (NVIDIA's overlay owns Alt+F10 on a great many machines, and
   * Alt+F10 is the default) sat on "Waiting for a press..." forever, on the
   * one screen that asks them to press it, with no way to change it.
   */
  const problem = $derived(clipHotkeyProblem(app.status?.clip_hotkey_bound, config));

  $effect(() => {
    void (async () => {
      try {
        const list = await call<{ monitors: Monitor[] }>('monitors.list');
        monitors = list.monitors;
        config = await call<Record<string, unknown>>('config.get');
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
    if (event.event === 'hotkey_rebound') {
      // Raised by this page's own `config.set`. Whether the new combination
      // actually bound reaches a client only through `status`, so re-read it:
      // that is what clears the warning once the user picks a key Windows will
      // give them, and what raises it again if their second choice is taken
      // too. Same reasoning, and the same call, as `Settings.svelte`'s.
      //
      // No toast beside it, which is where this page parts from Settings: the
      // strip below is already the answer to the question this step asks, and
      // in view. A toast would say the same sentence twice and then take one
      // of them away on a timer.
      void app.refreshStatus();
    }
    if (event.event === 'config_changed') {
      // The picker is the daemon's dialog, so the chosen folder never comes
      // back through this component's own call -- it arrives here, the same
      // way `Settings.svelte` learns about it. Without this the box would
      // keep showing the old path after the user had already picked.
      //
      // Merged whole rather than reading one key out, now that `config` backs
      // the hotkey row as well: the tray can change settings behind this page.
      config = { ...config, ...event.data };
      const resolved = event.data['clip_dir_resolved'];
      if (typeof resolved === 'string') clipDir = resolved;
    }
  });
  onDestroy(() => {
    void unlisten.then((fn) => fn());
  });

  function startCapture() {
    capture = hotkey;
  }

  function captureHotkey(e: KeyboardEvent) {
    // Unconditional, before the combination is worked out: a bare Alt press
    // still reaches this handler, and letting that one through would move
    // focus to the window menu mid-recording.
    e.preventDefault();
    const combo = comboFromEvent(e);
    if (combo !== null) capture = combo;
  }

  async function saveHotkey() {
    if (capture === null) return;
    try {
      // Saved here rather than carried to `finish`, because the only way to
      // find out whether Windows will hand Trix a combination is to have the
      // daemon try to register one -- and `config.set` is what makes it try.
      // A hotkey chosen now and written only at the end would leave step 2 a
      // press-it-now check that cannot check the thing it was just given.
      //
      // The cost is deliberate. First run *is* the absence of config.toml
      // (spec 7.4), so writing one here ends it: quit the wizard after
      // changing the hotkey and the next launch opens on the grid, with the
      // monitor and the clip folder left on their defaults. Both defaults work
      // and are one visit to Settings away; a hotkey nobody can change is what
      // leaves an install unusable.
      const result = await call<{ accepted: Record<string, unknown> }>('config.set', {
        clip_hotkey: capture,
      });
      // Read back from `accepted`, not from `capture`: the daemon takes it out
      // of the saved config rather than echoing the request, so an accepted
      // value that differs from what was pressed is what the row should show.
      config = { ...config, ...result.accepted };
      capture = null;
      // The check is about a combination, not about this page. A press of the
      // key they just replaced proves nothing about the one they chose.
      heard = false;
    } catch (e) {
      app.toast('error', String(e));
    }
  }

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
      //
      // `clip_hotkey` still goes with it even though step 2 may already have
      // saved it -- the common path is a user who never touched the row, and
      // for them this is the write that puts it in the file.
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
    <p class="hint">Press it now. Some overlays quietly take a hotkey inside games, so this checks Trix really gets it. Click the keys to pick a different combination.</p>
    <div class="hkrow">
      <KeycapInput
        combo={capture ?? hotkey}
        capturing={capture !== null}
        label="Clip hotkey"
        oncapture={captureHotkey}
        onstart={startCapture} />
      {#if capture !== null && capture !== hotkey}
        <Button size="sm" variant="primary" onclick={saveHotkey}>Save</Button>
      {/if}
    </div>
    <!--
      A status strip rather than the 11.5px grey hint this used to be. It is
      the entire answer to the question the step asks, and on the machines
      where it matters it is bad news -- so it is sized and coloured like a
      result instead of like a caption.

      `aria-live`, because all three states arrive after the page has painted:
      the press, the rebind, and the warning that was already true at load.
    -->
    <p class="status" class:ok={heard && problem === null} class:bad={problem !== null} aria-live="polite">
      {#if problem !== null}
        <Icon name="alert" size={15} />
      {:else if heard}
        <Icon name="check" size={15} />
      {:else}
        <span class="dot"></span>
      {/if}
      <span>{problem ?? (heard ? 'Trix received it.' : 'Waiting for a press...')}</span>
    </p>
    <Button variant="primary" onclick={() => (step = 3)}>Next</Button>
  {:else}
    <h2>Where should clips go?</h2>
    <!-- Read-only because the daemon owns the picker, and a typed path that
         does not exist is a refusal the user has to decode. There is no Reset
         here because the box already shows the default -- nothing has been
         changed away from yet. -->
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
  .hkrow { display: flex; align-items: center; gap: 8px; }
  .status {
    display: flex;
    align-items: center;
    gap: 9px;
    width: 100%;
    margin: 2px 0;
    padding: 10px 13px;
    border-radius: var(--r-md);
    border: 1px solid var(--line);
    background: var(--surface);
    color: var(--dim);
    font-size: 12.5px;
    line-height: 1.45;
  }
  /* --accent, not --live. This confirms the hotkey reached Trix past any
     overlay, which is not Trix being live -- and the contract keeps --live
     to the arm control, its dot, the buffer meter and the re-arm notice, so
     that green means one thing wherever it appears. */
  .status.ok {
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 10%, var(--bg));
    border-color: color-mix(in srgb, var(--accent) 40%, transparent);
  }
  .status.bad {
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 10%, var(--bg));
    border-color: color-mix(in srgb, var(--danger) 40%, transparent);
  }
  /* The icon lives in a child component, so a scoped selector would not reach
     it. Colour comes from `currentColor` and needs no rule; only the flex
     behaviour does, or a long warning squashes it. */
  .status :global(svg) { flex: none; }
  /* Something that moves, because waiting is the state the user is meant to
     act on and static grey text is what reads as "nothing happens here".
     `app.css` flattens this under prefers-reduced-motion. */
  .dot {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: var(--r-full);
    background: var(--faint);
    animation: listening 1.4s ease-in-out infinite;
  }
  @keyframes listening {
    50% { background: var(--accent); transform: scale(1.3); }
  }
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
