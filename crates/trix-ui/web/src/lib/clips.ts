import { convertFileSrc } from '@tauri-apps/api/core';
import type { ClipMeta } from './types';

/** The library is flat (spec §5.1), so every sidecar is the id plus a suffix. */
function clipFile(clipDir: string, id: string, ext: string): string {
  return `${clipDir}\\${id}.${ext}`;
}

/**
 * Playable URL for a clip.
 *
 * `asset:` rather than reading bytes through IPC: the asset protocol answers
 * Range requests, which is what makes seeking in a 20 s 1080p60 MP4 work at
 * all. Sending the file over IPC would mean buffering the whole clip in the
 * webview before the first frame.
 */
export function clipUrl(clipDir: string, id: string): string {
  return convertFileSrc(clipFile(clipDir, id, 'mp4'));
}

export function thumbUrl(clipDir: string, id: string): string {
  return convertFileSrc(clipFile(clipDir, id, 'jpg'));
}

export function formatDuration(ms: number): string {
  const total = Math.round(ms / 1000);
  const minutes = Math.floor(total / 60);
  const seconds = total % 60;
  return `${minutes}:${String(seconds).padStart(2, '0')}`;
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ['KB', 'MB', 'GB'];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  return `${value.toFixed(1)} ${units[unit]}`;
}

/**
 * Folds a `clip_saved` (or a renamed/favorited clip) into the list.
 *
 * Replaces by id rather than always prepending: a reconnect re-runs
 * `library.list` while events are still arriving, and two cards for one file
 * is what that race looks like on screen.
 */
export function mergeSaved(clips: ClipMeta[], saved: ClipMeta): ClipMeta[] {
  const existing = clips.findIndex((c) => c.id === saved.id);
  if (existing >= 0) {
    const copy = clips.slice();
    copy[existing] = saved;
    return copy;
  }
  return [saved, ...clips];
}

export function counterLabel(index: number, total: number): string {
  return total === 0 ? '' : `${index + 1} of ${total}`;
}
