import { describe, expect, it } from 'vitest';
import { FIELDS, READ_ONLY_EXTRAS, unknownKeys, validate } from './settings';

describe('FIELDS', () => {
  it('covers every config key the daemon has today', () => {
    const shipped = [
      'fps', 'bitrate_kbps', 'max_bitrate_kbps', 'rate_control', 'replay_seconds',
      'monitor_index', 'clip_hotkey', 'gpu_priority', 'stats_seconds', 'clip_dir',
      'max_library_gb', 'autostart', 'system_volume', 'mic_volume',
    ];
    const covered = FIELDS.map((f) => f.key);
    for (const key of shipped) expect(covered).toContain(key);
  });

  it('renders both levels as sliders in the Audio section', () => {
    for (const key of ['system_volume', 'mic_volume']) {
      const field = FIELDS.find((f) => f.key === key);
      expect(field, `${key} has no field`).toBeDefined();
      expect(field?.kind).toBe('slider');
      expect(field?.section).toBe('Audio');
    }
  });

  it('bounds both levels at 0 to 100, mirroring the daemon', () => {
    // 100 is unity and the maximum. A page that let someone ask for 150
    // would produce a round trip that fails for a reason the field cannot
    // explain.
    expect(validate('mic_volume', 101)).toMatch(/0 to 100/);
    expect(validate('system_volume', 101)).toMatch(/0 to 100/);
    expect(validate('mic_volume', 0)).toBeNull();
    expect(validate('system_volume', 100)).toBeNull();
  });
});

describe('unknownKeys', () => {
  it('ignores every read-only extra config.get adds', () => {
    // Built from `READ_ONLY_EXTRAS` -- the same list `unknownKeys` allowlists
    // -- rather than a hand-typed copy of it, so this fixture cannot drift
    // out from under the production code the way it once did: this test used
    // to be named "the two read-only extras" while carrying only one, and a
    // third (`config_file_exists`) shipped without it, showing every user a
    // false "this daemon has settings this app does not render yet" banner.
    const config: Record<string, unknown> = { fps: 60, autostart: false };
    for (const key of READ_ONLY_EXTRAS) config[key] = 'placeholder';
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

describe('settings fields', () => {
  it('renders check_for_updates, so nobody sees the unknown-settings banner', () => {
    // config.get returns every Config key. A key with no FIELDS entry lands in
    // unknownKeys, and Settings.svelte tells the user their app is out of date
    // with their daemon -- which would be false and unfixable.
    const fromDaemon = { check_for_updates: true, ...Object.fromEntries(READ_ONLY_EXTRAS.map((k) => [k, ''])) };
    expect(unknownKeys(fromDaemon)).toEqual([]);
    expect(FIELDS.find((f) => f.key === 'check_for_updates')?.kind).toBe('bool');
  });
});
