import { call, onConnected, onDaemonEvent, onDisconnected } from './ipc';
import type { ClipMeta, Status } from './types';

export type View = 'grid' | 'clip' | 'settings' | 'firstrun';
export type Toast = { id: number; kind: 'error' | 'info'; text: string };

let nextToastId = 1;

class AppState {
  connected = $state(false);
  status = $state<Status | null>(null);
  view = $state<View>('grid');
  clips = $state<ClipMeta[]>([]);
  total = $state(0);
  /** Index into `clips` of the clip the clip page is showing. */
  selected = $state(0);
  toasts = $state<Toast[]>([]);
  /** Live ring seconds while armed, from `stats`; falls back to `status`. */
  ringUsed = $state(0);

  get armed() {
    return this.status?.armed ?? false;
  }

  get clipDir() {
    return this.status?.clip_dir ?? '';
  }

  toast(kind: Toast['kind'], text: string) {
    const toast = { id: nextToastId++, kind, text };
    this.toasts = [...this.toasts, toast];
    setTimeout(() => {
      this.toasts = this.toasts.filter((t) => t.id !== toast.id);
    }, 6000);
  }

  async refreshStatus() {
    try {
      this.status = await call<Status>('status');
      this.ringUsed = this.status.ring_seconds_used;
    } catch (e) {
      this.toast('error', String(e));
    }
  }

  async toggleArm() {
    try {
      const next = await call<Status>(this.armed ? 'disarm' : 'arm');
      // `disarm` answers {} rather than a status, so re-read rather than
      // trusting the shape of the reply.
      this.status = 'armed' in next ? next : await call<Status>('status');
    } catch (e) {
      this.toast('error', String(e));
    }
  }
}

export const app = new AppState();

/** Subscribes the store to the daemon. Call once, from App.svelte. */
export function wireDaemon() {
  onConnected(async () => {
    app.connected = true;
    await app.refreshStatus();
    // Stats drive the ring meter; per spec §4.4 the daemon measures nothing
    // until a client asks, so nobody pays for this while no UI is open.
    try {
      await call('stats.subscribe', { enabled: true });
    } catch {
      // A daemon that will not subscribe is still a usable daemon; the meter
      // just falls back to the value `status` reported.
    }
  });

  onDisconnected(() => {
    app.connected = false;
    app.status = null;
  });

  onDaemonEvent((event) => {
    const data = event.data as Record<string, never>;
    switch (event.event) {
      case 'armed':
      case 'disarmed':
        void app.refreshStatus();
        break;
      case 'stats':
        app.ringUsed = Number(data['ring_seconds_used'] ?? 0);
        break;
      case 'error':
        app.toast('error', String(data['error'] ?? 'the daemon reported an error'));
        break;
    }
  });
}
