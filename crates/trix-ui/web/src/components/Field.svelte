<script lang="ts">
  import type { Field } from '../lib/settings';
  import type { Monitor } from '../lib/types';

  /**
   * One row of the settings page: a label, the control for `field.kind`, and
   * its help text. Pulled out of `Settings.svelte` because the control chain
   * is a six-branch `{#if}` on `field.kind` (and, for `select`, its
   * `dynamic`) nested inside two `{#each}` blocks -- that reads far better as
   * its own component than inline.
   *
   * Owns no state of its own. `capture`/`listening`/`heard` live in
   * `Settings.svelte` because the daemon's `hotkey_pressed`/`hotkey_rebound`
   * events (wired up there, not here) write to them directly; this component
   * only renders what they say and reports user actions back up through the
   * `on*` callbacks.
   */
  let {
    field,
    config,
    monitors,
    capture,
    listening,
    heard,
    onset,
    oncapture,
    onsavehotkey,
    ontogglelisten,
  }: {
    field: Field;
    config: Record<string, unknown>;
    monitors: Monitor[];
    capture: string | null;
    listening: boolean;
    heard: boolean;
    onset: (key: string, value: unknown) => void;
    oncapture: (e: KeyboardEvent) => void;
    onsavehotkey: () => void;
    ontogglelisten: () => void;
  } = $props();
</script>

<div class="row">
  <label for={field.key}>{field.label}</label>
  <div class="control">
    {#if field.kind === 'bool'}
      <input id={field.key} type="checkbox" checked={Boolean(config[field.key])}
        onchange={(e) => onset(field.key, e.currentTarget.checked)} />
    {:else if field.kind === 'select' && field.dynamic === 'monitors'}
      <select id={field.key} value={String(config[field.key] ?? 0)}
        onchange={(e) => onset(field.key, Number(e.currentTarget.value))}>
        {#each monitors as monitor (monitor.index)}
          <option value={String(monitor.index)}>
            {monitor.name} - {monitor.width}x{monitor.height} ({monitor.adapter})
          </option>
        {/each}
      </select>
    {:else if field.kind === 'select'}
      <select id={field.key} value={String(config[field.key] ?? '')}
        onchange={(e) => onset(field.key, e.currentTarget.value)}>
        {#each field.options ?? [] as option (option.value)}
          <option value={option.value}>{option.label}</option>
        {/each}
      </select>
    {:else if field.kind === 'number'}
      <input id={field.key} type="number" min={field.min} max={field.max}
        value={Number(config[field.key] ?? 0)}
        onchange={(e) => onset(field.key, Number(e.currentTarget.value))} />
    {:else if field.kind === 'folder'}
      <input id={field.key} readonly value={String(config['clip_dir_resolved'] ?? '')} />
      <span class="hint">Change it from the Trix tray icon.</span>
    {:else if field.kind === 'hotkey'}
      <input id={field.key} readonly value={capture ?? String(config[field.key] ?? '')}
        onkeydown={oncapture} placeholder="click, then press a combination" />
      {#if capture && capture !== config[field.key]}
        <button onclick={onsavehotkey}>Save</button>
      {/if}
      <button onclick={ontogglelisten}>{listening ? 'Stop test' : 'Test'}</button>
      {#if listening}
        <span class="hint" class:ok={heard}>
          {heard ? 'Trix received it.' : 'Press the hotkey now...'}
        </span>
      {/if}
    {/if}
  </div>
  <p class="help">{field.help}</p>
</div>

<style>
  .row { display: grid; grid-template-columns: 180px 1fr; gap: 6px 14px; align-items: center; padding: 8px 0; border-bottom: 1px solid var(--line); }
  .control { display: flex; align-items: center; gap: 8px; }
  .help { grid-column: 2; margin: 0; font-size: 12px; color: var(--dim); }
  .hint { font-size: 12px; color: var(--dim); }
  .hint.ok { color: var(--accent); }
  input, select { padding: 6px 10px; border-radius: 6px; border: 1px solid var(--line); background: var(--bg); color: var(--text); font: inherit; min-width: 120px; }
  input[readonly] { color: var(--dim); }
  button { padding: 5px 12px; border-radius: 6px; border: 1px solid var(--line); background: var(--panel); color: var(--text); font: inherit; cursor: pointer; }
</style>
