// Mirrors trix-proto. Reimplemented rather than generated on purpose: spec §3.2
// says a UI in any language reimplements these and is in no way second-class,
// and a code generator here would be a privilege our own UI has and nobody
// else's does.

export type ClipMeta = {
  id: string;
  title: string;
  created: string;
  duration_ms: number;
  bytes: number;
  width: number;
  height: number;
  fps: number;
  encoder: string;
  has_audio: boolean;
  favorite: boolean;
};

export type Status = {
  armed: boolean;
  encoder: string | null;
  monitor_index: number;
  ring_seconds_used: number;
  ring_seconds_total: number;
  version: string;
  clip_dir: string;
};

export type Monitor = {
  index: number;
  name: string;
  width: number;
  height: number;
  left: number;
  top: number;
  adapter: string;
};

export type Encoder = { name: string; codec: string; hardware: boolean };

export type DaemonEvent = { event: string; data: Record<string, unknown> };
