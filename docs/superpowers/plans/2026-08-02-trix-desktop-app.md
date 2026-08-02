# Trix Desktop App Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `trix-ui` — the Tauri desktop app of spec §6 — driving the daemon entirely over the control socket, so a clip appears in a grid within a second of the hotkey, plays, and can be renamed, favorited, revealed, and deleted; plus a settings page over every config key and the three-step first run of §7.4.

**Architecture:** A fifth workspace crate, `crates/trix-ui`, holding a Tauri v2 shell whose Rust half is a named-pipe client and nothing else. A reader thread owns the socket, correlates responses by request id, and forwards daemon events into the webview; a supervisor thread reconnects with backoff and can launch `trix-daemon.exe` when the pipe is absent. The frontend is Svelte 5 + TypeScript in `crates/trix-ui/web`, and reaches the daemon only through one `trix_call` command. Clips are played and thumbnailed straight off disk through Tauri's `asset:` protocol, whose scope is extended at runtime to whatever `clip_dir` currently resolves to.

**Tech Stack:** Rust 1.97 / edition 2024 · Tauri 2.11 (`protocol-asset`, `tauri-plugin-single-instance`) · Svelte 5 + TypeScript + Vite · Vitest · WebView2 (present on this machine at 150.0.4078.105) · `cargo tauri` CLI, installed in Task 1.

## Global Constraints

Every task's requirements implicitly include this section.

- **`trix-ui` must not depend on `trix-core`** (spec §3.2). This is the plan's one non-negotiable rule. `trix-ui`'s only workspace dependency is `trix-proto`. Task 1 builds the gate that enforces it; no later task may add `trix-core`, directly or transitively, to work around a missing capability. A capability that is missing over the socket is a protocol gap to be filled in the daemon, not a reason to link the engine.
- **No panicking calls on any path reachable from the socket or the webview.** The workspace builds `panic = "abort"` in release, so an `unwrap`, `expect`, or slicing panic in the client takes the whole app down. Return `Result`. (`expect` is permitted only in `#[cfg(test)]` code and in `main`'s startup, before a window exists.)
- **ASCII-only PowerShell scripts.** PowerShell 5.1 reads a BOM-less `.ps1` as ANSI, so an em dash or a curly quote breaks parsing mid-file. Use `--`, `-`, and straight quotes.
- **Under `Set-StrictMode -Version Latest`, member enumeration over an empty array is an error.** `@().Name` throws. Use `($items | ForEach-Object { $_.Name }) -join ', '`, and remember the `Check` helper evaluates its `Detail` argument on the passing path too.
- **No Claude/Anthropic attribution in any commit.** No `Co-Authored-By` trailer. Author stays `tnhnblgl <tnhnblgl@gmail.com>`.
- **Never push.** Not to any remote, not tags, not "just the branch". The user asks for pushes explicitly, every time.
- **Daemon tests must not touch the developer's real library.** Anything that starts a real daemon does so with a scratch `%APPDATA%` and a scratch `clip_dir`, the way `scripts/daemon-smoke.ps1` already does.
- `cargo fmt` clean and zero warnings before every commit. Run `cargo fmt` after edits; it frequently needs a second pass when later edits re-wrap a line.

## What already exists — do not rebuild it

Read this before Task 1. Roughly half of what spec §6 needs is already shipped and verified.

- **Every command in §4.3 except `library.export`** is implemented and tested: `status`, `arm`, `disarm`, `clip`, `config.get`, `config.set`, `library.list/delete/rename/favorite/reveal`, `monitors.list`, `encoders.list`, `stats.subscribe`. See `crates/trix-daemon/src/dispatch.rs`.
- **Events** `armed`, `disarmed`, `clip_saved`, `error`, `stats` are broadcast today. `export_progress` / `export_done` are not — they arrive with `library.export`, which is **out of scope for this plan** (it is the next plan).
- **The 1-second GOP pin** that §6.3 requires for fast-mode trim already shipped: `crates/trix-core/src/encode/h264.rs:143`.
- **The pipe is byte-mode** (`PIPE_TYPE_BYTE | PIPE_READMODE_BYTE`, `pipe.rs:48`) with newline framing, so a client needs nothing but `std::fs::File` and `BufRead`. **No `windows` crate in `trix-ui`** for the socket.
- **Thumbnails** are already written at clip time as `<id>.jpg` beside `<id>.mp4`.
- **`ensure_writable`** already proves the clip directory at `config.set`, at `arm`, and at startup.
- **The tray** has "Open Trix" (`tray::ID_OPEN_UI`), and it currently opens the clips folder as a placeholder — `window.rs:234` maps it to `Action::OpenClipsFolder` with a comment saying "Until stage 4 ships one". Task 11 is where that comment gets deleted.

Three gaps this plan closes in the daemon, each because the app exposes them:

1. `clip_hotkey` is registered **once at startup** (`main.rs:213`) and is not in `REQUIRES_REARM`, so changing it in settings would do nothing until the next restart — a settings page that lies. Task 8.
2. `config.get` cannot say whether a config file exists, and §7.4's first run is defined as "no config file". `Config::load()` never writes one (`config.rs:137-151`), so the fact is real and only needs reporting. Task 10.
3. Nothing is logged at info level when a client connects, so the stage-4 gate cannot assert that the app reached the daemon. Task 12.

## Wire shapes the frontend binds to

Copied from the running code, not from the spec prose. These are the exact keys.

```jsonc
// status / arm  -> data
{"armed":false,"encoder":null,"monitor_index":0,"ring_seconds_used":0.0,
 "ring_seconds_total":15.0,"version":"0.2.0","clip_dir":"C:\\Users\\...\\Videos\\Trix"}

// library.list -> data
{"clips":[ClipMeta,...],"total":47,"offset":0}

// ClipMeta (also the payload of clip_saved, library.rename, library.favorite)
{"id":"20260726_143012","title":"clip_20260726_143012",
 "created":"2026-07-26T14:30:12+03:00","duration_ms":20016,"bytes":19812352,
 "width":1920,"height":1200,"fps":60,"encoder":"NVENC H.264",
 "has_audio":true,"favorite":false}

// config.get -> data : every Config key, plus:
//   "clip_dir_resolved": absolute dir an empty clip_dir resolves to (not settable)
//   "autostart": read live from the registry, not the file
// config.set -> data
{"accepted":{"fps":60},"requires_rearm":["fps"]}

// library.delete / library.reveal -> data
{"clip_id":"20260726_143012"}

// monitors.list -> {"monitors":[{"index","name","width","height","left","top","adapter"},...]}
// encoders.list -> {"encoders":[{"name","codec","hardware"},...]}

// stats event data
{"encoder":"...","monitor_index":0,"width":1920,"height":1200,"fps":60,
 "ring_seconds_used":12.5,"ring_seconds_total":15.0,"frames":750,
 "dropped":0,"paced":0,"working_set":178000000}

// error event data
{"cmd":"clip","error":"the replay ring is empty"}
```

Config keys and their bounds, mirrored from `config.rs` and `state.rs:250`:

| key | type | bounds | re-arm? |
|---|---|---|---|
| `fps` | u32 | 1–480 | yes |
| `bitrate_kbps` | u32 | 1–200000 | yes |
| `max_bitrate_kbps` | u32 | 0–200000 (0 = auto) | yes |
| `rate_control` | `"vbr"` \| `"cbr"` | — | yes |
| `replay_seconds` | u32 | 1–600 | yes |
| `monitor_index` | u32 | 0–63 | yes |
| `gpu_priority` | `"low"` \| `"normal"` | — | yes |
| `clip_hotkey` | string | `mods+key` | no (Task 8 rebinds live) |
| `stats_seconds` | u32 | 0–86400 | no |
| `clip_dir` | string | must be writable | no |
| `max_library_gb` | u32 | 0–10000 (0 = off) | no |
| `autostart` | bool | — | no |

## File Structure

```
crates/trix-ui/
  Cargo.toml              tauri, tauri-plugin-single-instance, trix-proto, serde_json, anyhow
  build.rs                tauri_build::build()
  tauri.conf.json         window, CSP, asset protocol, frontendDist/devUrl
  capabilities/default.json
  icons/                  generated by `cargo tauri icon`
  icon-source.png         generated by scripts/make-icon.ps1
  src/main.rs             entry point; wires plugins, state, commands
  src/pipe.rs             Connection: framing, id correlation, event routing  [tested]
  src/daemon.rs           discovery, launch, reconnect supervisor             [tested]
  src/commands.rs         the Tauri command surface the webview may call
  web/
    package.json  vite.config.ts  tsconfig.json  index.html
    src/main.ts
    src/App.svelte
    src/app.css
    src/lib/ipc.ts            call() + onDaemonEvent()
    src/lib/state.svelte.ts   the one app store (runes)
    src/lib/clips.ts          pure helpers                                    [tested]
    src/lib/settings.ts       the settings field table + unknownKeys()        [tested]
    src/lib/keys.ts           keyboard maps                                   [tested]
    src/views/Rail.svelte  Grid.svelte  ClipPage.svelte  Settings.svelte
    src/views/FirstRun.svelte  DaemonDown.svelte
    src/components/ClipCard.svelte  Toasts.svelte  Field.svelte
scripts/ui-isolation.ps1    the §3.2 gate
scripts/make-icon.ps1       deterministic icon source
scripts/ui-smoke.ps1        the stage-4 machine gate
```

---

### Task 1: The crate, the window, and the gate that keeps it honest

The §3.2 rule is the reason this project's protocol is worth anything, so its enforcement ships before the first line of UI. This task ends with a window that opens and a script that fails loudly if anyone ever links the engine into it.

**Files:**
- Create: `crates/trix-ui/Cargo.toml`, `build.rs`, `tauri.conf.json`, `capabilities/default.json`, `src/main.rs`
- Create: `crates/trix-ui/web/package.json`, `vite.config.ts`, `tsconfig.json`, `index.html`, `src/main.ts`, `src/App.svelte`, `src/app.css`
- Create: `scripts/make-icon.ps1`, `scripts/ui-isolation.ps1`
- Modify: `Cargo.toml` (workspace members), `.gitignore`

- [ ] **Step 1: Install the Tauri CLI**

```bash
cargo install tauri-cli --version "^2" --locked
cargo tauri --version
```

Expected: prints `tauri-cli 2.x`. This machine has Node v26.5.0, npm 11.17.0, and WebView2 runtime 150.0.4078.105 already; nothing else is needed.

- [ ] **Step 2: Add the crate to the workspace**

In the root `Cargo.toml`, extend `members`:

```toml
members = ["crates/trix-core", "crates/trix-cli", "crates/trix-daemon", "crates/trix-proto", "crates/trix-ui"]
```

Leave the `default-members` comment below it alone — it records why bare `cargo test` must keep seeing every crate.

- [ ] **Step 3: Write `crates/trix-ui/Cargo.toml`**

```toml
[package]
name = "trix-ui"
version.workspace = true
edition.workspace = true
description = "Trix desktop app"

[[bin]]
name = "trix-ui"
path = "src/main.rs"

[build-dependencies]
tauri-build = { version = "2", features = [] }

[dependencies]
# `protocol-asset` is what makes `asset:` available, and with it
# `Manager::asset_protocol_scope()` — the runtime hook Task 5 needs, because
# `clip_dir` is a config key and cannot be a static scope in tauri.conf.json.
tauri = { version = "2", features = ["protocol-asset", "tray-icon"] }
tauri-plugin-single-instance = "2"
trix-proto = { path = "../trix-proto" }
serde = { workspace = true }
serde_json = { workspace = true }
anyhow = { workspace = true }

# DELIBERATELY ABSENT: trix-core. See spec 3.2 and scripts/ui-isolation.ps1.
# The UI's every capability arrives over the control socket, which is the only
# thing that keeps that socket good enough for a third-party UI to use.
```

- [ ] **Step 4: Write `crates/trix-ui/build.rs`**

```rust
fn main() {
    tauri_build::build()
}
```

- [ ] **Step 5: Generate the icon source, then the icon set**

Write `scripts/make-icon.ps1` (ASCII only):

```powershell
# Generates crates/trix-ui/icon-source.png: the tray glyph at app-icon size.
#
# Procedural rather than a checked-in binary so the app icon and the tray icon
# cannot drift apart, and so nobody has to find a PNG editor to change it. The
# shape is the ARMED tray icon of tray.rs -- a filled disc -- on the dark plate
# the app's own background uses, because an all-white glyph is invisible on a
# light taskbar.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$side = 512
$bmp = New-Object System.Drawing.Bitmap($side, $side)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
$g.Clear([System.Drawing.Color]::Transparent)

$plate = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 22, 24, 29))
$path = New-Object System.Drawing.Drawing2D.GraphicsPath
$r = 96
$path.AddArc(0, 0, $r, $r, 180, 90)
$path.AddArc($side - $r, 0, $r, $r, 270, 90)
$path.AddArc($side - $r, $side - $r, $r, $r, 0, 90)
$path.AddArc(0, $side - $r, $r, $r, 90, 90)
$path.CloseFigure()
$g.FillPath($plate, $path)

$disc = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 255, 255, 255))
$inset = 140
$g.FillEllipse($disc, $inset, $inset, $side - 2 * $inset, $side - 2 * $inset)

$out = Join-Path $PSScriptRoot '..\crates\trix-ui\icon-source.png'
$bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
Write-Host "wrote $out"
```

Run:

```bash
powershell -ExecutionPolicy Bypass -File scripts/make-icon.ps1
cargo tauri icon crates/trix-ui/icon-source.png --output crates/trix-ui/icons
```

Expected: `crates/trix-ui/icons/` fills with `32x32.png`, `128x128.png`, `icon.ico`, `icon.png` and friends.

- [ ] **Step 6: Write `crates/trix-ui/tauri.conf.json`**

```json
{
  "$schema": "https://schema.tauri.app/config/2",
  "productName": "Trix",
  "version": "0.2.0",
  "identifier": "dev.trix.desktop",
  "build": {
    "frontendDist": "web/dist",
    "devUrl": "http://localhost:1420",
    "beforeDevCommand": "npm run dev",
    "beforeBuildCommand": "npm run build"
  },
  "app": {
    "windows": [
      {
        "label": "main",
        "title": "Trix",
        "width": 1180,
        "height": 760,
        "minWidth": 880,
        "minHeight": 560,
        "resizable": true,
        "visible": true
      }
    ],
    "security": {
      "csp": "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' asset: http://asset.localhost data:; media-src 'self' asset: http://asset.localhost; connect-src 'self' ipc: http://ipc.localhost",
      "assetProtocol": {
        "enable": true,
        "scope": []
      }
    }
  },
  "bundle": {
    "active": false,
    "icon": ["icons/32x32.png", "icons/128x128.png", "icons/icon.ico"]
  }
}
```

Note there is no `--prefix web` on either hook: the Tauri CLI already runs both `beforeDevCommand` and `beforeBuildCommand` with the working directory set to the frontend directory it derives from `frontendDist` (here `crates/trix-ui/web`), so adding `--prefix web` would resolve to the nonexistent `web/web`.

Two things here are load-bearing and easy to get wrong:

- `assetProtocol.scope` is **empty on purpose**. The clip directory is a config key the user can change from the tray, so it cannot be written down here. Task 5 extends the same scope at runtime with `allow_directory`, which is exactly what the empty starting scope is for. Do not "fix" this by putting `"**"` in it — that would hand the webview the whole filesystem.
- `media-src` must list `asset:` and `http://asset.localhost` or the `<video>` tag silently plays nothing.
- `"bundle": {"active": false}` — this plan ships a dev/release build, not an installer. The MSI, updater, and minisign signing of spec §8 are their own plan.

- [ ] **Step 7: Write `crates/trix-ui/capabilities/default.json`**

```json
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "default",
  "description": "Trix's main window. Everything it can do to the machine, it does through the control socket; these are only the core APIs the shell itself needs.",
  "windows": ["main"],
  "permissions": ["core:default"]
}
```

- [ ] **Step 8: Write the minimal `crates/trix-ui/src/main.rs`**

```rust
//! `trix-ui`: the Trix desktop app.
//!
//! Its Rust half is a named-pipe client and a window, and that is the whole
//! design. Every capability this app has — arming, clipping, listing, deleting,
//! settings — arrives over the control socket, because spec §3.2 forbids this
//! crate from depending on `trix-core`. That rule is not stylistic: it is the
//! only thing that keeps the protocol honest enough for a UI somebody else
//! writes to be a first-class client.

// No console window behind the app in release. Debug keeps one, because
// `tracing`-style eprintln debugging of the socket is the whole reason to run
// a debug build.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("the Tauri runtime failed to start");
}
```

The `expect` is permitted here by the Global Constraints: it is `main`, before a window exists, and there is no window to report into.

- [ ] **Step 9: Scaffold the frontend**

```bash
mkdir -p crates/trix-ui/web/src/lib crates/trix-ui/web/src/views crates/trix-ui/web/src/components
cd crates/trix-ui/web
npm init -y
npm install -D vite @sveltejs/vite-plugin-svelte svelte typescript svelte-check @tsconfig/svelte vitest
npm install @tauri-apps/api
```

Versions are whatever npm resolves; `package-lock.json` is the record and gets committed. Then replace the generated `package.json` with:

```json
{
  "name": "trix-web",
  "private": true,
  "type": "module",
  "scripts": {
    "dev": "vite",
    "build": "vite build",
    "check": "svelte-check --tsconfig ./tsconfig.json",
    "test": "vitest run"
  }
}
```

(Keep the `dependencies` and `devDependencies` blocks npm just wrote.)

- [ ] **Step 10: Write the frontend config files**

`crates/trix-ui/web/vite.config.ts`:

```ts
import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

export default defineConfig({
  plugins: [svelte()],
  // Fixed port: tauri.conf.json's devUrl points at it, and a port that moves
  // when 1420 is busy would leave `cargo tauri dev` staring at a blank window.
  server: { port: 1420, strictPort: true },
  build: { target: 'esnext', emptyOutDir: true },
});
```

`crates/trix-ui/web/tsconfig.json`:

```json
{
  "extends": "@tsconfig/svelte/tsconfig.json",
  "compilerOptions": {
    "target": "ESNext",
    "module": "ESNext",
    "moduleResolution": "bundler",
    "strict": true,
    "types": ["vite/client"]
  },
  "include": ["src/**/*.ts", "src/**/*.svelte"]
}
```

`crates/trix-ui/web/index.html`:

```html
<!doctype html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>Trix</title>
  </head>
  <body>
    <div id="app"></div>
    <script type="module" src="/src/main.ts"></script>
  </body>
</html>
```

`crates/trix-ui/web/src/main.ts`:

```ts
import { mount } from 'svelte';
import App from './App.svelte';
import './app.css';

export default mount(App, { target: document.getElementById('app')! });
```

`crates/trix-ui/web/src/App.svelte`:

```svelte
<main class="boot">
  <h1>Trix</h1>
</main>

<style>
  .boot {
    display: grid;
    place-items: center;
    height: 100vh;
  }
</style>
```

`crates/trix-ui/web/src/app.css`:

```css
:root {
  --bg: #16181d;
  --panel: #1e2128;
  --line: #2b2f39;
  --text: #e8eaee;
  --dim: #99a0ad;
  --accent: #4a9eff;
  --danger: #ff5c5c;
  color-scheme: dark;
}

* { box-sizing: border-box; }

body {
  margin: 0;
  background: var(--bg);
  color: var(--text);
  font: 14px/1.45 "Segoe UI Variable Text", "Segoe UI", system-ui, sans-serif;
  user-select: none;
}
```

- [ ] **Step 11: Write the §3.2 gate, `scripts/ui-isolation.ps1`**

```powershell
# Spec 3.2: trix-ui must not depend on trix-core, directly or transitively.
#
# This is the only automated thing standing between the published control
# protocol and a first-party UI that quietly grows privileged access. Once the
# UI can call into the engine, every gap in the protocol stops being a bug
# somebody has to fix and starts being an inconvenience somebody can route
# around -- and a third-party UI, which has no such shortcut, becomes a
# second-class client. The rule is cheap to keep and impossible to restore
# after it has been broken for a release.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Push-Location (Join-Path $PSScriptRoot '..')
try {
    # -e no-dev: a dev-dependency on the engine in a test would be a different
    # (and much smaller) problem than the shipped binary linking it.
    $tree = & cargo tree -p trix-ui -e no-dev 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-Host "FAIL: cargo tree failed"
        $tree | ForEach-Object { Write-Host "  $_" }
        exit 1
    }

    $offenders = @($tree | Where-Object { $_ -match '\btrix-core\b' })
    if ($offenders.Count -gt 0) {
        Write-Host "FAIL: trix-ui depends on trix-core (spec 3.2)"
        # ForEach-Object rather than .Trim() over the array: under
        # Set-StrictMode member enumeration on an empty array is an error, and
        # this branch is one edit away from being reachable with none.
        $offenders | ForEach-Object { Write-Host "  $($_.Trim())" }
        exit 1
    }

    Write-Host "OK: trix-ui does not depend on trix-core"
    exit 0
}
finally {
    Pop-Location
}
```

- [ ] **Step 12: Prove the gate can fail**

Temporarily add to `crates/trix-ui/Cargo.toml` under `[dependencies]`:

```toml
trix-core = { path = "../trix-core" }
```

Run:

```bash
powershell -ExecutionPolicy Bypass -File scripts/ui-isolation.ps1
```

Expected: `FAIL: trix-ui depends on trix-core (spec 3.2)` and exit code 1. **Now remove the line again** and re-run; expected `OK: trix-ui does not depend on trix-core`. A gate never observed failing is not a gate.

- [ ] **Step 13: Ignore the build output**

Append to `.gitignore`:

```
/crates/trix-ui/web/node_modules
/crates/trix-ui/web/dist
/crates/trix-ui/gen
```

- [ ] **Step 14: Build and look at it**

```bash
cargo tauri build --no-bundle --config crates/trix-ui/tauri.conf.json
```

Or, from `crates/trix-ui`, `cargo tauri dev`. Expected: a dark window titled "Trix" saying "Trix". Also run the full suite to confirm nothing regressed:

```bash
cargo test --workspace
powershell -ExecutionPolicy Bypass -File scripts/ui-isolation.ps1
```

Expected: 123 passed / 0 failed, and `OK: trix-ui does not depend on trix-core`.

- [ ] **Step 15: Commit**

```bash
git add crates/trix-ui Cargo.toml .gitignore scripts/ui-isolation.ps1 scripts/make-icon.ps1
git commit -m "feat(ui): add the trix-ui crate and the gate that keeps it off trix-core"
```

---

### Task 2: The control-socket client

**Files:**
- Create: `crates/trix-ui/src/pipe.rs`
- Modify: `crates/trix-ui/src/main.rs` (add `mod pipe;`)

**Interfaces:**
- Consumes: `trix_proto::{Request, Response, Event, encode_line}`; the byte-mode pipe at `\\.\pipe\trix-control`.
- Produces: `Connection::start(reader, writer, on_event, on_close) -> Result<Arc<Connection>>`, `Connection::call(cmd, args, timeout) -> Result<Value, String>`, `Connection::close()`. Task 3 owns the pipe handles and passes them in.

- [ ] **Step 1: Write the failing tests**

Create `crates/trix-ui/src/pipe.rs` with the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::time::Duration;

    const FAST: Duration = Duration::from_secs(2);

    /// A `Read` fed line-by-line from a channel, so a test can hold the
    /// connection open and answer out of order — which a `Cursor` cannot do,
    /// and which is the entire behaviour under test.
    struct ChanReader {
        rx: Receiver<Vec<u8>>,
        buf: Vec<u8>,
    }

    impl Read for ChanReader {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            if self.buf.is_empty() {
                match self.rx.recv() {
                    Ok(bytes) => self.buf = bytes,
                    // Sender dropped: EOF, which is how a daemon exit looks.
                    Err(_) => return Ok(0),
                }
            }
            let n = out.len().min(self.buf.len());
            out[..n].copy_from_slice(&self.buf[..n]);
            self.buf.drain(..n);
            Ok(n)
        }
    }

    /// A `Write` that publishes every line it is given, so a test can see the
    /// exact bytes the client put on the wire.
    struct ChanWriter(Sender<String>);

    impl std::io::Write for ChanWriter {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            let _ = self.0.send(String::from_utf8_lossy(data).into_owned());
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    struct Harness {
        conn: Arc<Connection>,
        to_client: Sender<Vec<u8>>,
        sent: Receiver<String>,
        events: Receiver<Event>,
        closed: Receiver<()>,
    }

    fn harness() -> Harness {
        let (to_client, rx) = channel();
        let (tx_sent, sent) = channel();
        let (tx_event, events) = channel();
        let (tx_closed, closed) = channel();
        let conn = Connection::start(
            BufReader::new(ChanReader { rx, buf: Vec::new() }),
            ChanWriter(tx_sent),
            move |event| {
                let _ = tx_event.send(event);
            },
            move || {
                let _ = tx_closed.send(());
            },
        )
        .expect("the connection should start");
        Harness { conn, to_client, sent, events, closed }
    }

    #[test]
    fn a_call_writes_one_line_and_returns_the_matching_response() {
        let h = harness();
        let conn = Arc::clone(&h.conn);
        let call = std::thread::spawn(move || conn.call("status", Map::new(), FAST));

        let line = h.sent.recv_timeout(FAST).expect("the client should write a request");
        assert!(line.ends_with('\n'), "every request is one newline-terminated line: {line:?}");
        let request: Value = serde_json::from_str(line.trim_end()).expect("valid JSON");
        assert_eq!(request["cmd"], "status");
        let id = request["id"].as_u64().expect("a request carries an id");
        assert_ne!(id, trix_proto::RESERVED_ID, "id 0 is reserved for unparseable lines");

        h.to_client
            .send(format!("{{\"id\":{id},\"ok\":true,\"data\":{{\"armed\":true}}}}\n").into_bytes())
            .expect("send the response");
        let data = call.join().expect("the call thread should finish").expect("ok response");
        assert_eq!(data["armed"], true);
    }

    /// The reason responses carry an id at all. A UI that fires `library.list`
    /// and `status` together must not be able to receive the wrong answer.
    #[test]
    fn responses_are_matched_by_id_not_by_arrival_order() {
        let h = harness();
        let a = Arc::clone(&h.conn);
        let first = std::thread::spawn(move || a.call("library.list", Map::new(), FAST));
        let line_a = h.sent.recv_timeout(FAST).expect("first request");
        let id_a = serde_json::from_str::<Value>(line_a.trim_end()).unwrap()["id"].as_u64().unwrap();

        let b = Arc::clone(&h.conn);
        let second = std::thread::spawn(move || b.call("status", Map::new(), FAST));
        let line_b = h.sent.recv_timeout(FAST).expect("second request");
        let id_b = serde_json::from_str::<Value>(line_b.trim_end()).unwrap()["id"].as_u64().unwrap();
        assert_ne!(id_a, id_b, "each request gets a fresh id");

        // Answered backwards on purpose.
        h.to_client
            .send(format!("{{\"id\":{id_b},\"ok\":true,\"data\":{{\"who\":\"status\"}}}}\n").into_bytes())
            .unwrap();
        h.to_client
            .send(format!("{{\"id\":{id_a},\"ok\":true,\"data\":{{\"who\":\"list\"}}}}\n").into_bytes())
            .unwrap();

        assert_eq!(second.join().unwrap().unwrap()["who"], "status");
        assert_eq!(first.join().unwrap().unwrap()["who"], "list");
    }

    #[test]
    fn an_error_response_becomes_the_error_text() {
        let h = harness();
        let conn = Arc::clone(&h.conn);
        let call = std::thread::spawn(move || conn.call("clip", Map::new(), FAST));
        let line = h.sent.recv_timeout(FAST).unwrap();
        let id = serde_json::from_str::<Value>(line.trim_end()).unwrap()["id"].as_u64().unwrap();

        h.to_client
            .send(format!("{{\"id\":{id},\"ok\":false,\"error\":\"the replay ring is empty\"}}\n").into_bytes())
            .unwrap();
        let err = call.join().unwrap().expect_err("ok:false is an error");
        assert_eq!(err, "the replay ring is empty");
    }

    #[test]
    fn events_reach_the_sink_and_do_not_disturb_pending_calls() {
        let h = harness();
        let conn = Arc::clone(&h.conn);
        let call = std::thread::spawn(move || conn.call("status", Map::new(), FAST));
        let line = h.sent.recv_timeout(FAST).unwrap();
        let id = serde_json::from_str::<Value>(line.trim_end()).unwrap()["id"].as_u64().unwrap();

        h.to_client
            .send(b"{\"event\":\"armed\",\"data\":{\"armed\":true}}\n".to_vec())
            .unwrap();
        let event = h.events.recv_timeout(FAST).expect("the event should be routed");
        assert_eq!(event.event, "armed");

        h.to_client.send(format!("{{\"id\":{id},\"ok\":true,\"data\":{{}}}}\n").into_bytes()).unwrap();
        assert!(call.join().unwrap().is_ok(), "the pending call survived an interleaved event");
    }

    /// A daemon that sent one bad line must not cost the app its socket: the
    /// window would go to "daemon not running" while the daemon is right there.
    #[test]
    fn a_junk_line_is_skipped_rather_than_killing_the_reader() {
        let h = harness();
        h.to_client.send(b"this is not json\n".to_vec()).unwrap();
        h.to_client.send(b"{\"event\":\"disarmed\",\"data\":{}}\n".to_vec()).unwrap();
        let event = h.events.recv_timeout(FAST).expect("the reader kept going");
        assert_eq!(event.event, "disarmed");
    }

    #[test]
    fn a_timed_out_call_errors_and_forgets_its_slot() {
        let h = harness();
        let conn = Arc::clone(&h.conn);
        let err = conn
            .call("status", Map::new(), Duration::from_millis(50))
            .expect_err("no response was ever sent");
        assert!(err.contains("timed out"), "the error should say what happened: {err}");
        assert_eq!(conn.pending_len(), 0, "a timed-out call must not leak its slot");
    }

    #[test]
    fn eof_closes_the_connection_and_fails_calls_fast() {
        let h = harness();
        drop(h.to_client); // the daemon exited
        h.closed.recv_timeout(FAST).expect("on_close should fire on EOF");
        let err = h
            .conn
            .call("status", Map::new(), FAST)
            .expect_err("a closed connection cannot carry a call");
        assert!(err.contains("not connected"), "{err}");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p trix-ui`
Expected: FAIL — `cannot find type Connection in this scope`.

- [ ] **Step 3: Write the implementation**

Above the test module in `crates/trix-ui/src/pipe.rs`:

```rust
//! The control-socket client: framing, request/response correlation, and the
//! reader thread that routes daemon events.
//!
//! Deliberately transport-agnostic. `start` takes any `BufRead` and any
//! `Write`, which is what lets the tests below drive every failure mode —
//! out-of-order responses, junk lines, EOF mid-call — without a daemon, a
//! pipe, or a timing assumption. `daemon.rs` is the only thing that knows
//! `\\.\pipe\trix-control` exists.
//!
//! No `windows` crate anywhere in here: the daemon's pipe is byte-mode
//! (`PIPE_TYPE_BYTE | PIPE_READMODE_BYTE`) with newline framing, so a plain
//! `File` opened on the pipe path is a complete client.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result};
use serde_json::{Map, Value};
use trix_proto::{Event, Request, Response, encode_line};

/// How long any one command may take before the caller is told it did not
/// answer. Matches `scripts/daemon-smoke.ps1`'s own 60 s ceiling, and it has
/// to be generous: `arm` builds a capture session and `clip` muxes an MP4.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// One live connection to the daemon.
///
/// Requests may be issued from any thread; each blocks its own caller until
/// the matching id comes back, and nothing serializes them behind each other
/// except the brief lock taken to write one line.
pub struct Connection {
    writer: Mutex<Box<dyn Write + Send>>,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, SyncSender<Response>>>,
    closed: AtomicBool,
}

impl Connection {
    /// Starts the reader thread and returns the connection.
    ///
    /// `on_event` is called for every unsolicited event; `on_close` fires
    /// exactly once, when the socket ends, and is how `daemon.rs` learns to
    /// start reconnecting.
    pub fn start<R, W>(
        reader: R,
        writer: W,
        on_event: impl Fn(Event) + Send + 'static,
        on_close: impl FnOnce() + Send + 'static,
    ) -> Result<Arc<Self>>
    where
        R: BufRead + Send + 'static,
        W: Write + Send + 'static,
    {
        let connection = Arc::new(Self {
            writer: Mutex::new(Box::new(writer)),
            // Starts at 1: `RESERVED_ID` (0) is the daemon's answer to a line
            // too malformed to recover an id from, and a client must never
            // send it.
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
        });

        // Weak, so the reader thread cannot keep a dead connection alive: when
        // the supervisor drops its Arc the thread's next line finds nothing to
        // deliver to and exits.
        let weak = Arc::downgrade(&connection);
        std::thread::Builder::new()
            .name("trix-pipe-reader".into())
            .spawn(move || {
                let mut reader = reader;
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                    let Some(connection) = weak.upgrade() else { break };
                    connection.deliver(line.trim_end(), &on_event);
                }
                if let Some(connection) = weak.upgrade() {
                    connection.close();
                }
                on_close();
            })
            .context("could not start the socket reader thread")?;

        Ok(connection)
    }

    /// Sends one command and waits for its answer.
    ///
    /// `Err` is the daemon's own error text where there is one, so it can be
    /// shown to the user verbatim — the daemon writes those messages for
    /// people, and rewording them here would only make them worse.
    pub fn call(&self, cmd: &str, args: Map<String, Value>, timeout: Duration) -> Result<Value, String> {
        if self.closed.load(Ordering::Acquire) {
            return Err("not connected to the Trix daemon".to_string());
        }

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = Request { id, cmd: cmd.to_string(), args };
        let line = encode_line(&request).map_err(|e| format!("could not encode {cmd}: {e}"))?;

        // Depth 1: exactly one response is ever sent for one id, so the
        // reader must never block here even if this caller has already
        // walked away after a timeout.
        let (tx, rx) = sync_channel(1);
        match self.pending.lock() {
            Ok(mut pending) => {
                pending.insert(id, tx);
            }
            Err(_) => return Err("the socket client is poisoned".to_string()),
        }

        let written = match self.writer.lock() {
            Ok(mut writer) => writer.write_all(line.as_bytes()).and_then(|()| writer.flush()),
            Err(_) => {
                self.forget(id);
                return Err("the socket client is poisoned".to_string());
            }
        };
        if let Err(e) = written {
            self.forget(id);
            return Err(format!("could not send {cmd}: {e}"));
        }

        let answer = rx.recv_timeout(timeout);
        // Unconditional: on the success path the slot is already gone, and
        // removing it twice is free. On every failure path leaving it behind
        // would leak one entry per timed-out call for the life of the app.
        self.forget(id);

        match answer {
            Ok(response) if response.ok => Ok(response.data.unwrap_or(Value::Null)),
            Ok(response) => Err(response.error.unwrap_or_else(|| format!("{cmd} failed"))),
            Err(_) if self.closed.load(Ordering::Acquire) => {
                Err("not connected to the Trix daemon".to_string())
            }
            Err(_) => Err(format!("{cmd} timed out after {}s", timeout.as_secs())),
        }
    }

    /// Marks the connection dead and fails every waiting call.
    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        // Dropping the senders wakes every blocked `recv_timeout` at once
        // rather than making each caller serve out its own timeout.
        if let Ok(mut pending) = self.pending.lock() {
            pending.clear();
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// One line off the wire: a response for a waiting call, an event, or
    /// neither.
    ///
    /// "Neither" is dropped rather than propagated. A daemon that emitted one
    /// unparseable line has a bug worth fixing, but tearing the socket down
    /// over it would put the app in "daemon not running" while the daemon is
    /// running fine — the worse of the two failures by a distance.
    fn deliver(&self, line: &str, on_event: &impl Fn(Event)) {
        if line.is_empty() {
            return;
        }
        if let Ok(response) = serde_json::from_str::<Response>(line) {
            if let Ok(mut pending) = self.pending.lock() {
                if let Some(tx) = pending.remove(&response.id) {
                    let _ = tx.try_send(response);
                }
            }
            return;
        }
        if let Ok(event) = serde_json::from_str::<Event>(line) {
            on_event(event);
        }
    }

    fn forget(&self, id: u64) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&id);
        }
    }

    #[cfg(test)]
    fn pending_len(&self) -> usize {
        self.pending.lock().map(|p| p.len()).unwrap_or(0)
    }
}
```

Add `mod pipe;` to `crates/trix-ui/src/main.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p trix-ui`
Expected: PASS, 7 tests.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add crates/trix-ui/src
git commit -m "feat(ui): add the control-socket client with id correlation and event routing"
```

---

### Task 3: Finding, launching, and reconnecting to the daemon

Spec §4.5: "`trix-ui` attempts to connect on launch; finding nothing, it offers to start the daemon rather than erroring. Lost connections retry with backoff behind a visible 'daemon not running' state."

**Files:**
- Create: `crates/trix-ui/src/daemon.rs`, `crates/trix-ui/src/commands.rs`
- Modify: `crates/trix-ui/src/main.rs`

**Interfaces:**
- Consumes: `pipe::Connection`.
- Produces: `daemon::Supervisor` (managed Tauri state) with `call`, `connection`, `launch`; the Tauri commands `trix_call`, `start_daemon`, `daemon_connected`; the webview events `trix-connected`, `trix-disconnected`, `trix-event`.

- [ ] **Step 1: Write the failing tests**

Create `crates/trix-ui/src/daemon.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_daemon_is_looked_for_beside_this_exe() {
        let ui = Path::new(r"C:\Program Files\Trix\trix-ui.exe");
        assert_eq!(
            daemon_path_beside(ui),
            Some(PathBuf::from(r"C:\Program Files\Trix\trix-daemon.exe")),
            "installed layout: spec §8 puts all three binaries in one directory"
        );

        let dev = Path::new(r"C:\src\trix\target\debug\trix-ui.exe");
        assert_eq!(
            daemon_path_beside(dev),
            Some(PathBuf::from(r"C:\src\trix\target\debug\trix-daemon.exe")),
            "cargo puts both binaries in the same profile directory, so dev needs no special case"
        );

        assert_eq!(daemon_path_beside(Path::new("trix-ui.exe")), None, "no parent, no guess");
    }

    /// Backoff exists so a daemon the user never intends to start does not
    /// cost a reconnect attempt every frame, and so one that is restarting is
    /// picked up quickly rather than after a fixed long wait.
    #[test]
    fn backoff_grows_from_prompt_to_patient_and_stops_there() {
        let mut delay = FIRST_RETRY;
        let mut seen = vec![delay];
        for _ in 0..8 {
            delay = next_retry(delay);
            seen.push(delay);
        }
        assert_eq!(seen[0], Duration::from_millis(400), "the first retry is quick");
        assert!(seen[1] > seen[0] && seen[2] > seen[1], "it must actually back off");
        assert!(
            seen.iter().all(|d| *d <= MAX_RETRY),
            "and it must never exceed the cap: {seen:?}"
        );
        assert_eq!(*seen.last().expect("non-empty"), MAX_RETRY, "it settles at the cap");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p trix-ui daemon`
Expected: FAIL — `cannot find function daemon_path_beside`.

- [ ] **Step 3: Write the implementation**

Above the tests in `crates/trix-ui/src/daemon.rs`:

```rust
//! Finding the daemon, connecting to it, launching it, and reconnecting when
//! it goes away.
//!
//! The app is useless without the daemon and must never look broken because of
//! it: spec §4.5 says a missing daemon is an offer to start one, not an error
//! dialog. So this module always has an answer — connected, or connecting, or
//! "not running" with a button — and never a stack trace.

use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Map, Value};
use tauri::{AppHandle, Emitter as _, Manager as _};

use crate::pipe::{CALL_TIMEOUT, Connection};

/// The daemon's published socket. Byte-mode, newline-framed, ACL'd to the
/// current user (spec §4.1) — a plain `File` open is a complete client.
const PIPE_PATH: &str = r"\\.\pipe\trix-control";

/// First reconnect delay. Short enough that the app is back before the user
/// has read the "not running" panel when the daemon merely restarted.
pub(crate) const FIRST_RETRY: Duration = Duration::from_millis(400);
/// Ceiling on the reconnect delay. A daemon the user has no intention of
/// starting must not cost a syscall every frame forever, and 5 s is still
/// quick enough that starting it from the tray feels instant here.
pub(crate) const MAX_RETRY: Duration = Duration::from_secs(5);

pub(crate) fn next_retry(current: Duration) -> Duration {
    (current * 2).min(MAX_RETRY)
}

/// Where `trix-daemon.exe` lives, given this executable's path.
///
/// Beside us, always: spec §8 ships all three binaries in one directory, and
/// cargo puts them in one profile directory too, so the installed and the
/// development layouts need the same single rule.
pub(crate) fn daemon_path_beside(ui_exe: &Path) -> Option<PathBuf> {
    Some(ui_exe.parent()?.join("trix-daemon.exe"))
}

/// Owns the current connection and the thread that keeps trying to make one.
pub struct Supervisor {
    app: AppHandle,
    current: Mutex<Option<Arc<Connection>>>,
}

impl Supervisor {
    pub fn start(app: AppHandle) -> Arc<Self> {
        let supervisor = Arc::new(Self { app, current: Mutex::new(None) });
        let worker = Arc::clone(&supervisor);
        // If this thread cannot start there is nothing useful left to do, but
        // there is still a window: it stays on the "not running" panel, whose
        // Start button calls `launch` directly.
        let _ = std::thread::Builder::new()
            .name("trix-daemon-supervisor".into())
            .spawn(move || worker.run());
        supervisor
    }

    fn run(&self) {
        let mut delay = FIRST_RETRY;
        loop {
            match self.connect() {
                Ok(connection) => {
                    delay = FIRST_RETRY;
                    self.on_connected(&connection);
                    // Park until the reader thread reports the socket gone.
                    while !connection.is_closed() {
                        std::thread::sleep(Duration::from_millis(120));
                    }
                    if let Ok(mut current) = self.current.lock() {
                        *current = None;
                    }
                    // No `trix-disconnected` here: the reader thread's
                    // `on_close` already emitted one for this same drop, up to
                    // a poll interval earlier. Emitting again would double
                    // every disconnect the frontend sees, which is fine for a
                    // flag and wrong for anything counted or shown once.
                }
                Err(()) => {
                    std::thread::sleep(delay);
                    delay = next_retry(delay);
                }
            }
        }
    }

    fn connect(&self) -> Result<Arc<Connection>, ()> {
        // Read+write on the same handle, then a duplicate for the reader: two
        // handles to one pipe instance are safe to use concurrently from
        // different threads, and it is the only way to read and write at once
        // without overlapped I/O.
        let write_half = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(PIPE_PATH)
            .map_err(|_| ())?;
        let read_half = write_half.try_clone().map_err(|_| ())?;

        let app = self.app.clone();
        let closed_app = self.app.clone();
        let connection = Connection::start(
            BufReader::new(read_half),
            write_half,
            move |event| {
                // One channel for every daemon event; the frontend switches on
                // `event`. A Tauri event per protocol event would mean a
                // listener to register for each, and a silent miss whenever the
                // daemon grows one.
                let _ = app.emit("trix-event", event);
            },
            move || {
                let _ = closed_app.emit("trix-disconnected", ());
            },
        )
        .map_err(|_| ())?;

        if let Ok(mut current) = self.current.lock() {
            *current = Some(Arc::clone(&connection));
        }
        Ok(connection)
    }

    /// Announces the connection, and opens the asset scope onto wherever clips
    /// currently live.
    ///
    /// The scope has to be opened here rather than in `tauri.conf.json`
    /// because `clip_dir` is a config key the user can change from the tray at
    /// any time; a static scope would be a guess that goes stale the first
    /// time they do.
    fn on_connected(&self, connection: &Arc<Connection>) {
        if let Ok(status) = connection.call("status", Map::new(), CALL_TIMEOUT) {
            if let Some(dir) = status.get("clip_dir").and_then(Value::as_str) {
                self.allow_clip_dir(dir);
            }
            let _ = self.app.emit("trix-connected", status);
        } else {
            let _ = self.app.emit("trix-connected", Value::Null);
        }
    }

    /// Lets the webview load `<clip_dir>\*.mp4` and `*.jpg` through `asset:`.
    ///
    /// Non-recursive: the library is flat by design (spec §5.1), so the
    /// directory's own children are exactly the grant needed and subdirectories
    /// are not this app's business.
    pub fn allow_clip_dir(&self, dir: &str) {
        let _ = self.app.asset_protocol_scope().allow_directory(dir, false);
    }

    pub fn connection(&self) -> Option<Arc<Connection>> {
        self.current.lock().ok().and_then(|c| c.clone())
    }

    pub fn is_connected(&self) -> bool {
        self.connection().is_some_and(|c| !c.is_closed())
    }

    pub fn call(&self, cmd: &str, args: Map<String, Value>) -> Result<Value, String> {
        let connection = self.connection().ok_or("not connected to the Trix daemon")?;
        connection.call(cmd, args, CALL_TIMEOUT)
    }

    /// Starts `trix-daemon.exe`. The supervisor's own retry loop picks the
    /// socket up; this does not wait for it.
    pub fn launch(&self) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| format!("could not locate trix-ui.exe: {e}"))?;
        let daemon = daemon_path_beside(&exe).ok_or("could not work out where trix-daemon.exe is")?;
        if !daemon.exists() {
            return Err(format!("trix-daemon.exe is not beside the app at {}", daemon.display()));
        }

        // CREATE_NO_WINDOW: the daemon is a console-subsystem binary, so
        // spawning it from a windowed app flashes a console on screen and then
        // hands it a window nobody asked for.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        use std::os::windows::process::CommandExt as _;
        std::process::Command::new(&daemon)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| format!("could not start the Trix daemon: {e}"))?;
        Ok(())
    }
}
```

- [ ] **Step 4: Write the command surface**

Create `crates/trix-ui/src/commands.rs`:

```rust
//! Everything the webview is allowed to ask the Rust half to do.
//!
//! Three commands, and two of them are about the daemon being absent. There is
//! deliberately no per-protocol-command wrapper: `trix_call` is a straight
//! pass-through, so a command added to the daemon is usable from the frontend
//! the same day without touching this file. The daemon validates its own
//! arguments and says so in words meant for a person; re-validating here would
//! only produce a second, worse message.

use std::sync::Arc;

use serde_json::{Map, Value};
use tauri::State;

use crate::daemon::Supervisor;

#[tauri::command]
pub async fn trix_call(
    supervisor: State<'_, Arc<Supervisor>>,
    cmd: String,
    args: Map<String, Value>,
) -> Result<Value, String> {
    let supervisor = Arc::clone(&supervisor);
    // spawn_blocking, because `call` parks until the daemon answers and an
    // `arm` takes seconds: doing that on a runtime worker would stall every
    // other command behind it.
    tauri::async_runtime::spawn_blocking(move || {
        let result = supervisor.call(&cmd, args);
        // A clip directory change has to reach the asset scope or the grid
        // renders broken thumbnails from a directory the webview may not read.
        if result.is_ok() && cmd == "config.set" {
            if let Ok(status) = supervisor.call("status", Map::new()) {
                if let Some(dir) = status.get("clip_dir").and_then(Value::as_str) {
                    supervisor.allow_clip_dir(dir);
                }
            }
        }
        result
    })
    .await
    .map_err(|e| format!("the call could not be scheduled: {e}"))?
}

#[tauri::command]
pub fn start_daemon(supervisor: State<'_, Arc<Supervisor>>) -> Result<(), String> {
    supervisor.launch()
}

#[tauri::command]
pub fn daemon_connected(supervisor: State<'_, Arc<Supervisor>>) -> bool {
    supervisor.is_connected()
}
```

- [ ] **Step 5: Wire it into `main.rs`**

Replace the body of `crates/trix-ui/src/main.rs`'s `main`:

```rust
mod commands;
mod daemon;
mod pipe;

use tauri::Manager as _;

fn main() {
    tauri::Builder::default()
        // Single instance first, before anything expensive: the second copy's
        // whole job is to hand focus to the first and exit. The tray's "Open
        // Trix" (Task 11) runs the exe unconditionally, so this is what makes
        // clicking it twice raise one window instead of opening two.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .setup(|app| {
            let supervisor = daemon::Supervisor::start(app.handle().clone());
            app.manage(supervisor);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::trix_call,
            commands::start_daemon,
            commands::daemon_connected,
        ])
        .run(tauri::generate_context!())
        .expect("the Tauri runtime failed to start");
}
```

- [ ] **Step 6: Run the tests**

Run: `cargo test -p trix-ui`
Expected: PASS, 9 tests.

- [ ] **Step 7: Verify against a real daemon**

Start the daemon, then run the app, and check the window's devtools console shows a `trix-connected` payload with a real `clip_dir`:

```bash
cargo build --release -p trix-daemon
start "" target/release/trix-daemon.exe
cd crates/trix-ui && cargo tauri dev
```

Expected: no error panel; `trix-connected` fires. Then kill the daemon and confirm `trix-disconnected` fires within about a second.

- [ ] **Step 8: Commit**

```bash
cargo fmt
git add crates/trix-ui/src
git commit -m "feat(ui): connect to the daemon, launch it when absent, and reconnect with backoff"
```

---

### Task 4: The app shell

**Files:**
- Create: `crates/trix-ui/web/src/lib/ipc.ts`, `src/lib/types.ts`, `src/lib/state.svelte.ts`, `src/views/Rail.svelte`, `src/views/DaemonDown.svelte`, `src/components/Toasts.svelte`
- Modify: `crates/trix-ui/web/src/App.svelte`

**Interfaces:**
- Consumes: the `trix_call` / `start_daemon` commands and the `trix-event` / `trix-connected` / `trix-disconnected` events from Task 3.
- Produces: `call()`, the `app` store (`connected`, `status`, `view`, `clips`, `toasts`), and the rail.

- [ ] **Step 1: Write the wire types**

`crates/trix-ui/web/src/lib/types.ts`:

```ts
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
```

- [ ] **Step 2: Write the IPC wrapper**

`crates/trix-ui/web/src/lib/ipc.ts`:

```ts
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { DaemonEvent } from './types';

/** One command to the daemon. Rejects with the daemon's own error text. */
export async function call<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  return await invoke<T>('trix_call', { cmd, args });
}

export async function startDaemon(): Promise<void> {
  await invoke('start_daemon');
}

export function onDaemonEvent(handler: (event: DaemonEvent) => void) {
  return listen<DaemonEvent>('trix-event', (e) => handler(e.payload));
}

export function onConnected(handler: (status: unknown) => void) {
  return listen('trix-connected', (e) => handler(e.payload));
}

export function onDisconnected(handler: () => void) {
  return listen('trix-disconnected', () => handler());
}
```

- [ ] **Step 3: Write the store**

`crates/trix-ui/web/src/lib/state.svelte.ts`:

```ts
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
```

- [ ] **Step 4: Write the rail and the daemon-down panel**

`crates/trix-ui/web/src/views/Rail.svelte`:

```svelte
<script lang="ts">
  import { app } from '../lib/state.svelte';

  const pct = $derived(
    app.status && app.status.ring_seconds_total > 0
      ? Math.min(100, (app.ringUsed / app.status.ring_seconds_total) * 100)
      : 0,
  );
</script>

<nav class="rail">
  <div class="brand">Trix</div>

  <button class="arm" class:armed={app.armed} disabled={!app.connected} onclick={() => app.toggleArm()}>
    {app.armed ? 'Armed' : 'Arm'}
  </button>

  <div class="ring" title="Replay buffer">
    <div class="bar"><div class="fill" style="width: {pct}%"></div></div>
    <span>{app.ringUsed.toFixed(0)}s / {app.status?.ring_seconds_total.toFixed(0) ?? '0'}s</span>
  </div>

  <div class="nav">
    <button class:active={app.view === 'grid'} onclick={() => (app.view = 'grid')}>Clips</button>
    <button class:active={app.view === 'settings'} onclick={() => (app.view = 'settings')}>Settings</button>
  </div>

  <div class="version">{app.status?.version ?? ''}</div>
</nav>

<style>
  .rail {
    width: 200px;
    flex: 0 0 200px;
    height: 100vh;
    padding: 18px 14px;
    background: var(--panel);
    border-right: 1px solid var(--line);
    display: flex;
    flex-direction: column;
    gap: 18px;
  }
  .brand { font-weight: 600; letter-spacing: 0.04em; }
  .arm {
    padding: 10px;
    border-radius: 8px;
    border: 1px solid var(--line);
    background: transparent;
    color: var(--text);
    font: inherit;
    cursor: pointer;
  }
  .arm.armed { background: var(--accent); border-color: var(--accent); color: #06121f; font-weight: 600; }
  .arm:disabled { opacity: 0.4; cursor: default; }
  .ring { display: grid; gap: 6px; font-size: 12px; color: var(--dim); }
  .bar { height: 4px; background: var(--line); border-radius: 2px; overflow: hidden; }
  .fill { height: 100%; background: var(--accent); transition: width 200ms linear; }
  .nav { display: grid; gap: 4px; }
  .nav button {
    text-align: left;
    padding: 8px 10px;
    border: 0;
    border-radius: 6px;
    background: transparent;
    color: var(--dim);
    font: inherit;
    cursor: pointer;
  }
  .nav button.active { background: var(--line); color: var(--text); }
  .version { margin-top: auto; font-size: 11px; color: var(--dim); }
</style>
```

`crates/trix-ui/web/src/views/DaemonDown.svelte`:

```svelte
<script lang="ts">
  import { startDaemon } from '../lib/ipc';
  import { app } from '../lib/state.svelte';

  let starting = $state(false);

  async function start() {
    starting = true;
    try {
      await startDaemon();
    } catch (e) {
      app.toast('error', String(e));
      starting = false;
    }
    // Deliberately not cleared on success: the supervisor reconnects on its
    // own and this whole panel disappears when it does.
  }
</script>

<div class="down">
  <h2>Trix isn't running</h2>
  <p>The background service owns the replay buffer and the clip hotkey. Nothing is being captured right now.</p>
  <button onclick={start} disabled={starting}>{starting ? 'Starting...' : 'Start Trix'}</button>
</div>

<style>
  .down { display: grid; place-content: center; justify-items: center; gap: 12px; height: 100vh; text-align: center; padding: 40px; }
  h2 { margin: 0; font-size: 20px; }
  p { margin: 0; max-width: 42ch; color: var(--dim); }
  button { padding: 10px 18px; border-radius: 8px; border: 1px solid var(--accent); background: var(--accent); color: #06121f; font: inherit; font-weight: 600; cursor: pointer; }
  button:disabled { opacity: 0.6; cursor: default; }
</style>
```

`crates/trix-ui/web/src/components/Toasts.svelte`:

```svelte
<script lang="ts">
  import { app } from '../lib/state.svelte';
</script>

<div class="toasts">
  {#each app.toasts as toast (toast.id)}
    <div class="toast" class:error={toast.kind === 'error'}>{toast.text}</div>
  {/each}
</div>

<style>
  .toasts { position: fixed; right: 18px; bottom: 18px; display: grid; gap: 8px; z-index: 20; }
  .toast { padding: 10px 14px; border-radius: 8px; background: var(--panel); border: 1px solid var(--line); max-width: 46ch; font-size: 13px; }
  .toast.error { border-color: var(--danger); }
</style>
```

- [ ] **Step 5: Wire the shell**

`crates/trix-ui/web/src/App.svelte`:

```svelte
<script lang="ts">
  import Rail from './views/Rail.svelte';
  import DaemonDown from './views/DaemonDown.svelte';
  import Toasts from './components/Toasts.svelte';
  import { app, wireDaemon } from './lib/state.svelte';

  wireDaemon();
</script>

{#if !app.connected}
  <DaemonDown />
{:else}
  <div class="shell">
    <Rail />
    <main class="content">
      {#if app.view === 'grid'}
        <p class="placeholder">Clips arrive in Task 5.</p>
      {:else if app.view === 'settings'}
        <p class="placeholder">Settings arrive in Task 9.</p>
      {/if}
    </main>
  </div>
{/if}

<Toasts />

<style>
  .shell { display: flex; height: 100vh; }
  .content { flex: 1; overflow: auto; padding: 20px 24px; }
  .placeholder { color: var(--dim); }
</style>
```

- [ ] **Step 6: Verify**

```bash
cd crates/trix-ui && cargo tauri dev
```

Expected, with no daemon running: the "Trix isn't running" panel, and **Start Trix** brings the rail up within a second. With the daemon running: the rail, a version, and an **Arm** button that turns blue and reads "Armed" when clicked, with the ring meter filling.

- [ ] **Step 7: Commit**

```bash
git add crates/trix-ui/web/src crates/trix-ui/web/package.json crates/trix-ui/web/package-lock.json
git commit -m "feat(ui): add the shell, the arm toggle, the ring meter, and the daemon-down panel"
```

---

### Task 5: The clip grid

**Files:**
- Create: `crates/trix-ui/web/src/lib/clips.ts`, `src/lib/clips.test.ts`, `src/lib/keys.ts`, `src/lib/keys.test.ts`, `src/views/Grid.svelte`, `src/components/ClipCard.svelte`
- Modify: `crates/trix-ui/web/src/App.svelte`, `src/lib/state.svelte.ts`

**Interfaces:**
- Consumes: `library.list` → `{clips, total, offset}`; `app.clipDir` from `status`.
- Produces: `clipUrl()`, `thumbUrl()`, `formatDuration()`, `formatBytes()`, `mergeSaved()`, `moveSelection()`; the grid view.

- [ ] **Step 1: Write the failing tests**

`crates/trix-ui/web/src/lib/clips.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { formatBytes, formatDuration, mergeSaved, counterLabel } from './clips';
import type { ClipMeta } from './types';

const clip = (id: string, favorite = false): ClipMeta => ({
  id,
  title: `clip_${id}`,
  created: '2026-07-26T14:30:12+03:00',
  duration_ms: 20016,
  bytes: 19812352,
  width: 1920,
  height: 1200,
  fps: 60,
  encoder: 'NVENC H.264',
  has_audio: true,
  favorite,
});

describe('formatDuration', () => {
  it('reads as a clip length, not as milliseconds', () => {
    expect(formatDuration(20016)).toBe('0:20');
    expect(formatDuration(65000)).toBe('1:05');
    expect(formatDuration(0)).toBe('0:00');
  });
});

describe('formatBytes', () => {
  it('uses the unit a person would', () => {
    expect(formatBytes(19812352)).toBe('18.9 MB');
    expect(formatBytes(1024)).toBe('1.0 KB');
    expect(formatBytes(0)).toBe('0 B');
  });
});

describe('mergeSaved', () => {
  it('puts a new clip at the front, because the grid is newest-first', () => {
    const list = [clip('20260726_120000')];
    expect(mergeSaved(list, clip('20260726_143012'))[0].id).toBe('20260726_143012');
  });

  it('replaces rather than duplicates when the id is already there', () => {
    // library.list and a clip_saved event can race after a reconnect; two
    // cards for one file is the visible bug that causes.
    const list = [clip('20260726_143012'), clip('20260726_120000')];
    const merged = mergeSaved(list, clip('20260726_143012', true));
    expect(merged).toHaveLength(2);
    expect(merged[0].favorite).toBe(true);
  });
});

describe('counterLabel', () => {
  it('is one-based, the way spec §6.2 writes it', () => {
    expect(counterLabel(0, 47)).toBe('1 of 47');
    expect(counterLabel(2, 47)).toBe('3 of 47');
  });

  it('says nothing when there is nothing', () => {
    expect(counterLabel(0, 0)).toBe('');
  });
});
```

`crates/trix-ui/web/src/lib/keys.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { moveSelection } from './keys';

describe('moveSelection', () => {
  it('walks the grid by one and by a row', () => {
    expect(moveSelection(0, 'ArrowRight', 10, 4)).toBe(1);
    expect(moveSelection(0, 'ArrowDown', 10, 4)).toBe(4);
    expect(moveSelection(5, 'ArrowUp', 10, 4)).toBe(1);
  });

  it('stops at the ends instead of wrapping', () => {
    // Wrapping from the newest clip to the oldest is how you delete the wrong
    // thing with the Del key.
    expect(moveSelection(0, 'ArrowLeft', 10, 4)).toBe(0);
    expect(moveSelection(9, 'ArrowRight', 10, 4)).toBe(9);
    expect(moveSelection(8, 'ArrowDown', 10, 4)).toBe(8);
  });

  it('leaves the selection alone for keys it does not own', () => {
    expect(moveSelection(3, 'Enter', 10, 4)).toBe(3);
  });

  it('cannot select anything in an empty library', () => {
    expect(moveSelection(0, 'ArrowRight', 0, 4)).toBe(0);
  });
});
```

- [ ] **Step 2: Run them to verify they fail**

Run: `npm --prefix crates/trix-ui/web test`
Expected: FAIL — cannot resolve `./clips` and `./keys`.

- [ ] **Step 3: Write the helpers**

`crates/trix-ui/web/src/lib/clips.ts`:

```ts
import { convertFileSrc } from '@tauri-apps/api/core';
import type { ClipMeta } from './types';

/** The library is flat (spec §5.1), so every sidecar is the id plus a suffix. */
function clipFile(clipDir: string, id: string, ext: string): string {
  return `${clipDir}\\${id}.${ext}`;
}

/**
 * Playable URL for a clip.
 *
 * `asset:` rather than reading bytes through IPC: the asset protocol answers
 * Range requests, which is what makes seeking in a 20 s 1080p60 MP4 work at
 * all. Sending the file over IPC would mean buffering the whole clip in the
 * webview before the first frame.
 */
export function clipUrl(clipDir: string, id: string): string {
  return convertFileSrc(clipFile(clipDir, id, 'mp4'));
}

export function thumbUrl(clipDir: string, id: string): string {
  return convertFileSrc(clipFile(clipDir, id, 'jpg'));
}

export function formatDuration(ms: number): string {
  const total = Math.round(ms / 1000);
  const minutes = Math.floor(total / 60);
  const seconds = total % 60;
  return `${minutes}:${String(seconds).padStart(2, '0')}`;
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ['KB', 'MB', 'GB'];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  return `${value.toFixed(1)} ${units[unit]}`;
}

/**
 * Folds a `clip_saved` (or a renamed/favorited clip) into the list.
 *
 * Replaces by id rather than always prepending: a reconnect re-runs
 * `library.list` while events are still arriving, and two cards for one file
 * is what that race looks like on screen.
 */
export function mergeSaved(clips: ClipMeta[], saved: ClipMeta): ClipMeta[] {
  const existing = clips.findIndex((c) => c.id === saved.id);
  if (existing >= 0) {
    const copy = clips.slice();
    copy[existing] = saved;
    return copy;
  }
  return [saved, ...clips];
}

export function counterLabel(index: number, total: number): string {
  return total === 0 ? '' : `${index + 1} of ${total}`;
}
```

`crates/trix-ui/web/src/lib/keys.ts`:

```ts
/**
 * Where the selection lands after an arrow key in a grid `columns` wide.
 *
 * Clamps rather than wraps, on purpose. The grid is newest-first and `Del` acts
 * on the selection, so wrapping from the newest clip to the oldest puts the
 * clip you care least about under a destructive key.
 */
export function moveSelection(
  current: number,
  key: string,
  count: number,
  columns: number,
): number {
  if (count === 0) return 0;
  const delta =
    key === 'ArrowRight' ? 1
    : key === 'ArrowLeft' ? -1
    : key === 'ArrowDown' ? columns
    : key === 'ArrowUp' ? -columns
    : 0;
  if (delta === 0) return current;
  const next = current + delta;
  return next < 0 || next >= count ? current : next;
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `npm --prefix crates/trix-ui/web test`
Expected: PASS, 10 tests.

- [ ] **Step 5: Write the grid**

`crates/trix-ui/web/src/components/ClipCard.svelte`:

```svelte
<script lang="ts">
  import { formatBytes, formatDuration, thumbUrl } from '../lib/clips';
  import type { ClipMeta } from '../lib/types';

  let {
    clip,
    clipDir,
    selected = false,
    onopen,
    onselect,
  }: {
    clip: ClipMeta;
    clipDir: string;
    selected?: boolean;
    onopen: () => void;
    onselect: () => void;
  } = $props();
</script>

<button class="card" class:selected onclick={onselect} ondblclick={onopen}>
  <div class="thumb">
    <img src={thumbUrl(clipDir, clip.id)} alt="" loading="lazy" />
    <span class="len">{formatDuration(clip.duration_ms)}</span>
    {#if clip.favorite}<span class="fav" title="Favorite">*</span>{/if}
  </div>
  <div class="meta">
    <span class="title">{clip.title}</span>
    <span class="sub">{formatBytes(clip.bytes)}</span>
  </div>
</button>

<style>
  .card { display: grid; gap: 8px; padding: 0; border: 2px solid transparent; border-radius: 10px; background: transparent; color: inherit; font: inherit; text-align: left; cursor: pointer; }
  .card.selected { border-color: var(--accent); }
  .thumb { position: relative; aspect-ratio: 16 / 10; border-radius: 8px; overflow: hidden; background: var(--panel); }
  .thumb img { width: 100%; height: 100%; object-fit: cover; display: block; }
  .len { position: absolute; right: 6px; bottom: 6px; padding: 1px 5px; border-radius: 4px; background: rgba(0, 0, 0, 0.7); font-size: 11px; }
  .fav { position: absolute; left: 6px; top: 6px; color: var(--accent); font-size: 16px; }
  .meta { display: grid; padding: 0 2px 4px; }
  .title { font-size: 13px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .sub { font-size: 11px; color: var(--dim); }
</style>
```

`crates/trix-ui/web/src/views/Grid.svelte`:

```svelte
<script lang="ts">
  import ClipCard from '../components/ClipCard.svelte';
  import { app } from '../lib/state.svelte';
  import { moveSelection } from '../lib/keys';
  import { clipUrl } from '../lib/clips';

  /** Kept in sync with the CSS grid below so ArrowDown moves one visual row. */
  let columns = $state(4);
  let previewing = $state<string | null>(null);
  let gridEl = $state<HTMLDivElement | null>(null);

  $effect(() => {
    if (!gridEl) return;
    const observer = new ResizeObserver(() => {
      const style = getComputedStyle(gridEl!);
      columns = Math.max(1, style.gridTemplateColumns.split(' ').length);
    });
    observer.observe(gridEl);
    return () => observer.disconnect();
  });

  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'Enter') {
      app.view = 'clip';
      return;
    }
    if (e.key === ' ') {
      // Spec §6.2: in the grid, Space previews in place. The "did it save?"
      // glance must not cost a page load.
      e.preventDefault();
      const clip = app.clips[app.selected];
      previewing = clip && previewing !== clip.id ? clip.id : null;
      return;
    }
    const next = moveSelection(app.selected, e.key, app.clips.length, columns);
    if (next !== app.selected) {
      e.preventDefault();
      app.selected = next;
      previewing = null;
    }
  }
</script>

<svelte:window {onkeydown} />

{#if app.clips.length === 0}
  <div class="empty">
    <p>No clips yet.</p>
    <p class="hint">Arm Trix, then press your clip hotkey while you play.</p>
  </div>
{:else}
  <div class="grid" bind:this={gridEl}>
    {#each app.clips as clip, i (clip.id)}
      {#if previewing === clip.id}
        <!-- svelte-ignore a11y_media_has_caption -->
        <video class="preview" src={clipUrl(app.clipDir, clip.id)} autoplay loop muted></video>
      {:else}
        <ClipCard
          {clip}
          clipDir={app.clipDir}
          selected={i === app.selected}
          onselect={() => (app.selected = i)}
          onopen={() => {
            app.selected = i;
            app.view = 'clip';
          }}
        />
      {/if}
    {/each}
  </div>
{/if}

<style>
  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(230px, 1fr)); gap: 16px; }
  .preview { width: 100%; aspect-ratio: 16 / 10; border-radius: 8px; background: #000; object-fit: cover; }
  .empty { display: grid; place-content: center; height: 60vh; text-align: center; gap: 6px; }
  .hint { color: var(--dim); }
</style>
```

- [ ] **Step 6: Load the library**

In `state.svelte.ts`, add to `AppState`:

```ts
  async loadClips() {
    try {
      const page = await call<{ clips: ClipMeta[]; total: number; offset: number }>('library.list', {
        offset: 0,
        limit: 200,
      });
      this.clips = page.clips;
      this.total = page.total;
      this.selected = 0;
    } catch (e) {
      this.toast('error', String(e));
    }
  }
```

and call it from `wireDaemon`'s `onConnected` handler, after `refreshStatus`. Render `<Grid />` in `App.svelte` where the Task 4 placeholder sits.

**One page, deliberately.** `limit: 200` fetches the newest 200 clips and stops; `total` is read but only the loaded list is rendered and counted, so the app is internally consistent and simply does not show clip 201. This is a real limit, not an oversight: `library.list` pages properly (its `MAX_LIST_LIMIT` is 500) and the daemon serves the whole library from RAM, so adding scroll-triggered paging later is a change to this one function and nothing else. It is left out here because spec §9 wants the library measured at 5,000 clips before v1, and virtualizing a grid against a page size nobody has measured is guessing. If the hand-verification finds someone's library is already past 200, that measurement is the thing to do first.

- [ ] **Step 7: Verify against real clips**

```bash
cd crates/trix-ui && cargo tauri dev
```

Expected: the grid fills with real thumbnails from `%USERPROFILE%\Videos\Trix`. **If the thumbnails are broken images, the asset scope is the cause** — check `Supervisor::on_connected` actually got a `clip_dir` and that `media-src`/`img-src` in `tauri.conf.json` list `asset:` and `http://asset.localhost`. Arrow keys move the blue selection ring; Space plays the selected card in place.

- [ ] **Step 8: Commit**

```bash
git add crates/trix-ui/web/src
git commit -m "feat(ui): add the clip grid with thumbnails, keyboard nav, and inline preview"
```

---

### Task 6: The clip page

**Files:**
- Create: `crates/trix-ui/web/src/views/ClipPage.svelte`
- Modify: `crates/trix-ui/web/src/App.svelte`, `src/lib/state.svelte.ts`

**Interfaces:**
- Consumes: `library.rename`, `library.favorite`, `library.reveal`, `library.delete`; `clipUrl`, `counterLabel`.
- Produces: the clip page, and `app.removeClip(id)`.

- [ ] **Step 1: Add the library mutations to the store**

In `state.svelte.ts`, add to `AppState`:

```ts
  get current(): ClipMeta | null {
    return this.clips[this.selected] ?? null;
  }

  step(delta: number) {
    const next = this.selected + delta;
    if (next >= 0 && next < this.clips.length) this.selected = next;
  }

  async rename(id: string, title: string) {
    try {
      const updated = await call<ClipMeta>('library.rename', { clip_id: id, title });
      this.clips = mergeSaved(this.clips, updated);
    } catch (e) {
      this.toast('error', String(e));
    }
  }

  async setFavorite(id: string, favorite: boolean) {
    try {
      const updated = await call<ClipMeta>('library.favorite', { clip_id: id, favorite });
      this.clips = mergeSaved(this.clips, updated);
    } catch (e) {
      this.toast('error', String(e));
    }
  }

  async reveal(id: string) {
    try {
      await call('library.reveal', { clip_id: id });
    } catch (e) {
      this.toast('error', String(e));
    }
  }

  async remove(id: string) {
    try {
      await call('library.delete', { clip_id: id });
      const index = this.clips.findIndex((c) => c.id === id);
      this.clips = this.clips.filter((c) => c.id !== id);
      this.total = Math.max(0, this.total - 1);
      // Keep the selection on a real clip: the one that slid into this slot,
      // or the new last one if the deleted clip was at the end.
      this.selected = Math.min(index < 0 ? 0 : index, Math.max(0, this.clips.length - 1));
      if (this.clips.length === 0) this.view = 'grid';
    } catch (e) {
      this.toast('error', String(e));
    }
  }
```

Import `mergeSaved` from `./clips` at the top of the file.

- [ ] **Step 2: Write the clip page**

`crates/trix-ui/web/src/views/ClipPage.svelte`:

```svelte
<script lang="ts">
  import { app } from '../lib/state.svelte';
  import { clipUrl, counterLabel, formatBytes, formatDuration } from '../lib/clips';

  let video = $state<HTMLVideoElement | null>(null);
  let renaming = $state(false);
  let draftTitle = $state('');
  let confirmingDelete = $state(false);

  const clip = $derived(app.current);

  function playPause() {
    if (!video) return;
    if (video.paused) void video.play();
    else video.pause();
  }

  function onkeydown(e: KeyboardEvent) {
    if (renaming) {
      if (e.key === 'Escape') renaming = false;
      return;
    }
    switch (e.key) {
      case 'Escape':
        app.view = 'grid';
        break;
      case 'ArrowLeft':
        e.preventDefault();
        app.step(-1);
        break;
      case 'ArrowRight':
        e.preventDefault();
        app.step(1);
        break;
      case ' ':
        e.preventDefault();
        playPause();
        break;
      case 'Delete':
        confirmingDelete = true;
        break;
    }
  }

  function startRename() {
    if (!clip) return;
    draftTitle = clip.title;
    renaming = true;
  }

  async function commitRename() {
    if (clip && draftTitle.trim()) await app.rename(clip.id, draftTitle.trim());
    renaming = false;
  }
</script>

<svelte:window {onkeydown} />

{#if clip}
  <header class="head">
    <button class="back" onclick={() => (app.view = 'grid')}>&lsaquo; Clips</button>
    <span class="counter">{counterLabel(app.selected, app.clips.length)}</span>
  </header>

  <div class="stage">
    <button class="step" onclick={() => app.step(-1)} disabled={app.selected === 0}>&lsaquo;</button>
    <!-- svelte-ignore a11y_media_has_caption -->
    <video bind:this={video} src={clipUrl(app.clipDir, clip.id)} controls autoplay></video>
    <button class="step" onclick={() => app.step(1)} disabled={app.selected >= app.clips.length - 1}>&rsaquo;</button>
  </div>

  <!--
    Spec §6.2 puts the filmstrip trim bar here, full width, between the player
    and the metadata. It is deliberately absent: trim needs `library.export`,
    which does not exist yet and is the next plan. The slot is left rather than
    the layout redrawn, so adding it is one component and no reshuffle.
  -->

  <p class="meta">
    {clip.width}x{clip.height} &middot; {clip.fps} fps &middot; {formatDuration(clip.duration_ms)} &middot;
    {formatBytes(clip.bytes)} &middot; {clip.encoder}{clip.has_audio ? ' + audio' : ''}
  </p>

  <div class="actions">
    {#if renaming}
      <input bind:value={draftTitle} onkeydown={(e) => e.key === 'Enter' && commitRename()} />
      <button onclick={commitRename}>Save</button>
      <button onclick={() => (renaming = false)}>Cancel</button>
    {:else}
      <span class="title">{clip.title}</span>
      <button onclick={startRename}>Rename</button>
      <button onclick={() => app.setFavorite(clip.id, !clip.favorite)}>
        {clip.favorite ? 'Unfavorite' : 'Favorite'}
      </button>
      <button onclick={() => app.reveal(clip.id)}>Show in Explorer</button>
      <button class="danger" onclick={() => (confirmingDelete = true)}>Delete</button>
    {/if}
  </div>

  {#if confirmingDelete}
    <div class="confirm">
      <p>Delete <strong>{clip.title}</strong>? The mp4, its metadata, and its thumbnail all go.</p>
      <button
        class="danger"
        onclick={async () => {
          confirmingDelete = false;
          await app.remove(clip.id);
        }}>Delete</button
      >
      <button onclick={() => (confirmingDelete = false)}>Keep</button>
    </div>
  {/if}
{/if}

<style>
  .head { display: flex; align-items: center; justify-content: space-between; margin-bottom: 12px; }
  .back { background: none; border: 0; color: var(--dim); font: inherit; cursor: pointer; padding: 0; }
  .counter { color: var(--dim); font-size: 13px; }
  .stage { display: flex; align-items: center; gap: 10px; }
  .stage video { flex: 1; width: 100%; max-height: 62vh; background: #000; border-radius: 10px; }
  .step { background: none; border: 0; color: var(--text); font-size: 26px; cursor: pointer; padding: 0 6px; }
  .step:disabled { opacity: 0.25; cursor: default; }
  .meta { color: var(--dim); font-size: 12px; margin: 12px 0; }
  .actions { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .actions .title { margin-right: auto; font-weight: 600; }
  .actions button, .confirm button { padding: 6px 12px; border-radius: 6px; border: 1px solid var(--line); background: var(--panel); color: var(--text); font: inherit; cursor: pointer; }
  .danger { border-color: var(--danger) !important; color: var(--danger) !important; }
  .confirm { margin-top: 14px; padding: 12px 14px; border: 1px solid var(--danger); border-radius: 8px; display: flex; align-items: center; gap: 10px; }
  .confirm p { margin: 0 auto 0 0; }
  input { padding: 6px 10px; border-radius: 6px; border: 1px solid var(--line); background: var(--bg); color: var(--text); font: inherit; }
</style>
```

- [ ] **Step 3: Route to it**

In `App.svelte`, add `{:else if app.view === 'clip'}<ClipPage />` to the view switch, and import it.

- [ ] **Step 4: Verify**

Run `cargo tauri dev` and check every key of the §6.2 table that this plan owns: `Esc` returns to the grid, `←`/`→` walk clips with the counter tracking, `Space` toggles playback, `Del` asks before deleting. Rename, favorite, and Show in Explorer each act on the right clip. (`I`/`O`/`Ctrl+E` belong to trim and are the next plan's.)

- [ ] **Step 5: Commit**

```bash
git add crates/trix-ui/web/src
git commit -m "feat(ui): add the clip page with playback, prev/next, and the library actions"
```

---

### Task 7: Live updates

Spec §10 gate 4 opens with "clip appears in the grid within a second of the hotkey". That is this task.

**Files:**
- Modify: `crates/trix-ui/web/src/lib/state.svelte.ts`

- [ ] **Step 1: Handle `clip_saved`**

Extend the `onDaemonEvent` switch in `wireDaemon`:

```ts
      case 'clip_saved': {
        const saved = event.data as unknown as ClipMeta;
        const wasEmpty = app.clips.length === 0;
        app.clips = mergeSaved(app.clips, saved);
        app.total += 1;
        // The selection is an index, and a prepend shifts every clip down one.
        // Without this, a clip landing while the user is reading clip 3 silently
        // moves them to clip 2 -- and `Del` is on that selection.
        if (!wasEmpty && app.view !== 'grid') app.selected += 1;
        app.toast('info', `Saved ${saved.title}`);
        break;
      }
```

Import `ClipMeta` and `mergeSaved` at the top of the file.

- [ ] **Step 2: Verify with a real hotkey**

With `cargo tauri dev` running and the daemon armed, press the clip hotkey while the grid is on screen.

Expected: a new card at the front, with its thumbnail, inside a second, plus a "Saved ..." toast. Then open a clip, press the hotkey again, and confirm the page is still showing the same clip afterwards.

- [ ] **Step 3: Verify the failure path**

Disarm, then press the hotkey. Expected: an error toast carrying the daemon's own words (the ring is empty), and no phantom card.

- [ ] **Step 4: Commit**

```bash
git add crates/trix-ui/web/src/lib/state.svelte.ts
git commit -m "feat(ui): show clips in the grid the moment the daemon saves them"
```

---

### Task 8: Make the hotkey setting real

The settings page of Task 9 offers a `clip_hotkey` field. Today that field would be a lie: `window::spawn` registers the hotkey once at startup (`main.rs:213`) and `clip_hotkey` is not in `REQUIRES_REARM`, so nothing re-registers it — the comment at `state.rs:602` says exactly this. This task is in the **daemon**, and it is Rust-only and fully testable.

**Files:**
- Modify: `crates/trix-daemon/src/window.rs`, `crates/trix-daemon/src/state.rs`, `crates/trix-daemon/src/dispatch.rs`
- Modify: `docs/superpowers/specs/2026-07-26-trix-desktop-ui-design.md` (§4.4 events list)

**Interfaces:**
- Produces: `WindowHandle::rebind_hotkey(&str)`; the events `hotkey_pressed` (`{}`) and `hotkey_rebound` (`{"spec":"alt+f10","registered":true}`).

- [ ] **Step 1: Write the failing test**

In `crates/trix-daemon/src/window.rs`'s test module:

```rust
    /// A hotkey the user changes in settings has to work now, not after a
    /// restart they were never told to perform. The pump owns the
    /// registration (`RegisterHotKey` posts to the *registering thread's*
    /// queue), so a rebind is a message to the pump plus the new spec left
    /// where the pump can read it.
    #[test]
    fn a_rebind_leaves_the_new_spec_for_the_pump() {
        set_pending_hotkey("ctrl+shift+f9");
        assert_eq!(take_pending_hotkey().as_deref(), Some("ctrl+shift+f9"));
        assert_eq!(
            take_pending_hotkey(),
            None,
            "the pump takes the request once; a second WM_TRIX_REHOTKEY must not re-register a stale spec"
        );
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p trix-daemon window`
Expected: FAIL — `cannot find function set_pending_hotkey`.

- [ ] **Step 3: Implement the rebind**

In `window.rs`, beside the other `WM_APP` constants:

```rust
/// Asks the pump to re-register the clip hotkey from [`PENDING_HOTKEY`].
pub(crate) const WM_TRIX_REHOTKEY: u32 = WM_APP + 0x12;

/// The spec a pending rebind wants, left here because `PostMessageW` carries
/// two integers and a `String` is neither of them.
static PENDING_HOTKEY: Mutex<Option<String>> = Mutex::new(None);

fn set_pending_hotkey(spec: &str) {
    if let Ok(mut pending) = PENDING_HOTKEY.lock() {
        *pending = Some(spec.to_string());
    }
}

fn take_pending_hotkey() -> Option<String> {
    PENDING_HOTKEY.lock().ok().and_then(|mut pending| pending.take())
}
```

`window.rs` imports `std::sync::Arc` but not `Mutex`; widen it to `use std::sync::{Arc, Mutex};`.

Add the message arm to the window procedure, beside `WM_TRIX_ARMED`. The `Hotkey` API is `trix_core::control::Hotkey`, already imported here — `parse(&str) -> anyhow::Result<Self>` and `unsafe fn register(&self, HWND, i32) -> Result<()>`, exactly as `spawn` calls them at `window.rs:316-330`:

```rust
        WM_TRIX_REHOTKEY => {
            if let Some(spec) = take_pending_hotkey() {
                // Unregistered first and unconditionally: leaving the old
                // binding alive would mean two live hotkeys, with the one the
                // user just replaced still clipping.
                unsafe {
                    let _ = UnregisterHotKey(Some(hwnd), HOTKEY_ID);
                }
                // Same three outcomes, same severity, as the startup
                // registration above: unparseable, taken, or ours. A hotkey
                // that will not bind is a warning, never a failure — the
                // daemon is still fully usable over the socket and the tray.
                let registered = match Hotkey::parse(&spec) {
                    Ok(hotkey) => match unsafe { hotkey.register(hwnd, HOTKEY_ID) } {
                        Ok(()) => {
                            tracing::info!(hotkey = %hotkey, "clip hotkey re-registered");
                            true
                        }
                        Err(e) => {
                            tracing::warn!(hotkey = %hotkey, error = %format!("{e:#}"), "the new clip hotkey is already taken");
                            false
                        }
                    },
                    Err(e) => {
                        tracing::warn!(spec = %spec, error = %format!("{e:#}"), "the new clip_hotkey is not parseable");
                        false
                    }
                };
                ACTIONS.with(|a| {
                    if let Some(tx) = a.borrow().as_ref() {
                        offer(tx, Action::HotkeyRebound { spec, registered });
                    }
                });
            }
            LRESULT(0)
        }
```

Add to `Action`:

```rust
    /// The pump finished a rebind. Carries the outcome so the settings page
    /// can say "that combination is taken" instead of going quiet.
    HotkeyRebound { spec: String, registered: bool },
```

And a free function beside it — **not** a `WindowHandle` method, and not a new field on `Daemon`. `window.rs` already keeps the pump's window in the `WINDOW_HWND` global (`window.rs:43`) precisely so callers that do not hold the handle can post to it, which is what `state.rs` is. Going through it means this task touches no `main.rs` wiring at all:

```rust
/// Asks the pump to re-register the clip hotkey. Returns immediately; the
/// outcome arrives as an `Action::HotkeyRebound`.
///
/// A no-op when there is no pump — unit tests and the window of shutdown after
/// the pump has gone. A hotkey that cannot be rebound because nothing is
/// listening is not an error worth propagating into `config.set`, which has
/// already saved the value the next startup will register.
pub fn rebind_hotkey(spec: &str) {
    let hwnd = WINDOW_HWND.load(Ordering::Relaxed);
    if hwnd == 0 {
        return;
    }
    set_pending_hotkey(spec);
    unsafe {
        let _ = PostMessageW(
            Some(HWND(hwnd as *mut core::ffi::c_void)),
            WM_TRIX_REHOTKEY,
            WPARAM(0),
            LPARAM(0),
        );
    }
}
```

- [ ] **Step 4: Emit `hotkey_pressed`, and rebind on `config.set`**

In the worker's `Action::Clip` handler (`window.rs`'s `handle_action`), add one line as the **first** statement of the existing arm, leaving everything already there untouched below it:

```rust
        Action::Clip => {
            // Emitted before the clip is attempted, and regardless of whether
            // one is possible: this is the only honest answer to the settings
            // page's "press it now" test, which asks whether the *daemon*
            // received the combination — not whether a clip resulted. Spec
            // §6.4 exists because NVIDIA's overlay silently eats Alt+F10
            // inside hooked games, and nothing the webview can observe would
            // ever catch that. `Action::Clip` reaches here only from
            // `WM_HOTKEY`, so this cannot fire for anything but a real press.
            daemon.clients.broadcast(&Event::new("hotkey_pressed", Value::Object(Map::new())));

            // ... the arm's existing body stays exactly as it is ...
        }
```

And add the new arm beside it:

```rust
        Action::HotkeyRebound { spec, registered } => {
            daemon.clients.broadcast(&Event::new(
                "hotkey_rebound",
                serde_json::json!({"spec": spec, "registered": registered}),
            ));
        }
```

In `state.rs`'s `set_config`, after the config file is written and `*config = updated` has landed:

```rust
        // After the write, not before: a rebind that beat a failed write would
        // leave the running hotkey and the saved hotkey disagreeing, and
        // `config.set` is all-or-nothing everywhere else.
        if values.contains_key("clip_hotkey") {
            crate::window::rebind_hotkey(&config.clip_hotkey);
        }
```

Replace the now-wrong comment at `state.rs:602` ("the hotkey is registered once at startup and re-reading it would not re-register it") — it was true and no longer is.

- [ ] **Step 5: Add the dispatch test**

In `dispatch.rs`'s tests, alongside the existing `config.set` cases:

```rust
    /// The settings page has a hotkey field, so `config.set` must both save it
    /// and make it live. Only the saving half is observable without a message
    /// pump, and that is what this asserts; the registration itself is
    /// `window.rs`'s test and the hand verification.
    #[test]
    fn config_set_saves_a_new_clip_hotkey_and_does_not_demand_a_rearm() {
        let (daemon, _path, _dir) = with_scratch_config("hotkey");
        let response = daemon.dispatch(
            1,
            &request_with(1, "config.set", &[("clip_hotkey", "ctrl+shift+f9".into())]),
        );
        assert!(response.ok, "{:?}", response.error);
        let data = response.data.expect("config.set carries data");
        assert_eq!(data["accepted"]["clip_hotkey"], "ctrl+shift+f9");
        // The rebind is live, so telling the user to re-arm would be asking
        // for a capture restart that changes nothing.
        assert_eq!(
            data["requires_rearm"].as_array().map(Vec::len),
            Some(0),
            "clip_hotkey takes effect without a re-arm"
        );

        let after = daemon.dispatch(1, &request(2, "config.get"));
        assert_eq!(after.data.expect("data")["clip_hotkey"], "ctrl+shift+f9");
    }
```

- [ ] **Step 6: Run the tests**

Run: `cargo test --workspace`
Expected: PASS, 125+ tests.

- [ ] **Step 7: Document the two new events**

In the spec, §4.4, extend the event list to `armed`, `disarmed`, `clip_saved`, `hotkey_pressed`, `hotkey_rebound`, `export_progress`, `export_done`, `error`, `stats`, and add a sentence: `hotkey_pressed` fires whenever the registered clip hotkey reaches the daemon, whether or not a clip results, because that is the only observable that answers §6.4's live test; `hotkey_rebound` reports whether a `config.set clip_hotkey` actually took the binding.

- [ ] **Step 8: Commit**

```bash
cargo fmt
git add crates/trix-daemon/src docs/superpowers/specs
git commit -m "feat(daemon): rebind the clip hotkey live, and report presses and rebinds as events"
```

---

### Task 9: Settings

**Files:**
- Create: `crates/trix-ui/web/src/lib/settings.ts`, `src/lib/settings.test.ts`, `src/views/Settings.svelte`, `src/components/Field.svelte`
- Modify: `crates/trix-ui/web/src/App.svelte`

- [ ] **Step 1: Write the failing tests**

`crates/trix-ui/web/src/lib/settings.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { FIELDS, unknownKeys, validate } from './settings';

describe('FIELDS', () => {
  it('covers every config key the daemon has today', () => {
    const shipped = [
      'fps', 'bitrate_kbps', 'max_bitrate_kbps', 'rate_control', 'replay_seconds',
      'monitor_index', 'clip_hotkey', 'gpu_priority', 'stats_seconds', 'clip_dir',
      'max_library_gb', 'autostart',
    ];
    const covered = FIELDS.map((f) => f.key);
    for (const key of shipped) expect(covered).toContain(key);
  });
});

describe('unknownKeys', () => {
  it('ignores the two read-only extras config.get adds', () => {
    const config = { fps: 60, clip_dir_resolved: 'C:\\x', autostart: false };
    expect(unknownKeys(config)).toEqual([]);
  });

  it('reports a key the daemon grew that this page does not render', () => {
    // The daemon is the source of truth for what is configurable. A settings
    // page that silently drops a new key is worse than one that renders it
    // plainly, because the user cannot tell it is missing.
    expect(unknownKeys({ fps: 60, mystery_key: 3 })).toEqual(['mystery_key']);
  });
});

describe('validate', () => {
  it('mirrors the daemon bounds so the error arrives before the round trip', () => {
    expect(validate('fps', 0)).toMatch(/1 to 480/);
    expect(validate('fps', 60)).toBeNull();
    expect(validate('replay_seconds', 601)).toMatch(/1 to 600/);
    expect(validate('max_library_gb', 0)).toBeNull();
  });

  it('says nothing about keys it has no bound for', () => {
    expect(validate('clip_hotkey', 'alt+f10')).toBeNull();
  });
});
```

- [ ] **Step 2: Run them to verify they fail**

Run: `npm --prefix crates/trix-ui/web test`
Expected: FAIL — cannot resolve `./settings`.

- [ ] **Step 3: Write the field table**

`crates/trix-ui/web/src/lib/settings.ts`:

```ts
export type FieldKind = 'number' | 'text' | 'select' | 'bool' | 'folder' | 'hotkey';

export type Field = {
  key: string;
  label: string;
  kind: FieldKind;
  section: 'Capture' | 'Quality' | 'Clips' | 'Trix';
  help: string;
  min?: number;
  max?: number;
  options?: { value: string; label: string }[];
  /** Filled at runtime from monitors.list / encoders.list. */
  dynamic?: 'monitors';
};

/**
 * Bounds mirrored from `state.rs`'s NUMERIC_BOUNDS.
 *
 * Duplicated deliberately, and it is not a DRY violation to fix: the daemon
 * must keep validating for third-party clients that never see this file, and
 * this copy exists only so a typo is caught before a round trip. The daemon
 * remains the authority -- if the two ever disagree, its answer is the one the
 * user sees.
 */
const BOUNDS: Record<string, [number, number]> = {
  fps: [1, 480],
  bitrate_kbps: [1, 200000],
  max_bitrate_kbps: [0, 200000],
  replay_seconds: [1, 600],
  monitor_index: [0, 63],
  stats_seconds: [0, 86400],
  max_library_gb: [0, 10000],
};

export const FIELDS: Field[] = [
  { key: 'monitor_index', label: 'Monitor', kind: 'select', section: 'Capture', dynamic: 'monitors', help: 'Which screen is captured.' },
  { key: 'fps', label: 'Frame rate', kind: 'number', section: 'Capture', ...span('fps'), help: 'Capture and encode rate.' },
  { key: 'replay_seconds', label: 'Replay buffer', kind: 'number', section: 'Capture', ...span('replay_seconds'), help: 'Seconds kept in RAM. Memory cost scales with this times the bitrate.' },
  { key: 'gpu_priority', label: 'GPU priority', kind: 'select', section: 'Capture', options: [
      { value: 'low', label: 'Low - never cost game fps' },
      { value: 'normal', label: 'Normal - smoother capture' },
    ], help: 'Low drops capture frames under contention instead of taking frames from the game.' },

  { key: 'bitrate_kbps', label: 'Bitrate', kind: 'number', section: 'Quality', ...span('bitrate_kbps'), help: 'Target average, in kbit/s.' },
  { key: 'max_bitrate_kbps', label: 'Peak bitrate', kind: 'number', section: 'Quality', ...span('max_bitrate_kbps'), help: '0 means 1.5x the target. This cap is also the replay buffer\'s worst-case RAM.' },
  { key: 'rate_control', label: 'Rate control', kind: 'select', section: 'Quality', options: [
      { value: 'vbr', label: 'VBR - quality-leaning' },
      { value: 'cbr', label: 'CBR - predictable size' },
    ], help: 'How the encoder spends its bitrate.' },

  { key: 'clip_dir', label: 'Clips folder', kind: 'folder', section: 'Clips', help: 'Where clips are saved. Empty means Videos\\Trix.' },
  { key: 'max_library_gb', label: 'Library limit', kind: 'number', section: 'Clips', ...span('max_library_gb'), help: 'GB. When exceeded the oldest non-favorite clips are deleted. 0 turns the limit off.' },

  { key: 'clip_hotkey', label: 'Clip hotkey', kind: 'hotkey', section: 'Trix', help: 'Press the combination to test it. Overlays can silently take a hotkey inside games.' },
  { key: 'autostart', label: 'Start with Windows', kind: 'bool', section: 'Trix', help: 'Off by default. Writes the registry Run entry, which is the source of truth.' },
  { key: 'stats_seconds', label: 'Stats interval', kind: 'number', section: 'Trix', ...span('stats_seconds'), help: 'Seconds between performance reports. 0 turns them off.' },
];

function span(key: string): { min: number; max: number } {
  const [min, max] = BOUNDS[key];
  return { min, max };
}

/** Keys `config.get` returned that this page has no field for. */
export function unknownKeys(config: Record<string, unknown>): string[] {
  // `clip_dir_resolved` is documented as not a config key and not settable.
  const rendered = new Set([...FIELDS.map((f) => f.key), 'clip_dir_resolved']);
  return Object.keys(config).filter((key) => !rendered.has(key));
}

/** The daemon's own bound, checked early. `null` means acceptable. */
export function validate(key: string, value: unknown): string | null {
  const bound = BOUNDS[key];
  if (!bound || typeof value !== 'number') return null;
  const [min, max] = bound;
  return value < min || value > max ? `${key} accepts ${min} to ${max}` : null;
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `npm --prefix crates/trix-ui/web test`
Expected: PASS, 15 tests.

- [ ] **Step 5: Write the settings page**

`crates/trix-ui/web/src/views/Settings.svelte`:

```svelte
<script lang="ts">
  import { call, onDaemonEvent } from '../lib/ipc';
  import { app } from '../lib/state.svelte';
  import { FIELDS, unknownKeys, validate } from '../lib/settings';
  import type { Monitor } from '../lib/types';

  let config = $state<Record<string, unknown>>({});
  let monitors = $state<Monitor[]>([]);
  let rearmNeeded = $state<string[]>([]);
  let extras = $state<string[]>([]);

  // Hotkey live test (spec §6.4).
  let listening = $state(false);
  let heard = $state(false);
  let capture = $state<string | null>(null);

  $effect(() => {
    void load();
  });

  onDaemonEvent((event) => {
    if (event.event === 'hotkey_pressed' && listening) heard = true;
    if (event.event === 'hotkey_rebound' && event.data['registered'] === false) {
      app.toast('error', `Windows would not give Trix ${event.data['spec']}. Another app already owns it.`);
    }
  });

  async function load() {
    try {
      config = await call<Record<string, unknown>>('config.get');
      extras = unknownKeys(config);
      const list = await call<{ monitors: Monitor[] }>('monitors.list');
      monitors = list.monitors;
    } catch (e) {
      app.toast('error', String(e));
    }
  }

  async function set(key: string, value: unknown) {
    const complaint = validate(key, value);
    if (complaint) {
      app.toast('error', complaint);
      return;
    }
    try {
      const result = await call<{ accepted: Record<string, unknown>; requires_rearm: string[] }>(
        'config.set',
        { [key]: value },
      );
      // Read back from `accepted`, which the daemon takes out of the saved
      // config rather than echoing the request: an accepted value that differs
      // from what was typed is exactly what the field should now show.
      config = { ...config, ...result.accepted };
      if (result.requires_rearm.length > 0 && app.armed) {
        rearmNeeded = [...new Set([...rearmNeeded, ...result.requires_rearm])];
      }
      if (key === 'clip_dir') await app.refreshStatus();
    } catch (e) {
      app.toast('error', String(e));
      // The daemon refused, so nothing was written and nothing was applied:
      // put the field back to the truth rather than leaving the typed value on
      // screen looking saved.
      await load();
    }
  }

  function captureHotkey(e: KeyboardEvent) {
    e.preventDefault();
    const parts: string[] = [];
    if (e.ctrlKey) parts.push('ctrl');
    if (e.altKey) parts.push('alt');
    if (e.shiftKey) parts.push('shift');
    if (e.metaKey) parts.push('win');
    const key = e.key.toLowerCase();
    if (['control', 'alt', 'shift', 'meta'].includes(key)) return;
    parts.push(key);
    capture = parts.join('+');
  }
</script>

<h1>Settings</h1>

{#if rearmNeeded.length > 0}
  <div class="notice">
    Re-arm to apply: {rearmNeeded.join(', ')}
    <button
      onclick={async () => {
        await call('disarm');
        await call('arm');
        rearmNeeded = [];
        await app.refreshStatus();
      }}>Re-arm now</button
    >
  </div>
{/if}

{#each ['Capture', 'Quality', 'Clips', 'Trix'] as section (section)}
  <section>
    <h2>{section}</h2>
    {#each FIELDS.filter((f) => f.section === section) as field (field.key)}
      <div class="row">
        <label for={field.key}>{field.label}</label>
        <div class="control">
          {#if field.kind === 'bool'}
            <input id={field.key} type="checkbox" checked={Boolean(config[field.key])}
              onchange={(e) => set(field.key, e.currentTarget.checked)} />
          {:else if field.kind === 'select' && field.dynamic === 'monitors'}
            <select id={field.key} value={String(config[field.key] ?? 0)}
              onchange={(e) => set(field.key, Number(e.currentTarget.value))}>
              {#each monitors as monitor (monitor.index)}
                <option value={String(monitor.index)}>
                  {monitor.name} - {monitor.width}x{monitor.height} ({monitor.adapter})
                </option>
              {/each}
            </select>
          {:else if field.kind === 'select'}
            <select id={field.key} value={String(config[field.key] ?? '')}
              onchange={(e) => set(field.key, e.currentTarget.value)}>
              {#each field.options ?? [] as option (option.value)}
                <option value={option.value}>{option.label}</option>
              {/each}
            </select>
          {:else if field.kind === 'number'}
            <input id={field.key} type="number" min={field.min} max={field.max}
              value={Number(config[field.key] ?? 0)}
              onchange={(e) => set(field.key, Number(e.currentTarget.value))} />
          {:else if field.kind === 'folder'}
            <input id={field.key} readonly value={String(config['clip_dir_resolved'] ?? '')} />
            <span class="hint">Change it from the Trix tray icon.</span>
          {:else if field.kind === 'hotkey'}
            <input id={field.key} readonly value={capture ?? String(config[field.key] ?? '')}
              onkeydown={captureHotkey} placeholder="click, then press a combination" />
            {#if capture && capture !== config[field.key]}
              <button onclick={() => { void set('clip_hotkey', capture); capture = null; }}>Save</button>
            {/if}
            <button
              onclick={() => {
                listening = !listening;
                heard = false;
              }}>{listening ? 'Stop test' : 'Test'}</button
            >
            {#if listening}
              <span class="hint" class:ok={heard}>
                {heard ? 'Trix received it.' : 'Press the hotkey now...'}
              </span>
            {/if}
          {/if}
        </div>
        <p class="help">{field.help}</p>
      </div>
    {/each}
  </section>
{/each}

{#if extras.length > 0}
  <p class="help">
    This daemon has settings this app does not render yet: {extras.join(', ')}. Edit them in config.toml.
  </p>
{/if}

<style>
  h1 { font-size: 18px; margin: 0 0 18px; }
  h2 { font-size: 12px; text-transform: uppercase; letter-spacing: 0.08em; color: var(--dim); margin: 22px 0 10px; }
  .row { display: grid; grid-template-columns: 180px 1fr; gap: 6px 14px; align-items: center; padding: 8px 0; border-bottom: 1px solid var(--line); }
  .control { display: flex; align-items: center; gap: 8px; }
  .help { grid-column: 2; margin: 0; font-size: 12px; color: var(--dim); }
  .hint { font-size: 12px; color: var(--dim); }
  .hint.ok { color: var(--accent); }
  input, select { padding: 6px 10px; border-radius: 6px; border: 1px solid var(--line); background: var(--bg); color: var(--text); font: inherit; min-width: 120px; }
  input[readonly] { color: var(--dim); }
  button { padding: 5px 12px; border-radius: 6px; border: 1px solid var(--line); background: var(--panel); color: var(--text); font: inherit; cursor: pointer; }
  .notice { display: flex; align-items: center; gap: 12px; padding: 10px 14px; border: 1px solid var(--accent); border-radius: 8px; margin-bottom: 16px; }
  .notice button { margin-left: auto; }
</style>
```

Route it in `App.svelte` where the Task 4 placeholder sits.

- [ ] **Step 6: Verify**

Run `cargo tauri dev`. Expected:
- Monitor dropdown lists real monitors with adapter names, encoders and bounds behave, and setting `fps` while armed raises the "Re-arm to apply" banner.
- Typing an out-of-range number is refused with the daemon's own wording, and the field snaps back to the saved value.
- **The hotkey test is the one that matters:** click **Test**, press the hotkey, and the hint turns "Trix received it." Then bind something an overlay owns and confirm the message is honest.

- [ ] **Step 7: Commit**

```bash
git add crates/trix-ui/web/src
git commit -m "feat(ui): add the settings page over every config key, with a live hotkey test"
```

---

### Task 10: First run

Spec §7.4: no config file means a three-step setup, not an empty grid.

**Files:**
- Modify: `crates/trix-daemon/src/state.rs`, `crates/trix-daemon/src/dispatch.rs`
- Create: `crates/trix-ui/web/src/views/FirstRun.svelte`
- Modify: `crates/trix-ui/web/src/App.svelte`, `src/lib/state.svelte.ts`, the spec §4.3

- [ ] **Step 1: Write the failing daemon test**

In `dispatch.rs`'s tests. Note `with_scratch_config`, not `idle`: `idle` builds a daemon with `config_path: None`, where a `config.set` has nowhere to write and so could never flip this flag.

```rust
    /// Spec §7.4 defines first run as "no config file". `Config::load` never
    /// writes one, so the fact is real and only the daemon can see it — a UI
    /// cannot tell a default from a saved value that happens to equal it.
    #[test]
    fn config_get_reports_whether_a_config_file_exists() {
        let (daemon, path, _dir) = with_scratch_config("first-run");
        assert!(!path.exists(), "the scratch config starts absent");

        let before = daemon.dispatch(1, &request(1, "config.get"));
        let data = before.data.expect("config.get carries data");
        assert_eq!(data["config_file_exists"], false, "no file yet, so this is first run");

        let set = daemon.dispatch(1, &request_with(2, "config.set", &[("fps", 30.into())]));
        assert!(set.ok, "{:?}", set.error);

        let after = daemon.dispatch(1, &request(3, "config.get"));
        assert_eq!(
            after.data.expect("data")["config_file_exists"],
            true,
            "the first config.set is what ends first run"
        );
    }

    /// A daemon with nowhere to persist has no config file by definition, and
    /// must say so rather than reporting on some other file's existence.
    #[test]
    fn a_daemon_with_no_config_path_is_always_first_run() {
        let response = idle("no-config-path").dispatch(1, &request(1, "config.get"));
        assert_eq!(response.data.expect("data")["config_file_exists"], false);
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p trix-daemon config_file_exists`
Expected: FAIL — `config_file_exists` is `Null`, not `false`.

- [ ] **Step 3: Implement it**

Entirely in `state.rs`'s `config_json`, beside the `clip_dir_resolved` insert:

```rust
        // Spec §7.4's first run is "no config file", and `Config::load` never
        // writes one, so this is an observation rather than a flag somebody
        // has to remember to clear.
        //
        // Read from this daemon's own `config_path` rather than from
        // `Config::path()`. They are the same thing in production, and very
        // much not in a test: a static lookup would consult the developer's
        // real `%APPDATA%\trix\config.toml` and answer `true` no matter what
        // this daemon was pointed at, which would make the test below pass
        // for the wrong reason and hide the case it exists to cover.
        object.insert(
            "config_file_exists".to_string(),
            Value::Bool(self.config_path.as_ref().is_some_and(|path| path.is_file())),
        );
```

Document it in the spec's §4.3 `config.get` row, next to `clip_dir_resolved`: like it, `config_file_exists` is not a config key and is not settable.

- [ ] **Step 4: Write the wizard**

`crates/trix-ui/web/src/views/FirstRun.svelte`:

```svelte
<script lang="ts">
  import { call, onDaemonEvent } from '../lib/ipc';
  import { app } from '../lib/state.svelte';
  import type { Monitor } from '../lib/types';

  let step = $state(1);
  let monitors = $state<Monitor[]>([]);
  let monitorIndex = $state(0);
  let hotkey = $state('alt+f10');
  let heard = $state(false);
  let clipDir = $state('');

  $effect(() => {
    void (async () => {
      try {
        const list = await call<{ monitors: Monitor[] }>('monitors.list');
        monitors = list.monitors;
        const config = await call<Record<string, unknown>>('config.get');
        hotkey = String(config['clip_hotkey'] ?? 'alt+f10');
        clipDir = String(config['clip_dir_resolved'] ?? '');
      } catch (e) {
        app.toast('error', String(e));
      }
    })();
  });

  onDaemonEvent((event) => {
    if (event.event === 'hotkey_pressed') heard = true;
  });

  async function finish() {
    try {
      // One call, because config.set is all-or-nothing: a wizard that wrote
      // three keys in three calls could leave a half-configured install behind
      // if the second failed.
      await call('config.set', { monitor_index: monitorIndex, clip_hotkey: hotkey });
      await call('arm');
      await app.refreshStatus();
      app.view = 'grid';
    } catch (e) {
      app.toast('error', String(e));
    }
  }
</script>

<div class="wizard">
  <h1>Set up Trix</h1>
  <p class="step">Step {step} of 3</p>

  {#if step === 1}
    <h2>Which screen do you play on?</h2>
    <div class="choices">
      {#each monitors as monitor (monitor.index)}
        <button class:chosen={monitorIndex === monitor.index} onclick={() => (monitorIndex = monitor.index)}>
          <strong>{monitor.name}</strong>
          <span>{monitor.width}x{monitor.height} - {monitor.adapter}</span>
        </button>
      {/each}
    </div>
    <button class="next" onclick={() => (step = 2)}>Next</button>
  {:else if step === 2}
    <h2>Confirm your clip hotkey</h2>
    <p class="hint">Press it now. Some overlays quietly take a hotkey inside games, so this checks Trix really gets it.</p>
    <input readonly value={hotkey} />
    <p class="hint" class:ok={heard}>{heard ? 'Trix received it.' : 'Waiting for a press...'}</p>
    <button class="next" onclick={() => (step = 3)}>Next</button>
  {:else}
    <h2>Where should clips go?</h2>
    <input readonly value={clipDir} />
    <p class="hint">You can change this any time from the Trix tray icon.</p>
    <button class="next" onclick={finish}>Finish and arm</button>
  {/if}
</div>

<style>
  .wizard { max-width: 560px; margin: 8vh auto; display: grid; gap: 10px; }
  h1 { font-size: 22px; margin: 0; }
  h2 { font-size: 16px; margin: 14px 0 4px; }
  .step { color: var(--dim); margin: 0; font-size: 12px; }
  .choices { display: grid; gap: 8px; }
  .choices button { display: grid; gap: 2px; text-align: left; padding: 10px 12px; border-radius: 8px; border: 1px solid var(--line); background: var(--panel); color: var(--text); font: inherit; cursor: pointer; }
  .choices button.chosen { border-color: var(--accent); }
  .choices span { color: var(--dim); font-size: 12px; }
  .hint { color: var(--dim); font-size: 12px; margin: 2px 0; }
  .hint.ok { color: var(--accent); }
  input { padding: 8px 12px; border-radius: 6px; border: 1px solid var(--line); background: var(--bg); color: var(--dim); font: inherit; }
  .next { justify-self: start; margin-top: 12px; padding: 9px 20px; border-radius: 8px; border: 1px solid var(--accent); background: var(--accent); color: #06121f; font: inherit; font-weight: 600; cursor: pointer; }
</style>
```

- [ ] **Step 5: Route it**

In `wireDaemon`'s `onConnected`, after `refreshStatus`:

```ts
    try {
      const config = await call<Record<string, unknown>>('config.get');
      if (config['config_file_exists'] === false) app.view = 'firstrun';
    } catch {
      // A config.get that fails is not a reason to force a wizard on someone
      // who may have a perfectly good config; the grid is the safer default.
    }
```

and add `{:else if app.view === 'firstrun'}<FirstRun />` to `App.svelte` — outside the rail layout, since the wizard owns the window.

- [ ] **Step 6: Verify with a scratch profile**

```powershell
$scratch = Join-Path $env:TEMP ("trix-firstrun-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force $scratch | Out-Null
$env:APPDATA = $scratch
# then start the daemon and the app from this same shell
```

Expected: the wizard, not the grid. Complete it and confirm `%APPDATA%\trix\config.toml` now exists, the daemon is armed, and restarting the app goes straight to the grid.

- [ ] **Step 7: Run everything and commit**

```bash
cargo test --workspace
npm --prefix crates/trix-ui/web test
cargo fmt
git add crates/trix-core/src crates/trix-daemon/src crates/trix-ui/web/src docs/superpowers/specs
git commit -m "feat: report whether a config file exists, and open first-run setup when none does"
```

---

### Task 11: The tray opens the app

`window.rs:234` maps "Open Trix" to `Action::OpenClipsFolder` with the comment "Until stage 4 ships one". Stage 4 ships one.

**Files:**
- Modify: `crates/trix-daemon/src/window.rs`

- [ ] **Step 1: Write the failing test**

In `window.rs`'s tests:

```rust
    /// Both the menu item and a left click open the app now. Until this task
    /// they opened the clips folder, which was the right placeholder for a
    /// daemon with no app and is the wrong behaviour for one that has it.
    #[test]
    fn opening_trix_launches_the_app_beside_the_daemon() {
        let daemon = Path::new(r"C:\Program Files\Trix\trix-daemon.exe");
        assert_eq!(
            ui_path_beside(daemon),
            Some(PathBuf::from(r"C:\Program Files\Trix\trix-ui.exe"))
        );
        assert_eq!(ui_path_beside(Path::new("trix-daemon.exe")), None);
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p trix-daemon opening_trix`
Expected: FAIL — `cannot find function ui_path_beside`.

- [ ] **Step 3: Implement it**

First extract the existing folder-opening body — today it is inline in the `Action::OpenClipsFolder` arm at `window.rs:415-422`, and this task needs to call it from a second place:

```rust
/// Opens the clips folder in Explorer.
fn open_clips_folder(daemon: &Arc<Daemon>) {
    let dir = daemon.clip_dir();
    // `explorer.exe` returns a non-zero exit code even on success, so its
    // status is deliberately ignored rather than logged as a failure the user
    // would see in the log for no reason.
    let _ = std::process::Command::new("explorer.exe").arg(&dir).spawn();
    tracing::debug!(dir = %dir.display(), "opened the clips folder");
}
```

Leave `Action::OpenClipsFolder => open_clips_folder(daemon),` behind — the menu item still exists and still does exactly this. Then add:

```rust
/// Where `trix-ui.exe` lives, given the daemon's own path: beside it, the way
/// spec §8's MSI installs all three binaries and the way cargo builds them.
fn ui_path_beside(daemon_exe: &Path) -> Option<PathBuf> {
    Some(daemon_exe.parent()?.join("trix-ui.exe"))
}

/// Opens the desktop app, or the clips folder if it is not installed.
///
/// The fallback is not politeness: `trix-daemon.exe` is a supported thing to
/// run on its own (it is what the autostart entry runs, and what the smoke
/// scripts drive), so "Open Trix" has to do something useful on a machine that
/// has the daemon and no app. The app's own single-instance plugin handles the
/// second click — this side always just runs the exe.
fn open_app(daemon: &Arc<Daemon>) {
    let ui = std::env::current_exe().ok().and_then(|exe| ui_path_beside(&exe));
    let Some(ui) = ui.filter(|path| path.exists()) else {
        open_clips_folder(daemon);
        return;
    };
    if let Err(error) = std::process::Command::new(&ui).spawn() {
        tracing::warn!(path = %ui.display(), %error, "could not start the Trix app");
        open_clips_folder(daemon);
    }
}
```

Point both `ID_OPEN_UI` and the left-click arm at a new `Action::OpenApp`, handled with `open_app(daemon)`, and delete the "Until stage 4 ships one" comment.

- [ ] **Step 4: Verify**

Build both binaries into one directory and click the tray icon.

```bash
cargo build --release -p trix-daemon
cargo tauri build --no-bundle --config crates/trix-ui/tauri.conf.json
```

Expected: left-clicking the tray icon opens the app; clicking again focuses the same window rather than opening a second one; "Open Trix" in the menu does the same.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add crates/trix-daemon/src/window.rs
git commit -m "feat(daemon): open the desktop app from the tray now that there is one"
```

---

### Task 12: The stage-4 gate

**Files:**
- Create: `scripts/ui-smoke.ps1`
- Modify: `crates/trix-daemon/src/pipe.rs`, `README.md`, the spec §10

- [ ] **Step 1: Make a client connection observable**

In `pipe.rs`, at the point a client is accepted, add an info-level line:

```rust
            tracing::info!("client connected");
```

Without this the gate can only assert that a process started and did not exit, which is not the same as the app having reached the daemon — and "the window opened but nothing loaded" is exactly the failure worth catching.

- [ ] **Step 2: Write the gate**

`scripts/ui-smoke.ps1` — ASCII only, and note the strict-mode trap the stage-3 gate already cost a run: never `$items.Name` over a possibly-empty array, and remember `Check` evaluates its `Detail` on the passing path too.

```powershell
<#
.SYNOPSIS
    Stage-4 machine gate: the desktop app builds, stays off the engine, and
    finds the daemon on its own.

.DESCRIPTION
    This gate cannot see a webview, and does not pretend to. What it proves is
    the half that is mechanical: that trix-ui still does not depend on
    trix-core, that both test suites pass, that the two binaries land in one
    directory, that the app survives a missing daemon instead of dying on it,
    that it connects by itself once one appears, and that a second launch
    focuses rather than duplicates.

    Everything a person has to look at -- the grid filling within a second of
    the hotkey, playback, seeking, the settings round trip -- is in the plan's
    hand-verification checklist and is NOT in here. A green run of this script
    is not stage 4.

    Runs the app against a SCRATCH %APPDATA%, so it can neither read nor
    rewrite the developer's real config.toml or clip library.

    Requires:
        cargo build --release --workspace
        cargo tauri build --no-bundle --config crates/trix-ui/tauri.conf.json
#>
[CmdletBinding()]
param(
    [string]$UiPath,
    [string]$DaemonPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$script:Checks = New-Object System.Collections.Generic.List[object]

$RepoRoot = Split-Path -Parent $PSScriptRoot
if (-not $UiPath) { $UiPath = Join-Path $RepoRoot 'target\release\trix-ui.exe' }
if (-not $DaemonPath) { $DaemonPath = Join-Path $RepoRoot 'target\release\trix-daemon.exe' }

function Check {
    param([Parameter(Mandatory)][string]$Name, [Parameter(Mandatory)][bool]$Condition, [string]$Detail = '')
    $status = if ($Condition) { 'PASS' } else { 'FAIL' }
    $line = "  [$status] $Name"
    if ($Detail -and -not $Condition) { $line += " -- $Detail" }
    if ($Condition) { Write-Host $line -ForegroundColor Green } else { Write-Host $line -ForegroundColor Red }
    $script:Checks.Add([PSCustomObject]@{ Name = $Name; Passed = $Condition; Detail = $Detail })
}

# Not fail-fast, for daemon-smoke.ps1's reason: this builds a Tauri app, so a
# run that stops at the first failure costs minutes to learn one fact.

$scratch = Join-Path $env:TEMP ("trix-ui-smoke-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force $scratch | Out-Null
$log = Join-Path $scratch 'daemon.log'
$started = New-Object System.Collections.Generic.List[object]

try {
    # --- 1. The rule ----------------------------------------------------------
    & powershell -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'ui-isolation.ps1') | Out-Null
    Check 'trix-ui does not depend on trix-core' ($LASTEXITCODE -eq 0) 'see scripts/ui-isolation.ps1'

    # --- 2. The suites --------------------------------------------------------
    Push-Location $RepoRoot
    try {
        & cargo test --workspace --quiet 2>&1 | Out-Null
        Check 'cargo test --workspace passes' ($LASTEXITCODE -eq 0)

        & npm --prefix crates/trix-ui/web test 2>&1 | Out-Null
        Check 'the frontend unit tests pass' ($LASTEXITCODE -eq 0) 'npm --prefix crates/trix-ui/web test'
    }
    finally {
        Pop-Location
    }

    # --- 3. The layout both launch paths assume -------------------------------
    Check 'trix-ui.exe was built' (Test-Path -LiteralPath $UiPath) $UiPath
    Check 'trix-daemon.exe was built' (Test-Path -LiteralPath $DaemonPath) $DaemonPath
    Check 'both binaries share one directory' `
        ((Split-Path -Parent $UiPath) -eq (Split-Path -Parent $DaemonPath)) `
        'the tray launches the app, and the app launches the daemon, by looking beside itself'

    # --- 4. A missing daemon is a panel, not a crash --------------------------
    $env:APPDATA = $scratch
    $ui = Start-Process -FilePath $UiPath -PassThru
    $started.Add($ui)
    Start-Sleep -Seconds 5
    Check 'the app survives having no daemon' (-not $ui.HasExited) `
        'it should show the "Trix isn''t running" panel and keep retrying'

    # --- 5. It finds a daemon that appears later ------------------------------
    $daemon = Start-Process -FilePath $DaemonPath -ArgumentList '-v' -PassThru `
        -RedirectStandardError $log -WindowStyle Hidden
    $started.Add($daemon)

    $deadline = (Get-Date).AddSeconds(10)
    $connected = $false
    while ((Get-Date) -lt $deadline -and -not $connected) {
        if (Test-Path -LiteralPath $log) {
            $connected = (Select-String -LiteralPath $log -Pattern 'client connected' -Quiet) -eq $true
        }
        Start-Sleep -Milliseconds 250
    }
    Check 'the app reconnects on its own once a daemon exists' $connected `
        "no 'client connected' in $log within 10s -- the backoff loop in daemon.rs"

    # --- 6. One window, not two ----------------------------------------------
    $second = Start-Process -FilePath $UiPath -PassThru
    $started.Add($second)
    $exited = $second.WaitForExit(5000)
    Check 'a second launch focuses the first instead of opening a window' $exited `
        'tauri-plugin-single-instance should make the second copy hand over focus and exit'
}
finally {
    foreach ($p in $started) {
        # -ErrorAction on Stop-Process suppresses the message but still fails
        # the tool, so the whole thing is wrapped: a process that already
        # exited is the normal case here, not a problem.
        try { if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force -ErrorAction Stop } } catch {}
    }
    Remove-Item -Recurse -Force $scratch -ErrorAction SilentlyContinue
}

$passed = @($script:Checks | Where-Object { $_.Passed }).Count
$failed = @($script:Checks | Where-Object { -not $_.Passed }).Count

''
if ($failed -gt 0) {
    Write-Host "STAGE-4 MACHINE GATE FAILED  ($passed passed, $failed failed)" -ForegroundColor Red
    # ForEach-Object, not .Name: under Set-StrictMode member enumeration over
    # an empty collection throws, and this list is empty on every green run.
    $script:Checks | Where-Object { -not $_.Passed } | ForEach-Object {
        Write-Host "  - $($_.Name)" -ForegroundColor Red
    }
    exit 1
}
Write-Host "STAGE-4 MACHINE GATE PASSED  ($passed checks)" -ForegroundColor Green
Write-Host 'This gate cannot see a webview. Stage 4 is the hand-verification checklist in the plan.'
```

- [ ] **Step 3: Prove it can fail**

Temporarily rename `target/release/trix-daemon.exe` and run the gate.

Expected: check 7 fails with a clear message, and the script exits non-zero. Rename it back and confirm a clean run.

- [ ] **Step 4: Document the stage**

In the spec's §10, stage 4's line, record what is machine-gated and what is not: the app is verified by `scripts/ui-smoke.ps1` plus hand-verification, and **trim and export are explicitly not part of this stage** — they arrive with `library.export` in the next plan, which is when the second half of gate 4's sentence ("trims, exports; fast-mode export is lossless and sub-second") becomes checkable.

Add a short "Running the app" section to the README: `cargo tauri dev` from `crates/trix-ui` for development, `cargo tauri build --no-bundle` for a release binary, and the note that the MSI is a later plan.

- [ ] **Step 5: Run the whole thing**

```bash
powershell -ExecutionPolicy Bypass -File scripts/ui-smoke.ps1
powershell -ExecutionPolicy Bypass -File scripts/daemon-smoke.ps1
```

Expected: both green, and `daemon-smoke.ps1` still at 28 checks — this plan changed the daemon three times and none of them may cost it a check.

- [ ] **Step 6: Commit**

```bash
cargo fmt
git add scripts/ui-smoke.ps1 crates/trix-daemon/src/pipe.rs README.md docs/superpowers/specs
git commit -m "test: add the stage-4 gate for the desktop app"
```

---

## The stage-4 hand-verification gate

Machine checks cannot see a webview. Per spec §10 the user hand-verifies real artifacts before the next plan begins; this is that checklist. Hand it over, do not perform it and report success.

1. **The clip arrives.** Arm from the rail, play something, press the clip hotkey. A card with a real thumbnail appears in the grid **within a second**, and the thumbnail is the moment of the press.
2. **It plays.** Click it. The clip plays with audio in sync, and the scrubber seeks without stalling. (Seeking is the asset protocol's Range support — if scrubbing hangs, that is the thing to suspect.)
3. **The grid answers the glance.** `Space` on a selected card previews in place without leaving the grid. Arrows walk it and stop at the ends.
4. **The library actions are real.** Rename, favorite, Show in Explorer, and Delete each act on the clip on screen, and Delete confirms first.
5. **Settings do not lie.** Change the monitor and re-arm; capture follows. Change the clip hotkey and press the new one **without restarting anything** — the clip saves.
6. **The hotkey test tells the truth.** Bind a combination an overlay owns, press it, and the panel says Trix did not receive it.
7. **The daemon can be absent.** Quit from the tray with the app open: it falls to "Trix isn't running". **Start Trix** brings it back.
8. **First run works on a clean profile.** With a scratch `%APPDATA%`, the wizard appears, and finishing it leaves an armed daemon and a config file.
9. **The tray opens one window.** Click the tray icon twice; one window, focused.
10. **It costs nothing while you play.** With the app **closed** and the daemon armed, game fps is unchanged from before this plan — §10.1's ceilings bind the daemon, and the app's ~90-180 MB is only owed while its window is open.

**Not in this gate, deliberately:** trim, export, fast-vs-precise, `I`/`O`/`Ctrl+E`, and the MSI. The first four need `library.export` (the next plan); the MSI is the packaging plan.
