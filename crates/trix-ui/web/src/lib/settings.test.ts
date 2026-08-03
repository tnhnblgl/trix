import { describe, expect, it } from 'vitest';
import { FIELDS, unknownKeys, validate } from './settings';

describe('FIELDS', () => {
  it('covers every config key the daemon has today', () => {
    const shipped = [
      'fps', 'bitrate_kbps', 'max_bitrate_kbps', 'rate_control', 'replay_seconds',
      'monitor_index', 'clip_hotkey', 'gpu_priority', 'stats_seconds', 'clip_dir',
      'max_library_gb', 'autostart',
    ];
    const covered = FIELDS.map((f) => f.key);
    for (const key of shipped) expect(covered).toContain(key);
  });
});

describe('unknownKeys', () => {
  it('ignores the two read-only extras config.get adds', () => {
    const config = { fps: 60, clip_dir_resolved: 'C:\\x', autostart: false };
    expect(unknownKeys(config)).toEqual([]);
  });

  it('reports a key the daemon grew that this page does not render', () => {
    // The daemon is the source of truth for what is configurable. A settings
    // page that silently drops a new key is worse than one that renders it
    // plainly, because the user cannot tell it is missing.
    expect(unknownKeys({ fps: 60, mystery_key: 3 })).toEqual(['mystery_key']);
  });
});

describe('validate', () => {
  it('mirrors the daemon bounds so the error arrives before the round trip', () => {
    expect(validate('fps', 0)).toMatch(/1 to 480/);
    expect(validate('fps', 60)).toBeNull();
    expect(validate('replay_seconds', 601)).toMatch(/1 to 600/);
    expect(validate('max_library_gb', 0)).toBeNull();
  });

  it('says nothing about keys it has no bound for', () => {
    expect(validate('clip_hotkey', 'alt+f10')).toBeNull();
  });
});
