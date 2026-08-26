/**
 * Which Tauri permission gates which window call, and which of those
 * `core:default` already grants.
 *
 * This exists because a missing window permission is the one defect class
 * this project's whole toolchain is blind to. `svelte-check`, `vite build`
 * and `cargo build` all succeed; the call is refused at runtime, in the
 * webview, and the control simply does nothing. It cost a full build and a
 * hand-check to find that `data-tauri-drag-region` was inert because
 * `core:window:allow-start-dragging` had never been granted.
 *
 * The subtle part -- and the reason this is a table rather than a list -- is
 * that `core:default` covers *some* of these and not others, in a way that
 * does not follow the shape of the API. `internal_toggle_maximize` is
 * granted by default, so double-clicking the drag region maximizes the
 * window; `start_dragging` is not, so dragging the very same element does
 * nothing. Two behaviours of one attribute, split by the permission table.
 * Reading the API surface will not tell you that. This will.
 */
export type WindowCall = {
  /** The permission string that gates it. */
  permission: string;
  /** Whether `core:default` already grants it, making an explicit grant redundant. */
  inCoreDefault: boolean;
};

/**
 * Keyed by the method name as it is called in source. `inCoreDefault` is
 * transcribed from `core:window:default`'s own grant list in
 * `crates/trix-ui/gen/schemas/desktop-schema.json` -- regenerate that schema
 * and this table can go stale, which is what the test over the real files is
 * for.
 */
export const WINDOW_CALLS: Record<string, WindowCall> = {
  minimize: { permission: 'core:window:allow-minimize', inCoreDefault: false },
  maximize: { permission: 'core:window:allow-maximize', inCoreDefault: false },
  unmaximize: { permission: 'core:window:allow-unmaximize', inCoreDefault: false },
  toggleMaximize: { permission: 'core:window:allow-toggle-maximize', inCoreDefault: false },
  close: { permission: 'core:window:allow-close', inCoreDefault: false },
  destroy: { permission: 'core:window:allow-destroy', inCoreDefault: false },
  show: { permission: 'core:window:allow-show', inCoreDefault: false },
  hide: { permission: 'core:window:allow-hide', inCoreDefault: false },
  setFocus: { permission: 'core:window:allow-set-focus', inCoreDefault: false },
  setFullscreen: { permission: 'core:window:allow-set-fullscreen', inCoreDefault: false },
  startDragging: { permission: 'core:window:allow-start-dragging', inCoreDefault: false },
  isMaximized: { permission: 'core:window:allow-is-maximized', inCoreDefault: true },
  isMinimized: { permission: 'core:window:allow-is-minimized', inCoreDefault: true },
  isFullscreen: { permission: 'core:window:allow-is-fullscreen', inCoreDefault: true },
  isVisible: { permission: 'core:window:allow-is-visible', inCoreDefault: true },
};

/**
 * The permissions `data-tauri-drag-region` needs. Dragging is an IPC call
 * like any other -- the attribute is sugar over `startDragging()`, not a
 * webview-level behaviour that sidesteps the permission system. The
 * double-click-to-maximize half of the attribute rides on
 * `internal_toggle_maximize`, which `core:default` does grant.
 */
export const DRAG_REGION_CALLS = ['startDragging'] as const;

/**
 * Window methods `source` calls, found by their receiver.
 *
 * Only `win.foo()` and `getCurrentWindow().foo()` are recognised: binding the
 * window to some other name hides the call from this scan. That is a real
 * limitation, and the reason the test that uses this also asserts it found
 * something in a file known to drive the window -- a scan that silently
 * matches nothing is worse than no scan.
 */
export function calledWindowMethods(source: string): string[] {
  const found = new Set<string>();
  for (const m of source.matchAll(/(?:\bwin|getCurrentWindow\(\))\.(\w+)\s*\(/g)) {
    if (m[1] in WINDOW_CALLS) found.add(m[1]);
  }
  if (source.includes('data-tauri-drag-region')) {
    for (const c of DRAG_REGION_CALLS) found.add(c);
  }
  return [...found].sort();
}

/**
 * Permissions `source` needs that `granted` does not supply.
 *
 * Anything `core:default` already covers is not reported, so the result is
 * only what a human has to add to `capabilities/default.json`. Empty means
 * every window call in this source is permitted at runtime.
 */
export function missingWindowPermissions(source: string, granted: string[]): string[] {
  const has = new Set(granted);
  const coreDefault = has.has('core:default');
  return calledWindowMethods(source)
    .map((name) => WINDOW_CALLS[name])
    .filter((call) => !(call.inCoreDefault && coreDefault) && !has.has(call.permission))
    .map((call) => call.permission)
    .sort();
}

/**
 * Window permissions `granted` carries that nothing in `source` calls.
 *
 * The other direction of the same check, and the one nothing else in the
 * toolchain will ever mention: a control that is removed or rewritten leaves
 * its grant behind, and an over-granted capability file fails no build, no
 * test and no hand-check. It simply widens what a compromised webview could
 * ask the shell to do, quietly, forever.
 *
 * Only permissions `WINDOW_CALLS` knows about are considered. `core:default`
 * and any non-window grant are somebody else's business and are not reported
 * as excess.
 */
export function grantedButUncalled(source: string, granted: string[]): string[] {
  const needed = new Set(calledWindowMethods(source).map((name) => WINDOW_CALLS[name].permission));
  const known = new Set(Object.values(WINDOW_CALLS).map((call) => call.permission));
  return granted.filter((p) => known.has(p) && !needed.has(p)).sort();
}
