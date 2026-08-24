<script lang="ts">
  import { clamp, stepBy } from '../../lib/ui';

  let {
    value,
    min,
    max,
    step = 1,
    unit,
    label,
    onchange,
  }: {
    value: number;
    min: number;
    max: number;
    step?: number;
    unit?: string;
    label: string;
    onchange: (v: number) => void;
  } = $props();

  function commit(raw: string) {
    const parsed = Number(raw);
    // An unparseable box is a typo, not an instruction: put the old value
    // back rather than sending the daemon a NaN it will refuse.
    onchange(Number.isFinite(parsed) ? clamp(parsed, min, max) : value);
  }

  function onkeydown(e: KeyboardEvent) {
    const delta = e.key === 'ArrowUp' ? 1 : e.key === 'ArrowDown' ? -1 : 0;
    if (delta !== 0) {
      e.preventDefault();
      onchange(stepBy(value, delta, min, max, step));
    } else if (e.key === 'Home') {
      e.preventDefault();
      onchange(min);
    } else if (e.key === 'End') {
      e.preventDefault();
      onchange(max);
    } else if (e.key === 'Enter') {
      commit((e.currentTarget as HTMLInputElement).value);
    }
  }
</script>

<div class="st">
  <button
    type="button"
    class="pm"
    aria-label="{label}: decrease"
    disabled={value <= min}
    onclick={() => onchange(stepBy(value, -1, min, max, step))}>&minus;</button>
  <input
    class="v tnum"
    type="text"
    inputmode="numeric"
    aria-label={label}
    {value}
    onchange={(e) => commit(e.currentTarget.value)}
    onblur={(e) => commit(e.currentTarget.value)}
    {onkeydown} />
  {#if unit}<span class="u">{unit}</span>{/if}
  <button
    type="button"
    class="pm"
    aria-label="{label}: increase"
    disabled={value >= max}
    onclick={() => onchange(stepBy(value, 1, min, max, step))}>+</button>
</div>

<style>
  .st {
    display: flex;
    align-items: center;
    background: var(--bg);
    border: 1px solid var(--line-strong);
    border-radius: var(--r);
    overflow: hidden;
  }
  .pm {
    width: 28px;
    height: 30px;
    flex: 0 0 auto;
    border: 0;
    background: transparent;
    color: var(--dim);
    font: inherit;
    font-size: 14px;
    cursor: pointer;
  }
  .pm:hover:not(:disabled) { background: var(--hover); color: var(--text); }
  .pm:disabled { opacity: 0.3; cursor: default; background: transparent; }
  .v {
    width: 58px;
    padding: 0;
    border: 0;
    background: transparent;
    color: var(--text);
    font: inherit;
    font-size: 12.5px;
    text-align: center;
  }
  .v:focus-visible { outline: none; }
  .st:focus-within { border-color: var(--accent); box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 18%, transparent); }
  .u { color: var(--faint); font-size: 11px; padding-right: 8px; }
</style>
