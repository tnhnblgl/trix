# Microphone settings

Add an input-device chooser in Settings > Audio, a captured microphone level
directly below it, and concise Hotkey detection help. The choice belongs to
Trix; it does not change the Windows default device or add CLI/tray controls.

## Approach

Use the existing daemon protocol and WASAPI capture. Persist `mic_device` as a
Windows endpoint ID; an empty string preserves the Windows-default behavior
of older configurations. A missing selected device must not silently switch
to a different microphone. Device changes use the existing re-arm notice so
the replay buffer is not discarded without the user's action.

Read the meter from captured microphone PCM, scaled by Trix's microphone
gain. A separate preview stream would add capture work and could show sound
that is absent from the clip, so it is not used. The meter therefore works
while armed, with clear off/unavailable/re-arm states otherwise.

Settings polls the meter at most 10 times per second while visible, with one
request in flight. Each request renews a short measurement lease; leaving
Settings, hiding the window, disconnecting, or crashing the UI stops the
extra sample scan automatically. No new runtime dependencies, permanent
timer/thread, audio buffer allocation, or UI dependency on trix-core.

Disconnected capture streams release their WASAPI clients and retry once per
second on the same audio thread. Windows-default selection resolves again;
an explicit microphone ID never falls back to a different device. The mixer
keeps its channel and timeline, so the missing audio is silence and recovery
does not clear the replay buffer. Disarming interrupts the retry wait. Normal
silence does not reopen a healthy stream; timeout reads still detect a removed
device even if its event stops firing. Healthy streams do not enumerate devices.

## Product details

- List available microphones by friendly name and retain an unavailable
  saved choice visibly. Offer refresh for newly connected devices.
- Keep mic volume zero as off; the meter never opens a device itself.
- Show silence distinctly from capture unavailable and pending re-arm.
- Keep hotkey help focused on when to choose Low level, with the existing
  anti-cheat and administrator limitations stated briefly.

These implementation choices follow the existing settings and performance
contracts. The user authorized implementation on one new branch.

## Protocol additions

- `config.get` / `config.set`: `mic_device` is a string endpoint ID; `""`
  selects the Windows default at arm time and when recovering a lost stream.
  Changing the saved choice requires re-arming.
- `audio.inputs`: returns `{ "inputs": [{ "id": "...", "name": "...", "is_default": true }] }`.
  Only active capture endpoints are listed. Enumeration opens no capture stream.
- `audio.input_level`: returns `{ "active": true, "peak": 0.5, "requires_rearm": [] }`.
  Peak is the microphone's normalized peak after Trix gain, before mixing with
  system audio. The pending list can contain `mic_device` and `mic_volume` and
  compares saved settings with the current capture session, including after
  a UI reconnect. Reading renews a 500 ms measurement lease. Clients should
  stop requesting when hidden; one outstanding request at 100 ms intervals
  is sufficient. This command never opens a microphone.
