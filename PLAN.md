# Trix Core Engine — Architecture Plan & Roadmap (CLI MVP)

> Ultra-lightweight, hardware-accelerated screen/clip recorder for Windows.
> Target: zero gameplay impact, < 30 MB working set, 100% Rust, no UI.

---

## 0. Guiding Design Decisions (read this first)

Three decisions define the entire engine. Everything else follows from them.

### Decision 1 — Frames never touch the CPU
The #1 cause of recorder-induced stutter is copying raw frames from VRAM to system RAM
(a 1080p60 BGRA stream is ~475 MB/s of PCIe + memcpy traffic). Trix will keep the
pipeline **GPU-resident end to end**:

```
GPU framebuffer → capture texture (VRAM) → hardware encoder (VRAM) → tiny H.264 packets (RAM)
```

Only *encoded* packets (~1 MB/s at 8 Mbps) ever enter system memory. This single
decision is what makes the < 30 MB RAM target and the "no micro-stutter" promise
achievable on low-end machines.

### Decision 2 — Media Foundation first, FFmpeg never (for the MVP)
Instead of FFmpeg bindings, use **Windows Media Foundation (MF)** via the official
`windows` crate:

- MF's **hardware MFT encoders** are thin OS-level wrappers around the *same*
  NVENC / AMF / QuickSync silicon FFmpeg would use. You lose nothing.
- MF's **SinkWriter** does hardware encode **and** MP4 muxing in one API — you hand it
  D3D11 textures and it writes a valid `.mp4`. No muxer code to write.
- Zero external DLLs, zero build-time C toolchain pain, ~0 binary size cost
  (MF ships with Windows). An FFmpeg build adds 15–70 MB of DLLs and a large
  private working set — instantly blowing the 30 MB budget.
- Vendor fallback is automatic: MF enumerates whatever hardware encoder exists
  (NVENC on NVIDIA, AMF on AMD, QuickSync on Intel) and falls back to the
  software H.264 MFT only if no hardware exists.

FFmpeg (via `ffmpeg-next`) remains a documented escape hatch for a future
"advanced formats" feature (AV1 tuning, MKV, filters), not for the MVP.

### Decision 3 — Native threads + lock-free channels, no async runtime
Tokio/async buys nothing here: the engine has ~4 long-lived, latency-critical,
mostly-blocking-on-OS-events threads. An async runtime adds scheduler jitter,
binary size, and RAM. Use plain `std::thread` with event-driven blocking waits
(the threads sleep in the kernel at 0% CPU until the OS hands them a frame).

---

## 1. Engine Architecture

### 1.1 Process model
A single background CLI process (daemon-style). Two operating modes:

- **`record` mode** — start → encode → stop → `output.mp4` (classic recorder).
- **`replay` mode** — continuously encode into an in-RAM **ring buffer of encoded
  packets**; on hotkey, flush the last N seconds to disk (the Medal.tv "clip that"
  feature). Because the buffer holds *compressed* packets, 30 seconds of 8 Mbps
  video costs ~30 MB — and is tunable down (e.g., 15 s @ 4 Mbps ≈ 7.5 MB).

### 1.2 Thread topology (5 threads total)

```
┌──────────────┐   D3D11 texture handles    ┌──────────────┐
│ VideoCapture │ ─────(SPSC channel)──────▶ │              │
│  (WGC/DXGI)  │                            │   Encoder    │  H.264/HEVC packets
└──────────────┘                            │ (MF SinkWriter│ ──────────────┐
┌──────────────┐   f32 PCM chunks           │  or MFT loop)│               ▼
│ AudioCapture │ ─────(SPSC ring)─────────▶ │              │        ┌─────────────┐
│(WASAPI loop.)│                            └──────────────┘        │ Mux/Storage │
└──────────────┘                                                    │ (MP4 write  │
┌──────────────┐  commands (start/stop/clip)                        │  or replay  │
│ Control      │ ──────(mpsc)──────────────▶ all threads            │  ring buf)  │
│ (CLI/hotkey) │                                                    └─────────────┘
└──────────────┘        + Main thread (owns lifecycle, joins everything)
```

| Thread | Blocks on | Priority | Job |
|---|---|---|---|
| **VideoCapture** | WGC `FrameArrived` event | Above-normal | Grab D3D11 texture, timestamp (QPC), forward *handle* (not pixels) |
| **AudioCapture** | WASAPI event handle | Time-critical (audio drops are audible) | Pull loopback packets, timestamp, push to ring |
| **Encoder** | channel recv | Above-normal | Feed textures + PCM to SinkWriter / MFTs |
| **Control** | hotkey message loop / stdin | Normal | Parse commands, trigger clip flush, graceful shutdown |
| **Main** | join handles | Normal | Config, spawn, supervise, teardown |

### 1.3 Memory management rules (the "zero bottleneck" contract)

1. **No allocation on the hot path.** All buffers are created at startup:
   - A fixed **pool of 3–4 D3D11 textures** (in VRAM) recycled between capture and
     encoder — classic triple-buffering. Pool exhausted ⇒ *drop the frame*, never block.
   - A preallocated **audio ring buffer** (`rtrb`) sized for ~500 ms of PCM.
   - The replay ring is a preallocated `VecDeque<EncodedPacket>` with byte-budget
     eviction (pop oldest GOP when over budget).
2. **Backpressure = frame drop, never stall.** The capture thread must *never* wait
   on the encoder. If the encoder falls behind (driver hiccup, disk stall), frames
   are skipped and a counter is incremented. A dropped recording frame is invisible;
   a stalled capture thread can back-pressure the compositor and stutter the game.
3. **Channels:** `rtrb` (wait-free SPSC) for audio samples; `crossbeam-channel`
   bounded(4) for texture handles; small `std::sync::mpsc` for control messages.
   No mutexes on any per-frame path.
4. **Timestamps:** every video frame and audio packet is stamped from
   `QueryPerformanceCounter` at capture time. A/V sync is done arithmetically from
   these stamps (converted to 100 ns MF units) — never by "arrival order".

### 1.4 Module layout (single binary, workspace-ready)

```
trix/
├── Cargo.toml
└── src/
    ├── main.rs          // CLI parsing, mode dispatch, thread supervision
    ├── config.rs        // Encoder/bitrate/fps/buffer settings (TOML + CLI overrides)
    ├── capture/
    │   ├── video.rs     // Windows.Graphics.Capture session + D3D11 texture pool
    │   └── audio.rs     // WASAPI loopback client (+ optional mic input later)
    ├── encode/
    │   ├── mf.rs        // Media Foundation SinkWriter setup, HW MFT selection
    │   └── types.rs     // EncodedPacket, stream descriptions, QPC↔MF time math
    ├── pipeline.rs      // Thread spawning, channels, backpressure policy
    ├── replay.rs        // Encoded-packet ring buffer + clip flush (GOP-aligned)
    ├── control.rs       // Hotkey registration, stdin commands, shutdown signal
    └── stats.rs         // Dropped frames, working set, encode latency (--verbose)
```

---

## 2. Crate Recommendations

### Core (MVP)

| Concern | Crate | Why this one |
|---|---|---|
| All Windows APIs (WGC, D3D11, WASAPI, MF, hotkeys) | **`windows`** (windows-rs, official Microsoft) | One dependency covers *every* OS surface Trix needs. Zero-cost COM bindings, maintained by Microsoft. |
| Screen capture | **`windows-capture`** | Production-quality wrapper over Windows.Graphics.Capture: event-driven, GPU textures, per-monitor/per-window, handles device-lost. Use it to move fast; its source doubles as a reference if you later inline raw WGC calls. |
| Audio loopback | **`wasapi`** | Purpose-built WASAPI crate with first-class *loopback* support (event-driven shared mode). Preferred over `cpal` here — cpal's loopback support is newer and its abstraction hides the timestamping you need for A/V sync. |
| HW encode + MP4 mux | **`windows`** (Media Foundation: `IMFSinkWriter`) | See Decision 2. Encoder *and* muxer in one OS API, hardware-selected automatically. |
| SPSC audio ring | **`rtrb`** | Wait-free, allocation-free, built for real-time audio. |
| Texture-handle channel | **`crossbeam-channel`** | Bounded, fast, `select!` support for shutdown signaling. |
| CLI | **`clap`** (derive) | Standard; `record`, `replay`, `list-encoders` subcommands. |
| Config file | **`serde` + `toml`** | `%APPDATA%\trix\config.toml`. |
| Errors | **`thiserror`** (library-ish modules) + **`anyhow`** (main) | Idiomatic split. |
| Logging | **`tracing` + `tracing-subscriber`** (compact fmt) | Structured; cheap when disabled. |
| Hotkeys | `windows` (`RegisterHotKey`) or **`global-hotkey`** | RegisterHotKey is ~20 lines via `windows`; crate optional. |

### Deliberately excluded (and why)

- **`ffmpeg-next` / `ffmpeg-sidecar`** — DLL weight & RAM footprint kill the 30 MB
  budget; MF covers the MVP. Revisit only for exotic formats.
- **`tokio`** — no async I/O problem exists here (Decision 3).
- **`scrap` / GDI / BitBlt approaches** — CPU-copy capture; exactly the stutter
  source Trix exists to avoid.
- **Raw DXGI Desktop Duplication** as the primary path — WGC supersedes it on
  Win10 1903+: better multi-monitor behavior, capture-exclusion support, and
  equivalent performance. Keep DXGI DD in mind only as a legacy-OS fallback.

---

## 3. Phased Development Roadmap

Each phase ends with a runnable binary and a verifiable exit criterion.

### Phase 0 — Skeleton & environment (½ day)
- `cargo new trix`, set up `clap` subcommands (`record`, `replay`, `probe`), `tracing`, config loading.
- Add release profile hardening now (see §4).
- **Exit:** `trix probe` prints OS version, monitors, and enumerated MF hardware encoders (proves the `windows` crate + MF activation works on your machine).

### Phase 1 — Video capture proof (1–2 days)
- Stand up `windows-capture`: pick monitor, receive frames on the capture callback.
- Build the D3D11 texture pool + QPC timestamping.
- Verification hack: copy *one* frame to CPU and dump a `.png` (temporary code).
- **Exit:** `trix probe --snapshot` writes a correct screenshot; logs show steady 60 fps frame arrival with 0% CPU while idle-waiting.

### Phase 2 — Hardware encode → MP4 (video only) (2–4 days) ★ hardest phase
- Create `IMFSinkWriter` on the output file with an H.264 stream (NV12 input, D3D11 device manager attached so the encoder reads VRAM directly).
- Convert capture BGRA → NV12 on the GPU (MF's Video Processor MFT does this; no shader needed).
- Wire capture thread → encoder thread via the bounded channel; implement the frame-drop policy.
- **Exit:** `trix record -d 10` produces a 10-second `output.mp4` that plays in MPC/VLC, encoded by the *hardware* MFT (verify encoder name in logs), with game-load CPU usage of the trix process < 2%.

### Phase 3 — Audio loopback capture (1–2 days)
- WASAPI shared-mode loopback on the default render device, event-driven, into the `rtrb` ring with QPC stamps.
- Verification hack: write a raw `.wav` to confirm clean audio (no gaps/crackle).
- **Exit:** `trix probe --audio 5` records 5 s of system audio to a valid `.wav`.

### Phase 4 — A/V mux & sync (1–2 days)
- Add an AAC audio stream to the SinkWriter (MF's AAC encoder MFT).
- Feed both streams with QPC-derived MF timestamps; handle the "audio starts before video" head-trim.
- **Exit:** 60-second recording of a YouTube A/V sync test video shows no perceptible drift start-to-end; pause/unpause of the source stays in sync.

### Phase 5 — Replay buffer + clip hotkey (2–3 days) ★ the Medal killer feature
- Redirect encoder output into the byte-budgeted packet ring instead of a file (MF detail: use a SinkWriter with a custom byte stream, or run the encoder MFT manually and mux on flush — decide during Phase 2 based on how SinkWriter behaves; the manual-MFT route gives clean GOP-aligned packets and is the safer architecture for the ring).
- Force IDR keyframes at a fixed interval (e.g., every 2 s) so clips can start on a GOP boundary.
- `RegisterHotKey` (e.g., Alt+F10) → flush ring to `clip_<timestamp>.mp4`.
- **Exit:** play a game for 5 minutes, hit the hotkey, get a correct last-30-seconds clip; process RAM stays flat the whole time.

### Phase 6 — Daemonization & robustness (1–2 days)
- Configurable clip hotkey: `clip_hotkey` in config.toml (default `alt+f10`).
  Real-world collision found in the field: NVIDIA App's overlay owns Alt+F10
  ("Save Instant Replay") and its low-level hook consumes the key *before*
  `RegisterHotKey` matching in games it has hooked (seen in ETS2 — the game
  still takes its own F10 screenshot via raw input, Trix never fires), while
  Roblox's anticheat blocks the overlay so the key falls through and works.
  Users need a rebind path; no fixed key is safe from every overlay.
- Detached background operation, single-instance guard, graceful Ctrl+C / hotkey shutdown that finalizes the MP4 (an unfinalized MP4 is corrupt — always flush the moov atom).
- Handle device-lost (GPU driver reset, monitor unplug, resolution change) by rebuilding the capture session without exiting.
- **Exit:** survives alt-tab, display mode changes, and a 2-hour soak run without leaks (working set flat) or crashes.

### Phase 7 — Performance certification (ongoing, 1–2 days focused)
- `stats.rs`: dropped-frame counters, encode latency histogram, working-set self-report.
- Measure against the §4 budget on the *lowest-end* target machine; fix regressions.
- **Exit:** all §4 numbers met while a demanding game runs.

**Total MVP estimate: ~2–3 weeks of focused work. Phases 2 and 5 carry the risk; everything else is plumbing.**

---

## 4. Performance Strategy — Staying Under 30 MB

### 4.1 Where the bytes go (budget)

| Component | Budget | Note |
|---|---|---|
| Binary + std | ~2 MB | With release hardening below |
| Texture pool | **0 MB RAM** | Lives in VRAM — the core trick |
| Audio ring (500 ms f32 stereo @48 kHz) | ~0.4 MB | |
| Encoder/MF internal buffers | ~5–10 MB | OS-side; measure, don't guess |
| Replay ring (15 s @ 4 Mbps) | ~7.5 MB | *The* tunable knob; config-exposed |
| Channels, logs, misc heap | ~2 MB | |
| **Total (record mode)** | **~10–15 MB** | |
| **Total (replay mode, 15 s)** | **~18–25 MB** | 30 s @ 8 Mbps would add ~30 MB — document the tradeoff in config |

### 4.2 Build hardening (`Cargo.toml`)
- `opt-level = 3`, `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`,
  `strip = "symbols"`.
- No default allocator swap needed; if fragmentation shows in soak tests, evaluate
  `mimalloc` (it usually *raises* baseline RSS slightly — only adopt with data).

### 4.3 Runtime rules
1. **Event-driven everything, zero polling.** All three worker threads block in the kernel on OS events. Idle CPU must be 0.0%.
2. **GPU-resident pipeline** (Decision 1) — never map/copy a raw frame in steady state (debug snapshot paths gated behind flags).
3. **Fixed-capacity structures only** in steady state; startup is the only allocation window. Enforce with a debug-mode allocation counter around the hot loop.
4. **Drop, don't buffer, under pressure** — bounded channels mean memory cannot balloon when the disk or encoder stalls.
5. **Measure continuously:** `--stats` flag prints working set (via `K32GetProcessMemoryInfo`), dropped frames, and encode latency every 5 s; soak test in Phase 6 charts it over hours. The 30 MB claim becomes a CI-checkable number, not a hope.
6. **Respect the game:** thread priorities as in §1.2, but never `REALTIME` class; register the process as "background recording" behavior-wise — capture at the game's cadence, don't force one.

### 4.4 Known risk register

| Risk | Mitigation |
|---|---|
| SinkWriter's opaque buffering complicates the replay ring | Phase 5 fallback: drive encoder MFT manually, mux only on clip flush |
| Fullscreen-exclusive games bypass WGC | Most modern titles use borderless/flip-model (capturable); document limitation; DXGI DD fallback later |
| WGC yellow capture border (pre-Win11) | Runtime-gated: `capture::border_settings()` sets `IsBorderRequired = false` where the property exists (Win11 / build 20348+), else falls back to `Default` (border shown) so capture still starts on Win10 ≤19045 — the property is absent there and a non-`Default` setting aborts capture with `BorderConfigUnsupported`. Verified on AMD RX 6650 XT & RX 550, both Win10 19045. |
| Low-end iGPU encoder quality (old QuickSync) | Expose bitrate/preset in config; quality is tunable, stutter is not |
| A/V drift on long recordings | Single QPC clock domain for both streams; verified in Phase 4 exit test |

---

## 5. Progress Log

- **Phase 0 ✅** — `trix probe` enumerates OS/GPUs/monitors and MF hardware encoders.
  Findings: hybrid-graphics laptop — monitor on Intel iGPU (QSV), NVENC on discrete
  RTX 5060; encoder selection must prefer the capture adapter.
- **Phase 1 ✅** — `probe --snapshot` (verified PNG) and `probe --capture N`:
  55 fps steady, 0.03 ms jitter, QPC timestamps working. WGC reports physical
  pixels (1920×1200); take encoder dimensions from frames, not monitor descs.
- **Phase 2 ✅ (with caveat)** — `trix record -d N -o file.mp4` produces valid
  hardware-encoded H.264 MP4 (GPU video engine ~29% during encode, CPU ~1.3% of
  machine, release binary 1.41 MB). **Caveat:** built on `windows_capture::encoder::
  VideoEncoder` (MediaStreamSource → MediaTranscoder), whose working set is
  ~199 MB — far over the 30 MB budget. DECISION: the hand-rolled MF SinkWriter/MFT
  path (`encode/mf.rs`) is now mandatory, not a fallback; it lands with Phase 5's
  replay ring (which needs manual packet access anyway). The crate encoder stays
  only as the Phase 2–4 stepping stone.
- **Phase 3 ✅** — `probe --audio N` captures WASAPI shared-mode loopback
  (event-driven, f32 48 kHz stereo with autoconvert) to WAV. Verified: valid RIFF,
  0 discontinuities, tones captured at exact frequencies (Goertzel-checked), QPC
  span matches PCM length. Finding: loopback delivers packets **only while
  something renders** — Phase 4 must synthesize silence for idle gaps, using each
  packet's QPC timestamp to place audio correctly on the timeline.
- **Phase 4 ✅** — `trix record` now muxes AAC system audio (192 kbps CBR, 48 kHz
  stereo). Sync design forced by the encoder's audio API (it ignores timestamps,
  clocking audio by samples-sent): we feed one continuous PCM timeline starting at
  the first video frame's QPC — real packets placed by QPC, head/overlap trimmed,
  idle gaps filled with synthesized silence (100 ms grace so silence never
  pre-empts late real packets). Verified: both tracks within 20 ms of each other
  (video 9.98 s / audio 9.96 s), AAC sample math exact, real-vs-silence seconds
  match the played test tones. `--no-audio` flag available.
- **Phase 4 bugfix ✅ (user-verified)** — crackling audio in recordings: zero-tolerance
  QPC re-anchoring spliced ~100 clicks/s into continuous audio (measured: 518 splices
  in 5.5 s of tone). Fixed with a 20 ms continuity dead-band — flowing packets append
  verbatim on the sample-count clock; QPC re-anchoring only for real gaps/overlaps.
  Post-fix: 0 jitter splices; user confirmed clean audio by ear.

- **Phase 5a ✅** — `encode/mf.rs` + `encode/convert.rs`: `trix record` now runs on
  our own `IMFSinkWriter` (DXGI device manager, NV12 fed straight to the hardware
  H.264 MFT, PCM→AAC with explicit timestamps) plus our own D3D11 VideoProcessor
  BGRA→NV12 blit (3-texture NV12 pool, `MF_LOW_LATENCY`). The MediaTranscoder
  pipeline is gone. Measurements (1920×1200@60, 8 Mbps): working set 199→159 MB
  (170 with audio), 0 dropped frames, hardware engine 27% during encode, MP4
  box-validated (avc1/mp4a durations lock within 40 ms). **Memory attribution
  finding:** GPU process counters show ~154 MB of the working set is GPU surface
  memory (WGC pool + NV12 pool + QSV encoder DPB/internal pool) charged to the
  process because the capture iGPU is UMA — *the CPU-side footprint is ~12–15 MB,
  within the 30 MB budget*. On a machine whose capture adapter has dedicated
  VRAM those surfaces leave the process entirely; on UMA they are physics every
  recorder pays. Budget reporting (Phase 7) must therefore split CPU-side vs
  GPU-side memory.

- **Phase 5b ✅** — `trix replay`: the Medal-style replay buffer. Manual async
  hardware H.264 MFT (`encode/h264.rs`: MFTEnum2 pinned to the capture adapter's
  LUID, event-driven NeedInput/HaveOutput, D3D-manager-fed NV12 → compressed
  packets in RAM), GOP-aligned `VecDeque` ring (evicts whole GOPs, front always a
  keyframe, 15 s + 3 s keyframe margin) plus a PCM ring on the same QPC timeline.
  Alt+F10 (dedicated `RegisterHotKey` thread, `control.rs`) → snapshot under the
  handler mutex → `clip_<ts>.mp4` muxed on the control thread: H.264 passthrough
  (no re-encode) using the encoder's *negotiated* output type, audio AAC-encoded
  at flush. Verified end-to-end via hidden `--auto-clip/--exit-after` hooks:
  14.2 s video / 14.2 s audio clip (tracks lock within 41 ms), box-validated
  (avc1 1920×1200, first sample is a keyframe, GOP = 128 frames, mp4a 48 kHz),
  muxed in 105 ms, 0 dropped frames. Debugging lessons: async MFT needs
  `MFStartup` before its event queue exists; `MF_E_TRANSFORM_STREAM_CHANGE`
  renegotiation must *wait for a fresh HaveOutput* (inline retry → E_UNEXPECTED);
  clip SinkWriter needs `MF_SINK_WRITER_DISABLE_THROTTLING` (video written before
  audio → interleave throttle deadlocks `WriteSample`); audio must be drained to
  "now" at snapshot time because `on_frame_arrived` stops firing on a static
  screen (WGC is change-driven) and the clip would lose trailing audio.
- **Phase 5c ✅** — replay-mode memory certification (1920×1200@60, 8 Mbps,
  15 s ring, steady state with eviction active): working set 165.6 MB, of which
  GPU process counters attribute 157.9 MB (74.5 local + 83.4 shared) to
  UMA-charged GPU surfaces → **CPU-side ≈ 8 MB including the ~4.2 MB compressed
  ring** — within the §4.1 replay budget (18–25 MB). Ring bounded correctly:
  508 frames encoded, ring held 251 packets (~18 s budget), 1 frame dropped
  (0.2 %), 0 audio gaps/trims.

- **Phase 6a ✅** — configurable clip hotkey. Field finding: in ETS2 the clip
  never fired while Roblox worked — NVIDIA App's overlay owns Alt+F10 ("Save
  Instant Replay") and its low-level hook consumes the key before
  `RegisterHotKey` matching in games it hooks (ETS2 still took its own F10
  screenshot via raw input; Roblox's anticheat blocks the overlay, so the key
  fell through to us). Fix: `clip_hotkey` in config.toml — `mods+key` parser
  in `control.rs` (ctrl/alt/shift/win + a-z/0-9/f1-f24; bare letters/digits
  rejected so a typo can't swallow a key system-wide), banner shows the
  active combination, invalid values fail fast with the accepted format,
  registration failure explains the another-app-owns-it case. 5 unit tests.
- **Phase 6b ✅** — graceful shutdown. `SetConsoleCtrlHandler` routes
  Ctrl+C/Ctrl+Break/console-close into a polled atomic; `record` finalizes
  the MP4 early (verified: Ctrl+C at 6 s into a 30 s recording → valid file,
  moov flushed, video 6.15 s / audio 6.25 s locked, exit < 1 s), `replay`
  breaks into its normal diagnostics+close path. Console-close holds the
  handler thread until finalize completes (Windows kills ~5 s after return).
- **Phase 6c ✅** — single-instance guard: named mutex
  (`Local\trix-capture-single-instance`) acquired before record/replay; a
  second session exits with a friendly error (verified live), `probe` stays
  allowed alongside.
- **Phase 6d ✅** — display-change / device-lost robustness, two layers.
  (1) Resolution changes don't interrupt anything: windows-capture recreates
  its frame pool and keeps delivering at the new size, and our per-frame
  VideoProcessor blit scales any input into the encoder's fixed resolution —
  verified live by switching 1920x1200→1920x1080→back mid-replay: session
  survived, both transitions logged ("capture input resized"), auto-clip
  spanning the change is box-valid at constant 1920x1200, 0 drops.
  (2) If the capture item actually dies (monitor unplug, topology change,
  GPU driver reset), `replay` now rebuilds the whole session in a loop:
  re-query monitor + audio, fresh encoder + ring (ring restarts empty —
  logged), 2 s settle between attempts, Ctrl+C responsive during waits,
  stale hotkey presses drained, failure budget of 30 consecutive
  fast-failures before giving up. Real unplug/TDR still needs hands-on
  verification. `record` on device loss finalizes early with a valid MP4
  (same path as 6b).

- **Phase 6e ✅** — soak test, and it earned its keep: the first 12-minute
  idle-replay run leaked ~190 KB/s (working set 134 → 244 MB). Root cause:
  audio was only pumped inside `on_frame_arrived`, and WGC delivers no
  frames on a static screen — the unbounded loopback mpsc channel buffered
  PCM at exactly the loopback rate (48 kHz × 4 B). Three-part fix:
  (1) `idle_pump()` drains audio on the control thread's 250 ms tick in both
  replay and record; (2) the audio ring is span-bounded on its own
  (replay + 3 s measured from the newest sample) because the existing trim
  rule keys off the *video* ring front, which stops moving during a stall;
  (3) clip PCM assembly no longer splices silence in front when older audio
  was trimmed — the snapshot carries `audio_offset_100ns` and the audio
  track starts late (a 2-hour static stretch would otherwise have allocated
  GBs of zeros at clip time). Re-run: working set 133.6 → 136.0 MB over
  12 min — +2 MB settle while the audio ring fills to its bound, then flat
  to the 0.1 MB for the entire back half; private bytes identical; clean
  exit, 0 gaps. Static-screen clip verified box-valid (1 video packet +
  13.9 s audio).

- **Phase 7 ✅** — performance self-certification. `stats.rs`: a fixed-bucket
  `LatencyHistogram` (no alloc, O(1) record, quantiles read as "≤ bound") on
  the capture hot path, and a `StatsReporter` pairing OS process counters
  (`K32GetProcessMemoryInfo`) with DXGI per-process video memory
  (`IDXGIAdapter3::QueryVideoMemoryInfo`). Both `record` and `replay` measure
  the *per-frame capture-callback time* — the wall time Trix occupies the WGC
  thread, i.e. the gameplay-impact number — and emit a `perf` line every
  `stats_seconds` (config, default 60; 0 silences) plus a final summary at
  close. 6 unit tests; self-report validated against an external
  `Get-Process WorkingSet64` probe (matched to 0.1 MB).

  Certified numbers (1920×1200@60, 8 Mbps, this UMA iGPU):
  - **Per-frame latency** — replay p50 ≤2 ms / p99 ≤4 ms / max 5.8 ms over
    1472 frames, 0 dropped; record p50 ≤1 ms / p99 ≤2 ms / max 2.8 ms over
    988 frames, 0 dropped. Both are a small fraction of the 16.7 ms frame
    budget — the "zero-impact" claim holds.
  - **Memory** — working set ~174 MB (replay) / ~170 MB (record). The
    exactly-measured CPU-side ring is **13.2 MB** (`cpu_ring_mb`, video+audio),
    within the §4.1 replay budget once the 8 Mbps + 3 s keyframe margin is
    accounted for. Everything else is GPU/UMA surfaces.

  Measurement finding worth remembering: on this integrated adapter DXGI's
  *in-process* `QueryVideoMemoryInfo` reports only the ~67–70 MB **dedicated
  (local)** segment; the **non-local (shared)** segment returns 0, so the
  ~83 MB of GPU-accessible *shared system memory* (WGC frame pool, NV12
  staging, encoder surfaces) that Phase 5c's external perfmon "GPU Process
  Memory" counters caught is invisible from inside the process. Consequently
  `ws_minus_gpu_mb` (WS − DXGI-dedicated ≈ 103 MB) is only an *upper bound* on
  CPU-side RAM, not the heap — the honest CPU-side figure is the exact ring
  (`cpu_ring_mb`), reconciling with 5c's ~8 MB CPU-side conclusion. On a
  discrete GPU the same field would instead undercount (local VRAM never
  enters WS); it is a bound either way.

**MVP core complete (Phases 0–7).** User-verifiable leftovers: rebind
`clip_hotkey` (already set to bare `f10`) and confirm ETS2 clips fire;
unplug/replug an external monitor during `trix replay` to exercise the
rebuild loop with real hardware events. Post-MVP candidates: software-encoder
fallback when no HW encoder is present, AMD/AMF validation on other hardware.

**Post-MVP AMD field fixes (2026-07-21, friends' RX 6650 XT & RX 550, Win10 19045):**
- *Capture wouldn't start* — WGC `IsBorderRequired` absent pre-Win11; hardcoded
  `WithoutBorder` aborted with `BorderConfigUnsupported`. Fixed by
  `capture::border_settings()` (runtime-gated). The yellow border that remains
  is Windows' live on-screen capture indicator only — NOT in the output files
  (confirmed: recordings/snapshots are clean).
- *Clips 4× oversized (64 MB / 15 s)* — `MF_MT_AVG_BITRATE` is advisory; AMD AMF
  ignored it and ran ~34 Mbps (also blew the replay ring's RAM). Fixed by forcing
  rate control via `ICodecAPI` (`encode::mf::apply_rate_control`) on both encoder
  paths, plus config knobs `rate_control` (vbr default / cbr) and
  `max_bitrate_kbps` (0=auto 1.5×). Verified on Intel; AMD size-drop pending
  friend re-test. CQP/quality mode deferred as a `record`-only future option
  (unbounded bitrate is unsafe for the RAM ring).

**Game-fps optimization (2026-07-24, "Trix slightly reduces fps in games"):**
- *Root cause 1 — no fps pacing*: WGC delivers a frame per DWM composition, so a
  144/165 Hz monitor made Trix convert + encode up to 165 fps while configured
  for 60 (~3× the intended GPU load, all competing with the game). Fixed by a
  QPC pacer in both capture callbacks: frames arriving more than half a frame
  early are skipped before any GPU work; skips are logged as `paced` (distinct
  from backpressure `dropped`). Verified: 165 Hz panel now encodes 59.9 fps at
  fps=60, exactly 30 fps at fps=30, dropped=0, callback p99 ≤2 ms unchanged.
- *Root cause 2 — GPU priority parity*: the BGRA→NV12 `VideoProcessorBlt` ran
  at the same GPU scheduling priority as the game. Fixed by
  `capture::lower_gpu_priority()` — `D3DKMTSetProcessSchedulingPriorityClass`
  BELOW_NORMAL (feature `Wdk_Graphics_Direct3D`), so contention now costs
  (rare) capture drops instead of game fps. Config `gpu_priority = "low"`
  (default) / `"normal"` escape hatch.
- *Root cause 3 — DWM frame-pool copies at full refresh*: where the OS has
  `GraphicsCaptureSession.MinUpdateInterval` (runtime-gated like the border;
  present on the user's Win11, absent on Win10 19045), WGC delivery is capped
  at ~4/3 of target fps (¾ frame period, avoiding compositor-cadence beating).
  Observed 165→80/s delivery on the dev laptop.

## 6. First Concrete Step

Phase 0: `cargo new trix`, add `windows`, `clap`, `tracing`; write `trix probe`
to enumerate monitors and MF hardware encoders. That one command validates the
entire riskiest assumption (MF hardware encoder availability via windows-rs)
before any pipeline code exists.
