/**
 * The clip player's volume rules, pulled out of `VideoPlayer.svelte` so they
 * can be tested without a DOM.
 *
 * A level is a whole percent, 0..100 -- what the slider shows. The element's
 * own `volume` is that divided by 100. There is no boost above 100 here: an
 * HTML media element stops at 1.0, and the recording-side 0..200 levels in
 * Settings are a different thing that has already been applied to the file.
 */
import type { IconName } from './icons';
import { clamp, stepBy } from './ui';

/** What one arrow press or one wheel notch moves the level by. */
export const VOLUME_STEP = 5;

/**
 * Where the level is remembered between launches. Per machine and per
 * webview profile, which is the right scope for how loud playback is on the
 * speakers in front of you -- not a setting worth a round trip to the daemon.
 */
export const VOLUME_STORAGE_KEY = 'trix.player.volume';

/** The speaker glyph for a level: crossed out when silent, one wave when quiet. */
export function volumeIcon(level: number, muted: boolean): IconName {
  if (muted || level <= 0) return 'volume-mute';
  return level < 50 ? 'volume-low' : 'volume';
}

/**
 * A stored level read back, or full volume when there is nothing usable.
 *
 * The blank check comes before `Number()`, which reads `''` as 0: a cleared
 * or corrupted entry would otherwise open every clip silent.
 */
export function parseStoredVolume(raw: string | null): number {
  if (raw === null || raw.trim() === '') return 100;
  const n = Number(raw);
  return Number.isFinite(n) ? clamp(Math.round(n), 0, 100) : 100;
}

/** The level after one step up (1) or down (-1), on the step grid. */
export function nudgeVolume(level: number, direction: 1 | -1): number {
  return stepBy(level, direction, 0, 100, VOLUME_STEP);
}

/**
 * What unmuting returns to.
 *
 * Muting keeps the level where it was, so unmuting normally just restores it.
 * A level of 0 is the exception: "unmute" there would change nothing anyone
 * can hear, so it comes back at the last level that could be heard.
 */
export function unmutedLevel(level: number, lastAudible: number): number {
  return level > 0 ? level : lastAudible > 0 ? lastAudible : 100;
}
