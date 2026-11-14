import type { InputDevice, InputLevel } from './types';

export function inputOptions(devices: InputDevice[], selected: string) {
  const options = [
    // The default resolves when capture starts or recovers, not on list refresh.
    { value: '', label: 'Windows default' },
    ...devices.map(d => ({ value: d.id, label: d.name })),
  ];
  if (selected && !devices.some(d => d.id === selected)) {
    options.push({ value: selected, label: 'Selected microphone (unavailable)' });
  }
  return options;
}

/** Keep other pending settings; the recorder knows which input changes applied. */
export function reconcileInputRearm(current: string[], reported: string[]): string[] {
  const isInput = (key: string) => key === 'mic_device' || key === 'mic_volume';
  const next = [...current.filter(key => !isInput(key)), ...reported.filter(isInput)];
  return next.length === current.length && next.every(key => current.includes(key)) ? current : next;
}

type MeterState = {
  connected: boolean;
  armed: boolean;
  volume: number;
  pending: boolean;
  error: string | null;
  level: InputLevel | null;
};

export function inputMeterView(state: MeterState) {
  const idle = (message: string) => ({ percent: 0, reading: '', clipping: false, message });
  if (!state.connected) return idle('Recorder disconnected.');
  if (!state.armed) return idle('Arm Trix to check your microphone.');
  if (state.pending || state.level?.requires_rearm?.length) return idle('Re-arm to apply microphone changes.');
  if (state.volume === 0) return idle('Microphone is off.');
  if (state.error) return idle('Input level unavailable. Refresh to retry.');
  if (!state.level) return idle('Listening...');
  if (!state.level.active) return idle('Microphone unavailable. Check the device and Windows microphone access.');
  const peak = Number.isFinite(state.level.peak) ? Math.max(0, Math.min(1, state.level.peak)) : 0;
  const db = peak > 0 ? 20 * Math.log10(peak) : -Infinity;
  return {
    percent: Math.round(Math.max(0, Math.min(100, (db + 60) / 60 * 100))),
    reading: peak > 0 ? `${Math.round(db)} dB` : 'Silent',
    clipping: peak >= 0.999,
    message: peak >= 0.999 ? 'Too loud. Lower microphone volume.' : peak > 0.001 ? 'Input detected.' : 'No input signal. Try speaking.',
  };
}

/** One request at a time. Disposal ignores late replies and cancels the timer. */
export function pollInputLevel(
  read: () => Promise<InputLevel>,
  update: (level: InputLevel) => void,
  error: (message: string) => void,
): () => void {
  let stopped = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  async function tick() {
    try {
      const level = await read();
      if (stopped) return;
      update(level);
      timer = setTimeout(() => void tick(), 100);
    } catch (e) {
      if (!stopped) error(String(e));
    }
  }
  void tick();
  return () => {
    stopped = true;
    clearTimeout(timer);
  };
}
