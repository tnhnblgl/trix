import { clamp } from './ui';

/**
 * Geometry and snapping for the unified seek-and-trim band.
 *
 * Pulled out of the component so it can be tested without a DOM. `snapStart`
 * is a move of `TrimBar.svelte`'s `snap()`, unchanged in behaviour -- it is
 * the same rule the daemon applies in `trix-core/src/export.rs`'s
 * `snap_start`, so the handle sits where the export will actually cut.
 */

/** Milliseconds a step covers when a clip has no keyframe index at all. */
const NO_KEYFRAME_STEP_MS = 1000;

/**
 * Latest keyframe at or before `ms`.
 *
 * An empty list is not "snap to zero": the ticks are still loading, or the
 * clip could not be indexed and `keyframesFor` deliberately degraded to a bar
 * with no ticks. Pinning the in-point to 0 in either case would make the
 * control look broken. With nothing to snap against, the raw position is the
 * honest answer and the daemon snaps it for real anyway.
 */
export function snapStart(ms: number, keyframes: number[]): number {
  if (keyframes.length === 0) return ms;
  let best = 0;
  for (const k of keyframes) if (k <= ms) best = k;
  return best;
}

/**
 * The neighbouring keyframe in `dir`, or `ms` itself at either end.
 *
 * Strictly past `ms`, so a handle already sitting on a keyframe moves off it
 * rather than snapping back to where it already is.
 */
export function keyframeStep(ms: number, keyframes: number[], dir: 1 | -1): number {
  if (keyframes.length === 0) return Math.max(0, ms + dir * NO_KEYFRAME_STEP_MS);
  if (dir === 1) {
    for (const k of keyframes) if (k > ms) return k;
    return ms;
  }
  let best = ms;
  for (const k of keyframes) if (k < ms) best = k;
  return best === ms ? ms : best;
}

/** Where a pointer at `ratio` along the band lands, in whole milliseconds. */
export function msFromRatio(ratio: number, durationMs: number): number {
  if (durationMs <= 0) return 0;
  return Math.round(clamp(ratio, 0, 1) * durationMs);
}

/** How far along the band `ms` sits, as 0..1. */
export function ratioFromMs(ms: number, durationMs: number): number {
  if (durationMs <= 0) return 0;
  return clamp(ms / durationMs, 0, 1);
}

/**
 * `m:ss.t`.
 *
 * A tenth, unlike `formatDuration`'s whole seconds: that is the right grain
 * for a clip's length in a metadata line and the wrong one for a playhead,
 * where a person is picking a moment.
 */
export function formatClock(ms: number): string {
  const safe = Math.max(0, ms);
  const tenths = Math.floor(safe / 100) % 10;
  const totalSeconds = Math.floor(safe / 1000);
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  return `${minutes}:${String(seconds).padStart(2, '0')}.${tenths}`;
}

/**
 * How far an arrow key moves the playhead, and the finer grain Shift asks for.
 *
 * Five seconds is what Chromium's own control bar stepped by, and that bar is
 * exactly what this timeline replaces -- a keyboard user who knew the old
 * player should find the new one moves the same distance.
 */
export const SEEK_STEP_MS = 5000;
export const SEEK_STEP_FINE_MS = 1000;

/**
 * Where the playhead lands for `key` pressed on the seek control, or `null`
 * for a key the control does not own.
 *
 * `null` rather than `ms` so the caller can tell "nothing to do" from "stay
 * where you are": the band must let an unclaimed key reach `ClipPage`, which
 * steps between clips on the arrows and plays on Space. Returning the current
 * position for every unrecognised key would swallow them all.
 */
export function seekKeyTarget(
  ms: number,
  durationMs: number,
  key: string,
  shift: boolean,
): number | null {
  const step = shift ? SEEK_STEP_FINE_MS : SEEK_STEP_MS;
  if (key === 'ArrowRight') return clamp(ms + step, 0, durationMs);
  if (key === 'ArrowLeft') return clamp(ms - step, 0, durationMs);
  if (key === 'Home') return 0;
  if (key === 'End') return durationMs;
  return null;
}
