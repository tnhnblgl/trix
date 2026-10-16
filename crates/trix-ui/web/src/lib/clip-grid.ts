/** A bounded set of complete rows, with spacers for the unmounted thumbnails. */
export function gridWindow(count: number, columns: number, stride: number, scroll: number, height: number) {
  const rows = Math.ceil(count / columns);
  const top = Math.min(Math.max(0, scroll), Math.max(0, rows * stride - height));
  const first = Math.max(0, Math.floor(top / stride) - 2);
  const last = Math.min(rows, Math.ceil((top + height) / stride) + 2);
  return { start: first * columns, end: Math.min(count, last * columns), top: first * stride, bottom: (rows - last) * stride };
}

/** Range selection follows the filtered list, not the mounted DOM or file names. */
export function toggleClipSelection(checked: Set<string>, visible: string[], id: string, anchor: string | null, range: boolean): Set<string> {
  const next = new Set(checked);
  const index = visible.indexOf(id);
  if (index < 0) return next;
  const start = anchor === null ? -1 : visible.indexOf(anchor);
  if (range && start >= 0) {
    for (const selected of visible.slice(Math.min(start, index), Math.max(start, index) + 1)) next.add(selected);
  } else if (next.has(id)) next.delete(id);
  else next.add(id);
  return next;
}
