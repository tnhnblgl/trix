export type FieldKind = 'number' | 'text' | 'select' | 'bool' | 'folder' | 'hotkey' | 'slider';

export type Field = {
  key: string;
  label: string;
  kind: FieldKind;
  section: 'Capture' | 'Audio' | 'Quality' | 'Clips' | 'Trix' | 'Updates';
  help: string;
  min?: number;
  max?: number;
  options?: { value: string; label: string }[];
  /** Filled at runtime from monitors.list / encoders.list. */
  dynamic?: 'monitors';
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

export const FIELDS: Field[] = [
  { key: 'monitor_index', label: 'Monitor', kind: 'select', section: 'Capture', dynamic: 'monitors', help: 'Which screen is captured.' },
  { key: 'fps', label: 'Frame rate', kind: 'number', section: 'Capture', ...span('fps'), help: 'Capture and encode rate.' },
  { key: 'replay_seconds', label: 'Replay buffer', kind: 'number', section: 'Capture', ...span('replay_seconds'), help: 'Seconds kept in RAM. Memory cost scales with this times the bitrate.' },
  { key: 'gpu_priority', label: 'GPU priority', kind: 'select', section: 'Capture', options: [
      { value: 'low', label: 'Low - never cost game fps' },
      { value: 'normal', label: 'Normal - smoother capture' },
    ], help: 'Low drops capture frames under contention instead of taking frames from the game.' },

  { key: 'system_volume', label: 'PC sound', kind: 'slider', section: 'Audio', ...span('system_volume'), help: 'How loud your PC\'s own sound is in the clip. Affects the recording only, never your Windows volume. 0 turns it off.' },
  { key: 'mic_volume', label: 'Microphone', kind: 'slider', section: 'Audio', ...span('mic_volume'), help: 'How loud your voice is in the clip. 0 closes the microphone entirely, so Windows stops showing Trix as using it.' },

  { key: 'bitrate_kbps', label: 'Bitrate', kind: 'number', section: 'Quality', ...span('bitrate_kbps'), help: 'Target average, in kbit/s.' },
  { key: 'max_bitrate_kbps', label: 'Peak bitrate', kind: 'number', section: 'Quality', ...span('max_bitrate_kbps'), help: '0 means 1.5x the target. This cap is also the replay buffer\'s worst-case RAM.' },
  { key: 'rate_control', label: 'Rate control', kind: 'select', section: 'Quality', options: [
      { value: 'vbr', label: 'VBR - quality-leaning' },
      { value: 'cbr', label: 'CBR - predictable size' },
    ], help: 'How the encoder spends its bitrate.' },

  { key: 'clip_dir', label: 'Clips folder', kind: 'folder', section: 'Clips', help: 'Where clips are saved. Empty means Videos\\Trix.' },
  { key: 'max_library_gb', label: 'Library limit', kind: 'number', section: 'Clips', ...span('max_library_gb'), help: 'GB. When exceeded the oldest non-favorite clips are deleted. 0 turns the limit off.' },

  { key: 'clip_hotkey', label: 'Clip hotkey', kind: 'hotkey', section: 'Trix', help: 'Press the combination to test it. Overlays can silently take a hotkey inside games.' },
  { key: 'autostart', label: 'Start with Windows', kind: 'bool', section: 'Trix', help: 'Off by default. Writes the registry Run entry, which is the source of truth.' },
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
