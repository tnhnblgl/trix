<script lang="ts">
  import { resolveStepperInput, stepBy } from '../../lib/ui';

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

  function commit(el: HTMLInputElement) {
    const resolved = resolveStepperInput(el.value, value, min, max);
    // The box takes `value` as a one-way attribute, so if the resolved value
    // matches what's already in effect, Svelte has nothing to re-render and
    // the box would otherwise keep showing whatever the user typed -- most
    // visibly a rejected (unparseable) edit that fell back to the old value.
    // Writing it back onto the element directly makes the box always show
    // what actually took effect.
    el.value = String(resolved);
    // An unresolved or unchanged edit doesn't reach the daemon at all.
    if (resolved !== value) onchange(resolved);
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
      commit(e.currentTarget as HTMLInputElement);
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
    onchange={(e) => commit(e.currentTarget)}
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
  /* `:has(:focus-visible)`, not `:focus-within`: the latter is `:focus`-based,
     so pressing + or - left the whole box ringed until focus went somewhere
     else -- the exact thing app.css's focus rule exists to avoid ("so clicking
     a control does not leave a ring"). The text box still rings on click,
     because a text field always matches `:focus-visible` when focused. */
  .st:has(:focus-visible) { border-color: var(--accent); box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 18%, transparent); }
  .u { color: var(--faint); font-size: 11px; padding-right: 8px; }
</style>
