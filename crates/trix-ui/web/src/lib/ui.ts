/**
 * Control maths for the drawn primitives in `components/ui/`.
 *
 * Everything here is pure and DOM-free on purpose. The project has no
 * component test framework and adding one would mean a new dependency, so a
 * primitive's correctness lives in whatever part of it can be pulled out to
 * here and tested. A `.svelte` file that grows arithmetic is a file with an
 * untested branch in it.
 */

/** `value`, pinned into `[min, max]`. */
export function clamp(value: number, min: number, max: number): number {
  return value < min ? min : value > max ? max : value;
}

/**
 * `value` rounded onto the grid of `step`s starting at `min`.
 *
 * Measured from `min` rather than from zero: a control running 20..80 in
 * fifteens can sit on 20, 35, 50, 65 and 80, and rounding from zero would
 * offer 30, 45 and 60 instead -- positions the control cannot actually hold.
 */
export function snapToStep(value: number, min: number, step: number): number {
  if (step <= 1) return value;
  return min + Math.round((value - min) / step) * step;
}

/** Where a pointer at `ratio` along a track lands, snapped and clamped. */
export function ratioToValue(ratio: number, min: number, max: number, step: number): number {
  const raw = min + clamp(ratio, 0, 1) * (max - min);
  return clamp(snapToStep(raw, min, step), min, max);
}

/** How far along its track `value` sits, as 0..1. */
export function valueToRatio(value: number, min: number, max: number): number {
  // A zero-width range is a real state -- a clip of no length, a bound the
  // daemon collapsed -- and dividing by it yields NaN, which reaches CSS as
  // `left: NaN%` and drops the thumb out of the control entirely.
  if (max <= min) return 0;
  return clamp((value - min) / (max - min), 0, 1);
}

/**
 * `value` moved `delta` steps.
 *
 * Snapped after moving, not before, so a value the daemon handed back that is
 * off the UI's own grid is pulled onto it by the first arrow key rather than
 * carrying its offset for the rest of the drag. The snap rounds toward the
 * direction of travel -- floor when moving up, ceil when moving down --
 * rather than to the nearest grid point: rounding to nearest can land a
 * forward step behind where it started, which reads as the key doing
 * nothing or, worse, going backward.
 */
export function stepBy(value: number, delta: number, min: number, max: number, step: number): number {
  const s = step <= 0 ? 1 : step;
  const moved = value + delta * s;
  const idx = delta >= 0 ? Math.floor((moved - min) / s) : Math.ceil((moved - min) / s);
  return clamp(min + idx * s, min, max);
}

/**
 * What a Stepper's text box should commit as: `raw` parsed and clamped, or
 * `current` if `raw` cannot be read as a number.
 *
 * The empty string gets its own check before `Number()` ever runs. `Number('')`
 * -- and `Number('   ')` -- coerce to `0`, not `NaN`, so a naive
 * `Number.isFinite` test would read a cleared box as "the user typed zero"
 * instead of "the user typed nothing," silently committing 0 rather than
 * putting the previous value back.
 */
export function resolveStepperInput(raw: string, current: number, min: number, max: number): number {
  const trimmed = raw.trim();
  if (trimmed === '') return current;
  const parsed = Number(trimmed);
  return Number.isFinite(parsed) ? clamp(parsed, min, max) : current;
}

/**
 * The next index in a list of `count`, clamped rather than wrapped.
 *
 * Same rule as `moveSelection` in `keys.ts`, and for a related reason: a menu
 * whose last item is Delete must not put Delete under the cursor because
 * someone pressed Down once too often.
 */
export function nextIndex(current: number, delta: number, count: number): number {
  if (count <= 0) return -1;
  return clamp(current + delta, 0, count - 1);
}
