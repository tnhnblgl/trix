import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { DaemonEvent } from './types';

/** One command to the daemon. Rejects with the daemon's own error text. */
export async function call<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  return await invoke<T>('trix_call', { cmd, args });
}

export async function startDaemon(): Promise<void> {
  await invoke('start_daemon');
}

/** Whether the supervisor is connected right now, asked rather than awaited. */
export async function daemonConnected(): Promise<boolean> {
  return await invoke<boolean>('daemon_connected');
}

export function onDaemonEvent(handler: (event: DaemonEvent) => void) {
  return listen<DaemonEvent>('trix-event', (e) => handler(e.payload));
}

export function onConnected(handler: (status: unknown) => void) {
  return listen('trix-connected', (e) => handler(e.payload));
}

export function onDisconnected(handler: () => void) {
  return listen('trix-disconnected', () => handler());
}
