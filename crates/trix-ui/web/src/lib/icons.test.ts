import { describe, expect, it } from 'vitest';
import { ICONS, type IconName } from './icons';

/**
 * Every name the app asks for by string somewhere. A missing entry is a
 * blank square in the UI and nothing in the console, so it is checked here
 * rather than discovered on screen.
 */
const REQUIRED: IconName[] = [
  'play', 'pause', 'clips', 'settings', 'star', 'star-filled', 'pencil',
  'trash', 'folder', 'scissors', 'volume', 'volume-low', 'volume-mute', 'fullscreen',
  'fullscreen-exit', 'chevron-left', 'chevron-right', 'chevron-down', 'dots',
  'check', 'rearm', 'minimize', 'maximize', 'restore', 'close', 'alert',
  'camera', 'copy',
];

describe('ICONS', () => {
  it('has every icon the app names', () => {
    for (const name of REQUIRED) {
      expect(ICONS[name], `missing icon: ${name}`).toBeDefined();
    }
  });

  it('gives every icon a non-empty path', () => {
    for (const [name, def] of Object.entries(ICONS)) {
      expect(def.d.trim().length, `empty path: ${name}`).toBeGreaterThan(0);
    }
  });

  it('never continues a flattened path with a relative move', () => {
    // Lucide writes some pieces as `<path d="m15 5 4 4">`. A lowercase opening
    // move is absolute only while it opens a path; flattened after another
    // piece it moves relative to wherever that piece ended, and the stroke
    // lands off the icon -- the pencil lost its tip and the mute icon its
    // cross exactly this way, with every other check green.
    for (const [name, def] of Object.entries(ICONS)) {
      expect(def.d.slice(1), `relative move inside: ${name}`).not.toMatch(/m/);
    }
  });

  it('carries no icon the app does not ask for', () => {
    // YAGNI, enforced. An unused icon is dead weight in every bundle.
    expect(Object.keys(ICONS).sort()).toEqual([...REQUIRED].sort());
  });
});
