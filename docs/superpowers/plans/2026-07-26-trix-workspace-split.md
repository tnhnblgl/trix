# Trix Workspace Split Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Convert the single `trix` binary crate into a Cargo workspace with a reusable `trix-core` library and a `trix-cli` binary, with zero change in observable behaviour.

**Architecture:** Every module under `src/` except `main.rs` moves verbatim into `crates/trix-core/src/` behind a new `lib.rs` that re-exports them as `pub mod`. `main.rs` moves to `crates/trix-cli/src/main.rs` and swaps its `mod` declarations for `use trix_core::…`. Shared dependency versions hoist to `[workspace.dependencies]`; the release profile hoists to the workspace root. No logic is edited.

**Tech Stack:** Rust edition 2024, Cargo workspaces (resolver 3), windows-rs 0.62, windows-capture 2.0, clap 4.

**This is plan 1 of 4** for the desktop UI work specified in
[2026-07-26-trix-desktop-ui-design.md](../specs/2026-07-26-trix-desktop-ui-design.md). It implements
spec §3.1 (workspace layout) and §3.3 (CLI bypasses the daemon), and satisfies the Stage 1 gate in
§10. Plans 2–4 (control protocol, daemon + library, Tauri UI) are written after this one is verified.

## Global Constraints

- **Windows-only.** All crates target Windows; no cross-platform abstraction is introduced.
- **Rust edition 2024** on every crate, inherited via `[workspace.package]`.
- **No new third-party dependencies.** This plan adds zero crates — it only relocates existing ones.
- **The binary must remain `trix.exe`.** Existing verification commands, scripts, and the user's
  muscle memory depend on the exact name and CLI surface. Enforced via `[[bin]] name = "trix"`.
- **Zero behavioural change.** Any observable difference in output, timing, file size, or logging is
  a defect in this plan, not an improvement. Refactoring, renaming, and "while I'm here" cleanups are
  out of scope.
- **`trix-core` must not depend on `clap` or `tracing-subscriber`.** Argument parsing and subscriber
  installation are CLI concerns. The daemon (plan 3) installs its own subscriber.
- **Release profile values are copied verbatim** from the current root `Cargo.toml`:
  `opt-level = 3`, `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`, `strip = "symbols"`.

## File Structure

**Created:**

| Path | Responsibility |
|---|---|
| `Cargo.toml` (rewritten) | Virtual workspace manifest: members, shared dep versions, release profile |
| `crates/trix-core/Cargo.toml` | Library manifest — engine deps only, no CLI deps |
| `crates/trix-core/src/lib.rs` | Declares the eight public modules. The crate's entire surface. |
| `crates/trix-core/tests/public_api.rs` | Integration test proving the engine is reachable from outside the crate |
| `crates/trix-cli/Cargo.toml` | Binary manifest producing `trix.exe` |
| `crates/trix-cli/src/main.rs` | Moved from `src/main.rs`; `mod` → `use trix_core::…` |

**Moved verbatim** (`git mv`, contents untouched except where noted):

```
src/capture/{mod,audio,video}.rs   -> crates/trix-core/src/capture/
src/encode/{mod,convert,h264,mf}.rs -> crates/trix-core/src/encode/
src/{config,control,probe,record,replay,stats}.rs -> crates/trix-core/src/
src/main.rs                        -> crates/trix-cli/src/main.rs   (edited, Task 2)
```

**Why this decomposition:** `trix-core` is the engine and nothing else — it has no opinion about who
drives it, which is what lets the daemon (plan 3) and the CLI consume it as peers. `trix-cli` keeps
its direct dependency on `trix-core` rather than routing through the future daemon, because it is the
ground-truth verification harness for every phase of this project; putting the thing under test
behind the thing under test would destroy that.

---

### Task 1: Workspace skeleton and the `trix-core` library

**Files:**
- Modify: `Cargo.toml` (full rewrite — becomes a virtual workspace manifest)
- Create: `crates/trix-core/Cargo.toml`
- Create: `crates/trix-core/src/lib.rs`
- Create: `crates/trix-core/tests/public_api.rs`
- Move: all of `src/` except `src/main.rs` → `crates/trix-core/src/`

**Interfaces:**
- Consumes: nothing (first task).
- Produces: the `trix_core` library crate exposing exactly these eight modules —
  `capture`, `config`, `control`, `encode`, `probe`, `record`, `replay`, `stats`.
  Task 2 consumes six of them:
  `trix_core::capture::video::snapshot(monitor_index: u32, path: PathBuf) -> Result<()>`,
  `trix_core::capture::video::measure(monitor_index: u32, seconds: u64) -> Result<()>`,
  `trix_core::capture::audio::record_wav(seconds: u64, path: &Path) -> Result<()>`,
  `trix_core::config::Config::load() -> Config`,
  `trix_core::control::install_shutdown_handler() -> Result<()>`,
  `trix_core::control::acquire_single_instance() -> Result<()>`,
  `trix_core::probe::run() -> Result<()>`,
  `trix_core::record::run(config: &Config, options: RecordOptions) -> Result<()>` with
  `trix_core::record::RecordOptions { duration_secs: u64, output: PathBuf, no_audio: bool }`,
  `trix_core::replay::run(config: &Config, options: ReplayOptions) -> Result<()>` with
  `trix_core::replay::ReplayOptions { auto_clip_secs: Option<u64>, exit_after_secs: Option<u64> }`.

- [ ] **Step 1: Move the engine sources, preserving git history**

Run from the repo root. `git mv` keeps history attached to the new paths, which matters because
this code took seven phases to certify and `git log --follow` must keep working.

First tag the current commit. Task 3 builds this exact tree to prove the engine still behaves
identically, and a tag is deterministic where `HEAD~N` is not — the number of intervening commits
depends on how tasks get squashed.

```bash
git tag presplit-baseline
git tag -l presplit-baseline   # confirm: prints presplit-baseline
```

```bash
mkdir -p crates/trix-core/src crates/trix-cli/src
git mv src/capture crates/trix-core/src/capture
git mv src/encode  crates/trix-core/src/encode
git mv src/config.rs  crates/trix-core/src/config.rs
git mv src/control.rs crates/trix-core/src/control.rs
git mv src/probe.rs   crates/trix-core/src/probe.rs
git mv src/record.rs  crates/trix-core/src/record.rs
git mv src/replay.rs  crates/trix-core/src/replay.rs
git mv src/stats.rs   crates/trix-core/src/stats.rs
git mv src/main.rs    crates/trix-cli/src/main.rs
```

Verify `src/` is now empty and remove it:

```bash
ls src 2>/dev/null || echo "src/ is gone"
```

Expected: `src/ is gone` (git mv removes the directory once it is empty).

- [ ] **Step 2: Write the workspace manifest**

Replace the entire contents of `Cargo.toml`. The `[dependencies]` and `[dependencies.windows]`
sections become `[workspace.dependencies]`; `[profile.release]` stays at the root because Cargo only
honours profiles in the workspace root manifest.

```toml
[workspace]
resolver = "3"
members = ["crates/trix-core", "crates/trix-cli"]

[workspace.package]
version = "0.1.0"
edition = "2024"

[workspace.dependencies]
anyhow = "1"
clap = { version = "4", features = ["derive"] }
serde = { version = "1", features = ["derive"] }
thiserror = "2"
toml = "0.8"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
wasapi = "0.23.0"
windows-capture = "2.0.0"

[workspace.dependencies.windows]
version = "0.62"
features = [
    "Wdk_Graphics_Direct3D",
    "Win32_Foundation",
    "Win32_Graphics_Direct3D11",
    "Win32_Graphics_Dxgi",
    "Win32_Graphics_Dxgi_Common",
    "Win32_Graphics_Gdi",
    "Win32_Media_MediaFoundation",
    "Win32_System_Com",
    "Win32_System_Console",
    "Win32_System_LibraryLoader",
    "Win32_System_Ole",
    "Win32_System_ProcessStatus",
    "Win32_System_SystemInformation",
    "Win32_System_Threading",
    "Win32_System_Variant",
    "Win32_UI_Input_KeyboardAndMouse",
    "Win32_UI_WindowsAndMessaging",
]

[profile.release]
opt-level = 3
lto = "fat"
codegen-units = 1
panic = "abort"
strip = "symbols"
```

- [ ] **Step 3: Write the `trix-core` manifest**

Create `crates/trix-core/Cargo.toml`. Note the deliberate absence of `clap` and
`tracing-subscriber` — the engine has no business parsing arguments or installing a global
subscriber.

```toml
[package]
name = "trix-core"
version.workspace = true
edition.workspace = true
description = "Trix capture engine — hardware-accelerated screen capture, encode, replay ring, and mux"

[dependencies]
anyhow.workspace = true
serde.workspace = true
thiserror.workspace = true
toml.workspace = true
tracing.workspace = true
wasapi.workspace = true
windows-capture.workspace = true
windows.workspace = true
```

- [ ] **Step 4: Write the `trix-cli` manifest**

Cargo refuses to load a workspace whose members lack manifests, so this must exist before any
`cargo` command works — even one scoped to `trix-core`. Its `src/main.rs` will not compile yet
(Task 2 fixes that), but `cargo test -p trix-core` never builds it.

Create `crates/trix-cli/Cargo.toml`:

```toml
[package]
name = "trix-cli"
version.workspace = true
edition.workspace = true
description = "Trix command-line interface"

[[bin]]
name = "trix"
path = "src/main.rs"

[dependencies]
trix-core = { path = "../trix-core" }
anyhow.workspace = true
clap.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
```

`[[bin]] name = "trix"` is what keeps the output at `trix.exe` despite the package being named
`trix-cli`. Without it the binary would become `trix-cli.exe` and every existing verification
command would break.

- [ ] **Step 5: Write the failing public-API test**

Create `crates/trix-core/tests/public_api.rs`. An integration test lives outside the crate, so it can
only see genuinely public items — which makes it a real guard that the engine is consumable by the
daemon in plan 3, not just internally coherent.

```rust
//! Proves the engine is reachable from outside the crate. If the daemon
//! (plan 3) can't call these, neither can this test.

use std::path::PathBuf;

use trix_core::config::{Config, RateControl};
use trix_core::record::RecordOptions;
use trix_core::replay::ReplayOptions;

#[test]
fn config_is_publicly_constructible() {
    let config = Config::default();
    assert_eq!(config.fps, 60);
    assert_eq!(config.bitrate_kbps, 8000);
    assert_eq!(config.stats_seconds, 0, "stats must stay off by default");
    assert_eq!(config.rate_control(), RateControl::PeakVbr);
    assert!(config.gpu_priority_low(), "gpu priority must default to low");
}

#[test]
fn session_option_structs_are_publicly_constructible() {
    let _record = RecordOptions {
        duration_secs: 10,
        output: PathBuf::from("out.mp4"),
        no_audio: false,
    };
    let _replay = ReplayOptions { auto_clip_secs: Some(8), exit_after_secs: Some(12) };
}

#[test]
fn bitrate_helpers_are_public() {
    let config = Config::default();
    assert_eq!(config.bitrate_bps(), 8_000_000);
    assert_eq!(config.max_bitrate_bps(), 12_000_000, "0 means auto = 1.5x target");
}
```

- [ ] **Step 6: Run the test to verify it fails**

Run: `cargo test -p trix-core --test public_api`

Expected: FAIL with `error: failed to parse manifest` … `no targets specified in the manifest` —
`crates/trix-core/` has source files but no `lib.rs`, so Cargo sees no library to build and the
`use trix_core::…` imports have nothing to resolve against.

This is a genuine red state: the modules exist on disk but are not yet a public library. Step 7
turns them into one.

If it instead compiles and fails on an assertion, a module is already reachable by some other
route — investigate before continuing.

- [ ] **Step 7: Write `lib.rs` to turn the modules into a library**

Create `crates/trix-core/src/lib.rs`. This is the whole public surface — the same eight modules
`main.rs` declared privately, now `pub`.

```rust
//! Trix capture engine.
//!
//! Hardware-accelerated screen capture, H.264/AAC encode, an in-RAM replay
//! ring, and MP4 muxing — with no opinion about what drives it. The CLI and
//! the daemon are peers, both consuming this crate.

pub mod capture;
pub mod config;
pub mod control;
pub mod encode;
pub mod probe;
pub mod record;
pub mod replay;
pub mod stats;
```

- [ ] **Step 8: Run the core tests and verify they pass**

Run: `cargo test -p trix-core`

Expected: PASS — `11 passed` from the unit tests (5 in `control.rs`, 6 in `stats.rs`) plus
`3 passed` from `public_api`. The `trix-cli` crate still fails to compile at this point; that is
Task 2's job. Confirm the core result specifically:

```bash
cargo test -p trix-core 2>&1 | grep -E "test result|running"
```

Expected output contains `test result: ok. 11 passed` and `test result: ok. 3 passed`.

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml crates/trix-core crates/trix-cli/Cargo.toml
git commit -m "refactor: extract trix-core library crate

Move every engine module out of the binary crate into a workspace
library. Files move verbatim via git mv; no logic changes.

trix-core deliberately does not depend on clap or tracing-subscriber —
argument parsing and subscriber installation belong to whoever drives
the engine, not the engine.

Adds tests/public_api.rs, an integration test that can only see truly
public items, guarding the surface the daemon will consume."
```

---

### Task 2: The `trix-cli` binary

**Files:**
- Modify: `crates/trix-cli/src/main.rs:1-8` (replace the `mod` block with a `use`)
- Modify: `crates/trix-cli/src/main.rs` (append a test module)

**Interfaces:**
- Consumes: the eight `trix_core` modules listed in Task 1's *Produces* block.
- Produces: `target/{debug,release}/trix.exe` with a CLI surface byte-identical to the pre-split
  binary — subcommands `probe`, `record`, `replay`; global `-v/--verbose`; hidden `--auto-clip` and
  `--exit-after` on `replay`.

- [ ] **Step 1: Write the failing CLI-surface test**

The split's real risk is silently losing a flag. This test locks the surface down. Append to
`crates/trix-cli/src/main.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Every subcommand and flag the verification workflow depends on.
    /// If the split drops one, this fails instead of a test script failing
    /// three phases later.
    #[test]
    fn cli_surface_is_unchanged() {
        assert!(Cli::try_parse_from(["trix", "probe"]).is_ok());
        assert!(Cli::try_parse_from(["trix", "probe", "--snapshot"]).is_ok());
        assert!(Cli::try_parse_from(["trix", "probe", "--snapshot", "s.png"]).is_ok());
        assert!(Cli::try_parse_from(["trix", "probe", "--capture", "5"]).is_ok());
        assert!(Cli::try_parse_from(["trix", "probe", "--audio", "5"]).is_ok());
        assert!(Cli::try_parse_from(["trix", "record"]).is_ok());
        assert!(Cli::try_parse_from(["trix", "record", "-d", "10", "-o", "o.mp4"]).is_ok());
        assert!(Cli::try_parse_from(["trix", "record", "--no-audio"]).is_ok());
        assert!(Cli::try_parse_from(["trix", "replay"]).is_ok());
        assert!(Cli::try_parse_from(["trix", "-v", "replay"]).is_ok());
        assert!(
            Cli::try_parse_from(["trix", "replay", "--auto-clip", "8", "--exit-after", "12"])
                .is_ok(),
            "the hidden test flags are how every phase gets verified"
        );
        assert!(Cli::try_parse_from(["trix", "bogus"]).is_err());
    }

    #[test]
    fn record_defaults_match_the_pre_split_binary() {
        let cli = Cli::try_parse_from(["trix", "record"]).unwrap();
        let Command::Record { duration, output, no_audio } = cli.command else {
            panic!("expected Record");
        };
        assert_eq!(duration, 10);
        assert_eq!(output, std::path::PathBuf::from("output.mp4"));
        assert!(!no_audio);
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p trix-cli`

Expected: FAIL to compile — `main.rs` still starts with `mod capture;` … `mod stats;`, and those
directories no longer exist under `crates/trix-cli/src/`. Errors read
`file not found for module 'capture'`.

- [ ] **Step 3: Point `main.rs` at the library**

Replace lines 1–8 of `crates/trix-cli/src/main.rs` — the eight `mod` declarations — with a single
`use`. Nothing else in the file changes; every call site (`config::Config::load()`,
`record::run(…)`, `capture::video::snapshot(…)`, and so on) resolves exactly as before.

Replace:

```rust
mod capture;
mod config;
mod control;
mod encode;
mod probe;
mod record;
mod replay;
mod stats;
```

with:

```rust
use trix_core::{capture, config, control, probe, record, replay};
```

`encode` and `stats` are omitted deliberately — `main.rs` never referenced them directly; they are
reached through `record` and `replay`. Adding them would produce an `unused_imports` warning.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p trix-cli`

Expected: PASS — `test result: ok. 2 passed`.

- [ ] **Step 5: Run the whole workspace**

Run: `cargo test`

Expected: PASS across all three targets — `11 passed` (trix-core units), `3 passed` (public_api),
`2 passed` (trix-cli). Total 16, up from the pre-split 11, with no test removed.

- [ ] **Step 6: Verify the binary name and CLI surface by hand**

```bash
cargo build --release
ls -la target/release/trix.exe
./target/release/trix.exe --help
./target/release/trix.exe replay --help
```

Expected: `trix.exe` exists (roughly 1.5 MB), `--help` lists `probe`, `record`, `replay`, and
`replay --help` shows no `--auto-clip`/`--exit-after` (they are `hide = true`, and must stay hidden).

- [ ] **Step 7: Commit**

```bash
git add crates/trix-cli
git commit -m "refactor: point trix-cli at the trix-core library

Swaps eight private mod declarations for one use statement. Every call
site is unchanged, and [[bin]] name = \"trix\" keeps the output at
trix.exe so existing verification commands still work.

Adds a CLI-surface test so a dropped flag fails here rather than in a
verification script three phases later."
```

---

### Task 3: Behavioural equivalence and documentation

This task produces no new code. It is the Stage 1 gate from spec §10: proof that a seven-phase-
certified engine behaves identically after being moved.

**Files:**
- Modify: `PLAN.md` §1.4 "Module layout (single binary, workspace-ready)"

**Interfaces:**
- Consumes: `target/release/trix.exe` from Task 2.
- Produces: nothing consumed by later tasks. This is a verification and documentation gate.

- [ ] **Step 1: Build the pre-split binary alongside the new one**

Use the `presplit-baseline` tag from Task 1 Step 1 — deterministic regardless of how many commits
landed in between. A separate worktree lets both binaries exist at once, so the comparison runs
back-to-back on the same machine in the same session.

```bash
git status --short          # must be clean before starting
git worktree add ../trix-presplit presplit-baseline
cd ../trix-presplit && cargo build --release && cd -
```

Confirm the baseline really is pre-split:

```bash
ls ../trix-presplit/src/main.rs
```

Expected: the path exists. If it does not, the tag was placed after the move and the comparison is
meaningless — stop and re-tag from the correct commit.

- [ ] **Step 2: Run both binaries with identical settings**

Run each for the same duration against the same monitor. Trix writes tracing to **stdout**, not
stderr, so a plain redirect captures everything.

```bash
../trix-presplit/target/release/trix.exe replay --auto-clip 8 --exit-after 12 > before.log
./target/release/trix.exe            replay --auto-clip 8 --exit-after 12 > after.log
```

- [ ] **Step 3: Compare the summary lines**

```bash
grep -E "clip saved|frames=|encoder rate control" before.log
grep -E "clip saved|frames=|encoder rate control" after.log
```

Expected: the two `clip saved:` lines agree on duration (both ~8.0 s video) and packet count within
a few percent, and the megabyte figures land within ~10% of each other. Exact byte equality is **not**
expected — screen content differs between the two runs, and VBR responds to content. What must match
exactly is the *shape*: same fps, `dropped=0` in both, both logging
`encoder rate control configured`.

If `after.log` shows a different fps, non-zero drops that `before.log` did not have, or a missing
rate-control line, **stop** — the split changed behaviour and the cause must be found before
proceeding.

- [ ] **Step 4: Confirm both clips play**

Open `clip_*.mp4` from each run in a media player. Both must play with video and audio in sync.

- [ ] **Step 5: Clean up the comparison worktree**

```bash
git worktree remove ../trix-presplit
rm -f before.log after.log
```

- [ ] **Step 6: Update PLAN.md's module layout section**

`PLAN.md` §1.4 is titled "Module layout (single binary, workspace-ready)" and describes a layout that
no longer exists. Replace that section's body with the actual structure, and retitle it
"Module layout (workspace)":

```markdown
### 1.4 Module layout (workspace)

```
Cargo.toml                    virtual workspace: members, shared deps, release profile
crates/trix-core/             the engine — no CLI, no argument parsing, no subscriber
  src/lib.rs                    public surface: the eight modules below
  src/capture/{mod,audio,video}.rs
  src/encode/{mod,convert,h264,mf}.rs
  src/{config,control,probe,record,replay,stats}.rs
  tests/public_api.rs           guards the surface the daemon consumes
crates/trix-cli/              produces trix.exe
  src/main.rs                   clap surface + subscriber; depends on trix-core directly
```

The CLI depends on `trix-core` directly rather than routing through the daemon: it is the
ground-truth verification harness for every phase, and putting it behind the daemon would put the
thing under test behind the thing under test.

Planned additions (see `docs/superpowers/specs/2026-07-26-trix-desktop-ui-design.md`):
`trix-proto` (wire types), `trix-daemon` (tray + control socket), `trix-ui` (Tauri app).
```

- [ ] **Step 7: Verify the docs build and the tree is clean**

```bash
cargo test
git status --short
```

Expected: all 16 tests pass; `git status` shows only `M PLAN.md`.

- [ ] **Step 8: Commit**

```bash
git add PLAN.md
git commit -m "docs: describe the workspace layout in PLAN.md

Section 1.4 described a single-binary layout that no longer exists.

Stage 1 of the desktop UI spec is verified: 16 tests pass, and a
replay --auto-clip run against the pre-split binary produces a clip of
matching duration, fps, and drop count."
```

---

## Verification Summary

Stage 1 of spec §10 is satisfied when all of the following hold:

| Check | Command | Expected |
|---|---|---|
| Unit tests intact | `cargo test -p trix-core` | 11 passed (5 control, 6 stats) |
| Public surface reachable | `cargo test -p trix-core --test public_api` | 3 passed |
| CLI surface locked | `cargo test -p trix-cli` | 2 passed |
| Whole workspace | `cargo test` | 16 passed, 0 failed |
| Binary name preserved | `ls target/release/trix.exe` | exists, ~1.5 MB |
| Engine unchanged | `trix replay --auto-clip 8 --exit-after 12` | same fps, `dropped=0`, clip plays |
| History preserved | `git log --follow crates/trix-core/src/replay.rs` | shows pre-split commits |

## Self-Review Notes

Checked against the spec:

- **§3.1 workspace layout** — Tasks 1 and 2 create `trix-core` and `trix-cli`. `trix-proto`,
  `trix-daemon`, and `trix-ui` are deliberately deferred: nothing consumes them yet, and inventing
  wire types before a client exists would guess at the protocol rather than derive it. They arrive in
  plan 2.
- **§3.2 the `trix-ui` ↛ `trix-core` rule** — not applicable yet; no UI crate exists. The CI
  assertion enforcing it belongs to plan 4, where it can actually fail.
- **§3.3 CLI bypasses the daemon** — implemented by Task 2's direct `trix-core` dependency, and
  recorded in PLAN.md by Task 3.
- **§10 Stage 1** — Task 3 is the gate, comparing against a real artifact from the pre-split binary
  rather than asserting success.
- Everything else in the spec (§4 protocol, §5 library, §6 application, §7 lifecycle, §8 packaging)
  is out of scope for this plan by design.
