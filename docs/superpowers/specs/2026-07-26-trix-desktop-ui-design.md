# Trix Desktop — UI & Daemon Architecture

**Date:** 2026-07-26
**Status:** Approved design, not yet implemented
**Supersedes nothing.** Extends [PLAN.md](../../../PLAN.md), which covers the capture engine (Phases 0–7, complete).

---

## 1. Context

The Trix engine is finished and field-verified: hardware capture, NVENC/AMF/QuickSync encode, a
20-second replay ring, hotkey clipping, and A/V mux, in a 1.5 MB binary that costs approximately
zero game fps. It is driven entirely by `trix.exe` subcommands from a terminal.

(Earlier drafts of this section claimed "under 30 MB of working set" without qualification. That
number described a process that was not capturing; see §10.1, which measures both states and sets a
ceiling for each.)

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
process that is not running during gameplay.** §10.1's ceilings bind the daemon, which is the only
thing alive while a game is — and its idle ceiling, the state it is in while you are browsing
clips, is 30 MB.

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

`id: 0` is reserved for responses to lines too malformed to yield a usable id — a request that
cannot even be parsed as `{"id":…,"cmd":…}` still gets an error response, and `0` is what it
carries back since no real id could be recovered from it. Clients must never send a request
with `id: 0`.

### 4.3 Commands

Argument shapes below use `{clip_id}`, not `{id}`: `id` is already taken by the request's own
correlation id in the same flattened object (§4.2), so a clip identifier needs a different
name on the wire.

The `Returns` column below describes the `data` object of the success response —
`{"id":…,"ok":true,"data":{…}}`. A failure is always
`{"id":…,"ok":false,"error":"…"}` with no `data`, for every command. A
third-party UI binds to these shapes, so they are published here rather than
left to be reverse-engineered out of `dispatch.rs`.

**Wrapping convention.** Three rules, applied consistently:

1. **A list is always wrapped under a name** — `{"clips":[…]}`, `{"monitors":[…]}`,
   `{"encoders":[…]}`. Never a bare array, so a client never has to tell an array
   from an object before it can parse a response, and so a list can grow siblings
   (`total`, `offset`) without changing type.
2. **A single clip is sent bare** — `clip`, `library.rename` and `library.favorite`
   put the `ClipMeta` object (§5.2) directly in `data`, with no wrapper key. It is
   already an object and already self-describing; wrapping it would buy nothing.
3. **A command with nothing to return still names what it acted on** —
   `library.delete` and `library.reveal` answer `{"clip_id":"…"}` so a UI that
   pipelined several can act on the right row without keeping its own
   request-id-to-clip table. `disarm` has no subject and answers `{}`.

| Command | Arguments | Returns (`data`) |
|---|---|---|
| `status` | — | `{armed, encoder, monitor_index, ring_seconds_used, ring_seconds_total, version, clip_dir}`. `encoder` is `null` when disarmed, and may be `null` for up to one engine tick after `arm` |
| `arm` | — | the same object `status` returns. Idempotent: re-arming an armed daemon answers normally and broadcasts no `armed` event |
| `disarm` | — | `{}`. Idempotent: disarming an idle daemon is `ok:true` and broadcasts nothing |
| `clip` | — | the `ClipMeta` (§5.2) of the clip just saved, bare. An empty ring is an *error* response, not an `ok` with no clip |
| `config.get` | — | every `Config` key, plus `clip_dir_resolved` — the absolute directory an empty `clip_dir` actually resolves to — and `config_file_exists` — whether this daemon's config file is present on disk (§7.4: no file means first run). Like `clip_dir_resolved`, `config_file_exists` is not a config key and is not settable. **`autostart` is read live from the registry, not from the config file** (§7.3): a user who deleted the `Run` entry by hand has disabled autostart whatever the file says, so the file's value is advisory and this is the answer to trust |
| `config.set` | `{<key>: <value>, …}` | `{accepted:{<key>:<value>,…}, requires_rearm:[<key>,…]}`. `accepted` is read back out of the saved config, not echoed from the request. All-or-nothing: one bad key refuses the whole request and writes nothing. **`autostart` writes the registry**, and does so *before* the config file, so a failure there refuses the whole request rather than leaving the other keys persisted |
| `library.list` | `{offset, limit}` | `{clips:[ClipMeta,…], total, offset}`, newest first. `total` is the *unpaged* count, so "3 of 47" is renderable; `offset` is echoed back |
| `library.delete` | `{clip_id}` | `{clip_id}` — removes mp4, json, and jpg |
| `library.rename` | `{clip_id, title}` | the updated `ClipMeta`, bare — edits metadata only, the filename never moves, so the id is unchanged |
| `library.favorite` | `{clip_id, favorite}` | the updated `ClipMeta`, bare |
| `library.reveal` | `{clip_id}` | `{clip_id}` — opens Explorer with the file selected |
| `library.export` | `{clip_id, dest, start_ms, end_ms, mode}` | see §6.3. **Not part of stage 4** — trim/export land with the plan that follows it; absent from stages 2, 3, and 4 |
| `monitors.list` | — | `{monitors:[{index, name, width, height, left, top, adapter},…]}`. `index` is a `config.monitor_index` value, not an ordinal |
| `encoders.list` | — | `{encoders:[{name, codec, hardware},…]}`. `hardware:false` is what a settings page warns on |
| `stats.subscribe` | `{enabled}` | `{enabled}`, echoing the state this connection is now in. Per-connection, not daemon state: two UIs may disagree. Subscribes/unsubscribes this connection to `stats` events (§4.4) |

An unknown `clip_id` and a syntactically invalid one are both plain error
responses. Clip ids are validated against `YYYYMMDD_HHMMSS[_N]` before they are
joined to any path, so a hostile id is refused at the boundary rather than
reaching the filesystem.

### 4.4 Events

`armed`, `disarmed`, `clip_saved`, `hotkey_pressed`, `hotkey_rebound`, `config_changed`,
`export_progress`, `export_done`, `error`, and `stats`.

`stats` is emitted only while a client has subscribed. This makes the existing `stats_seconds = 0`
default coherent: with no UI attached, nothing measures anything.

`hotkey_pressed` fires whenever the registered clip hotkey reaches the daemon, whether or not a
clip results, because that is the only observable that answers §6.4's live test; `hotkey_rebound`
reports whether a `config.set clip_hotkey` actually took the binding.

`config_changed` fires for every accepted `config.set`, regardless of which client sent it, carrying
the same `{accepted:{…}}` shape `config.set` answers with plus `clip_dir_resolved`. The tray's
"Change clips folder…" is the only reachable way to change `clip_dir` outside a connected UI, and
without this event an already-open app has no way to learn its asset scope has gone stale.

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
folder, Change clips folder…, Quit. Left-click opens the UI. Closing the UI window leaves the daemon
running; Quit is the only action that stops capture.

**Change clips folder…** opens the standard Windows folder picker and applies the result through
`config.set clip_dir`, so it inherits that command's validation and its all-or-nothing write. It
needs no re-arm: `clip_dir` is read per clip, so a running capture keeps its ring and the next clip
lands in the new folder. The item exists because the tray is the only surface a v1 user has —
without it, changing where clips go means hand-editing `config.toml`.

**The clip directory is proved, never assumed.** A `clip_dir` is accepted only if it can be created
*and written to* — the daemon creates it and writes a probe file. `create_dir_all` alone would
accept `C:\`, `C:\Program Files`, or a read-only share, all of which fail later at clip time, which
is the one moment a user cannot afford an error. The same check runs at `arm` (an unusable directory
refuses the arm, rather than filling a ring whose clips can never be saved) and best-effort at
startup, which is what gives a fresh install an existing `%USERPROFILE%\Videos\Trix` for "Open clips
folder" to open.

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
   CLI path, and the memory ceilings of §10.1 hold.
4. **UI:** machine-gated by `scripts/ui-smoke.ps1` — trix-ui stays off trix-core, both test suites
   pass, the two binaries share one directory, the app survives a missing daemon and reconnects on its
   own once one appears, and a second launch focuses the first instead of opening another window. A
   green run of that script is not stage 4: everything a script cannot see — the clip appearing in the
   grid within a second of the hotkey, playback, seeking, the settings round trip — is
   hand-verified. **Trim and export are not part of this stage.** They arrive with `library.export` in
   the plan after this one, which is when the rest of this line's original claim ("trims, exports;
   fast-mode export is lossless and sub-second") becomes checkable.
5. **Cross-machine:** the AMD rigs (RX 6650 XT and RX 550, Win10 19045) install, launch, arm, clip,
   and play back — the same machines that caught the border and rate-control bugs.
6. **Game-fps regression:** League of Legends with the daemon armed and the UI closed, confirming the
   optimization work still holds with the daemon in place.

### 10.1 Memory ceilings

The original contract was a single number — "under 30 MB of working set" — and it was written when
Trix was a terminal tool you ran for the length of one session. A tray daemon lives a different
life: it sits idle for hours and is armed for minutes. One number cannot describe both, and the
armed number was never achievable anyway, because hardware capture and encode mean a D3D11 device,
a Windows.Graphics.Capture frame pool, and a vendor encoder MFT — none of which Trix allocates or
can shrink.

So the ceiling splits by state. Measured on the Intel QuickSync path at the shipped defaults
(1080p60, 8000 kbps target, 15 s ring):

| State | Ceiling | Measured | What it is |
|---|---|---|---|
| **Idle in the tray** | **30 MB** working set | 10.4 MB (1.3 MB private) | The window, the pump, the pipe, the clip index. No capture stack loaded. |
| **Armed** | **200 MB** working set | 178 MB (134 MB private) | The above, plus the ring, plus the GPU vendor's capture and encode stack. |
| **Trix's own allocations, armed** | `max_bitrate_kbps × replay_seconds`, +10% | ~22 MB | The replay ring. The only armed memory this project actually controls. |

The idle ceiling is the one that carries the product's promise, and it keeps the original 30 MB
number unchanged — because idle is where the daemon spends almost all of its life. A clip tool that
costs 10 MB to leave running is the point; whether it costs 130 MB or 30 MB for the two minutes it
is armed is not what a low-end PC notices. What a low-end PC notices is game fps, and that is
gate 6's job, not this one's.

Two things this table is honest about rather than quiet about:

- **Integrated GPUs are the worst case, and that is what is measured here.** On a UMA iGPU the
  capture and encoder surfaces are carved out of system RAM and count against our working set. On
  the discrete AMD rigs of gate 5 the same surfaces live in VRAM and do not. The 200 MB ceiling is
  therefore set by the least favourable hardware Trix supports, which is also the hardware its
  audience is most likely to have.
- **The armed ceiling is a regression gate, not an achievement.** It is set close enough to the
  measurement to catch a leak or an accidental buffer, and it is why `scripts/arm-cycle-leak.ps1`
  exists alongside it: a single armed sample cannot distinguish 178 MB that is stable from 178 MB
  on its way up.

## 11. Deferred

Automatic game detection · in-game overlay · cloud upload and share links · accounts and social feed ·
frame-accurate trimming beyond `precise` mode · multi-clip timeline editing · mouse-button hotkey
binding via `WH_MOUSE_LL` · software-encoder fallback · CQP quality mode for `record`.

**Microphone capture** (deferred 2026-07-28). Capturing a second WASAPI stream is straightforward;
mixing it is the work — the mic and render devices run on independent hardware clocks that drift
against each other, mic formats are frequently 44.1 kHz mono against loopback's 48 kHz stereo, and
summing two streams needs per-source gain and limiting to avoid clipping. Writing two separate MP4
audio tracks would dodge all of that, but most players only play the first track, so a shared clip
would be missing either the voice or the game. Mixing is the right answer and it is a phase of its
own. The capture side is independent of the daemon and UI work, so the natural slot is after both
land — doing it mid-plan would mean verifying audio sync twice.
