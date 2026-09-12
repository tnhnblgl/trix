import { describe, expect, it } from 'vitest';
import { nudgeVolume, parseStoredVolume, unmutedLevel, volumeIcon } from './volume';

describe('volumeIcon', () => {
  it('crosses the speaker out when muted, whatever the level', () => {
    expect(volumeIcon(80, true)).toBe('volume-mute');
  });

  it('shows a level of zero as muted, since that is what it sounds like', () => {
    expect(volumeIcon(0, false)).toBe('volume-mute');
  });

  it('switches from one wave to two at half volume', () => {
    expect(volumeIcon(1, false)).toBe('volume-low');
    expect(volumeIcon(49, false)).toBe('volume-low');
    expect(volumeIcon(50, false)).toBe('volume');
    expect(volumeIcon(100, false)).toBe('volume');
  });
});

describe('parseStoredVolume', () => {
  it('opens at full volume when nothing was ever stored', () => {
    expect(parseStoredVolume(null)).toBe(100);
  });

  it('reads a stored level back', () => {
    expect(parseStoredVolume('35')).toBe(35);
  });

  it('does not read a blank entry as silence', () => {
    // `Number('')` is 0. Every clip would open muted for no visible reason.
    expect(parseStoredVolume('')).toBe(100);
    expect(parseStoredVolume('   ')).toBe(100);
  });

  it('falls back to full volume for anything that is not a number', () => {
    expect(parseStoredVolume('loud')).toBe(100);
    expect(parseStoredVolume('NaN')).toBe(100);
  });

  it('pins an out-of-range entry into 0..100 as a whole percent', () => {
    expect(parseStoredVolume('250')).toBe(100);
    expect(parseStoredVolume('-5')).toBe(0);
    expect(parseStoredVolume('42.6')).toBe(43);
  });
});

describe('nudgeVolume', () => {
  it('moves one step each way', () => {
    expect(nudgeVolume(50, 1)).toBe(55);
    expect(nudgeVolume(50, -1)).toBe(45);
  });

  it('lands on the step grid from a level a drag left between steps', () => {
    expect(nudgeVolume(37, 1)).toBe(40);
    expect(nudgeVolume(37, -1)).toBe(35);
  });

  it('stops at both ends', () => {
    expect(nudgeVolume(100, 1)).toBe(100);
    expect(nudgeVolume(0, -1)).toBe(0);
  });
});

describe('unmutedLevel', () => {
  it('restores the level muting left alone', () => {
    expect(unmutedLevel(60, 60)).toBe(60);
  });

  it('comes back audible when the level had been dragged to zero', () => {
    expect(unmutedLevel(0, 40)).toBe(40);
  });

  it('comes back at full volume when nothing was ever audible', () => {
    expect(unmutedLevel(0, 0)).toBe(100);
  });
});
