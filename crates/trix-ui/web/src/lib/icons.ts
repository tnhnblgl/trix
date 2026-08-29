/**
 * Icon path data, on a 16x16 grid.
 *
 * Data rather than markup so the set is testable without a DOM -- see the
 * testing rule in the plan header. `Icon.svelte` is the only consumer.
 *
 * Stroked by default; `filled: true` marks the three that are solid shapes
 * (a play triangle and a favourited star read wrong as outlines, and the
 * overflow dots have no outline to draw).
 */
export type IconDef = { d: string; filled?: true };

export type IconName =
  | 'play' | 'pause' | 'clips' | 'settings' | 'star' | 'star-filled'
  | 'pencil' | 'trash' | 'folder' | 'scissors' | 'volume' | 'volume-mute'
  | 'fullscreen' | 'fullscreen-exit' | 'chevron-left' | 'chevron-right'
  | 'chevron-down' | 'dots' | 'check' | 'rearm' | 'minimize' | 'maximize'
  | 'restore' | 'close' | 'alert' | 'camera' | 'copy';

/** Shared by the outline and filled stars, which are one shape drawn twice. */
const STAR = 'M8 2l1.8 3.9 4.2.5-3.1 2.9.8 4.2L8 11.5 4.3 13.5l.8-4.2L2 6.4l4.2-.5z';

export const ICONS: Record<IconName, IconDef> = {
  play: { d: 'M4 2.5l9 5.5-9 5.5z', filled: true },
  pause: { d: 'M5 3v10M11 3v10' },
  // Inset to x 1.5..14.5, not 0..16. A path that reaches the edge of the
  // viewBox has half its 1.35 stroke clipped off, so the frame's left and
  // right sides render visibly thinner than its top and bottom -- and this
  // is the Clips nav icon, on screen at all times.
  clips: { d: 'M3 3h10a1.5 1.5 0 011.5 1.5v7A1.5 1.5 0 0113 13H3a1.5 1.5 0 01-1.5-1.5v-7A1.5 1.5 0 013 3zM6.5 6.2v3.6l3.2-1.8z' },
  // A cog, not a sun. The first draft of this set drew rays and it read as
  // weather rather than settings.
  settings: {
    d: 'M8 5.7a2.3 2.3 0 100 4.6 2.3 2.3 0 000-4.6z'
      + 'M12.9 9.7a1 1 0 00.2 1.1l.1.1a1.15 1.15 0 11-1.6 1.6l-.1-.1a1 1 0 00-1.1-.2 1 1 0 00-.6.9v.2a1.15 1.15 0 11-2.3 0v-.1a1 1 0 00-.7-.9 1 1 0 00-1.1.2l-.1.1a1.15 1.15 0 11-1.6-1.6l.1-.1a1 1 0 00.2-1.1 1 1 0 00-.9-.6h-.2a1.15 1.15 0 110-2.3h.1a1 1 0 00.9-.7 1 1 0 00-.2-1.1l-.1-.1a1.15 1.15 0 111.6-1.6l.1.1a1 1 0 001.1.2h.1a1 1 0 00.6-.9v-.2a1.15 1.15 0 112.3 0v.1a1 1 0 00.6.9 1 1 0 001.1-.2l.1-.1a1.15 1.15 0 111.6 1.6l-.1.1a1 1 0 00-.2 1.1v.1a1 1 0 00.9.6h.2a1.15 1.15 0 110 2.3h-.1a1 1 0 00-.9.6z',
  },
  star: { d: STAR },
  'star-filled': { d: STAR, filled: true },
  pencil: { d: 'M11 2.8l2.2 2.2L6 12.2 3.2 13l.8-2.8z' },
  trash: { d: 'M3 4.5h10M6.5 4.5V3h3v1.5M4.5 4.5l.6 8.2h5.8l.6-8.2' },
  folder: { d: 'M1.8 4.2h4.4l1.2 1.4h6.8v7.2H1.8z' },
  scissors: {
    d: 'M6 4a2 2 0 11-4 0 2 2 0 014 0zM6 12a2 2 0 11-4 0 2 2 0 014 0z'
      + 'M13.3 2.7L5.4 10.6M9.65 9.65L13.3 13.3M5.4 5.4L8 8',
  },
  volume: { d: 'M3 6h2.2L8.5 3.2v9.6L5.2 10H3zM11 6.2a2.6 2.6 0 010 3.6' },
  'volume-mute': { d: 'M3 6h2.2L8.5 3.2v9.6L5.2 10H3zM11 6.5l3 3M14 6.5l-3 3' },
  fullscreen: { d: 'M2.5 5.8V2.5h3.3M13.5 5.8V2.5h-3.3M2.5 10.2v3.3h3.3M13.5 10.2v3.3h-3.3' },
  'fullscreen-exit': { d: 'M5.8 2.5v3.3H2.5M10.2 2.5v3.3h3.3M5.8 13.5v-3.3H2.5M10.2 13.5v-3.3h3.3' },
  'chevron-left': { d: 'M10 3.5L5.5 8l4.5 4.5' },
  'chevron-right': { d: 'M6 3.5L10.5 8 6 12.5' },
  'chevron-down': { d: 'M3.5 6L8 10.5 12.5 6' },
  dots: {
    d: 'M4.35 8a1.15 1.15 0 11-2.3 0 1.15 1.15 0 012.3 0z'
      + 'M9.15 8a1.15 1.15 0 11-2.3 0 1.15 1.15 0 012.3 0z'
      + 'M13.95 8a1.15 1.15 0 11-2.3 0 1.15 1.15 0 012.3 0z',
    filled: true,
  },
  check: { d: 'M3 8.4l3 3 7-7' },
  rearm: { d: 'M13.5 8a5.5 5.5 0 11-1.6-3.9M13.5 2v3h-3' },
  minimize: { d: 'M3 8h10' },
  maximize: { d: 'M3.5 3.5h9v9h-9z' },
  restore: { d: 'M5 5V3.5h7.5V11H11M3.5 5H11v7.5H3.5z' },
  close: { d: 'M3.5 3.5l9 9M12.5 3.5l-9 9' },
  alert: { d: 'M8 2.8l5.7 10H2.3zM8 6.6v3M8 11.4v.6' },
  camera: { d: 'M2 5.5h2.8l1.2-1.5h4l1.2 1.5H14v7H2zM8 6.5a2.75 2.75 0 1 0 0 5.5 2.75 2.75 0 0 0 0-5.5z' },
  copy: { d: 'M5.5 2.5h8v8M2.5 5.5h8v8h-8z' },
};
