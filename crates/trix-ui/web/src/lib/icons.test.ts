import { describe, expect, it } from 'vitest';
import { ICONS, type IconName } from './icons';

/**
 * Every name the app asks for by string somewhere. A missing entry is a
 * blank square in the UI and nothing in the console, so it is checked here
 * rather than discovered on screen.
 */
const REQUIRED: IconName[] = [
  'play', 'pause', 'clips', 'settings', 'star', 'star-filled', 'pencil',
  'trash', 'folder', 'scissors', 'volume', 'volume-mute', 'fullscreen',
  'fullscreen-exit', 'chevron-left', 'chevron-right', 'chevron-down', 'dots',
  'check', 'rearm', 'minimize', 'maximize', 'restore', 'close', 'alert',
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

  it('carries no icon the app does not ask for', () => {
    // YAGNI, enforced. An unused icon is dead weight in every bundle.
    expect(Object.keys(ICONS).sort()).toEqual([...REQUIRED].sort());
  });
});
