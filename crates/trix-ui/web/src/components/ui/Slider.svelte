<script lang="ts">
  import { clamp, ratioToValue, stepBy, valueToRatio } from '../../lib/ui';

  let {
    value,
    min,
    max,
    step = 1,
    label,
    disabled = false,
    oninput,
    onchange,
  }: {
    value: number;
    min: number;
    max: number;
    step?: number;
    label: string;
    disabled?: boolean;
    /** Fires continuously during a drag. Display only -- do not call the daemon here. */
    oninput?: (v: number) => void;
    /** Fires once, when the change is finished. This is the one to act on. */
    onchange: (v: number) => void;
  } = $props();

  let track = $state<HTMLDivElement | null>(null);
  let dragging = $state(false);

  const ratio = $derived(valueToRatio(value, min, max));
  const pct = $derived(`${ratio * 100}%`);

  function valueAt(clientX: number): number {
    if (!track) return value;
    const box = track.getBoundingClientRect();
    return ratioToValue((clientX - box.left) / box.width, min, max, step);
  }

  function onpointerdown(e: PointerEvent) {
    if (disabled) return;
    // Capture on the track, so a drag that leaves the element -- which is
    // most of them, the hit area is 18px tall and the line the eye follows is
    // the 4px `.track` inside it -- keeps delivering moves here instead of to
    // whatever is underneath the pointer.
    e.preventDefault();
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    dragging = true;
    oninput?.(valueAt(e.clientX));
  }

  function onpointermove(e: PointerEvent) {
    if (!dragging) return;
    oninput?.(valueAt(e.clientX));
  }

  function onpointerup(e: PointerEvent) {
    if (!dragging) return;
    dragging = false;
    (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    onchange(valueAt(e.clientX));
  }

  function onkeydown(e: KeyboardEvent) {
    if (disabled) return;
    const delta =
      e.key === 'ArrowRight' || e.key === 'ArrowUp' ? 1
      : e.key === 'ArrowLeft' || e.key === 'ArrowDown' ? -1
      : e.key === 'PageUp' ? 10
      : e.key === 'PageDown' ? -10
      : 0;
    let next: number;
    if (delta !== 0) next = stepBy(value, delta, min, max, step);
    else if (e.key === 'Home') next = min;
    else if (e.key === 'End') next = max;
    else return;
    e.preventDefault();
    // A key press is a whole change on its own -- there is no release to
    // wait for -- so both events fire, in the order a drag would produce.
    oninput?.(next);
    onchange(next);
  }
</script>

<div
  bind:this={track}
  class="sl"
  class:disabled
  role="slider"
  tabindex={disabled ? -1 : 0}
  aria-label={label}
  aria-valuemin={min}
  aria-valuemax={max}
  aria-valuenow={clamp(value, min, max)}
  aria-disabled={disabled}
  {onpointerdown}
  {onpointermove}
  {onpointerup}
  {onkeydown}>
  <span class="track"></span>
  <span class="fill" style="width: {pct}"></span>
  <span class="thumb" style="left: {pct}"></span>
</div>

<style>
  .sl {
    position: relative;
    width: 186px;
    height: 18px;
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    cursor: pointer;
    touch-action: none;
  }
  .sl.disabled { opacity: 0.4; cursor: default; }
  .track {
    position: absolute;
    left: 0;
    right: 0;
    height: 4px;
    border-radius: 2px;
    background: var(--line-strong);
  }
  .fill { position: absolute; left: 0; height: 4px; border-radius: 2px; background: var(--accent); }
  .thumb {
    position: absolute;
    width: 15px;
    height: 15px;
    border-radius: 50%;
    background: var(--accent);
    border: 2px solid var(--text);
    transform: translateX(-7.5px);
    transition: box-shadow var(--t-fast) var(--ease);
  }
  /* `:not(.disabled)` rather than `:not(:disabled)` -- this is a div with
     role="slider", not a real form control, so `:disabled` never matches it
     either way. The class is the only thing standing in for that state, and
     without this guard a disabled-but-hovered slider still grows a focus-like
     ring on its thumb, which reads as interactive when it is not. */
  .sl:not(.disabled):hover .thumb { box-shadow: 0 0 0 4px color-mix(in srgb, var(--accent) 20%, transparent); }
</style>
