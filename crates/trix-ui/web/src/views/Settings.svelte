<script lang="ts">
  import { onDestroy } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { call, onDaemonEvent } from '../lib/ipc';
  import { app, updates } from '../lib/state.svelte';
  import { FIELDS, SECTIONS, unknownKeys, validate } from '../lib/settings';
  import Field from '../components/Field.svelte';
  import type { Monitor } from '../lib/types';

  let config = $state<Record<string, unknown>>({});
  let monitors = $state<Monitor[]>([]);
  let extras = $state<string[]>([]);

  // Hotkey live test (spec §6.4).
  let listening = $state(false);
  let heard = $state(false);
  let capture = $state<string | null>(null);

  $effect(() => {
    void load();
  });

  // Unlike `wireDaemon()` in state.svelte.ts (which lives for the whole app
  // and is meant to), this handler belongs to a page the user opens and
  // leaves through the rail. Left registered, every visit would stack another
  // handler that outlives its component, still writing to a dead instance's
  // `listening`/`heard` -- so the returned unlisten function is captured and
  // run when this component is destroyed.
  const unlisten = onDaemonEvent((event) => {
    if (event.event === 'hotkey_pressed' && listening) heard = true;
    if (event.event === 'hotkey_rebound' && event.data['registered'] === false) {
      app.toast('error', `Windows would not give Trix ${event.data['spec']}. Another app already owns it.`);
    }
    if (event.event === 'config_changed') {
      // The sound dialog and the tray change config behind this page's back;
      // without this the path box would keep showing the old file until the
      // page was reopened.
      config = { ...config, ...event.data };
    }
  });
  onDestroy(() => {
    void unlisten.then((fn) => fn());
  });

  async function load() {
    try {
      config = await call<Record<string, unknown>>('config.get');
      extras = unknownKeys(config);
      const list = await call<{ monitors: Monitor[] }>('monitors.list');
      monitors = list.monitors;
    } catch (e) {
      app.toast('error', String(e));
    }
  }

  async function set(key: string, value: unknown) {
    const complaint = validate(key, value);
    if (complaint) {
      app.toast('error', complaint);
      return;
    }
    try {
      const result = await call<{ accepted: Record<string, unknown>; requires_rearm: string[] }>(
        'config.set',
        { [key]: value },
      );
      // Read back from `accepted`, which the daemon takes out of the saved
      // config rather than echoing the request: an accepted value that differs
      // from what was typed is exactly what the field should now show.
      config = { ...config, ...result.accepted };
      if (result.requires_rearm.length > 0 && app.armed) {
        app.addRearmNeeded(result.requires_rearm);
      }
    } catch (e) {
      app.toast('error', String(e));
      // The daemon refused, so nothing was written and nothing was applied:
      // put the field back to the truth rather than leaving the typed value on
      // screen looking saved.
      await load();
    }
  }

  async function pickSound() {
    try {
      // Answers as soon as the dialog is open, not when it closes. The chosen
      // file arrives as a `config_changed` event, handled below.
      await call('sound.pick');
    } catch (e) {
      app.toast('error', String(e));
    }
  }

  async function testSound() {
    try {
      await call('sound.test');
    } catch (e) {
      app.toast('error', String(e));
    }
  }

  /** The hotkey field's own Save button: commit the captured combo, then clear it so the row falls back to showing the saved value. */
  function saveHotkey() {
    if (capture === null) return;
    void set('clip_hotkey', capture);
    capture = null;
  }

  function captureHotkey(e: KeyboardEvent) {
    e.preventDefault();
    const parts: string[] = [];
    if (e.ctrlKey) parts.push('ctrl');
    if (e.altKey) parts.push('alt');
    if (e.shiftKey) parts.push('shift');
    if (e.metaKey) parts.push('win');
    const key = e.key.toLowerCase();
    if (['control', 'alt', 'shift', 'meta'].includes(key)) return;
    parts.push(key);
    capture = parts.join('+');
  }

  let version = $state('');
  let checking = $state(false);
  let checked = $state<string | null>(null);

  invoke<string>('update_current_version').then((v) => (version = v));

  /**
   * Unlike the check on launch, this one reports either way -- the user asked,
   * so silence would read as a broken button.
   *
   * Renders from `update_check`'s return value, never from the `trix-update`
   * channel: that channel stays deliberately silent while an install is in
   * flight (see `update/mod.rs`'s `emit_check`), so a channel-driven button
   * would go dead with no feedback the moment it mattered most.
   */
  async function checkNow() {
    checking = true;
    checked = null;
    try {
      const found = await invoke<{ version: string } | null>('update_check');
      checked = found ? `Trix ${found.version} is available.` : 'Trix is up to date.';
    } catch (e) {
      // Rust already writes a complete, correctly-attributed sentence here
      // (`reported()`'s job) -- a network failure, a 403 from being
      // rate-limited, or a malformed feed each get their own wording, and
      // none of them start with "could not reach GitHub" except the one that
      // actually is that. Render it unprefixed, the same way App.svelte
      // renders `banner.error`; prefixing it here used to double up the
      // network case ("Could not reach GitHub: could not reach GitHub: ...")
      // and mislabel the other two as network failures they were not.
      checked = String(e);
    } finally {
      checking = false;
    }
  }
</script>

<h1>Settings</h1>

{#if app.rearmNeeded.length > 0}
  <div class="notice">
    Re-arm to apply: {app.rearmNeeded.join(', ')}
    <button onclick={() => app.rearmNow()}>Re-arm now</button>
  </div>
{/if}

{#each SECTIONS as section (section)}
  <section>
    <h2>{section}</h2>
    {#each FIELDS.filter((f) => f.section === section) as field (field.key)}
      <Field
        {field}
        {config}
        {monitors}
        {capture}
        {listening}
        {heard}
        onset={set}
        oncapture={captureHotkey}
        onsavehotkey={saveHotkey}
        onpicksound={pickSound}
        ontestsound={testSound}
        ontogglelisten={() => {
          listening = !listening;
          heard = false;
        }}
      />
    {/each}
    {#if section === 'Updates'}
      <div class="row">
        <label for="current-version">Version</label>
        <div class="control">
          <input id="current-version" readonly value={version} />
          <button onclick={checkNow} disabled={checking || updates.busy}>
            {checking ? 'Checking…' : 'Check now'}
          </button>
        </div>
        {#if checked}<p class="help">{checked}</p>{/if}
      </div>
    {/if}
  </section>
{/each}

{#if extras.length > 0}
  <p class="help">
    This daemon has settings this app does not render yet: {extras.join(', ')}. Edit them in config.toml.
  </p>
{/if}

<style>
  h1 { font-size: 18px; margin: 0 0 18px; }
  h2 { font-size: 12px; text-transform: uppercase; letter-spacing: 0.08em; color: var(--dim); margin: 22px 0 10px; }
  .row { display: grid; grid-template-columns: 180px 1fr; gap: 6px 14px; align-items: center; padding: 8px 0; border-bottom: 1px solid var(--line); }
  .control { display: flex; align-items: center; gap: 8px; }
  .help { grid-column: 2; margin: 0; font-size: 12px; color: var(--dim); }
  .notice { display: flex; align-items: center; gap: 12px; padding: 10px 14px; border: 1px solid var(--accent); border-radius: 8px; margin-bottom: 16px; }
  .notice button { margin-left: auto; padding: 5px 12px; border-radius: 6px; border: 1px solid var(--line); background: var(--panel); color: var(--text); font: inherit; cursor: pointer; }
</style>
