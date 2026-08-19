<script lang="ts">
  import type { Field } from '../lib/settings';
  import type { Monitor } from '../lib/types';

  /**
   * One row of the settings page: a label, the control for `field.kind`, and
   * its help text. Pulled out of `Settings.svelte` because the control chain
   * is a seven-branch `{#if}` on `field.kind` (and, for `select`, its
   * `dynamic`) nested inside two `{#each}` blocks -- that reads far better as
   * its own component than inline.
   *
   * Owns one piece of state, `dragging`, documented below. `capture`/
   * `listening`/`heard` live in `Settings.svelte` because the daemon's
   * `hotkey_pressed`/`hotkey_rebound` events (wired up there, not here) write
   * to them directly; this component only renders what they say and reports
   * user actions back up through the `on*` callbacks.
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
    onpickfolder,
    onpicksound,
    ontestsound,
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
    onpickfolder: () => void;
    onpicksound: () => void;
    ontestsound: () => void;
  } = $props();

  /**
   * The value under the user's thumb, shown while dragging.
   *
   * The one piece of state this component owns, and it is display-only: the
   * daemon is told on `change` (thumb released), not on `input`, so a drag
   * across the track is one round trip rather than eighty. Without it the
   * percentage beside the slider would sit at the old value for the whole
   * drag, which reads as a broken control.
   */
  let dragging = $state<number | null>(null);
  const shown = $derived(dragging ?? Number(config[field.key] ?? 0));

  // Clear the drag override whenever the authoritative value lands -- whether
  // that is the daemon accepting the change or the parent reloading the config
  // after refusing it. Without this a refused change would leave the slider
  // showing a value the daemon rejected.
  $effect(() => {
    void config[field.key];
    dragging = null;
  });
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
    {:else if field.kind === 'slider'}
      <input id={field.key} type="range" min={field.min} max={field.max} step="1"
        value={shown}
        oninput={(e) => (dragging = Number(e.currentTarget.value))}
        onchange={(e) => onset(field.key, Number(e.currentTarget.value))} />
      <span class="hint">{shown}%</span>
    {:else if field.kind === 'folder'}
      <!-- `clip_dir_resolved`, not `clip_dir`: the config key is empty by
           default and an empty box tells the user nothing about where their
           clips actually are. Readonly rather than editable for the same
           reason the sound row is -- the daemon owns the picker, and a typed
           path that does not exist is a refusal the user has to decode. Reset
           writes `clip_dir` itself (the empty string), because "back to the
           default" is a value the picker cannot express. -->
      <input id={field.key} readonly value={String(config['clip_dir_resolved'] ?? '')} />
      <button onclick={onpickfolder}>Choose...</button>
      <button disabled={!config[field.key]} onclick={() => onset(field.key, '')}>Reset</button>
    {:else if field.kind === 'sound'}
      <input id={field.key} readonly disabled={!config['clip_sound']}
        value={String(config[field.key] ?? '') || "Trix's built-in sound"} />
      <button disabled={!config['clip_sound']} onclick={onpicksound}>Choose...</button>
      <button disabled={!config['clip_sound']} onclick={ontestsound}>Test</button>
      <button disabled={!config['clip_sound'] || !config[field.key]}
        onclick={() => onset(field.key, '')}>Reset</button>
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
</style>
