/**
 * Where the selection lands after an arrow key in a grid `columns` wide.
 *
 * Clamps rather than wraps, on purpose. The grid is newest-first and `Del` acts
 * on the selection, so wrapping from the newest clip to the oldest puts the
 * clip you care least about under a destructive key.
 */
export function moveSelection(
  current: number,
  key: string,
  count: number,
  columns: number,
): number {
  if (count === 0) return 0;
  const delta =
    key === 'ArrowRight' ? 1
    : key === 'ArrowLeft' ? -1
    : key === 'ArrowDown' ? columns
    : key === 'ArrowUp' ? -columns
    : 0;
  if (delta === 0) return current;
  const next = current + delta;
  return next < 0 || next >= count ? current : next;
}
