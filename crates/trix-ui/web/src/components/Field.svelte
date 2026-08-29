<script lang="ts">
  import { onDaemonEvent } from '../lib/ipc';
  import { hotkeySaveTarget, hotkeySeed, type Field } from '../lib/settings';
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
   * Owns two kinds of per-instance, ephemeral UI state: `dragging` (below),
   * for sliders, and -- for a `hotkey` field -- its own `capture`/`listening`/
   * `heard` triple (spec §6.4). The hotkey trio used to live one level up in
   * `Settings.svelte`, back when there was exactly one hotkey row; a second
   * row then had no choice but to share it, so clicking into either row's
   * keycaps armed both of them and showed each other's combination. Scoping
   * it to the `Field` instance that owns each row -- the same place
   * `dragging` already lives, and the same thing keying `{#each FIELDS...}`
   * by `field.key` in `Settings.svelte` already does for the DOM -- keeps
   * every hotkey row independent instead.
   */
  let {
    field, config, monitors,
    onset, onpickfolder, onpicksound, ontestsound,
  }: {
    field: Field;
    config: Record<string, unknown>;
    monitors: Monitor[];
    onset: (key: string, value: unknown) => void;
    onpickfolder: () => void;
    onpicksound: () => void;
    ontestsound: () => void;
  } = $props();

  // Hotkey live test (spec §6.4). Declared unconditionally, the same way
  // `dragging` below is declared for every field even though only a slider
  // reads it -- only the `hotkey` branch of the template ever touches these.
  let listening = $state(false);
  let heard = $state(false);
  let capture = $state<string | null>(null);

  /**
   * Whether this row's press reaches the daemon as `hotkey_pressed` -- see
   * `Field`'s `liveTest` doc comment in `settings.ts` for which row that is
   * and why. Read off the descriptor rather than a `field.key === 'clip_hotkey'`
   * literal here, so the affordance is tied to the daemon arm that actually
   * broadcasts rather than to a string a third hotkey row could silently miss.
   */
  const testable = $derived(field.kind === 'hotkey' && field.liveTest === true);

  // Only the testable row subscribes: `heard` can only ever flip on a row
  // whose press is broadcast at all, and registering a listener nobody can
  // drive would just be a teardown to get right for no behaviour.
  //
  // An `$effect` rather than a plain top-level subscription: `testable`
  // derives from the `field` prop, and reading a prop-derived value into a
  // one-shot `const` outside a reactive block only captures its initial
  // value (Svelte flags exactly this as `state_referenced_locally`). The
  // effect's own returned callback is the cleanup, so there is no separate
  // `onDestroy` to keep in sync with it.
  $effect(() => {
    if (!testable) return;
    const unlistenPromise = onDaemonEvent((event) => {
      if (event.event === 'hotkey_pressed' && listening) heard = true;
    });
    return () => {
      void unlistenPromise.then((fn) => fn());
    };
  });

  function startCapture() {
    capture = hotkeySeed(field, config);
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

  /** This row's Save button: commit the captured combo, then clear it so the row falls back to showing the saved value. */
  function saveHotkey() {
    if (capture === null) return;
    const { key, value } = hotkeySaveTarget(field, capture);
    onset(key, value);
    capture = null;
  }

  function toggleListen() {
    listening = !listening;
    heard = false;
  }

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
        combo={capture ?? hotkeySeed(field, config)}
        capturing={capture !== null}
        label={field.label}
        oncapture={captureHotkey}
        onstart={startCapture} />
      {#if capture && capture !== config[field.key]}
        <Button size="sm" variant="primary" onclick={saveHotkey}>Save</Button>
      {/if}
      {#if testable}
        <Button size="sm" variant="ghost" onclick={toggleListen}>{listening ? 'Stop test' : 'Test'}</Button>
        {#if listening}
          <span class="hint" class:ok={heard}>{heard ? 'Trix received it.' : 'Press the hotkey now...'}</span>
        {/if}
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
  /* --accent, not --live. Confirming the hotkey reached Trix past an overlay
     is not Trix being live; the contract keeps --live to the arm control, its
     dot, the buffer meter and the re-arm notice. FirstRun.svelte carries the
     same line for the same sentence. */
  .hint.ok { color: var(--accent); }
</style>
