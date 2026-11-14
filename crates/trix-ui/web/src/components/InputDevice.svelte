<script lang="ts">
  import { onMount } from 'svelte';
  import { call } from '../lib/ipc';
  import { inputOptions, inputMeterView, pollInputLevel } from '../lib/audio-input';
  import type { InputDevice, InputLevel } from '../lib/types';
  import Select from './ui/Select.svelte';
  import Button from './ui/Button.svelte';

  let { value, volume, armed, connected, pending, onchange, onrearmneeded }: {
    value: string;
    volume: number;
    armed: boolean;
    connected: boolean;
    pending: boolean;
    onchange: (value: string) => Promise<void>;
    onrearmneeded: (keys: string[]) => void;
  } = $props();

  let devices = $state<InputDevice[]>([]);
  let loading = $state(true);
  let saving = $state(false);
  let listError = $state<string | null>(null);
  let levelError = $state<string | null>(null);
  let level = $state<InputLevel | null>(null);
  let visible = $state(document.visibilityState !== 'hidden');
  let retry = $state(0);
  let disposed = false;

  const options = $derived(inputOptions(devices, value));
  const meter = $derived(inputMeterView({ connected, armed, volume, pending, error: levelError, level }));

  async function loadDevices() {
    loading = true;
    listError = null;
    try {
      const result = await call<{ inputs: InputDevice[] }>('audio.inputs');
      if (!disposed) devices = result.inputs;
    } catch (e) {
      if (!disposed) listError = String(e);
    } finally {
      if (!disposed) loading = false;
    }
  }

  function refresh() {
    retry += 1;
    void loadDevices();
  }

  async function choose(id: string) {
    saving = true;
    try { await onchange(id); }
    finally { if (!disposed) saving = false; }
  }

  onMount(() => {
    void loadDevices();
    const visibilityChanged = () => { visible = document.visibilityState !== 'hidden'; };
    document.addEventListener('visibilitychange', visibilityChanged);
    return () => {
      disposed = true;
      document.removeEventListener('visibilitychange', visibilityChanged);
    };
  });

  $effect(() => {
    // Restart on a changed choice or explicit refresh, and discard old replies.
    void value;
    void volume;
    void retry;
    level = null;
    levelError = null;
    if (!visible || !connected || !armed || saving) return;
    return pollInputLevel(
      () => call<InputLevel>('audio.input_level'),
      next => {
        level = next;
        onrearmneeded(next.requires_rearm ?? []);
      },
      error => { levelError = error; },
    );
  });
</script>

<div class="row input-row">
  <div class="lt">
    <b>Input device</b>
    <span>Microphone used in your clips. Changes apply after re-arming.</span>
    {#if listError}<span class="problem">Could not list microphones. {listError}</span>{/if}
  </div>
  <div class="input-controls">
    <div class="chooser">
      <fieldset disabled={loading || saving || !connected} aria-label="Input device selection">
        <Select value={value} {options} label="Input device" onchange={choose} />
      </fieldset>
      <Button size="sm" disabled={loading || saving || !connected} onclick={refresh}>
        {loading ? 'Loading...' : 'Refresh'}
      </Button>
    </div>
    <div class="meter-caption"><span>Captured input level</span><span class="tnum">{meter.reading}</span></div>
    <div class="meter" class:clipping={meter.clipping} role="meter" aria-label="Captured microphone level"
      aria-valuemin={0} aria-valuemax={100} aria-valuenow={meter.percent}
      aria-valuetext={meter.reading || meter.message}>
      <div class="fill" style:transform="scaleX({meter.percent / 100})"></div>
    </div>
    <span class="meter-message" class:problem={meter.clipping}>{meter.message}</span>
  </div>
</div>

<style>
  .input-row { align-items: flex-start; }
  .input-controls { width: min(360px, 52%); min-width: 0; }
  .chooser { display: flex; gap: 8px; align-items: center; }
  fieldset { border: 0; margin: 0; padding: 0; flex: 1; min-width: 0; }
  fieldset:disabled { opacity: 0.55; }
  .meter-caption { display: flex; justify-content: space-between; gap: 8px; margin: 11px 0 5px; font-size: 11px; color: var(--dim); }
  .meter { height: 6px; border-radius: var(--r-full); background: var(--raised); overflow: hidden; }
  .fill { height: 100%; width: 100%; transform-origin: left; background: var(--live); transition: transform 100ms linear; }
  .clipping .fill { background: var(--danger); }
  .meter-message { display: block; margin-top: 5px; font-size: 11px; color: var(--dim); }
  .problem { color: var(--danger) !important; }
  @media (prefers-reduced-motion: reduce) { .fill { transition: none; } }
</style>
