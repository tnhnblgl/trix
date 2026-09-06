<div align="center">

<img src="assets/logo-transparent.png" width="112" alt="Trix logo">

# Trix

**A GPU-resident modern game clip recorder for Windows, for people whose PC cannot spare the frames.**

[![Version](https://img.shields.io/badge/version-1.0.0-2b7fff)](https://github.com/tnhnblgl/trix/releases)
[![Platform](https://img.shields.io/badge/platform-Windows%2010%20%7C%2011-0078D6)](#requirements)
[![Rust](https://img.shields.io/badge/rust-2024%20edition-CE422B)](https://www.rust-lang.org/)
[![Tests](https://img.shields.io/badge/tests-548%20passing-3fb950)](#verification)
[![License](https://img.shields.io/badge/license-MIT-3fb950)](LICENSE)

</div>

---

Trix keeps the last few seconds of your screen in a compressed ring buffer and writes them to a
file when you press a key. Capture, colour conversion and H.264 encoding all happen on the GPU —
a raw frame never crosses the bus into system RAM.

The measured cost to your game is **p99 ≤ 4 ms per frame against a 16.7 ms budget** — from an
**8.46 MB** install that holds **13.2 MB** of memory while recording, needs no runtime, and has no
account to create.

## Why Trix exists

Screen recorders are built for machines that can afford them. The usual design copies each frame
out of VRAM into system memory so the CPU can encode it — roughly 475 MB/s of bus and memcpy
traffic for a 1080p60 BGRA stream — and wraps the whole thing in a desktop app that spends the
frames the recorder just saved.

On a low-end PC that is the difference between playable and not. Trix is the recorder for that
machine: the pipeline never leaves the GPU, the app is a thin native shell over the OS webview,
and the background recorder blocks in the kernel at 0% CPU until Windows hands it a frame.

## Measured performance

All figures below were produced by the engine's own `stats` module — a fixed-bucket latency
histogram on the capture hot path plus OS process counters (`K32GetProcessMemoryInfo`) paired with
DXGI per-process video memory (`IDXGIAdapter3::QueryVideoMemoryInfo`). The self-report was
validated against an external `Get-Process WorkingSet64` probe and matched to 0.1 MB.

**Test configuration:** 1920×1200 @ 60 fps, 8 Mbps VBR, Intel QuickSync H.264 MFT, integrated
UMA adapter.

| Metric | Measured | Context |
| --- | --- | --- |
| Per-frame capture cost — replay | **p50 ≤ 2 ms · p99 ≤ 4 ms · max 5.8 ms** | 1,472 frames, 0 dropped |
| Per-frame capture cost — record | **p50 ≤ 1 ms · p99 ≤ 2 ms · max 2.8 ms** | 988 frames, 0 dropped |
| Frame budget at 60 fps | 16.7 ms | the numbers above are the share Trix occupies |
| CPU-side memory | **13.2 MB** | exactly measured replay ring, video + audio |
| Process working set | ~174 MB replay / ~170 MB record | mostly UMA GPU surfaces — see note |
| Frame pacing on a 165 Hz panel | **59.9 fps at `fps=60`**, 0 dropped | uncapped capture would encode at 165 |
| Install size | **8.46 MB** | three binaries, no runtime to install |
| Idle CPU | 0% | every thread blocks on an OS event |

> **On the working set number, honestly.** 174 MB looks bad next to the 13.2 MB ring, and the gap
> is a measurement artifact worth explaining rather than hiding. The capture adapter here is an
> integrated GPU with unified memory, so GPU-accessible surfaces — the WGC frame pool, NV12
> staging, encoder surfaces — are charged to the process working set even though they are not
> heap. DXGI's in-process query reports only the ~67–70 MB *dedicated* segment and returns 0 for
> the *shared* segment, so roughly 83 MB of real GPU allocation is invisible from inside the
> process. `working set − dedicated` is therefore an upper bound on CPU-side RAM, not the heap.
> The honest CPU-side figure is the exactly-measured ring, and it reconciles with an earlier
> external perfmon run that put CPU-side at ~8 MB. On a discrete GPU the same field undercounts
> instead. It is a bound either way, and the ring is the number to trust.

Two performance regressions were found and fixed by measurement rather than guesswork, and both
are documented in [`PLAN.md`](PLAN.md): an idle replay session leaking ~190 KB/s (working set
134 → 244 MB), and "Trix slightly reduces fps in games", which turned out to be three independent
root causes — no fps pacing on high-refresh panels, GPU scheduling priority parity with the game,
and DWM frame-pool copies at full refresh rate.

## Trix vs Medal.tv

Same job — the last few seconds of your game, saved on a keypress — at a fraction of the cost.

| | Trix | Medal.tv |
| --- | --- | --- |
| Memory while recording | **13.2 MB** | 400–500 MB |
| Install size | **8.46 MB** | 300–600 MB |
| A 15-second clip on disk | **~20 MB** | ~100 MB |
| Cost to your frame rate | **p99 ≤ 4 ms** of a 16.7 ms frame | Encoder runs at parity with your game |
| Account | **Not required — none exists** | Required for cloud and sharing |
| Your clips | **Plain `.mp4`, on your disk** | Local, plus a cloud library |
| Source | **MIT, all of it** | Closed |

That 8.46 MB does the same hardware-accelerated H.264 encode a 300 MB install does. The difference
is that Trix refuses to copy a frame into system RAM to get there — capture, colour conversion and
encode all stay on the GPU, and only compressed packets ever reach memory. That is the whole reason
the numbers above are what they are.

**Five times smaller clips at the same 60 fps.** Trix ships at 8 Mbps VBR with rate control forced
through `ICodecAPI` rather than requested politely, so a 15-second clip lands around 20 MB instead
of filling your drive at a bitrate you never asked for. Your library holds five times the clips per
gigabyte, and every one of them is a plain `.mp4` you can drag anywhere.

**Your game gets the GPU first.** Capture runs at below-normal GPU scheduling priority, so when the
GPU is contended the cost lands on Trix as a dropped capture frame — never on your frame rate. On a
165 Hz panel the pacer holds encoding to exactly the 60 fps you configured instead of chasing the
compositor to 165. Measured: p99 ≤ 4 ms of a 16.7 ms budget, 0 dropped frames across 1,472.

**Nothing to sign up for, nothing phoning home.** No account, no telemetry, no analytics, no feed.
Unzip 8.46 MB, press Start, press `Alt+F10`. The clip is already on your disk.

> *Medal figures are typical values for a current Windows desktop install; Trix figures are measured
> by the methodology above.*

## Architecture

```
 GPU framebuffer
       │
       ▼
  WGC capture ──────► VideoProcessorBlt ──────► Media Foundation ──────► H.264 packets
  (VRAM texture)       BGRA→NV12, on GPU        hardware encoder            ~1 MB/s
                                                                               │
  WASAPI loopback ─┐                                                           │
  + microphone     ├──► f32 PCM, wait-free SPSC ring ──────────────────┐       │
                   ┘                                                   ▼       ▼
                                                                  ┌────────────────┐
                                                                  │  replay ring   │
                                                                  │   compressed   │
                                                                  │    packets     │
                                                                  └───────┬────────┘
                                                                          │ hotkey
                                                                          ▼
                                                                       clip.mp4
```

Three decisions define the engine. Everything else follows from them.

**1 — Frames never touch the CPU.** The pipeline is GPU-resident end to end. Only encoded packets
(~1 MB/s at 8 Mbps) ever enter system memory. This single decision is what makes both the memory
footprint and the no-stutter promise achievable on low-end hardware. The one deliberate exception
is clip thumbnails, which stage exactly one frame per clip and free it immediately — paid once per
clip rather than once per frame, and measured at zero dropped frames across ten back-to-back clips.

**2 — Media Foundation, never FFmpeg.** MF's hardware MFTs are thin OS wrappers around the same
NVENC / AMF / QuickSync silicon FFmpeg would drive, and its SinkWriter does hardware encode *and*
MP4 muxing in one API. Zero external DLLs, zero C toolchain, and vendor fallback is automatic. An
FFmpeg build would have added 15–70 MB of DLLs to an 8 MB product.

**3 — Native threads, no async runtime.** The engine is a handful of long-lived threads that block
on OS events. Tokio would have bought nothing and added scheduler jitter, binary size and RAM.

| Thread | Blocks on | Priority | Job |
| --- | --- | --- | --- |
| VideoCapture | WGC `FrameArrived` | Above-normal | Grab texture, stamp with QPC, forward the *handle* |
| AudioCapture | WASAPI event handle | Time-critical | Pull loopback packets, stamp, push to ring |
| Encoder | Channel recv | Above-normal | Feed textures and PCM to the encoder |
| Control | Hotkey message loop | Normal | Parse commands, trigger clip flush, shut down |
| Main | Join handles | Normal | Config, spawn, supervise, teardown |

Two contracts hold the whole thing together. **No allocation on the hot path** — every buffer is
created at startup, and the steady state allocates nothing. **Backpressure means drop, never
stall** — the capture thread never waits on the encoder, because a dropped recording frame is
invisible but a stalled capture thread back-pressures the compositor and stutters the game.

## Features

- **Replay buffer** — the last N seconds are always in memory; one key writes them to disk.
- **Screenshots** — one key saves a picture off the same live capture and puts it on your clipboard.
- **Trimming** — stream-copy export, so a trim finishes in under a second and loses no quality.
- **Hardware encode** on any Intel, AMD or NVIDIA GPU from roughly the last decade.
- **Audio** — system sound and microphone on independent levels; 0 is a real off switch, not a
  mute, and Trix never opens the device at 0.
- **GPU priority** defaults to below-normal, so contention costs a capture frame, not your fps.
- **Tray daemon** with a Tauri desktop app over a documented control socket.
- **Discord presence**, over Discord's local named pipe — no network, no clip data.
- **In-app updates**, verified against a published `SHA256SUMS.txt`.

## Requirements

- Windows 10 or 11, 64-bit.
- A GPU with a hardware H.264 encoder — any Intel, AMD or NVIDIA graphics from roughly the last
  decade qualifies.

No runtime to install; the Visual C++ libraries are linked in. Verified on Intel QuickSync and on
AMD RX 6650 XT and RX 550 under Windows 10 19045.

## Install

Download the latest zip from [Releases](https://github.com/tnhnblgl/trix/releases), unzip it into
one folder, and run `trix-ui.exe`. It starts the background recorder for you.

Windows SmartScreen will warn you the first time, because these binaries are not code-signed:
**More info → Run anyway**.

| Default | Value |
| --- | --- |
| Replay length | 15 seconds |
| Frame rate | 60 fps |
| Bitrate | 8000 kbps, variable |
| Clip hotkey | `Alt+F10` |
| Screenshot hotkey | `Alt+F8` |
| Clips folder | `%USERPROFILE%\Videos\Trix` |
| Library cap | off — Trix never deletes a clip to save space |
| GPU priority | low — your game gets the GPU first |

## Build from source

```bash
npm --prefix crates/trix-ui/web install
npm --prefix crates/trix-ui/web run build
cargo build --release --workspace
cargo build --release -p trix-ui --features tauri/custom-protocol
```

> **The fourth line is not redundant.** Tauri decides dev-versus-production from a cargo *feature*,
> not a build profile: `tauri`'s build script computes `dev = !custom_protocol`. Without
> `--features tauri/custom-protocol`, a plain `--release` build still produces a **dev-mode**
> binary that loads `devUrl` and shows `ERR_CONNECTION_REFUSED` with no Vite server running. The
> `cargo tauri` CLI reaches the same feature by injecting a `[features]` block into
> `crates/trix-ui/Cargo.toml`; passing it to cargo directly touches no file.

Release binaries land in `target/release/`. The release profile is hardened: `lto = "fat"`,
`codegen-units = 1`, `panic = "abort"`, `strip = "symbols"`.

## Repository layout

| Crate | Produces | Responsibility |
| --- | --- | --- |
| [`trix-core`](crates/trix-core) | — | The engine: capture, encode, replay ring, mux. No CLI, no argument parsing. |
| [`trix-proto`](crates/trix-proto) | — | Wire types only — `Request`/`Response`/`Event`, `ClipMeta`. serde only, no `windows-rs`, no `unsafe`. |
| [`trix-daemon`](crates/trix-daemon) | `trix-daemon.exe` | Control socket, arm/disarm/clip, clip library, config, hotkeys, tray. |
| [`trix-ui`](crates/trix-ui) | `trix-ui.exe` | Tauri v2 + Svelte 5 desktop app. |
| [`trix-cli`](crates/trix-cli) | `trix.exe` | Command-line interface and the engine's verification harness. |

`trix-ui` **deliberately does not depend on `trix-core`.** Every capability it has arrives over the
control socket, which is the only thing that keeps that socket good enough for a third-party UI to
use. `scripts/ui-isolation.ps1` enforces it. For the same reason `trix-cli` depends on the engine
*directly* rather than through the daemon — it is the ground-truth harness, and putting it behind
the daemon would put the thing under test behind the thing under test.

Roughly 25,600 lines of Rust and 7,400 of TypeScript/Svelte, against 307 crates and exactly one
runtime npm dependency (`@tauri-apps/api`). The frontend bundle is 120 KB of JS and 23 KB of CSS.

## Verification

**548 tests pass, none fail** — 365 across the Rust workspace, 183 in the frontend.

```bash
cargo test --workspace
npm --prefix crates/trix-ui/web test
```

Beyond unit tests, `scripts/` holds the hardware-in-the-loop gates that unit tests cannot cover:

| Script | Proves |
| --- | --- |
| `arm-cycle-leak.ps1` | Repeated arm/disarm cycles do not leak memory or handles |
| `protocol-smoke.ps1` | The control socket honours its documented contract |
| `ui-isolation.ps1` | `trix-ui` has not grown a dependency on the engine |
| `update-smoke.ps1` | The updater downloads, verifies and replaces correctly |
| `daemon-smoke.ps1` | The daemon starts, arms, clips and shuts down cleanly |
| `ship-zip.ps1` | A release zip is refusable — see below |

`ship-zip.ps1` is mostly gates rather than packaging, because a release artifact is the one build
nobody re-checks. It refuses to write a zip unless the working tree is clean, the version agrees
across `Cargo.toml` and `tauri.conf.json`, the shipped README names the version being built, both
test suites pass, and the staged binaries report the expected version from the staged folder rather
than from `target/release`.

The updater is treated as attack surface, since it downloads and executes code. Downloads are
verified against a published `SHA256SUMS.txt`, and release URLs are validated against host
substitution, path traversal, percent-encoded traversal, scheme downgrade and fragment or query
smuggling — each with a test that asserts the malicious URL is refused.

## Privacy

Trix has no account, no telemetry and no analytics. The only outbound network requests in the
entire codebase go to `github.com` and `api.github.com`, to ask whether a newer version exists and
to fetch it — and the check can be turned off in Settings. Discord presence talks to a named pipe
owned by the Discord client on the same PC and sends nothing about you or your clips.

Exactly one crate in the workspace has an HTTP client at all, and it is reachable from a single
file — the updater. Everything else Trix does happens on your machine.

## Known limits

- **Trimming is fast-mode only.** The in point lands on the last keyframe at or before where you
  put it, about a second's grain. Frame-accurate trimming is not in yet.
- **Fullscreen-exclusive games bypass WGC.** Most modern titles use borderless or flip-model
  presentation and capture fine.

## Documentation

- [`PLAN.md`](PLAN.md) — engine architecture, the phased roadmap, and the full measurement log
  including every performance regression found and how it was root-caused.
- [`docs/superpowers/specs/`](docs/superpowers/specs/) — design specs for the desktop UI, trimming,
  Discord presence and screenshots.
- [`docs/ship/README.txt`](docs/ship/README.txt) — the user-facing readme that ships in the zip.

## License

MIT. See [`LICENSE`](LICENSE).
