import { afterEach, describe, expect, it, vi } from 'vitest';
import { inputOptions, inputMeterView, pollInputLevel, reconcileInputRearm } from './audio-input';
import type { InputDevice, InputLevel } from './types';

const devices: InputDevice[] = [
  { id: 'one', name: 'USB Microphone', is_default: true },
  { id: 'two', name: 'USB Microphone', is_default: false },
];
const live = { connected: true, armed: true, volume: 100, pending: false, error: null, level: { active: true, peak: 0.5 } };

describe('input device choices', () => {
  it('uses endpoint IDs even when names match and includes the Windows default', () => {
    const options = inputOptions(devices, 'two');
    expect(options.map(o => o.value)).toEqual(['', 'one', 'two']);
    expect(options[0].label).toContain('Windows default');
    expect(options[0].label).toBe('Windows default');
  });

  it('keeps a disconnected saved device visible without selecting another', () => {
    const options = inputOptions([], 'missing-id');
    expect(options.find(o => o.value === 'missing-id')?.label).toContain('unavailable');
    expect(inputOptions([], '')).toEqual([{ value: '', label: 'Windows default' }]);
  });
});

describe('captured input meter', () => {
  it('clears microphone prompts after manual re-arm or reverting a choice, preserving other settings', () => {
    expect(reconcileInputRearm(['fps', 'mic_device', 'mic_volume'], [])).toEqual(['fps']);
    expect(reconcileInputRearm(['fps'], ['mic_device'])).toEqual(['fps', 'mic_device']);
    const existing = ['fps', 'mic_device'];
    expect(reconcileInputRearm(existing, ['mic_device'])).toBe(existing);
  });
  it('uses a readable dB scale and identifies clipping', () => {
    expect(inputMeterView(live).percent).toBe(90);
    expect(inputMeterView(live).reading).toBe('-6 dB');
    expect(inputMeterView({ ...live, level: { active: true, peak: 1 } }).clipping).toBe(true);
  });

  it('never shows a live reading for off, idle, disconnected, pending or unavailable input', () => {
    for (const override of [
      { volume: 0 }, { armed: false }, { connected: false }, { pending: true },
      { level: { active: false, peak: 1 } }, { error: 'connection lost' }, { level: null },
    ]) expect(inputMeterView({ ...live, ...override }).percent).toBe(0);
    expect(inputMeterView({ ...live, pending: true, volume: 0 }).message).toContain('Re-arm');
    expect(inputMeterView({ ...live, armed: false }).message).toContain('Arm Trix');
  });

  it('distinguishes silence from unavailable capture and rejects non-finite readings', () => {
    expect(inputMeterView({ ...live, level: { active: true, peak: 0 } }).message).toContain('No input signal');
    expect(inputMeterView({ ...live, level: { active: false, peak: 0 } }).message).toContain('unavailable');
    expect(inputMeterView({ ...live, level: { active: true, peak: NaN } }).percent).toBe(0);
  });

  it('does not label the old device level as the new choice after reopening the app', () => {
    const view = inputMeterView({ ...live, level: { active: true, peak: 1, requires_rearm: ['mic_device'] } });
    expect(view.percent).toBe(0);
    expect(view.message).toContain('Re-arm');
  });
});

describe('input meter polling', () => {
  afterEach(() => vi.useRealTimers());

  it('keeps one request in flight and stops all updates after disposal', async () => {
    vi.useFakeTimers();
    let resolve!: (level: InputLevel) => void;
    const read = vi.fn(() => new Promise<InputLevel>(r => { resolve = r; }));
    const update = vi.fn();
    const stop = pollInputLevel(read, update, vi.fn());
    await vi.advanceTimersByTimeAsync(1000);
    expect(read).toHaveBeenCalledTimes(1);
    resolve({ active: true, peak: 0.5 });
    await vi.advanceTimersByTimeAsync(0);
    expect(update).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(100);
    expect(read).toHaveBeenCalledTimes(2);
    stop();
    resolve({ active: true, peak: 1 });
    await vi.advanceTimersByTimeAsync(1000);
    expect(update).toHaveBeenCalledTimes(1);
    expect(read).toHaveBeenCalledTimes(2);
    expect(vi.getTimerCount()).toBe(0);
  });

  it('cancels a scheduled poll when the page is hidden or left', async () => {
    vi.useFakeTimers();
    const read = vi.fn().mockResolvedValue({ active: true, peak: 0 });
    const stop = pollInputLevel(read, vi.fn(), vi.fn());
    await vi.advanceTimersByTimeAsync(0);
    stop();
    await vi.advanceTimersByTimeAsync(1000);
    expect(read).toHaveBeenCalledTimes(1);
    expect(vi.getTimerCount()).toBe(0);
  });

  it('reports a failed request once and stops rather than retrying an old daemon forever', async () => {
    vi.useFakeTimers();
    const read = vi.fn().mockRejectedValue('unknown command');
    const error = vi.fn();
    const stop = pollInputLevel(read, vi.fn(), error);
    await vi.advanceTimersByTimeAsync(1000);
    expect(error).toHaveBeenCalledWith('unknown command');
    expect(read).toHaveBeenCalledTimes(1);
    expect(vi.getTimerCount()).toBe(0);
    stop();
  });
});
