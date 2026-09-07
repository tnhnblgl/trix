export type FieldKind = 'number' | 'text' | 'select' | 'bool' | 'folder' | 'hotkey' | 'slider' | 'sound' | 'tier';

export type Field = {
  key: string;
  label: string;
  kind: FieldKind;
  section: 'Capture' | 'Audio' | 'Quality' | 'Clips' | 'Trix' | 'Updates';
  help: string;
  min?: number;
  max?: number;
  options?: { value: string; label: string }[];
  /**
   * A `tier` field's presets, in dropdown order. Lives on the descriptor
   * rather than in a table `Field.svelte` looks up by `field.key`, for the
   * same reason `liveTest` does: this page has already shipped one Critical
   * from a control keyed on a hard-coded config-key literal, and a second
   * tiered setting would repeat it.
   */
  tiers?: { value: number; label: string }[];
  /** Filled at runtime from monitors.list / encoders.list. */
  dynamic?: 'monitors';
  /**
   * Whether a press of this hotkey reaches the daemon as `hotkey_pressed`,
   * which is what `Field.svelte`'s "Test" button and its hint are answering.
   * Only `clip_hotkey`'s `WM_HOTKEY` arm broadcasts that event -- the
   * screenshot arm deliberately stays silent (`window.rs`, the arm for
   * `SHOT_HOTKEY_ID`) so a screenshot press can never make the clip row's
   * "press it now" check look answered.
   *
   * A flag on the descriptor rather than a `field.key === 'clip_hotkey'`
   * string check in the component: this branch already shipped one Critical
   * from a hard-coded `clip_hotkey` overwriting the wrong setting, and tying
   * a UI affordance to a literal key name is the same shape of mistake
   * waiting for a third hotkey row. `true` is the only value this ever takes,
   * so a missing flag and an absent one both mean "not live-testable" with
   * nothing to get out of sync.
   */
  liveTest?: true;
  /**
   * Which `status` field says whether this hotkey is actually listening.
   *
   * On the descriptor for the same reason `liveTest` is, and not derived by
   * building `${key}_bound` at the call site: a name assembled from a string
   * is a name TypeScript cannot check, and the day the daemon publishes a key
   * that does not follow the pattern the warning would just quietly stop
   * appearing. A row that declares nothing here shows no warning, which is
   * the right default for a row nobody has wired up.
   */
  boundKey?: 'clip_hotkey_bound' | 'screenshot_hotkey_bound';
};

/**
 * Bounds mirrored from `state.rs`'s NUMERIC_BOUNDS.
 *
 * Duplicated deliberately, and it is not a DRY violation to fix: the daemon
 * must keep validating for third-party clients that never see this file, and
 * this copy exists only so a typo is caught before a round trip. The daemon
 * remains the authority -- if the two ever disagree, its answer is the one the
 * user sees.
 */
const BOUNDS: Record<string, [number, number]> = {
  fps: [1, 480],
  bitrate_kbps: [1, 200000],
  max_bitrate_kbps: [0, 200000],
  replay_seconds: [1, 600],
  monitor_index: [0, 63],
  stats_seconds: [0, 86400],
  max_library_gb: [0, 10000],
  system_volume: [0, 100],
  mic_volume: [0, 100],
};

/**
 * The value `tierFor` reports when a saved number matches no preset, and the
 * option the dropdown shows for it. Not a number, so it can never collide
 * with a real bitrate.
 */
export const CUSTOM_TIER = 'custom';

/**
 * Target-bitrate presets, in kbit/s.
 *
 * Not stored anywhere: `bitrate_kbps` remains the plain number the daemon and
 * `config.toml` have always held, and the tier is derived back out of it by
 * `tierFor`. A saved tier would be a second source of truth for one value,
 * and the two would disagree the first time somebody edited config.toml by
 * hand.
 *
 * The steps are roughly 1.5x apart, and 8000 -- the shipped default, and the
 * bitrate every published Trix measurement was taken at -- is deliberately a
 * preset rather than a boundary, so an existing install shows `Medium` with
 * nothing migrated.
 */
export const BITRATE_TIERS: { value: number; label: string }[] = [
  { value: 5000, label: 'Low - 5000 kbps' },
  { value: 8000, label: 'Medium - 8000 kbps' },
  { value: 14000, label: 'High - 14000 kbps' },
  { value: 20000, label: 'Extra high - 20000 kbps' },
];

/**
 * Which dropdown entry a saved value shows as: the matching preset, or
 * `CUSTOM_TIER` for anything else -- including a number typed into an older
 * Trix, which must round-trip back into the box rather than being rounded
 * onto the nearest preset behind the user's back.
 */
export function tierFor(field: Field, value: unknown): string {
  const n = Number(value);
  return (field.tiers ?? []).some((t) => t.value === n) ? String(n) : CUSTOM_TIER;
}

/** The dropdown's options: the presets, then Custom. */
export function tierOptions(field: Field): { value: string; label: string }[] {
  return [
    ...(field.tiers ?? []).map((t) => ({ value: String(t.value), label: t.label })),
    { value: CUSTOM_TIER, label: 'Custom...' },
  ];
}

export const FIELDS: Field[] = [
  { key: 'monitor_index', label: 'Monitor', kind: 'select', section: 'Capture', dynamic: 'monitors', help: 'Which screen is captured.' },
  { key: 'fps', label: 'Frame rate', kind: 'number', section: 'Capture', ...span('fps'), help: 'Capture and encode rate.' },
  { key: 'replay_seconds', label: 'Replay buffer', kind: 'number', section: 'Capture', ...span('replay_seconds'), help: 'Seconds kept in RAM. Memory cost scales with this times the bitrate.' },
  { key: 'gpu_priority', label: 'GPU priority', kind: 'select', section: 'Capture', options: [
      { value: 'low', label: 'Low - never cost game fps' },
      { value: 'normal', label: 'Normal - smoother capture' },
    ], help: 'Low drops capture frames under contention instead of taking frames from the game.' },
  { key: 'capture_method', label: 'Capture method', kind: 'select', section: 'Capture', options: [
      { value: 'auto', label: 'Automatic' },
      { value: 'wgc', label: 'Windows Graphics Capture' },
      { value: 'dd', label: 'Desktop Duplication - no mouse cursor' },
    ], help: 'If Windows draws a yellow border around your screen while Trix is armed, choose Desktop Duplication -- it is not subject to that border. It cannot record the mouse cursor, in clips or in screenshots. Automatic is Windows Graphics Capture.' },

  { key: 'system_volume', label: 'PC sound', kind: 'slider', section: 'Audio', ...span('system_volume'), help: 'How loud your PC\'s own sound is in the clip. Affects the recording only, never your Windows volume. 0 turns it off.' },
  { key: 'mic_volume', label: 'Microphone', kind: 'slider', section: 'Audio', ...span('mic_volume'), help: 'How loud your voice is in the clip. 0 closes the microphone entirely, so Windows stops showing Trix as using it.' },

  { key: 'bitrate_kbps', label: 'Target bitrate', kind: 'tier', section: 'Quality', ...span('bitrate_kbps'), tiers: BITRATE_TIERS, help: 'How much data a second of video gets. Higher tiers hold up better in fast motion and cost more disk per clip -- and more RAM, because the replay buffer holds this many seconds of it. Custom takes any number.' },
  { key: 'max_bitrate_kbps', label: 'Peak bitrate', kind: 'number', section: 'Quality', ...span('max_bitrate_kbps'), help: '0 means 1.5x the target. This cap is also the replay buffer\'s worst-case RAM.' },
  { key: 'rate_control', label: 'Rate control', kind: 'select', section: 'Quality', options: [
      { value: 'vbr', label: 'VBR - quality-leaning' },
      { value: 'cbr', label: 'CBR - predictable size' },
    ], help: 'How the encoder spends its bitrate.' },

  { key: 'clip_dir', label: 'Clips folder', kind: 'folder', section: 'Clips', help: 'Where clips are saved. Empty means Videos\\Trix.' },
  { key: 'max_library_gb', label: 'Library limit', kind: 'number', section: 'Clips', ...span('max_library_gb'), help: 'GB. When exceeded the oldest non-favorite clips are deleted. 0 turns the limit off.' },

  { key: 'clip_hotkey', label: 'Clip hotkey', kind: 'hotkey', section: 'Trix', liveTest: true, boundKey: 'clip_hotkey_bound', help: 'Press the combination to test it. If the test does nothing, or if it works here but your game ignores the key, try Hotkey detection below -- overlays and some games take the key before Trix ever sees it.' },
  { key: 'screenshot_hotkey', label: 'Screenshot hotkey', kind: 'hotkey', section: 'Trix', boundKey: 'screenshot_hotkey_bound', help: 'Saves a picture of the screen and copies it to your clipboard. Only works while Trix is armed, because the picture comes from the recording that is already running.' },
  { key: 'hotkey_mode', label: 'Hotkey detection', kind: 'select', section: 'Trix', options: [
      { value: 'standard', label: 'Standard' },
      { value: 'low_level', label: 'Low level - sees keys other programs have taken' },
    ], help: 'Applies to both hotkeys above. If your hotkey does nothing in a game, this is the setting to try. Standard asks Windows to reserve the combination: that is refused outright when another program already owns it, and even when it succeeds some games and overlays take the key before Windows hands it over -- confirmed in Euro Truck Simulator 2, where Trix held the key and never saw a press. Low level watches the keyboard directly, so it sees the key either way, and passes the key on so whatever else uses it keeps working. Some anti-cheat software is wary of programs that watch the keyboard; OBS, Discord and Steam all do it, but Trix cannot promise how yours reads it. Neither mode helps if the game runs as administrator and Trix does not.' },
  { key: 'screenshot_sound', label: 'Screenshot sound', kind: 'bool', section: 'Trix', help: 'A short blip when a screenshot is saved, different from the clip sound so you can tell them apart without looking.' },
  { key: 'clip_sound', label: 'Clip sound', kind: 'bool', section: 'Trix', help: 'Plays a sound when a clip is saved, even when the Trix window is closed.' },
  { key: 'clip_sound_path', label: 'Sound file', kind: 'sound', section: 'Trix', help: 'Your own sound, or Trix\'s built-in one. mp3, wav, m4a and anything else Windows can play. Only the first 10 seconds are used.' },
  { key: 'autostart', label: 'Start with Windows', kind: 'bool', section: 'Trix', help: 'Off by default. Writes the registry Run entry, which is the source of truth.' },
  { key: 'discord_presence', label: 'Discord presence', kind: 'bool', section: 'Trix', help: 'Shows "Clipping with Trix" on your Discord profile, with a button your friends can use to get it, for as long as Trix is running. Talks only to the Discord app on this PC and sends nothing about you.' },
  { key: 'stats_seconds', label: 'Stats interval', kind: 'number', section: 'Trix', ...span('stats_seconds'), help: 'Seconds between performance reports. 0 turns them off.' },

  { key: 'check_for_updates', label: 'Check for updates', kind: 'bool', section: 'Updates', help: 'Asks github.com once per launch whether a newer Trix exists. Sends nothing about you. Updates are never installed without you clicking.' },
];

/**
 * The sections `Settings.svelte` renders, in display order, and the single
 * source of truth for it: the page iterates this constant instead of its own
 * literal, so a `FIELDS` entry naming a section absent here is invisible
 * rather than silently unrendered. `Updates` sits last because it is an
 * app-level concern, below the capture and clip settings above it.
 */
export const SECTIONS: Field['section'][] = ['Capture', 'Audio', 'Quality', 'Clips', 'Trix', 'Updates'];

function span(key: string): { min: number; max: number } {
  const [min, max] = BOUNDS[key];
  return { min, max };
}

/**
 * The read-only extras `config.get` adds on top of the real config keys
 * (`state.rs`'s `config_json`): `clip_dir_resolved`, the absolute directory an
 * empty `clip_dir` actually resolves to, and `config_file_exists`, spec §7.4's
 * "no file means first run" flag. Neither is a config key, neither is
 * settable, and neither belongs in config.toml -- so `unknownKeys` must not
 * flag them, or every user sees a false "this daemon has settings this app
 * does not render yet" banner on every visit (they did, once: this exact pair
 * is why this comment names both by name instead of trusting a future reader
 * to rediscover it from `state.rs`).
 *
 * Exported, not inlined into `unknownKeys`, so `settings.test.ts` can build
 * its fixture from this list instead of a hand-typed copy of it -- the two
 * cannot drift apart from each other, even though neither can see a key the
 * daemon grows that nobody adds here too.
 */
export const READ_ONLY_EXTRAS = ['clip_dir_resolved', 'config_file_exists'];

/** Keys `config.get` returned that this page has no field for. */
export function unknownKeys(config: Record<string, unknown>): string[] {
  const rendered = new Set([...FIELDS.map((f) => f.key), ...READ_ONLY_EXTRAS]);
  return Object.keys(config).filter((key) => !rendered.has(key));
}

/** The daemon's own bound, checked early. `null` means acceptable. */
export function validate(key: string, value: unknown): string | null {
  const bound = BOUNDS[key];
  if (!bound || typeof value !== 'number') return null;
  const [min, max] = bound;
  return value < min || value > max ? `${key} accepts ${min} to ${max}` : null;
}

/**
 * What a `hotkey` field's Save button commits: always the field's own key,
 * paired with the combination just captured.
 *
 * Pulled out of `Field.svelte` and into this plain module so it can be
 * pinned by a test -- the web suite has no DOM (see the rest of
 * `lib/*.test.ts`), so a `<button onclick>` inside a Svelte component can
 * never be driven directly from a test, but a function it calls can be. This
 * is exactly the seam that was missing when the Screenshot row's Save button
 * shipped calling `set('clip_hotkey', capture)` -- a string typed once for
 * the only hotkey there was, and never revisited when a second one arrived.
 */
export function hotkeySaveTarget(field: Field, capture: string): { key: string; value: string } {
  return { key: field.key, value: capture };
}

/**
 * Why this hotkey row is not working, or null when it is -- or when nobody
 * knows yet.
 *
 * `bound` is the daemon's tri-state (`clip_hotkey_bound` /
 * `screenshot_hotkey_bound` on `status`): true bound, false refused, and
 * null/undefined for "the pump has not tried yet", which every client sees
 * for a moment at startup and forever in a build with no window. Only an
 * explicit `false` is a problem -- warning on the unknown would put "your
 * hotkey is taken" on screen at every launch, which is the version of this
 * warning nobody reads by the third time.
 *
 * The advice depends on which mechanism failed, because the two fail for
 * opposite reasons. Standard mode is refused when another program already
 * owns the combination, and the answer is a different key or the low-level
 * mode that does not need to own anything. Low-level mode is refused when
 * Windows would not install the hook at all, and the answer is the other way
 * round.
 *
 * Pure and exported for the same reason `hotkeySaveTarget` is: the web suite
 * runs on node with no DOM, so the sentence a user reads can only be pinned
 * by a test if it is produced by a function rather than by markup.
 */
export function hotkeyProblem(
  field: Field,
  bound: boolean | null | undefined,
  config: Record<string, unknown>,
): string | null {
  if (field.kind !== 'hotkey' || bound !== false) return null;
  const combination = String(config[field.key] ?? '').toUpperCase() || 'That combination';
  return String(config['hotkey_mode'] ?? 'standard') === 'low_level'
    ? `Windows would not let Trix watch the keyboard for ${combination}. Set Hotkey detection back to Standard, or pick a different combination.`
    : `${combination} is already taken by another program, so Trix never sees it. Pick a different combination, or set Hotkey detection to Low level to hear it anyway.`;
}

/**
 * What a `hotkey` field's keycap recorder seeds `capture` with when the user
 * clicks in to arm it -- the field's own current value, not another field's.
 * Companion to `hotkeySaveTarget` above, extracted for the same reason: this
 * is the read-side half of the same bug (`Field.svelte` used to seed every
 * hotkey row from `config['clip_hotkey']`).
 */
export function hotkeySeed(field: Field, config: Record<string, unknown>): string {
  return String(config[field.key] ?? '');
}
