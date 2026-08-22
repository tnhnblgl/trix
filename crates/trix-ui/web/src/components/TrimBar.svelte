<script lang="ts">
  // Presentational on purpose: this component never calls the daemon. It is
  // given a duration, a playhead, a keyframe list and the current in/out, and
  // it reports back where the user dragged. That keeps every daemon call in
  // state.svelte.ts, where the rest of them already live.
  //
  // A plain bar with tick marks rather than the filmstrip spec §6.2 sketches:
  // thumbnails along the range would need a video decode path, and the daemon
  // has none -- it writes clips, it does not read frames back out of them.
  // The ticks carry the part that actually matters, which is where a fast-mode
  // cut can land.
  let {
    durationMs,
    playheadMs,
    keyframes,
    inMs,
    outMs,
    rangeError,
    onchange,
    onseek,
  }: {
    durationMs: number;
    playheadMs: number;
    keyframes: number[];
    inMs: number;
    outMs: number;
    /** Why the current range cannot be exported, or null when it can. Decided
        by the page (which owns the daemon's rules), shown here, so the user
        reads it while making the mistake rather than after pressing Export. */
    rangeError: string | null;
    onchange: (inMs: number, outMs: number) => void;
    onseek: (ms: number) => void;
  } = $props();

  const pct = (ms: number) => (durationMs > 0 ? (ms / durationMs) * 100 : 0);

  /** Seconds to one decimal -- `formatDuration` rounds to whole seconds, which
      is the right grain for a clip's length in the metadata line and the wrong
      one for a trim, where a person is picking a moment. */
  const secs = (ms: number) => `${(Math.max(0, ms) / 1000).toFixed(1)}s`;

  /** Latest keyframe at or before `ms` -- the same rule the daemon applies
      (`snap_start` in trix-core/src/export.rs), so the handle sits where the
      export will actually cut rather than where the pointer was released.

      An empty list is not "snap to zero": the ticks are still loading, or the
      clip could not be indexed and `keyframesFor` deliberately degraded to a
      bar with no ticks. Pinning the in-point to 0 in either case would make
      the slider look broken. With nothing to snap against, the raw position
      is the honest answer and the daemon snaps it for real anyway. */
  function snap(ms: number): number {
    if (keyframes.length === 0) return ms;
    let best = 0;
    for (const k of keyframes) if (k <= ms) best = k;
    return best;
  }

  function msAt(e: MouseEvent, el: HTMLElement): number {
    const box = el.getBoundingClientRect();
    const ratio = Math.min(1, Math.max(0, (e.clientX - box.left) / box.width));
    return Math.round(ratio * durationMs);
  }
</script>

<div class="trim">
  <!-- The track is a seek surface, not a control: it has no state of its own to
       operate by keyboard, and every position it can reach is reachable from
       the two sliders below and the video's own controls. Making it a <button>
       would be worse than the warning it silences -- WebView2 focuses a button
       on click, and `shouldHandleKey` then hands Space and Enter to it, taking
       play/pause away from the page the moment someone seeks. -->
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div class="track" onclick={(e) => onseek(msAt(e, e.currentTarget))}>
    {#each keyframes as k}
      <span class="tick" style="left: {pct(k)}%"></span>
    {/each}
    <span
      class="range"
      style="left: {pct(inMs)}%; width: {pct(Math.max(0, outMs - inMs))}%"
    ></span>
    <span class="playhead" style="left: {pct(playheadMs)}%"></span>
  </div>

  <div class="handles">
    <label>
      In
      <!-- The element's own value is written back after snapping. `value=` is
           one-way, so Svelte only touches the DOM when `inMs` actually
           changes -- and a drag from 1.0s to 1.8s over keyframes [0, 2000]
           snaps to 0 both times, which is the value the state already holds.
           No prop change, no DOM write, and the thumb stays under the pointer
           while the highlight and the readout sit at 0. Assigning it here is
           what makes the handle land where the export will really cut, which
           is the whole reason the ticks are drawn. -->
      <input
        type="range" min="0" max={durationMs} step="1" value={inMs}
        oninput={(e) => {
          const snapped = snap(Number(e.currentTarget.value));
          e.currentTarget.value = String(snapped);
          onchange(snapped, outMs);
        }} />
    </label>
    <label>
      Out
      <!-- Out is reported raw: fast mode cuts the end where it is asked to, so
           there is no snap to diverge from and the element already holds what
           the parent will store. If a transform is ever added here it needs
           the same write-back as In, for the same reason. -->
      <input
        type="range" min="0" max={durationMs} step="1" value={outMs}
        oninput={(e) => onchange(inMs, Number(e.currentTarget.value))} />
    </label>
  </div>

  <p class="readout" class:bad={rangeError !== null}>
    In {secs(inMs)} &middot; Out {secs(outMs)} &middot; {secs(outMs - inMs)} selected{#if rangeError}
      &middot; {rangeError}{/if}
  </p>
</div>

<style>
  .trim { margin: 12px 0; display: grid; gap: 6px; }
  .track { position: relative; height: 26px; border-radius: 6px; background: var(--panel); border: 1px solid var(--line); cursor: pointer; overflow: hidden; }
  .tick { position: absolute; top: 0; bottom: 0; width: 1px; background: var(--line); }
  .range { position: absolute; top: 0; bottom: 0; background: color-mix(in srgb, var(--accent) 28%, transparent); border-left: 2px solid var(--accent); border-right: 2px solid var(--accent); }
  .playhead { position: absolute; top: 0; bottom: 0; width: 2px; background: var(--text); }
  .handles { display: flex; gap: 16px; }
  .handles label { flex: 1; display: flex; align-items: center; gap: 8px; color: var(--dim); font-size: 12px; }
  .handles input { flex: 1; }
  .readout { margin: 0; color: var(--dim); font-size: 12px; }
  /* An unexportable range reads as a sliver on the track and "0.0s selected"
     in the numbers, neither of which says what is wrong. This does. */
  .readout.bad { color: var(--danger); }
</style>
