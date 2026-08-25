<script lang="ts">
  import type { Field } from '../lib/settings';
  import type { Monitor } from '../lib/types';
  import Button from './ui/Button.svelte';
  import KeycapInput from './ui/KeycapInput.svelte';
  import Select from './ui/Select.svelte';
  import Slider from './ui/Slider.svelte';
  import Stepper from './ui/Stepper.svelte';
  import Toggle from './ui/Toggle.svelte';

  /**
   * One row of the settings page: a label with its help text on the left, and
   * the control for `field.kind` on the right.
   *
   * Owns one piece of state, `dragging`, documented below. `capture`/
   * `listening`/`heard` live in `Settings.svelte` because the daemon's
   * `hotkey_pressed`/`hotkey_rebound` events (wired up there, not here) write
   * to them directly; this component only renders what they say and reports
   * user actions back up through the `on*` callbacks.
   */
  let {
    field, config, monitors, capture, listening, heard,
    onset, oncapture, onstartcapture, onsavehotkey, ontogglelisten,
    onpickfolder, onpicksound, ontestsound,
  }: {
    field: Field;
    config: Record<string, unknown>;
    monitors: Monitor[];
    capture: string | null;
    listening: boolean;
    heard: boolean;
    onset: (key: string, value: unknown) => void;
    oncapture: (e: KeyboardEvent) => void;
    onstartcapture: () => void;
    onsavehotkey: () => void;
    ontogglelisten: () => void;
    onpickfolder: () => void;
    onpicksound: () => void;
    ontestsound: () => void;
  } = $props();

  /**
   * The value under the user's thumb, shown while dragging.
   *
   * Display-only: the daemon is told on `change` (thumb released), not on
   * `input`, so a drag across the track is one round trip rather than eighty.
   * Without it the percentage beside the slider would sit at the old value for
   * the whole drag, which reads as a broken control.
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

  const monitorOptions = $derived(
    monitors.map((m) => ({
      value: String(m.index),
      label: `${m.name} - ${m.width}x${m.height} (${m.adapter})`,
    })),
  );

  /** The unit shown inside the stepper, so it is no longer only in the help text. */
  const UNITS: Record<string, string> = {
    fps: 'fps',
    replay_seconds: 's',
    bitrate_kbps: 'kbps',
    max_bitrate_kbps: 'kbps',
    max_library_gb: 'GB',
    stats_seconds: 's',
  };
</script>

<div class="row">
  <div class="lt">
    <b>{field.label}</b>
    <span>{field.help}</span>
  </div>

  <div class="rt">
    {#if field.kind === 'bool'}
      <Toggle
        checked={Boolean(config[field.key])}
        label={field.label}
        onchange={(next) => onset(field.key, next)} />

    {:else if field.kind === 'select' && field.dynamic === 'monitors'}
      <Select
        value={String(config[field.key] ?? 0)}
        options={monitorOptions}
        label={field.label}
        onchange={(v) => onset(field.key, Number(v))} />

    {:else if field.kind === 'select'}
      <Select
        value={String(config[field.key] ?? '')}
        options={field.options ?? []}
        label={field.label}
        onchange={(v) => onset(field.key, v)} />

    {:else if field.kind === 'number'}
      <Stepper
        value={Number(config[field.key] ?? 0)}
        min={field.min ?? 0}
        max={field.max ?? 0}
        unit={UNITS[field.key]}
        label={field.label}
        onchange={(v) => onset(field.key, v)} />

    {:else if field.kind === 'slider'}
      <Slider
        value={shown}
        min={field.min ?? 0}
        max={field.max ?? 100}
        label={field.label}
        oninput={(v) => (dragging = v)}
        onchange={(v) => onset(field.key, v)} />
      <span class="pct tnum">{shown}%</span>

    {:else if field.kind === 'folder'}
      <!-- `clip_dir_resolved`, not `clip_dir`: the config key is empty by
           default and an empty box tells the user nothing about where their
           clips actually are. Read-only rather than editable because the
           daemon owns the picker, and a typed path that does not exist is a
           refusal the user has to decode. Reset writes `clip_dir` itself (the
           empty string), because "back to the default" is a value the picker
           cannot express. -->
      <span class="path">{String(config['clip_dir_resolved'] ?? '')}</span>
      <Button size="sm" onclick={onpickfolder}>Choose...</Button>
      <Button size="sm" variant="ghost" disabled={!config[field.key]}
        onclick={() => onset(field.key, '')}>Reset</Button>

    {:else if field.kind === 'sound'}
      <span class="path" class:off={!config['clip_sound']}>
        {String(config[field.key] ?? '') || "Trix's built-in sound"}
      </span>
      <Button size="sm" disabled={!config['clip_sound']} onclick={onpicksound}>Choose...</Button>
      <Button size="sm" variant="ghost" disabled={!config['clip_sound']} onclick={ontestsound}>Test</Button>
      <Button size="sm" variant="ghost" disabled={!config['clip_sound'] || !config[field.key]}
        onclick={() => onset(field.key, '')}>Reset</Button>

    {:else if field.kind === 'hotkey'}
      <KeycapInput
        combo={capture ?? String(config[field.key] ?? '')}
        capturing={capture !== null}
        label="Clip hotkey"
        oncapture={oncapture}
        onstart={onstartcapture} />
      {#if capture && capture !== config[field.key]}
        <Button size="sm" variant="primary" onclick={onsavehotkey}>Save</Button>
      {/if}
      <Button size="sm" variant="ghost" onclick={ontogglelisten}>{listening ? 'Stop test' : 'Test'}</Button>
      {#if listening}
        <span class="hint" class:ok={heard}>{heard ? 'Trix received it.' : 'Press the hotkey now...'}</span>
      {/if}
    {/if}
  </div>
</div>

<style>
  /* `.row`, `.lt` and `.rt` are global, in app.css: `Settings.svelte`
     renders the same row shape inline for its Version entry, and a scoped
     copy here would mean maintaining both. Only what is unique to a field's
     control lives here. */
  .pct { min-width: 44px; text-align: right; font-size: 12.5px; color: var(--dim); }
  .path {
    display: block;
    max-width: 280px;
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
  .path.off { opacity: 0.45; }
  .hint { font-size: 11.5px; color: var(--dim); }
  .hint.ok { color: var(--live); }
</style>
