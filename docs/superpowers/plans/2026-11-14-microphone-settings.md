# Microphone Settings Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans for inline implementation.

**Goal:** Choose Trix's microphone, see its actual captured level, and shorten hotkey help.
**Architecture:** Persist the endpoint ID in Config and open it through the existing WASAPI source. An atomic, demand-driven meter feeds a visible Settings component through the daemon protocol.
**Tech Stack:** Rust, WASAPI 0.23, Tauri v2, Svelte 5, Vitest.
**Spec:** docs/superpowers/specs/2026-11-14-microphone-settings-design.md

## Global constraints

- One branch: `feat/microphone-settings-and-level`; work in the current checkout as requested.
- No new dependencies, Windows default-device changes, or permanent meter thread.
- Preserve zero-volume behavior and explicit re-arm for device changes.
- Preserve daemon/UI isolation.

## Review focus

- Old configs and duplicate microphone names: default correctly, select by ID.
- Missing/unplugged devices: retain the choice and show unavailable, never fall back silently.
- Closed/hidden/disconnected Settings: stop polling and expire measurement work.
- Pending re-arm: never label the old microphone's level as the new one.
- Capture errors, silence, negative full-scale PCM, and gain boost: truthful bounded readings.

## Task 1: Device selection

Files: core config, audio source/mixer/devices modules, replay/record callers;
proto command; daemon dispatch/state.

- [x] Test backward-compatible config, ID persistence/re-arm, command parsing.
- [x] Add `mic_device: String`, `audio.inputs` and `audio.input_level` commands.
- [x] Enumerate capture endpoints; open the chosen endpoint without fallback.
- [x] Verify targeted Rust tests.

## Task 2: Captured level

Files: core audio level/source/mixer modules; daemon dispatch/state.

- [x] Test meter silence, signed peak, gain/clamping, clearing, and lease expiry.
- [x] Add atomic input level to AudioGains and feed it from existing microphone packets.
- [x] Expose snapshots through `audio.input_level`, without opening another stream.
- [x] Verify targeted Rust tests.

## Task 3: Settings and copy

Files: web lib/audio-input.ts and tests, types/settings, InputDevice.svelte, Settings.svelte.

- [x] Test option identity, unavailable choice, meter states, and polling cleanup/errors.
- [x] Render chooser and meter using existing controls and shared config saves.
- [x] Poll while visible/armed, clean up pending responses, and allow device refresh.
- [x] Shorten Hotkey detection help and option label.
- [x] Run frontend tests/check/build, Rust workspace tests, and isolation gate.
- [x] Independent final review; fix material findings and verify.

## Progress

- Branch created before file changes; original worktree clean.
- README and relevant capture/config/protocol/UI paths read.
- Frontend baseline: 258 tests pass. Rust baseline sandbox registry restrictions resolved by elevated test execution.
- Ruling: implement inline in the requested checkout; avoid redundant worktree or design-approval pauses for this authorized task.
- Task 1: complete; backward compatibility, persistence, exact endpoint lookup, and protocol tests pass.
- Task 2: complete; four meter tests cover signed peaks, silence, gain, clipping, stop/reset, and lease expiry.
- Task 3: complete; 268 frontend tests pass; Svelte reports zero errors/warnings; production frontend build passes.
- Final verification: `cargo test --workspace --quiet` passes 421 tests (15 hardware/manual tests ignored). `cargo fmt --all -- --check` and `scripts/ui-isolation.ps1` pass.
- Hardware verification: explicitly ran `selected_microphone_stream_updates_meter_and_closes -- --ignored --nocapture`; a selected real microphone supplied PCM, reported active, and closed cleanly. No audio was saved.
- Browser verification: headless Edge at 880x560 checked selection, meter reading, manual re-arm reconciliation, hidden-page pause/resume, unmount cleanup, and absence of runtime errors. Temporary harness pages removed.
- Review fixed: avoid wasapi 0.23's dangling-pointer `get_device` implementation by enumerating active capture endpoints and matching exact IDs; resolver test passes.
- Review fixed: reconcile pending microphone changes from daemon responses, including empty lists after ordinary stop/start or reverting a choice; regression test and browser scenario pass.
- Review fixed: keep the Windows-default option label generic because default-device resolution happens at arm time; regression test passes.
- No deferred code-review findings. Physical unplug/replug and native WebView minimization remain manual scenarios; device absence and visibility cleanup have automated coverage.
