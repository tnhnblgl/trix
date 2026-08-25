<script lang="ts">
  import { formatClock, keyframeStep, msFromRatio, ratioFromMs, seekKeyTarget, snapStart } from '../lib/timeline';
  import { clamp } from '../lib/ui';

  /**
   * The unified seek-and-trim band -- one timeline where there used to be
   * two, Chromium's scrubber above the trim track.
   *
   * Presentational on purpose: it never calls the daemon. It is given a
   * duration, a playhead, a keyframe list and the current in/out, and reports
   * back where the user dragged. Every daemon call stays in state.svelte.ts.
   *
   * A band with tick marks rather than the filmstrip spec §6.2 sketches:
   * thumbnails along the range would need a video decode path, and the daemon
   * has none -- it writes clips, it does not read frames back out of them.
   * The ticks carry the part that actually matters, which is where a fast-mode
   * cut can land.
   */
  let {
    durationMs, playheadMs, keyframes, inMs, outMs, trimmable = true, onchange, onseek,
  }: {
    durationMs: number;
    playheadMs: number;
    keyframes: number[];
    inMs: number;
    outMs: number;
    /**
     * False leaves a seek-only band: the track, the playhead and both ways of
     * moving it stay, and everything that expresses a range -- both handles,
     * the fill between them and the dimming outside them -- is not rendered.
     *
     * For the clip the daemon will not trim. `library::scan` adopts a bare
     * .mp4 with `duration_ms: 0`, and `clamp_range` refuses it; drawing
     * handles that cannot produce an export would be an offer the daemon does
     * not honour. Seeking has nothing to do with the daemon, so it stays.
     *
     * `onchange` cannot fire while this is false. Two of its three callers --
     * a key press on a handle, and the handle's own grab -- are bound to
     * elements that are not rendered. The third is `move`, which is bound to
     * the band and so stays mounted; it drops a trim drag itself. `drag` is
     * component state rather than the handle's, so without that it would
     * survive the handles unmounting under it.
     */
    trimmable?: boolean;
    onchange: (inMs: number, outMs: number) => void;
    onseek: (ms: number) => void;
  } = $props();

  let band = $state<HTMLDivElement | null>(null);
  /** Which handle a pointer is currently dragging, or `seek` for the track. */
  let drag = $state<'in' | 'out' | 'seek' | null>(null);

  const pct = (ms: number) => `${ratioFromMs(ms, durationMs) * 100}%`;

  function msAt(clientX: number): number {
    if (!band) return 0;
    const box = band.getBoundingClientRect();
    return msFromRatio((clientX - box.left) / box.width, durationMs);
  }

  function grab(which: 'in' | 'out' | 'seek', e: PointerEvent) {
    e.preventDefault();
    e.stopPropagation();
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    drag = which;
    move(e);
  }

  function move(e: PointerEvent) {
    if (!drag) return;
    // A trim drag cannot outlive `trimmable`. Holding a handle and pressing an
    // arrow steps to the next clip -- `grab` preventDefault()s, so focus never
    // reaches the handle and the arrow belongs to the page -- and if that clip
    // is one the daemon will not trim, the handles unmount mid-drag while
    // `drag` still says `in`. Seeking is unaffected: it is the one drag this
    // band keeps either way.
    if (!trimmable && drag !== 'seek') { drag = null; return; }
    const ms = msAt(e.clientX);
    if (drag === 'seek') onseek(ms);
    // Snapped here, at the moment of the drag, for the same reason the old
    // trim bar wrote the snapped value back into its input: the handle must
    // sit where the export will really cut, not where the pointer was.
    else if (drag === 'in') onchange(snapStart(ms, keyframes), outMs);
    // Out is reported raw: fast mode cuts the end where it is asked to, so
    // there is no snap to diverge from.
    else onchange(inMs, ms);
  }

  function drop(e: PointerEvent) {
    if (!drag) return;
    (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    drag = null;
  }

  /**
   * Arrow keys on a focused handle.
   *
   * In moves by whole keyframes, which is the only unit an in-point has --
   * `keys.ts` records that the old `<input type="range">` handles used a 1ms
   * step, which moved the point by an invisible amount and cost the page its
   * prev/next shortcut for nothing. Out moves by a tenth of a second, or a
   * whole one with Shift.
   */
  function onHandleKey(which: 'in' | 'out', e: KeyboardEvent) {
    const dir = e.key === 'ArrowRight' ? 1 : e.key === 'ArrowLeft' ? -1 : 0;
    let next: number;
    if (dir !== 0) {
      if (which === 'in') next = keyframeStep(inMs, keyframes, dir);
      else next = clamp(outMs + dir * (e.shiftKey ? 1000 : 100), 0, durationMs);
    } else if (e.key === 'Home') next = 0;
    else if (e.key === 'End') next = durationMs;
    else return;
    // Stopped as well as prevented: `ClipPage` listens on
    // `<svelte:window>` and would otherwise also step to the next clip.
    e.preventDefault();
    e.stopPropagation();
    if (which === 'in') onchange(clamp(next, 0, durationMs), outMs);
    else onchange(inMs, clamp(next, 0, durationMs));
  }

  /**
   * Arrow keys on the focused playhead.
   *
   * Stopped as well as prevented, for the same reason `onHandleKey` does it:
   * `ClipPage` listens on `<svelte:window>` and would otherwise also step to
   * the next clip. Keys the playhead does not own come back null and are left
   * completely alone, so Space still plays and Escape still leaves the page.
   */
  function onSeekKey(e: KeyboardEvent) {
    const next = seekKeyTarget(playheadMs, durationMs, e.key, e.shiftKey);
    if (next === null) return;
    e.preventDefault();
    e.stopPropagation();
    onseek(next);
  }
</script>

<!-- The band itself is a pointer convenience: press or drag anywhere along it
     to seek. The playhead inside it is a real focusable
     slider, so seeking is not pointer-only: a keyboard moves it in 5s steps,
     1s with Shift, and jumps to either end. That is coarser than a drag, which
     lands on any millisecond -- the suppression below costs a keyboard user
     precision, not access.

     An earlier draft justified this by pointing at the two trim handles. That
     was wrong -- they move `inMs` and `outMs` through `onchange` and never
     touch the playhead. It came from TrimBar.svelte, whose version of the
     claim ended "and the video's own controls": the clause that made it true,
     and the one thing this design removes.

     The band cannot take the slider role itself. `slider` is a leaf role and
     this element contains one already -- the playhead -- plus both handles
     whenever `trimmable` is on. -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  bind:this={band}
  class="band"
  onpointerdown={(e) => grab('seek', e)}
  onpointermove={move}
  onpointerup={drop}>
  {#each keyframes as k (k)}
    <span class="tick" style="left: {pct(k)}"></span>
  {/each}

  {#if trimmable}
    <span class="scrim" style="left: 0; width: {pct(inMs)}"></span>
    <span class="scrim" style="left: {pct(outMs)}; right: 0"></span>
    <span class="range" style="left: {pct(inMs)}; width: {ratioFromMs(Math.max(0, outMs - inMs), durationMs) * 100}%"></span>
  {/if}
  <span
    class="playhead"
    style="left: {pct(playheadMs)}"
    role="slider"
    tabindex="0"
    aria-label="Playhead"
    aria-valuemin={0}
    aria-valuemax={durationMs}
    aria-valuenow={playheadMs}
    aria-valuetext={formatClock(playheadMs)}
    onkeydown={onSeekKey}></span>

  {#if trimmable}
    <span
      class="handle"
      class:dragging={drag === 'in'}
      style="left: {pct(inMs)}"
      role="slider"
      tabindex="0"
      aria-label="Trim start"
      aria-valuemin={0}
      aria-valuemax={durationMs}
      aria-valuenow={inMs}
      onpointerdown={(e) => grab('in', e)}
      onpointermove={move}
      onpointerup={drop}
      onkeydown={(e) => onHandleKey('in', e)}></span>

    <span
      class="handle"
      class:dragging={drag === 'out'}
      style="left: {pct(outMs)}"
      role="slider"
      tabindex="0"
      aria-label="Trim end"
      aria-valuemin={0}
      aria-valuemax={durationMs}
      aria-valuenow={outMs}
      onpointerdown={(e) => grab('out', e)}
      onpointermove={move}
      onpointerup={drop}
      onkeydown={(e) => onHandleKey('out', e)}></span>
  {/if}
</div>

<style>
  .band {
    position: relative;
    height: 38px;
    border-radius: var(--r);
    background: var(--raised);
    border: 1px solid var(--line);
    cursor: pointer;
    touch-action: none;
  }
  .tick { position: absolute; top: 0; bottom: 0; width: 1px; background: var(--line-strong); }
  .scrim { position: absolute; top: 0; bottom: 0; background: var(--scrim-soft); pointer-events: none; }
  .range {
    position: absolute;
    top: 0;
    bottom: 0;
    background: color-mix(in srgb, var(--accent) 20%, transparent);
    border-left: 2px solid var(--accent);
    border-right: 2px solid var(--accent);
    pointer-events: none;
  }
  .playhead { position: absolute; top: 0; bottom: 0; width: 2px; background: var(--text); pointer-events: none; }
  .playhead::before {
    content: '';
    position: absolute;
    top: -1px;
    left: -3px;
    width: 8px;
    height: 5px;
    background: var(--text);
    border-radius: 0 0 2px 2px;
  }
  .handle {
    position: absolute;
    top: 50%;
    width: 15px;
    height: 15px;
    border-radius: 50%;
    background: var(--accent);
    border: 2px solid var(--text);
    transform: translate(-50%, -50%);
    cursor: ew-resize;
    transition: box-shadow var(--t-fast) var(--ease);
  }
  .handle:hover,
  .handle.dragging { box-shadow: 0 0 0 5px color-mix(in srgb, var(--accent) 22%, transparent); }
</style>
