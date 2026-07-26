# Trix Desktop — UI & Daemon Architecture

**Date:** 2026-07-26
**Status:** Approved design, not yet implemented
**Supersedes nothing.** Extends [PLAN.md](../../../PLAN.md), which covers the capture engine (Phases 0–7, complete).

---

## 1. Context

The Trix engine is finished and field-verified: hardware capture, NVENC/AMF/QuickSync encode, a
20-second replay ring, hotkey clipping, and A/V mux, in a 1.5 MB binary that holds under 30 MB of
working set and costs approximately zero game fps. It is driven entirely by `trix.exe` subcommands
from a terminal.

That is not a product. This spec covers the end-user layer: a tray daemon and a desktop app, aimed
squarely at the audience Medal.tv serves badly — people on low-end PCs who want their plays saved
without paying for it in frames.

**The differentiator is the engine, and it already exists.** The UI's job is to not be the reason
someone picks Medal instead.

## 2. Scope

**In scope for v1:**

- Tray daemon: owns the replay ring, the clip hotkey, and a control socket
- Desktop app: clip library with thumbnails, playback, trim, export, delete, favorite
- Settings UI covering every existing `config.toml` key plus the new ones below
- First-run setup, opt-in autostart, MSI installer

**Explicitly out of scope for v1** (each would be its own spec):

- Accounts, cloud upload, share links, social feed
- In-game overlay
- Automatic game detection — capture is armed manually (see §7.1)
- Mobile or web viewers

**Ships offline. No backend, no hosting cost, no account.** The product is a local tool that
happens to be dramatically lighter than its competitor.

## 3. Architecture

### 3.1 Workspace layout

Today's single binary becomes a five-crate Cargo workspace:

```
crates/trix-proto/    Wire types only. serde structs, no windows-rs, no unsafe. ~200 lines.
crates/trix-core/     The engine: capture, encode, ring, mux, config, stats.
                      Today's src/ minus main.rs. mod -> pub mod. Otherwise untouched.
crates/trix-cli/      probe / record / replay. Depends on trix-core directly.
crates/trix-daemon/   Tray icon, hotkey, control-socket server, library manager.
                      Depends on trix-core + trix-proto.
crates/trix-ui/       Tauri app. Depends on trix-proto ONLY.
```

### 3.2 The one hard rule

**`trix-ui` must not depend on `trix-core`.**

Enforced in CI with a `cargo tree` assertion. Our own UI physically cannot capture a frame, read
the ring, or touch the encoder — every capability it has arrives over the control socket.

This is not stylistic. It is the only thing that keeps the protocol honest: if the first-party UI
had privileged access, the protocol would rot and third-party UIs would be second-class. Under this
rule, a Trix UI written in C#, Avalonia, Electron, or Python has exactly the access ours does.
`trix-proto` is merely the convenience crate for the ones that pick Rust.

### 3.3 Why the CLI bypasses the daemon

`trix-cli` talks to `trix-core` directly and never to the daemon. Every phase of this project has
been verified by running a command and inspecting a real artifact
(`trix replay --auto-clip 8 --exit-after 12`). Routing that through IPC would put the thing under
test behind the thing under test. The CLI stays the ground-truth harness and the field-debugging
tool.

### 3.4 UI toolkit: Tauri

Chosen over Iced. Reasoning:

- **Clip playback is free.** WebView2 hardware-decodes H.264 MP4 in a `<video>` tag. The Iced path
  needs either GStreamer (a 30–80 MB runtime, absurd next to a 1.5 MB engine) or a hand-written
  `IMFMediaEngine` integration (~500–800 lines of COM owned forever).
- **Polish per hour.** Virtualized grids, hover-preview, drag-select, animation — solved in the web
  stack, hand-built in Iced. These are exactly what a Medal competitor is judged on.
- **Size is a non-issue.** Tauri uses the OS WebView2; the app is ~3–6 MB, not Electron's ~150 MB.

The memory cost (~90–180 MB while the window is open) is acceptable because **the UI is a separate
process that is not running during gameplay.** The <30 MB contract binds the daemon, which is the
only thing alive while a game is.

If the webview ever disappoints, §3.2 means an Iced shell is a new crate against the same socket —
not a rewrite.

## 4. Control protocol

### 4.1 Transport

Named pipe `\\.\pipe\trix-control`, created with a DACL restricted to the current user's SID and
`PIPE_REJECT_REMOTE_CLIENTS`.

Not a localhost TCP socket: a listening port is reachable by any local process including any web
page the user has open, and pipe ACLs give real per-user isolation for free. The accepted tradeoff
is that a *plain browser page* cannot be a Trix UI; anything with a runtime connects to a named pipe
in one line.

Framing is newline-delimited JSON — one message per line, debuggable by piping a text file at it, no
schema compiler in the build.

### 4.2 Message shape

```jsonc
// request  — always carries an id
{"id":7,"cmd":"arm"}
// response — exactly one per request
{"id":7,"ok":true,"data":{"armed":true,"encoder":"NVENC H.264"}}
{"id":7,"ok":false,"error":"no hardware encoder available"}
// event    — unsolicited, never carries an id
{"event":"clip_saved","data":{ /* clip metadata, §5.2 */ }}
```

### 4.3 Commands

| Command | Returns |
|---|---|
| `status` | armed, encoder, monitor, ring seconds used/total, daemon version |
| `arm` / `disarm` | new armed state |
| `clip` | clip metadata for the clip just saved |
| `config.get` | full effective config |
| `config.set` | accepted values + which keys require a re-arm to take effect |
| `library.list` | `{offset, limit}` → paged clip metadata, newest first |
| `library.delete` | `{id}` — removes mp4, json, and jpg |
| `library.rename` | `{id, title}` — edits metadata only, filename never moves |
| `library.favorite` | `{id, favorite}` |
| `library.reveal` | `{id}` — opens Explorer with the file selected |
| `library.export` | `{id, dest, start_ms, end_ms, mode}` — see §6.3 |
| `monitors.list` / `encoders.list` | real probe data for the settings dropdowns |

### 4.4 Events

`armed`, `disarmed`, `clip_saved`, `export_progress`, `export_done`, `error`, and `stats`.

`stats` is emitted only while a client has subscribed. This makes the existing `stats_seconds = 0`
default coherent: with no UI attached, nothing measures anything.

### 4.5 Semantics

- **Multiple clients.** The daemon accepts several connections and broadcasts events to all of them.
  Commands are funnelled onto the existing engine thread through a channel, so the single-threaded
  session model in `replay.rs` does not change.
- **Malformed input** gets an error response; the connection stays open. An unknown `cmd` is an
  error response, not a disconnect.
- **Single instance.** The mutex currently in `control.rs` moves to the daemon and keeps its job: if
  the daemon is armed and someone runs `trix replay` from a terminal, the CLI bails with the message
  it already prints rather than two processes fighting over the encoder.
- **Discovery.** The pipe's existence is the liveness check. `trix-ui` attempts to connect on launch;
  finding nothing, it offers to start the daemon rather than erroring. Lost connections retry with
  backoff behind a visible "daemon not running" state.

## 5. Clip library

### 5.1 Location

`%USERPROFILE%\Videos\Trix\`, overridable via a new `clip_dir` config key. Flat — no per-day or
per-game subfolders, since manual arming means there is no game name to fold on, and a flat
timestamped directory sorts correctly in Explorer for people who never open the UI.

This replaces today's behaviour in `replay.rs::clip_path()`, which drops the file into whatever the
working directory happened to be.

### 5.2 Metadata: sidecars, not a database

```
Videos\Trix\
  20260726_143012.mp4
  20260726_143012.json
  20260726_143012.jpg
```

```json
{ "id": "20260726_143012", "title": "clip_20260726_143012",
  "created": "2026-07-26T14:30:12+03:00", "duration_ms": 20016,
  "bytes": 19812352, "width": 1920, "height": 1200, "fps": 60,
  "encoder": "NVENC H.264", "has_audio": true, "favorite": false }
```

Sidecars over a single index or SQLite, for three reasons: a crash mid-write loses one clip's
metadata rather than the library; deleting an `.mp4` in Explorer leaves an orphan that is pruned on
next scan rather than a phantom row; and SQLite means `rusqlite`, a C dependency and a ~1 MB binary
bump that is a strange price to pay when the entire engine is 1.5 MB.

The daemon scans once at startup, caches in RAM (~200 KB at a thousand clips), and updates the cache
incrementally. `library.list` never touches disk after the first scan.

Rename edits `title` only. The filename never moves — no collision logic, no broken handles.

### 5.3 Thumbnails

Generated by the daemon at clip time from the **frame at hotkey press**, taken straight from the
live D3D11 capture texture and WIC-encoded to JPEG. No decoder, no re-reading the MP4 — the frame is
already in VRAM. The thumbnail is therefore literally the moment the user pressed the button, which
is a better default than a mid-clip frame.

This requires one staging copy to CPU memory per clip (~9 MB, freed immediately) — a **deliberate,
documented exception** to Decision 1 in PLAN.md ("frames never touch the CPU"). PLAN.md gets amended
so it does not read as an accident.

### 5.4 Disk ceiling

A `max_library_gb` key (default 20). When exceeded, the daemon deletes the oldest clips not marked
`favorite`. Clip recorders are notorious for silently eating a drive; this is a few dozen lines that
prevents the most common complaint about the category.

## 6. The application

### 6.1 Shell

Persistent left rail holding the arm/disarm toggle, a ring-usage meter, and nav (Clips / Settings).
The clip grid fills the remainder.

Chosen over a chromeless full-bleed grid and over a two-pane triage layout because opening the app is
almost always "did that clip save?" — a grid answers in one glance — and because the rail gives arm
state and ring usage a permanent home instead of burying them in a title bar. It is also the only one
of the three that still looks right after favorites, filters, or game detection are added.

### 6.2 Clip view

Clicking a clip swaps the content pane for a dedicated clip page: player, full-width filmstrip trim
bar, metadata line, action row. Not a lightbox — trimming inside a floating sheet leaves no room for
the scrubber or the metadata, and traps the user in a modal.

`‹` `›` buttons flank the player with a **"3 of 47"** counter in the header, following the grid's
current sort order and dimming at the ends.

| Key | Action |
|---|---|
| `←` `→` | previous / next clip (same as the buttons) |
| `Esc` | back to the grid |
| `Space` | play / pause |
| `I` / `O` | set trim in / out at the playhead |
| `Ctrl+E` | export trimmed |
| `Del` | delete clip (confirms) |

In the **grid**, `Space` plays the selected thumbnail inline without navigating, and arrows walk the
grid. So the "did it save?" glance never costs a page load, while real work still gets a real screen.

Prev/next is a UI-side operation — the clip list is already held from `library.list`, so it just
repoints the `<video>` element. No daemon round trip.

Navigating away discards trim points silently. Trim is non-destructive: export writes a new file and
never touches the original, so there is nothing to warn about. Nothing is ever destroyed except by
`Del`.

### 6.3 Trim: two modes

A segmented control beside **Export trimmed**, persisted as a new `trim_mode` config key so it is a
real setting a third-party UI can read, not hidden UI state.

- **`fast` (default).** The filmstrip draws faint tick marks at every keyframe and the `I`/`O`
  handles snap to them, so the constraint is visible rather than surprising. Export is a stream copy:
  sub-second, lossless, no encoder involved.
- **`precise`.** Ticks disappear, handles move freely to any frame. Export decodes and re-encodes the
  range at source resolution and fps using the configured rate control.

Re-encode runs **in the daemon**, not the UI — the daemon owns Media Foundation and is the only
process that knows whether capture is live. `library.export` therefore takes a `mode`, and reports
via `export_progress` / `export_done` so the UI shows a bar and a cancel button rather than freezing.

**The replay path's GOP is pinned to 1 second** (`CODECAPI_AVEncMPVGOPSize` = fps). This caps
fast-mode snap error at ~1 s, and as a bonus makes the replay ring start closer to exactly
`replay_seconds` instead of overshooting to the previous keyframe. The bitrate cost at 8 Mbps is
small.

### 6.4 Settings

One page per config section, populated from `config.get` and written through `config.set`, with
dropdowns fed by `monitors.list` and `encoders.list` so they show real hardware rather than free
text. Keys that require a re-arm are marked as such from the `config.set` response.

The hotkey field includes a live "press it now" test. This is not decoration: NVIDIA's overlay
silently consumes Alt+F10 inside games it has hooked, which cost real debugging time on this project
already.

## 7. Daemon lifecycle

### 7.1 Arming

Manual. The user arms and disarms from the tray, the UI, or a hotkey. Rejected alternatives were
always-on-from-login (pays full RAM and continuous encode cost 24/7 including on battery, and fills
the ring with desktop footage) and automatic game detection.

Game detection is the better long-term UX and is planned for v2. It drops in behind the same
`arm`/`disarm` commands without any UI change, which is precisely why manual arming is a safe v1
choice rather than a dead end.

### 7.2 Tray

Icon reflects state — hollow when idle, filled when armed. Menu: Arm/Disarm, Open Trix, Open clips
folder, Quit. Left-click opens the UI. Closing the UI window leaves the daemon running; Quit is the
only action that stops capture.

### 7.3 Autostart

**Opt-in, off by default.** A Settings checkbox writes
`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`. Exposed through `config.get` / `config.set` as
`autostart` so third-party UIs can offer it, with the registry as the source of truth.

Adding yourself to startup uninvited is the behaviour people resent most in this category.

### 7.4 First run

No config file means the UI opens a three-step setup rather than an empty grid: pick monitor, confirm
the clip hotkey (with the live test), choose the clips folder. Then it arms and shows the grid.

## 8. Packaging

Tauri's bundler produces an MSI carrying all three binaries: `trix-ui.exe`, `trix-daemon.exe`, and
`trix.exe`. The CLI ships — it is 1.5 MB and it is the diagnostic tool when a user reports a problem.
WebView2 is included as the embedded bootstrapper so it self-heals on any machine missing it. Total
~6–8 MB.

Updates run through Tauri's updater against a GitHub Releases manifest signed with a **minisign**
key. That is update-integrity signing — it proves an update came from us and is unrelated to
Authenticode code signing below. It is free and always on. The installer stops the daemon, swaps
binaries, restarts it. Uninstall leaves clips and config alone.

**The build is not Authenticode code-signed.** This is a deliberate, accepted decision. Consequence: every user
meets a SmartScreen "Windows protected your PC" wall on first launch, and reputation cannot accrue
without a certificate. Mitigation is documentation — the download page states it plainly, shows the
exact dialog, and gives the "More info → Run anyway" path. Signing can be added later without
touching a line of code; only the installer gets re-signed.

## 9. Risks

| Risk | Mitigation |
|---|---|
| WebView2 absent on an old Win10 build | Embedded bootstrapper in the MSI, plus a runtime check with a clear message. This is the same failure class as the `BorderConfigUnsupported` bug that broke capture on Win10 19045 — assume nothing about what the OS provides. |
| Workspace refactor breaks verified engine code | The split is a file move, not a rewrite. `trix-cli` keeps its exact CLI surface, and the existing test suite plus a real `--auto-clip` artifact must pass before anything else starts. |
| `precise` export contends with the armed encoder on weak GPUs (RX 550) | Export runs in the daemon, which knows the armed state and can surface a warning. `fast` is the default and touches no encoder. |
| Named pipe excludes browser-only UIs | Accepted. Security of a user-ACL'd pipe outweighs supporting a UI form nobody has asked for. |
| Library scan slow at thousands of clips | Scan once at startup, cache in RAM, update incrementally. Measure at 5,000 clips before v1 ships. |
| Unsigned binary suppresses installs | Accepted; documented (§8). |

## 10. Verification

This does not ship as one drop. The stages below are also the delivery order, and each is gated on
the user hand-verifying it before the next begins — the same practice that carried the engine through
Phases 0–7. Stage 1 in particular must land and be verified on its own, because it touches code that
took seven phases to certify.

Following this project's established practice — the user triggers each phase and hand-verifies real
artifacts, never a claim of success:

1. **Workspace split:** `cargo test` passes and `trix replay --auto-clip 8 --exit-after 12` produces
   a clip byte-comparable to one from the current binary. No behavioural change is permitted here.
2. **Protocol:** a scripted client drives `arm` → `clip` → `library.list` → `disarm` over the pipe and
   the clip appears on disk with valid sidecars.
3. **Daemon:** tray arm/disarm works, hotkey clips while armed, `paced`/`dropped` counters match the
   CLI path, and working set stays under 30 MB while armed.
4. **UI:** clip appears in the grid within a second of the hotkey, plays, trims, exports; fast-mode
   export is lossless and sub-second, precise-mode re-encodes correctly.
5. **Cross-machine:** the AMD rigs (RX 6650 XT and RX 550, Win10 19045) install, launch, arm, clip,
   and play back — the same machines that caught the border and rate-control bugs.
6. **Game-fps regression:** League of Legends with the daemon armed and the UI closed, confirming the
   optimization work still holds with the daemon in place.

## 11. Deferred

Automatic game detection · in-game overlay · cloud upload and share links · accounts and social feed ·
frame-accurate trimming beyond `precise` mode · multi-clip timeline editing · mouse-button hotkey
binding via `WH_MOUSE_LL` · software-encoder fallback · CQP quality mode for `record`.
