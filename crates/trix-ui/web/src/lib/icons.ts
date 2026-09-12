/**
 * Icon path data, on Lucide's 24x24 grid.
 *
 * The shapes are Lucide's (https://lucide.dev), copied in rather than taken
 * as a package so the app gains no dependency. They are ISC licensed; the
 * notice is `THIRD-PARTY-NOTICES.txt` at the repository root, which
 * `scripts/ship-zip.ps1` puts in the release zip. Each icon's elements
 * (paths, circles, rects, lines) are flattened into one path string, which is
 * why a circle reads as two arcs here.
 *
 * Data rather than markup so the set is testable without a DOM -- see the
 * testing rule in the plan header. `Icon.svelte` is the only consumer, and it
 * supplies Lucide's own stroke: 2 units, round caps and joins.
 *
 * `filled: true` fills the shape as well as stroking it: a play triangle and
 * a favourited star read wrong as outlines, and keeping the stroke keeps
 * their rounded corners and their size identical to the outlined versions.
 */
export type IconDef = { d: string; filled?: true };

export type IconName =
  | 'play' | 'pause' | 'clips' | 'settings' | 'star' | 'star-filled'
  | 'pencil' | 'trash' | 'folder' | 'scissors' | 'volume' | 'volume-mute'
  | 'fullscreen' | 'fullscreen-exit' | 'chevron-left' | 'chevron-right'
  | 'chevron-down' | 'dots' | 'check' | 'rearm' | 'minimize' | 'maximize'
  | 'restore' | 'close' | 'alert' | 'camera' | 'copy';

const STAR = 'M11.525 2.295a.53.53 0 0 1 .95 0l2.31 4.679a2.123 2.123 0 0 0 1.595 1.16l5.166.756a.53.53 0 0 1 .294.904l-3.736 3.638a2.123 2.123 0 0 0-.611 1.878l.882 5.14a.53.53 0 0 1-.771.56l-4.618-2.428a2.122 2.122 0 0 0-1.973 0L6.396 21.01a.53.53 0 0 1-.77-.56l.881-5.139a2.122 2.122 0 0 0-.611-1.879L2.16 9.795a.53.53 0 0 1 .294-.906l5.165-.755a2.122 2.122 0 0 0 1.597-1.16z';

export const ICONS: Record<IconName, IconDef> = {
  play: { d: 'M5 5a2 2 0 0 1 3.008-1.728l11.997 6.998a2 2 0 0 1 .003 3.458l-12 7A2 2 0 0 1 5 19z', filled: true },
  pause: { d: 'M15 3h3a1 1 0 0 1 1 1v16a1 1 0 0 1 -1 1h-3a1 1 0 0 1 -1 -1v-16a1 1 0 0 1 1 -1zM6 3h3a1 1 0 0 1 1 1v16a1 1 0 0 1 -1 1h-3a1 1 0 0 1 -1 -1v-16a1 1 0 0 1 1 -1z' },
  clips: { d: 'M5 3h14a2 2 0 0 1 2 2v14a2 2 0 0 1 -2 2h-14a2 2 0 0 1 -2 -2v-14a2 2 0 0 1 2 -2zM9 9.003a1 1 0 0 1 1.517-.859l4.997 2.997a1 1 0 0 1 0 1.718l-4.997 2.997A1 1 0 0 1 9 14.996z' },
  settings: { d: 'M9.671 4.136a2.34 2.34 0 0 1 4.659 0 2.34 2.34 0 0 0 3.319 1.915 2.34 2.34 0 0 1 2.33 4.033 2.34 2.34 0 0 0 0 3.831 2.34 2.34 0 0 1-2.33 4.033 2.34 2.34 0 0 0-3.319 1.915 2.34 2.34 0 0 1-4.659 0 2.34 2.34 0 0 0-3.32-1.915 2.34 2.34 0 0 1-2.33-4.033 2.34 2.34 0 0 0 0-3.831A2.34 2.34 0 0 1 6.35 6.051a2.34 2.34 0 0 0 3.319-1.915M9 12a3 3 0 1 0 6 0a3 3 0 1 0 -6 0' },
  // One shape drawn twice: outlined, and filled for a favourited clip.
  star: { d: STAR },
  'star-filled': { d: STAR, filled: true },
  pencil: { d: 'M21.174 6.812a1 1 0 0 0-3.986-3.987L3.842 16.174a2 2 0 0 0-.5.83l-1.321 4.352a.5.5 0 0 0 .623.622l4.353-1.32a2 2 0 0 0 .83-.497zM15 5l4 4' },
  trash: { d: 'M10 11v6M14 11v6M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6M3 6h18M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2' },
  folder: { d: 'M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z' },
  scissors: { d: 'M3 6a3 3 0 1 0 6 0a3 3 0 1 0 -6 0M8.12 8.12 12 12M20 4 8.12 15.88M3 18a3 3 0 1 0 6 0a3 3 0 1 0 -6 0M14.8 14.8 20 20' },
  volume: { d: 'M11 4.702a.705.705 0 0 0-1.203-.498L6.413 7.587A1.4 1.4 0 0 1 5.416 8H3a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h2.416a1.4 1.4 0 0 1 .997.413l3.383 3.384A.705.705 0 0 0 11 19.298zM16 9a5 5 0 0 1 0 6M19.364 18.364a9 9 0 0 0 0-12.728' },
  'volume-mute': { d: 'M11 4.702a.7.7 0 0 0-1.203-.498L6.413 7.587A1.4 1.4 0 0 1 5.416 8H3a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h2.416a1.4 1.4 0 0 1 .997.413l3.383 3.384A.7.7 0 0 0 11 19.298zM16.5 14.5l5-5M16.5 9.5l5 5' },
  fullscreen: { d: 'M8 3H5a2 2 0 0 0-2 2v3M21 8V5a2 2 0 0 0-2-2h-3M3 16v3a2 2 0 0 0 2 2h3M16 21h3a2 2 0 0 0 2-2v-3' },
  'fullscreen-exit': { d: 'M8 3v3a2 2 0 0 1-2 2H3M21 8h-3a2 2 0 0 1-2-2V3M3 16h3a2 2 0 0 1 2 2v3M16 21v-3a2 2 0 0 1 2-2h3' },
  'chevron-left': { d: 'M15 18l-6-6 6-6' },
  'chevron-right': { d: 'M9 18l6-6-6-6' },
  'chevron-down': { d: 'M6 9l6 6 6-6' },
  dots: { d: 'M11 12a1 1 0 1 0 2 0a1 1 0 1 0 -2 0M18 12a1 1 0 1 0 2 0a1 1 0 1 0 -2 0M4 12a1 1 0 1 0 2 0a1 1 0 1 0 -2 0' },
  check: { d: 'M20 6 9 17l-5-5' },
  rearm: { d: 'M21 12a9 9 0 1 1-9-9c2.52 0 4.93 1 6.74 2.74L21 8M21 3v5h-5' },
  // The window's own caption buttons keep the shapes Windows draws for them,
  // not Lucide's: that is the one row where people expect exactly those.
  minimize: { d: 'M4.5 12h15' },
  maximize: { d: 'M5.25 5.25h13.5v13.5H5.25z' },
  restore: { d: 'M7.5 7.5V5.25h11.25V16.5H16.5M5.25 7.5H16.5v11.25H5.25z' },
  close: { d: 'M5.25 5.25l13.5 13.5M18.75 5.25l-13.5 13.5' },
  alert: { d: 'M21.73 18l-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3M12 9v4M12 17h.01' },
  camera: { d: 'M13.997 4a2 2 0 0 1 1.76 1.05l.486.9A2 2 0 0 0 18.003 7H20a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V9a2 2 0 0 1 2-2h1.997a2 2 0 0 0 1.759-1.048l.489-.904A2 2 0 0 1 10.004 4zM9 13a3 3 0 1 0 6 0a3 3 0 1 0 -6 0' },
  copy: { d: 'M10 8h10a2 2 0 0 1 2 2v10a2 2 0 0 1 -2 2h-10a2 2 0 0 1 -2 -2v-10a2 2 0 0 1 2 -2zM4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2' },
};
