import { describe, expect, it } from 'vitest';
import { BITRATE_TIERS, CUSTOM_TIER, FIELDS, READ_ONLY_EXTRAS, SECTIONS, hotkeySaveTarget, hotkeySeed, tierFor, tierOptions, unknownKeys, validate } from './settings';

describe('FIELDS', () => {
  it('covers every config key the daemon has today', () => {
    const shipped = [
      'fps', 'bitrate_kbps', 'max_bitrate_kbps', 'rate_control', 'replay_seconds',
      'monitor_index', 'clip_hotkey', 'gpu_priority', 'stats_seconds', 'clip_dir',
      'max_library_gb', 'autostart', 'system_volume', 'mic_volume', 'check_for_updates',
      'clip_sound', 'clip_sound_path', 'discord_presence',
      'screenshot_hotkey', 'screenshot_sound',
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

describe('bitrate tiers', () => {
  const field = FIELDS.find((f) => f.key === 'bitrate_kbps')!;

  it('renders the bitrate as a tier dropdown carrying its own presets', () => {
    // `tiers` on the descriptor, not a table keyed on 'bitrate_kbps' inside
    // the component -- see the `tiers` doc comment for why this page does not
    // key controls on config-key literals.
    expect(field.kind).toBe('tier');
    expect(field.tiers).toBe(BITRATE_TIERS);
  });

  it('shows every preset as itself', () => {
    for (const tier of BITRATE_TIERS) {
      expect(tierFor(field, tier.value), tier.label).toBe(String(tier.value));
    }
  });

  it('keeps the shipped 8000 default a preset, so no install needs migrating', () => {
    // The tier is derived from the number rather than stored, so an existing
    // config.toml is already correct -- but only while 8000 is on the list.
    // Drop it and every default install silently becomes `Custom`.
    expect(tierFor(field, 8000)).toBe('8000');
  });

  it('falls back to Custom for a number no preset matches', () => {
    // A value hand-typed into an older Trix. It must survive as itself; a
    // dropdown that rounded it onto the nearest preset would change what the
    // user records without telling them.
    expect(tierFor(field, 9500)).toBe(CUSTOM_TIER);
    expect(tierFor(field, 0)).toBe(CUSTOM_TIER);
    expect(tierFor(field, undefined)).toBe(CUSTOM_TIER);
  });

  it('lists the presets in order with Custom last', () => {
    expect(tierOptions(field).map((o) => o.value)).toEqual([
      '5000', '8000', '14000', '20000', CUSTOM_TIER,
    ]);
  });

  it('keeps every preset inside the bounds the daemon enforces', () => {
    // A preset outside them would be a dropdown entry whose only outcome is a
    // refusal the row cannot explain.
    for (const tier of BITRATE_TIERS) {
      expect(validate('bitrate_kbps', tier.value), tier.label).toBeNull();
    }
  });

  it('rises monotonically, so the labels mean what they say', () => {
    const values = BITRATE_TIERS.map((t) => t.value);
    expect([...values].sort((a, b) => a - b)).toEqual(values);
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

  it('has every FIELDS section rendered by SECTIONS', () => {
    // Settings.svelte iterates SECTIONS, not FIELDS, to decide what headings
    // to draw. A field whose `section` is not in SECTIONS is valid TypeScript
    // (svelte-check never checks the array literal against the union) but
    // never reaches the screen -- exactly what happened to `check_for_updates`
    // and `'Updates'` before SECTIONS existed.
    const usedSections = new Set(FIELDS.map((f) => f.section));
    for (const section of usedSections) expect(SECTIONS).toContain(section);
  });
});

describe('the clip sound fields', () => {
  it('puts both rows in a section the page already renders', () => {
    // A field in a section `SECTIONS` does not list renders nowhere at all --
    // the settings page iterates `SECTIONS`, not `FIELDS`.
    for (const key of ['clip_sound', 'clip_sound_path']) {
      const field = FIELDS.find((f) => f.key === key);
      expect(field, `${key} must be a settings field`).toBeDefined();
      expect(SECTIONS).toContain(field!.section);
    }
  });

  it('renders the sound file row with the sound control', () => {
    expect(FIELDS.find((f) => f.key === 'clip_sound_path')!.kind).toBe('sound');
    expect(FIELDS.find((f) => f.key === 'clip_sound')!.kind).toBe('bool');
  });

  it('says which formats work, because PlaySound is not the whole story', () => {
    const help = FIELDS.find((f) => f.key === 'clip_sound_path')!.help;
    expect(help).toMatch(/mp3/i);
    expect(help).toMatch(/10 seconds/i);
  });
});

describe('the screenshot fields', () => {
  it('renders the hotkey with the keycap control, like the clip hotkey', () => {
    // A `hotkey` field is the keycap recorder. A `text` one would make the
    // user type "alt+f8" by hand and get the grammar wrong.
    expect(FIELDS.find((f) => f.key === 'screenshot_hotkey')!.kind).toBe('hotkey');
    expect(FIELDS.find((f) => f.key === 'screenshot_sound')!.kind).toBe('bool');
  });

  it('puts both rows in a section the page actually renders', () => {
    // Settings.svelte iterates SECTIONS, not FIELDS: a field whose section is
    // absent from SECTIONS is valid TypeScript that never reaches the screen.
    for (const key of ['screenshot_hotkey', 'screenshot_sound']) {
      expect(SECTIONS).toContain(FIELDS.find((f) => f.key === key)!.section);
    }
  });

  it('says the screenshot needs Trix armed', () => {
    // The likeliest support question, and the help text is the only place it
    // can be answered before it is asked.
    expect(FIELDS.find((f) => f.key === 'screenshot_hotkey')!.help).toMatch(/armed/i);
  });

  it('does not offer a live test, because the screenshot hotkey arm never broadcasts hotkey_pressed', () => {
    // `window.rs`'s arm for SHOT_HOTKEY_ID deliberately stays silent so a
    // screenshot press can never make the clip row's "press it now" check
    // look answered -- `Field.svelte` reads this flag to decide whether to
    // show the Test button at all.
    expect(FIELDS.find((f) => f.key === 'screenshot_hotkey')!.liveTest).toBeUndefined();
  });
});

describe('liveTest', () => {
  it('is set only for the one hotkey row the daemon actually echoes back', () => {
    // A regression guard for the pattern this replaced: `Field.svelte` used to
    // read `field.key === 'clip_hotkey'` directly, which is exactly the kind
    // of hard-coded key literal that shipped a real Critical earlier on this
    // branch. Asserting the flag here, rather than the string, is what makes
    // a future third hotkey row a decision instead of a silent default.
    const hotkeyFields = FIELDS.filter((f) => f.kind === 'hotkey');
    expect(hotkeyFields.map((f) => f.key)).toEqual(['clip_hotkey', 'screenshot_hotkey']);
    expect(FIELDS.find((f) => f.key === 'clip_hotkey')!.liveTest).toBe(true);
    expect(hotkeyFields.filter((f) => f.liveTest === true).map((f) => f.key)).toEqual([
      'clip_hotkey',
    ]);
  });
});

describe('hotkeySaveTarget', () => {
  it('sends each hotkey row to its own key, never a shared literal', () => {
    // This is the regression the first pass of this task shipped: both the
    // clip and the screenshot row saved through `set('clip_hotkey', capture)`
    // because the key was typed once, for the only hotkey field that existed
    // at the time, and never revisited when a second one arrived. A field's
    // own key must come out the other end for every hotkey field, not just
    // the one that happened to be first.
    const clip = FIELDS.find((f) => f.key === 'clip_hotkey')!;
    const screenshot = FIELDS.find((f) => f.key === 'screenshot_hotkey')!;
    expect(hotkeySaveTarget(clip, 'alt+f10')).toEqual({ key: 'clip_hotkey', value: 'alt+f10' });
    expect(hotkeySaveTarget(screenshot, 'alt+f8')).toEqual({
      key: 'screenshot_hotkey',
      value: 'alt+f8',
    });
  });
});

describe('hotkeySeed', () => {
  it('seeds capture from each field\'s own config value, never another field\'s', () => {
    // The read-side half of the same bug: seeding every hotkey row's keycaps
    // from `config['clip_hotkey']` meant clicking into the Screenshot row
    // showed the *clip* combination, ready to be saved back over itself.
    const config = { clip_hotkey: 'alt+f10', screenshot_hotkey: 'alt+f8' };
    const clip = FIELDS.find((f) => f.key === 'clip_hotkey')!;
    const screenshot = FIELDS.find((f) => f.key === 'screenshot_hotkey')!;
    expect(hotkeySeed(clip, config)).toBe('alt+f10');
    expect(hotkeySeed(screenshot, config)).toBe('alt+f8');
  });

  it('falls back to an empty string when the config has no value yet', () => {
    expect(hotkeySeed(FIELDS.find((f) => f.key === 'clip_hotkey')!, {})).toBe('');
  });
});
