import { describe, expect, it } from 'vitest';
// Read through Vite's `?raw`, not `node:fs`. Reaching for the filesystem
// here would mean adding @types/node, and this project may not take a new
// dependency -- dev ones included.
import capabilitiesJson from '../../../capabilities/default.json?raw';
import titleBarSource from '../components/TitleBar.svelte?raw';
import { calledWindowMethods, missingWindowPermissions } from './capabilities';

describe('calledWindowMethods', () => {
  it('finds calls made through a `win` binding', () => {
    expect(calledWindowMethods('void win.minimize(); void win.close();')).toEqual(['close', 'minimize']);
  });

  it('finds calls made directly on getCurrentWindow()', () => {
    expect(calledWindowMethods('getCurrentWindow().toggleMaximize()')).toEqual(['toggleMaximize']);
  });

  it('ignores same-named methods on other receivers', () => {
    expect(calledWindowMethods('dialog.close(); menu.hide();')).toEqual([]);
  });

  it('counts the drag region as a startDragging call', () => {
    expect(calledWindowMethods('<div data-tauri-drag-region></div>')).toEqual(['startDragging']);
  });

  it('reports nothing for source that never touches the window', () => {
    expect(calledWindowMethods('const x = 1;')).toEqual([]);
  });
});

describe('missingWindowPermissions', () => {
  it('reports a call whose permission was never granted', () => {
    expect(missingWindowPermissions('win.minimize()', ['core:default'])).toEqual([
      'core:window:allow-minimize',
    ]);
  });

  it('accepts a call whose permission is granted explicitly', () => {
    expect(missingWindowPermissions('win.minimize()', ['core:window:allow-minimize'])).toEqual([]);
  });

  /**
   * The asymmetry that caused the bug this file exists for. Both halves of
   * `data-tauri-drag-region` look identical in the markup, but only the
   * double-click half is covered by `core:default`.
   */
  it('still reports start-dragging when core:default is the only grant', () => {
    expect(missingWindowPermissions('<div data-tauri-drag-region></div>', ['core:default'])).toEqual([
      'core:window:allow-start-dragging',
    ]);
  });

  it('does not ask for a permission core:default already covers', () => {
    expect(missingWindowPermissions('win.isMaximized()', ['core:default'])).toEqual([]);
  });

  it('asks for it when core:default is absent', () => {
    expect(missingWindowPermissions('win.isMaximized()', [])).toEqual([
      'core:window:allow-is-maximized',
    ]);
  });
});

/**
 * The gate itself, over the real files. `npm run check`, `npm run build` and
 * `cargo build` all pass with an ungranted window call in place -- it fails
 * only when a person clicks the control. This is the only automated check
 * that stands between that defect and a release.
 */
describe('the shipped title bar against the shipped capabilities', () => {
  const source = titleBarSource;
  const granted: string[] = JSON.parse(capabilitiesJson).permissions;

  it('drives the window through a receiver this scan can see', () => {
    // A scan that matches nothing would pass the assertion below for the
    // wrong reason, and go on passing forever.
    expect(calledWindowMethods(source).length).toBeGreaterThan(0);
  });

  it('grants every window call the title bar makes', () => {
    expect(missingWindowPermissions(source, granted)).toEqual([]);
  });
});
