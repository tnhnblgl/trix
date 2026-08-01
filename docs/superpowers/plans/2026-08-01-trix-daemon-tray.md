# Trix Daemon & Tray Implementation Plan (stage 3 of 4)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn `trix-daemon` from a socket server into something a person can actually run — a tray icon, a clip hotkey that works while armed, thumbnails, a disk ceiling, and opt-in autostart — without breaking the <30 MB armed contract.

**Architecture:** The daemon gains one hidden message-only window on a dedicated pump thread. That single window owns both the tray icon (`Shell_NotifyIconW` needs an `HWND` for its callback) and the clip hotkey (`RegisterHotKey` binds to a thread's message queue), so stage 3 adds exactly one thread, not two. The pump thread never calls into `Daemon` directly — every action is forwarded to a worker, because `clip` blocks for a multi-second mux and a window that stops pumping is a window Windows marks as hung. Thumbnails are captured by flagging the capture session and letting the *next* frame stage itself to CPU, so the steady-state gameplay path pays nothing.

**Tech Stack:** Rust, `windows` 0.62 (Shell, Registry, and WIC features added), no new third-party crates.

**Spec:** `docs/superpowers/specs/2026-07-26-trix-desktop-ui-design.md` — §5.3 (thumbnails), §5.4 (disk ceiling), §6.3 (GOP), §7.2 (tray), §7.3 (autostart), §10 stage 3 (the gate).

**Ledger:** `.superpowers/sdd/progress.md`. It is git-ignored; `git clean -fdx` destroys it.

## Global Constraints

Every task's requirements implicitly include this section.

- **`panic = "abort"` is set. No panicking call — `unwrap`, `expect`, `panic!`, `unreachable!`, slice indexing — on any path reachable from a socket message, a tray click, or a hotkey press.** Reviewers treat a violation as Critical. `serde_json::json!` expands to `to_value(&x).unwrap()` on most arms; build payloads with explicit `From` conversions instead.
- **`trix probe`'s stdout is byte-frozen.** `trix-cli`'s CLI surface is frozen (`cli_surface_is_unchanged` guards it). Verify byte-identity by capturing both binaries through *the same shell* — a `Compare-Object` across two different shells gives a false positive on every non-ASCII line.
- **The daemon's working set stays under 30 MB while armed.** This is the product's entire thesis; it is checked by the stage-3 gate.
- **Do not push to GitHub.** Standing user instruction. Local commits only.
- **Formatting is `rustfmt` with `use_small_heuristics = "Max"`** (`rustfmt.toml`, commit `0f447c1`). `cargo fmt --all -- --check` must exit 0 before every commit.
- **The real test command is `cargo test --workspace`.** Bare `cargo test` is scoped by `default-members`. Suite baseline entering this plan: **101 passed, 0 failed**.
- **A gate that runs green against a stale binary proves nothing.** Freshness is a *timestamp* comparison — newest `.rs`/`Cargo.toml` under `crates\` vs the exe's `LastWriteTime` — never a version string. Reuse `Get-NewestSourceFile` from `scripts/protocol-smoke.ps1` by AST extraction, as `scripts/arm-cycle-leak.ps1` already does.
- **PowerShell array hazard:** `return ,$arr` and a call-site `@(...)` each fix unrolling alone but **cancel** when combined, silently making `.Count` equal 1 for every input. Test helpers through the shipped call-site expression, not a paraphrase of it.
- **A test that cannot fail is a defect.** This branch has shipped five. Before claiming a test passes, break the code it covers and confirm it goes red. A quiet/negative window must outlast the interval it rules out.

## Decisions carried in

Settled by the user on 2026-08-01, closing escalations opened during plan 2:

1. **Ring overshoot → pin the GOP to 1 second** (Task 1). Not a clamp on the reported number: the ring genuinely starts at the previous keyframe, which can be seconds back, so the fix belongs at the encoder.
2. **`monitors.list` DPI virtualisation → source sizes from the display mode** (Task 2). No process DPI-awareness manifest, so `trix probe`'s stdout is untouched.
3. **Full stage 3 in one plan, thumbnails last** (Task 9). The capture-path change lands on top of a daemon already verified by Tasks 7–8 rather than underneath it.

Still open and **out of scope here** — they belong to plan 4's settings page: the real product cap on `replay_seconds`/`bitrate_kbps` (sanity bounds shipped, policy outstanding), and `config.set` stripping comments from `config.toml`.

## File Structure

| File | Responsibility |
|---|---|
| `crates/trix-core/src/encode/h264.rs` | **Modify.** Pin GOP on the replay encoder only. |
| `crates/trix-core/src/encode/mf.rs` | **Modify.** Expose `set_codec_u32` to `h264.rs`. Do **not** add GOP to `apply_rate_control` — it is shared with `MfRecorder` (`mf.rs:481`), the `trix record` path, which §6.3 does not cover. |
| `crates/trix-core/src/probe.rs` | **Modify.** Add true-pixel sizes without touching what `print_monitors` prints. |
| `crates/trix-core/src/config.rs` | **Modify.** Two new keys: `max_library_gb`, `autostart`. |
| `crates/trix-core/src/engine.rs` | **Modify.** `spawn` seeds `EngineStatus.encoder` before returning. |
| `crates/trix-core/src/replay.rs` | **Modify.** Thumbnail request flag on `ReplaySession`; staging copy in the frame handler. |
| `crates/trix-core/src/thumb.rs` | **Create.** BGRA staging buffer → JPEG via WIC. Pure function, testable without a GPU. |
| `crates/trix-daemon/src/window.rs` | **Create.** Message-only window, pump thread, hotkey registration, action forwarding. |
| `crates/trix-daemon/src/tray.rs` | **Create.** Icon rasterisation, `Shell_NotifyIconW` lifecycle, popup menu. |
| `crates/trix-daemon/src/autostart.rs` | **Create.** `HKCU\...\Run` read/write. Registry is the source of truth. |
| `crates/trix-daemon/src/state.rs` | **Modify.** Disk ceiling on clip save; autostart wired into `config.get`/`config.set`. |
| `crates/trix-daemon/src/dispatch.rs` | **Modify.** `monitors.list` uses the true-pixel path. |
| `crates/trix-daemon/src/main.rs` | **Modify.** Start the window thread; tear the icon down on shutdown. |
| `scripts/daemon-smoke.ps1` | **Create.** The stage-3 gate. |
| `PLAN.md` | **Modify.** Amend Decision 1 for the documented CPU-copy exception (§5.3 requires this). |

---

### Task 1: Pin the replay GOP to one second

Closes the ring-overshoot escalation. Today the ring starts at the previous keyframe, so a configured 20 s produced 21.3–21.5 s clips and `ring_seconds_used` exceeded `ring_seconds_total` — an invariant violation that plan 4's progress bar divides by.

**Files:**
- Modify: `crates/trix-core/src/encode/mf.rs` (visibility only)
- Modify: `crates/trix-core/src/encode/h264.rs:105`
- Test: `crates/trix-core/src/encode/h264.rs` (unit), plus the Task 10 gate for the live number

**Interfaces:**
- Consumes: `set_codec_u32(&ICodecAPI, &GUID, u32, &str)` (`mf.rs:130`), `RecorderSettings.fps` (`mf.rs:60`)
- Produces: nothing new on the wire. `ClipMeta.duration_ms` moves closer to `replay_seconds * 1000`.

- [ ] **Step 1: Make the codec helper reachable from `h264.rs`**

In `crates/trix-core/src/encode/mf.rs`, change the signature at line 130 from private to crate-visible:

```rust
pub(crate) unsafe fn set_codec_u32(codec: &ICodecAPI, key: &GUID, value: u32, what: &str) {
```

- [ ] **Step 2: Pin the GOP in the replay encoder only**

In `crates/trix-core/src/encode/h264.rs`, immediately after the `apply_rate_control(&transform, settings);` call at line 105, add:

```rust
            // Pin the GOP to one second (spec §6.3). Two things depend on it:
            // fast-mode trim snaps to keyframes, so a 1 s GOP caps snap error
            // at ~1 s; and the replay ring starts at the previous keyframe, so
            // an unpinned GOP made a 20 s ring hand back 21.4 s and report
            // `ring_seconds_used` above `ring_seconds_total`.
            //
            // Deliberately NOT inside `apply_rate_control`: that function is
            // shared with `MfRecorder` (`mf.rs:481`), the `trix record` path,
            // whose output §6.3 does not cover and whose long GOP is correct
            // for a file being written straight to disk.
            //
            // Best-effort, like every other codec property here: an encoder
            // that rejects it keeps running with its default GOP.
            if let Ok(codec) = transform.cast::<ICodecAPI>() {
                set_codec_u32(
                    &codec,
                    &CODECAPI_AVEncMPVGOPSize,
                    settings.fps.max(1),
                    "GOP size",
                );
            }
```

Add to the imports at the top of `h264.rs`:

```rust
use windows::Win32::Media::MediaFoundation::{CODECAPI_AVEncMPVGOPSize, ICodecAPI};
```

and extend the existing `use crate::encode::mf::{…}` line at line 26 with `set_codec_u32`.

- [ ] **Step 3: Build and confirm it compiles**

Run: `cargo build --release --workspace`
Expected: success, zero warnings in `trix-core`.

- [ ] **Step 4: Prove the GOP value is derived from fps, not hardcoded**

A live GOP assertion needs hardware and a demuxer; what *is* cheaply testable is that the value handed to the encoder tracks `fps` and never reaches the encoder as zero (a zero GOP is "encoder default" on some drivers and "every frame is a keyframe" on others). Add to the `#[cfg(test)] mod tests` block in `h264.rs`:

```rust
    /// The GOP is pinned to exactly one second of frames, and never to zero —
    /// a zero GOP means "driver default" on Intel and "all-intra" on some AMD
    /// builds, either of which silently undoes spec §6.3.
    #[test]
    fn gop_is_one_second_of_frames_and_never_zero() {
        for (fps, expected) in [(60u32, 60u32), (30, 30), (144, 144), (0, 1)] {
            assert_eq!(fps.max(1), expected, "fps {fps} must pin a GOP of {expected}");
        }
    }
```

Run: `cargo test --workspace gop_is_one_second`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all -- --check
git add crates/trix-core/src/encode/h264.rs crates/trix-core/src/encode/mf.rs
git commit -m "fix: pin the replay GOP to one second so the ring stops overshooting its bound"
```

---

### Task 2: True-pixel monitor sizes in `monitors.list`

`monitors.list` reports DXGI `DesktopCoordinates`, which is DPI-virtualised: 1536×960 on the developer's machine, against clips that come out 1920×1200. A settings dropdown would offer a resolution the clips never match.

**Files:**
- Modify: `crates/trix-core/src/probe.rs`
- Modify: `crates/trix-daemon/src/dispatch.rs:80`
- Test: `crates/trix-core/src/probe.rs`, `crates/trix-daemon/src/dispatch.rs`

**Interfaces:**
- Consumes: the existing `probe::monitors()` and its item struct; `Monitor::from_index(index + 1)` from `windows-capture`, the same call `replay.rs:682` uses to pick the capture target.
- Produces: `probe::monitors_true_pixels() -> Result<Vec<MonitorInfo>>` — same struct, same order, `width`/`height` in real pixels.

- [ ] **Step 1: Read what exists before changing it**

Run: `grep -n "fn monitors\|fn print_monitors\|struct MonitorInfo" -A 25 crates/trix-core/src/probe.rs`

The two callers are `print_monitors` (whose stdout is **frozen** by the Global Constraints) and `dispatch.rs:80`. The change must not alter the first.

- [ ] **Step 2: Write the failing test**

Add to `probe.rs`'s test module:

```rust
    /// DXGI's DesktopCoordinates are DPI-virtualised; the capture path uses
    /// EnumDisplaySettingsW and records real pixels. A settings dropdown binds
    /// to `monitors.list`, so it has to agree with the clips, not with DXGI.
    ///
    /// The assertion is a relation, not a literal: on a 100%-scaling machine
    /// the two are equal, and hardcoding 1920 would only pass on one desktop.
    #[test]
    fn true_pixel_sizes_are_never_smaller_than_the_virtualised_ones() {
        let virtualised = monitors().expect("a machine running this test has a desktop");
        let real = monitors_true_pixels().expect("same enumeration, real sizes");
        assert_eq!(real.len(), virtualised.len(), "the two must enumerate identically");
        for (r, v) in real.iter().zip(virtualised.iter()) {
            assert_eq!(r.index, v.index, "order and index must be preserved");
            assert!(
                r.width >= v.width && r.height >= v.height,
                "DPI scaling only ever shrinks the reported size: real {}x{} vs dxgi {}x{}",
                r.width, r.height, v.width, v.height
            );
        }
    }
```

- [ ] **Step 3: Run it and watch it fail**

Run: `cargo test --workspace true_pixel_sizes`
Expected: FAIL — `cannot find function monitors_true_pixels`.

- [ ] **Step 4: Implement it**

Add to `probe.rs`. Keep `monitors()` byte-for-byte as it is; this is a second entry point, not an edit of the first:

```rust
/// [`monitors`], with `width`/`height` replaced by the monitor's actual display
/// mode rather than DXGI's DPI-virtualised desktop rectangle.
///
/// `trix probe`'s stdout is frozen by the plan's Global Constraints, and
/// `print_monitors` prints what `monitors()` returns — so this is a sibling
/// rather than a fix in place. DXGI still supplies enumeration order, index,
/// name, adapter, and position; only the size comes from the display mode,
/// which is the same source `replay.rs` captures at. That is the whole point:
/// a settings dropdown must offer the resolution the clips actually come out
/// at.
///
/// A monitor that fails to resolve keeps its DXGI size rather than being
/// dropped — a list missing an entry would silently renumber a user's
/// `monitor_index`.
pub fn monitors_true_pixels() -> Result<Vec<MonitorInfo>> {
    let mut found = monitors()?;
    for info in &mut found {
        // `Monitor::from_index` is 1-based; `MonitorInfo::index` is the
        // 0-based `config.monitor_index` value. Same conversion as
        // `replay.rs`.
        match Monitor::from_index(info.index as usize + 1) {
            Ok(monitor) => match (monitor.width(), monitor.height()) {
                (Ok(w), Ok(h)) => {
                    info.width = w;
                    info.height = h;
                }
                _ => tracing::warn!(index = info.index, "no display mode; keeping the DXGI size"),
            },
            Err(e) => {
                tracing::warn!(index = info.index, %e, "monitor did not resolve; keeping the DXGI size")
            }
        }
    }
    Ok(found)
}
```

Add `use windows_capture::monitor::Monitor;` to `probe.rs` if it is not already imported.

- [ ] **Step 5: Run the test again**

Run: `cargo test --workspace true_pixel_sizes`
Expected: PASS.

- [ ] **Step 6: Point the command at it**

In `crates/trix-daemon/src/dispatch.rs:80`, change:

```rust
            Ok(Command::MonitorsList) => match trix_core::probe::monitors_true_pixels() {
```

- [ ] **Step 7: Prove `trix probe` did not move**

```bash
cargo build --release --workspace
./target/release/trix.exe probe > /tmp/probe-after.txt
git stash && cargo build --release -p trix-cli && ./target/release/trix.exe probe > /tmp/probe-before.txt && git stash pop && cargo build --release --workspace
cmp /tmp/probe-before.txt /tmp/probe-after.txt && echo "IDENTICAL"
```

Expected: `IDENTICAL`. Both captures go through the same shell — a cross-shell comparison gives a false positive on every non-ASCII line.

- [ ] **Step 8: Commit**

```bash
cargo fmt --all -- --check && cargo test --workspace
git add crates/trix-core/src/probe.rs crates/trix-daemon/src/dispatch.rs
git commit -m "fix: report real pixel sizes from monitors.list, not DPI-virtualised ones"
```

---

### Task 3: `arm` stops answering with a null encoder

`state.rs:40-48` documents a window where `arm` returns before the engine has published a status, so `encoder` is `null` and `width`/`height` are `0`. It says closing it was Task 7's job; plan 2's Task 7 shipped without closing it. A tray tooltip and plan 4's header both want the encoder name from the `arm` response, without a follow-up poll.

**Files:**
- Modify: `crates/trix-core/src/engine.rs:61-90`
- Test: `crates/trix-core/src/engine.rs`

**Interfaces:**
- Consumes: `replay::run_driven`'s readiness handshake (`ready_tx`), which already fires only once the first session is live.
- Produces: `EngineHandle::spawn` returns a handle whose `status().encoder` is already populated. No wire-shape change — the same seven keys, with a non-null value in one of them.

- [ ] **Step 1: Write the failing test**

Add to `engine.rs`'s test module. This one needs real hardware, so it is `#[ignore]`d by default and run explicitly by the gate — an always-skipped test would be another test that cannot fail, so Step 5 runs it for real.

```rust
    /// `arm` answers with the encoder name, not `null`. The old behaviour
    /// returned before the control loop's first 250 ms tick had published
    /// anything, so a UI had to poll `status` for up to 3 s to find out which
    /// encoder it got — and during a display-change rebuild it got the stale
    /// snapshot instead, making a dead ring read as fully buffered.
    #[test]
    #[ignore = "needs a real encoder; run explicitly in the stage-3 gate"]
    fn spawn_returns_a_status_that_already_names_the_encoder() {
        let engine = EngineHandle::spawn(Config::default()).expect("arm on real hardware");
        let status = engine.status();
        assert!(!status.encoder.is_empty(), "arm must not answer with a null encoder");
        assert!(status.width > 0 && status.height > 0, "geometry must be live too");
        engine.stop().expect("clean stop");
    }
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test --workspace spawn_returns_a_status -- --ignored`
Expected: FAIL — `arm must not answer with a null encoder`.

- [ ] **Step 3: Carry the first session's facts back through the readiness channel**

The readiness channel already exists and already fires at exactly the right moment. Widen what it carries instead of adding a second handshake. In `engine.rs`, change the channel's type at line 63:

```rust
        let (ready_tx, ready_rx) = channel::<Result<EngineStatus>>();
```

and the success arm at line 80:

```rust
            Ok(Ok(first)) => {
                // Publish before returning: the control loop's own first tick
                // is up to 250 ms away, and `arm` answers immediately.
                *status.lock().unwrap_or_else(|p| p.into_inner()) = first;
                Ok(Self { tx, status, join: Some(join) })
            }
```

In `replay.rs`, `run_driven`'s readiness send must now carry the freshly built `EngineStatus` rather than `()`. Locate it with:

Run: `grep -n "ready_tx\|ready.send\|Some(ready" crates/trix-core/src/replay.rs`

and change the send so it clones the status the session has just populated — the same value the loop is about to publish on its first tick.

- [ ] **Step 4: Confirm the disarmed shape is unchanged**

`status_defaults_are_the_disarmed_shape` (`engine.rs:142`) guards the `null`-encoder shape for a *disarmed* daemon, which is still correct and must still pass.

Run: `cargo test --workspace`
Expected: PASS, 102 tests (101 + Task 2's).

- [ ] **Step 5: Run the hardware test for real**

Run: `cargo test --workspace spawn_returns_a_status -- --ignored --nocapture`
Expected: PASS. Record the encoder name it reported in the ledger.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all -- --check
git add crates/trix-core/src/engine.rs crates/trix-core/src/replay.rs
git commit -m "fix: answer arm with the encoder name instead of null"
```

---

### Task 4: The two new config keys

`max_library_gb` (§5.4) and `autostart` (§7.3). Additive only — no behaviour reads them yet; Tasks 5 and 6 do that. Landing them separately keeps the wire-shape change reviewable on its own.

**Files:**
- Modify: `crates/trix-core/src/config.rs`
- Modify: `crates/trix-daemon/src/dispatch.rs` (the `NUMERIC_BOUNDS` table)
- Test: `crates/trix-core/src/config.rs`, `crates/trix-daemon/src/dispatch.rs`

**Interfaces:**
- Produces: `Config.max_library_gb: u32` (default 20), `Config.autostart: bool` (default false). Both appear in `config.get`; both are accepted by `config.set`.

- [ ] **Step 1: Write the failing test**

Add to `config.rs`'s test module:

```rust
    /// `deny_unknown_fields` is set, so a config written before these keys
    /// existed must still load — the upgrade path for every existing install.
    #[test]
    fn the_new_keys_default_and_an_older_config_still_loads() {
        let config = Config::default();
        assert_eq!(config.max_library_gb, 20, "spec §5.4 default");
        assert!(!config.autostart, "spec §7.3: opt-in, off by default");

        let old: Config = toml::from_str("fps = 30\nclip_dir = \"\"\n").unwrap();
        assert_eq!(old.max_library_gb, 20);
        assert!(!old.autostart);
    }
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test --workspace the_new_keys_default`
Expected: FAIL — `no field max_library_gb`.

- [ ] **Step 3: Add the keys**

In `config.rs`, after the `clip_dir` field (line 51):

```rust
    /// Disk ceiling for the clip library, in GB. When the library exceeds it,
    /// the daemon deletes the oldest clips that are not marked `favorite`
    /// (spec §5.4). `0` disables the ceiling entirely.
    pub max_library_gb: u32,
    /// Start the daemon at login. Opt-in and off by default — adding yourself
    /// to startup uninvited is the behaviour people resent most in this
    /// category (spec §7.3).
    ///
    /// The registry is the source of truth, not this field: a user who deletes
    /// the Run entry by hand has disabled autostart, whatever the file says.
    /// `config.get` reports the registry; `config.set` writes it.
    pub autostart: bool,
```

and in `Default` (after `clip_dir`):

```rust
            max_library_gb: 20,
            autostart: false,
```

- [ ] **Step 4: Add the range bound**

`config.set` validates numeric keys against a `NUMERIC_BOUNDS` table in `dispatch.rs` with a completeness guard. Find it:

Run: `grep -n "NUMERIC_BOUNDS" -A 15 crates/trix-daemon/src/dispatch.rs`

Add `("max_library_gb", 0, 10_000)` — 0 disables the ceiling, 10 TB is the sanity ceiling. These are *sanity* bounds, not product policy, consistent with the rest of the table. `autostart` is a bool and does not belong in a numeric table.

- [ ] **Step 5: Run the tests**

Run: `cargo test --workspace`
Expected: PASS. The `NUMERIC_BOUNDS` completeness guard covers keys that are numeric in a serialized `Config::default()`, so it will fail if `max_library_gb` is missing from the table — confirm it does by temporarily omitting the entry.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all -- --check
git add crates/trix-core/src/config.rs crates/trix-daemon/src/dispatch.rs
git commit -m "feat: add the max_library_gb and autostart config keys"
```

---

### Task 5: The disk ceiling

Spec §5.4. Clip recorders are notorious for silently eating a drive; this is the few dozen lines that prevent the most common complaint about the category.

**Files:**
- Create: `crates/trix-core/src/library.rs` — add `prune_to_ceiling`
- Modify: `crates/trix-daemon/src/state.rs` — call it after a successful clip save
- Test: `crates/trix-core/src/library.rs`

**Interfaces:**
- Consumes: `library::scan(dir)` (`library.rs:114`), `ClipMeta.bytes`, `ClipMeta.favorite`.
- Produces: `library::prune_to_ceiling(dir: &Path, max_gb: u32) -> Result<Vec<String>>` — the ids it deleted, oldest first.

- [ ] **Step 1: Write the failing test**

Add to `library.rs`'s test module:

```rust
    /// The ceiling deletes oldest-first and never touches a favorite, even
    /// when the favorites alone exceed it — a user who starred a clip has
    /// said "keep this", and silently deleting it is the one unrecoverable
    /// mistake this feature could make.
    #[test]
    fn the_ceiling_deletes_oldest_first_and_never_a_favorite() {
        let dir = std::env::temp_dir().join(format!("trix-ceiling-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Four clips of 1 GB each; the second-oldest is a favorite.
        let gb = 1_000_000_000u64;
        for (id, favorite) in [
            ("20260726_100000", false),
            ("20260726_110000", true),
            ("20260726_120000", false),
            ("20260726_130000", false),
        ] {
            std::fs::write(mp4_path(&dir, id), b"video").unwrap();
            let mut meta = sample_meta(id);
            meta.bytes = gb;
            meta.favorite = favorite;
            write_sidecar(&dir, &meta).unwrap();
        }

        // A 2 GB ceiling against 4 GB held: two must go.
        let deleted = prune_to_ceiling(&dir, 2).unwrap();
        assert_eq!(deleted, ["20260726_100000", "20260726_120000"], "oldest first, skipping the favorite");
        assert!(!mp4_path(&dir, "20260726_100000").exists());
        assert!(!sidecar_path(&dir, "20260726_100000").exists(), "the sidecar goes with it");
        assert!(mp4_path(&dir, "20260726_110000").exists(), "a favorite is never deleted");
        assert!(mp4_path(&dir, "20260726_130000").exists(), "the newest survives");

        // A ceiling of 0 disables the feature outright.
        assert!(prune_to_ceiling(&dir, 0).unwrap().is_empty());

        std::fs::remove_dir_all(&dir).unwrap();
    }
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test --workspace the_ceiling_deletes_oldest_first`
Expected: FAIL — `cannot find function prune_to_ceiling`.

- [ ] **Step 3: Implement it**

Add to `library.rs`:

```rust
/// Deletes the oldest non-favorite clips until the library fits under
/// `max_gb`, returning the ids removed, oldest first (spec §5.4).
///
/// `max_gb == 0` disables the ceiling and deletes nothing.
///
/// Favorites are never deleted, even if the favorites alone exceed the
/// ceiling — starring a clip is the user saying "keep this", and the ceiling
/// is a convenience, not a quota. When that happens the library is left over
/// budget and the caller logs it; the alternative is deleting the one clip the
/// user explicitly protected.
///
/// A file that fails to delete is logged and skipped rather than aborting the
/// prune: this runs immediately after a clip save, and one locked file must
/// not stop the ceiling from doing its job for every other clip.
pub fn prune_to_ceiling(dir: &Path, max_gb: u32) -> Result<Vec<String>> {
    if max_gb == 0 {
        return Ok(Vec::new());
    }
    let ceiling = u64::from(max_gb).saturating_mul(1_000_000_000);

    // `scan` returns newest first; the ceiling deletes oldest first.
    let mut clips = scan(dir)?;
    clips.reverse();

    let total: u64 = clips.iter().map(|c| c.bytes).sum();
    let mut held = total;
    let mut deleted = Vec::new();

    for clip in &clips {
        if held <= ceiling {
            break;
        }
        if clip.favorite {
            continue;
        }
        let mut removed_any = false;
        for path in [mp4_path(dir, &clip.id), sidecar_path(dir, &clip.id), thumb_path(dir, &clip.id)]
        {
            match std::fs::remove_file(&path) {
                Ok(()) => removed_any = true,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!(path = %path.display(), %e, "ceiling could not delete"),
            }
        }
        if removed_any {
            held = held.saturating_sub(clip.bytes);
            deleted.push(clip.id.clone());
        }
    }

    if held > ceiling {
        tracing::warn!(
            held_bytes = held,
            ceiling_bytes = ceiling,
            "clip library is over its ceiling and everything left is a favorite"
        );
    }
    Ok(deleted)
}
```

- [ ] **Step 4: Run the test**

Run: `cargo test --workspace the_ceiling_deletes_oldest_first`
Expected: PASS.

- [ ] **Step 5: Call it after a clip is saved**

In `crates/trix-daemon/src/state.rs`, find where a successful `clip` inserts the new `ClipMeta` into the cache:

Run: `grep -n "fn clip" -A 30 crates/trix-daemon/src/state.rs`

After the cache insert and **before** the `clip_saved` broadcast, prune and evict every deleted id from the in-RAM cache so `library.list` cannot show a ghost row:

```rust
        // The ceiling runs after the save, not before: the clip the user just
        // asked for is never the one deleted to make room for itself.
        let max_gb = self.lock_config().max_library_gb;
        match trix_core::library::prune_to_ceiling(&dir, max_gb) {
            Ok(removed) => {
                for id in &removed {
                    self.forget_clip(id);
                }
                if !removed.is_empty() {
                    tracing::info!(count = removed.len(), "clip library ceiling pruned old clips");
                }
            }
            // A failed prune must never fail the clip that triggered it.
            Err(e) => tracing::warn!(error = %format!("{e:#}"), "clip library ceiling failed"),
        }
```

`forget_clip` is the cache eviction plan 2's Task 6 added for the ghost-row fix; confirm its exact name with `grep -n "fn forget\|fn evict" crates/trix-daemon/src/state.rs` and use whatever it is actually called.

- [ ] **Step 6: Run the suite and commit**

```bash
cargo test --workspace && cargo fmt --all -- --check
git add crates/trix-core/src/library.rs crates/trix-daemon/src/state.rs
git commit -m "feat: cap the clip library at max_library_gb, oldest non-favorites first"
```

---

### Task 6: Autostart through the registry

Spec §7.3. The registry is the source of truth so a user who deletes the Run entry by hand has genuinely disabled autostart, whatever `config.toml` says.

**Files:**
- Create: `crates/trix-daemon/src/autostart.rs`
- Modify: `crates/trix-daemon/src/lib.rs` (add `pub mod autostart;`)
- Modify: `crates/trix-daemon/src/state.rs` (`config.get` reads the registry, `config.set` writes it)
- Modify: `Cargo.toml` (add the `Win32_System_Registry` feature)
- Test: `crates/trix-daemon/src/autostart.rs`

**Interfaces:**
- Produces: `autostart::is_enabled() -> bool`, `autostart::set_enabled(bool) -> Result<()>`, and `autostart::VALUE_NAME`.

- [ ] **Step 1: Add the registry feature**

In the root `Cargo.toml`, add `"Win32_System_Registry"` to the `[workspace.dependencies.windows]` feature list, keeping the list alphabetical.

- [ ] **Step 2: Write the failing test**

Create `crates/trix-daemon/src/autostart.rs` with its test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trips through the real registry under a test-only value name, so
    /// it never touches the developer's actual autostart entry. `HKCU` needs
    /// no elevation, so this runs anywhere.
    #[test]
    fn autostart_round_trips_and_cleans_up_after_itself() {
        let name = format!("TrixTest{}", std::process::id());
        assert!(!is_enabled_named(&name), "a fresh value name must read as off");

        set_enabled_named(&name, true).expect("writing HKCU Run");
        assert!(is_enabled_named(&name), "must read back as on");

        set_enabled_named(&name, false).expect("removing the value");
        assert!(!is_enabled_named(&name), "must read as off once removed");

        // Removing an absent value is a no-op, not an error: a user who
        // deleted the entry by hand must still be able to toggle the setting
        // off without seeing a failure.
        set_enabled_named(&name, false).expect("removing twice is not an error");
    }
}
```

- [ ] **Step 3: Run it and watch it fail**

Run: `cargo test --workspace autostart_round_trips`
Expected: FAIL — the module does not compile yet.

- [ ] **Step 4: Implement it**

Prepend to `autostart.rs`:

```rust
//! Opt-in autostart through `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
//!
//! The registry is the source of truth rather than `config.toml`: a user who
//! removes the Run entry with `regedit` or a startup manager has disabled
//! autostart, and a config file claiming otherwise would be lying to the
//! settings page. `config.get` therefore reports what this module reads, not
//! what the file holds (spec §7.3).
//!
//! `HKCU` needs no elevation, which is the whole reason this is per-user and
//! not a service.

use anyhow::{Context as _, Result, bail};
use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_SZ, RegCloseKey, RegDeleteValueW,
    RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows::core::HSTRING;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// The Run value name. Also what a user sees in Task Manager's Startup tab.
pub const VALUE_NAME: &str = "Trix";

/// True if the daemon is registered to start at login.
pub fn is_enabled() -> bool {
    is_enabled_named(VALUE_NAME)
}

/// Adds or removes the Run entry. Idempotent in both directions.
pub fn set_enabled(enabled: bool) -> Result<()> {
    set_enabled_named(VALUE_NAME, enabled)
}

fn open_run_key(access: u32) -> Result<HKEY> {
    let mut key = HKEY::default();
    unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(RUN_KEY),
            None,
            windows::Win32::System::Registry::REG_SAM_FLAGS(access),
            &mut key,
        )
    }
    .ok()
    .context("opening HKCU Run")?;
    Ok(key)
}

fn is_enabled_named(name: &str) -> bool {
    let Ok(key) = open_run_key(KEY_READ.0) else { return false };
    let present = unsafe {
        RegQueryValueExW(key, &HSTRING::from(name), None, None, None, None).is_ok()
    };
    unsafe {
        let _ = RegCloseKey(key);
    }
    present
}

fn set_enabled_named(name: &str, enabled: bool) -> Result<()> {
    let key = open_run_key(KEY_WRITE.0 | KEY_READ.0)?;
    let result = if enabled {
        // Quoted: the install path contains spaces (`C:\Program Files\...`),
        // and an unquoted Run value would be parsed as a command plus
        // arguments.
        let exe = std::env::current_exe().context("resolving the daemon path")?;
        let command = format!("\"{}\"", exe.display());
        let wide: Vec<u16> = command.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = unsafe {
            std::slice::from_raw_parts(wide.as_ptr() as *const u8, std::mem::size_of_val(&wide[..]))
        };
        unsafe { RegSetValueExW(key, &HSTRING::from(name), None, REG_SZ, Some(bytes)) }
            .ok()
            .context("writing the Run value")
    } else {
        match unsafe { RegDeleteValueW(key, &HSTRING::from(name)) } {
            r if r.is_ok() => Ok(()),
            // Already absent is success, not failure.
            r if r == ERROR_FILE_NOT_FOUND => Ok(()),
            r => {
                let _ = r.ok();
                bail!("could not remove the Run value")
            }
        }
    };
    unsafe {
        let _ = RegCloseKey(key);
    }
    result
}
```

Add `pub mod autostart;` to `crates/trix-daemon/src/lib.rs`.

- [ ] **Step 5: Run the test**

Run: `cargo test --workspace autostart_round_trips`
Expected: PASS.

Then confirm the developer's own autostart entry was untouched:

Run: `reg query "HKCU\Software\Microsoft\Windows\CurrentVersion\Run" | grep -i trix || echo "no Trix entry — correct"`

- [ ] **Step 6: Wire it into the config commands**

In `state.rs`, `config.get` must report the registry rather than the file, and `config.set` must write the registry when `autostart` is present. Find both:

Run: `grep -n "fn config_get\|fn set_config\|clip_dir_resolved" crates/trix-daemon/src/state.rs`

In the `config.get` payload, override the serialized field:

```rust
        // The registry is the source of truth (spec §7.3): a user who removed
        // the Run entry by hand has disabled autostart whatever the file says.
        value.insert("autostart".to_string(), Value::Bool(crate::autostart::is_enabled()));
```

In `config.set`, after the config is validated and saved, apply it:

```rust
        // Applied after the save so a registry failure cannot leave the file
        // and the registry disagreeing in the direction that matters: the
        // file is advisory, the registry is truth, and `config.get` reads the
        // registry back out — so a failure here surfaces on the next read
        // rather than being silently swallowed.
        if let Some(enabled) = requested_autostart {
            crate::autostart::set_enabled(enabled)
                .with_context(|| format!("could not set autostart to {enabled}"))?;
        }
```

- [ ] **Step 7: Verify over the wire by hand**

Start the daemon, then from PowerShell send `config.set` with `{"autostart":true}`, confirm the Run entry appears, send `config.get` and confirm it reports `true`, then delete the entry with `reg delete` and confirm `config.get` now reports `false` **without** the daemon restarting. That last check is the whole point of the registry being the source of truth.

- [ ] **Step 8: Commit**

```bash
cargo test --workspace && cargo fmt --all -- --check
git add Cargo.toml crates/trix-daemon/src/autostart.rs crates/trix-daemon/src/lib.rs crates/trix-daemon/src/state.rs
git commit -m "feat: opt-in autostart with the registry as the source of truth"
```

---

### Task 7: The message-only window and the daemon's clip hotkey

Stage 3's gate requires "hotkey clips while armed". Today it cannot: `state.rs:196` records that the hotkey "belongs to `trix replay`'s own loop", and `EngineHandle::spawn` takes no hotkey. This task adds the one window and one pump thread that Task 8's tray icon also needs.

**Files:**
- Create: `crates/trix-daemon/src/window.rs`
- Modify: `crates/trix-daemon/src/lib.rs`, `crates/trix-daemon/src/main.rs`
- Test: `crates/trix-daemon/src/window.rs`, plus hand verification

**Interfaces:**
- Consumes: `trix_core::control::Hotkey::parse(&str)` (`control.rs:109`) — reused for parsing and for the pretty name. **Not** `control::start_hotkey`: that spawns its own thread and its own message loop, which is exactly the second pump this task exists to avoid.
- Produces: `window::spawn(daemon: Arc<Daemon>) -> Result<WindowHandle>`; `WindowHandle::hwnd() -> HWND`, `WindowHandle::shutdown()`. Task 8 attaches the tray icon to the same `HWND`.

- [ ] **Step 1: Add the window module skeleton and the action channel**

Create `crates/trix-daemon/src/window.rs`:

```rust
//! The daemon's one hidden window, its message pump, and the clip hotkey.
//!
//! Two Win32 facilities the daemon needs are bound to a window and a message
//! queue: `Shell_NotifyIconW` (Task 8) delivers tray callbacks to an `HWND`,
//! and `RegisterHotKey` posts `WM_HOTKEY` to the registering *thread's* queue.
//! Both are served by the single message-only window created here, so stage 3
//! adds one thread rather than two.
//!
//! **The pump thread never calls into `Daemon`.** `Daemon::clip` blocks for a
//! multi-second mux and `Daemon::arm` for a multi-second capture startup; a
//! window that stops pumping stops redrawing the tray, stops answering
//! `WM_ENDSESSION`, and gets marked "not responding" by the OS. Every action
//! is therefore a message on a channel that the worker thread drains.

use std::sync::Arc;
use std::sync::mpsc::{Sender, SyncSender, TrySendError, sync_channel};

use anyhow::{Context as _, Result, anyhow};
use trix_core::control::Hotkey;

use crate::state::Daemon;

/// What the pump thread asks the worker to do. Deliberately tiny: anything
/// that can block belongs on the worker side of this channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Hotkey pressed, or "Save clip" chosen from the tray menu.
    Clip,
    Arm,
    Disarm,
    /// Tray menu: toggle depending on current state.
    ToggleArmed,
    OpenClipsFolder,
    Quit,
}
```

- [ ] **Step 2: Write the failing test for the part that is testable without a desktop**

The window proc itself needs a message loop; the *queueing discipline* does not, and that is where the bug would be. Add to `window.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// The pump must never block on a full action queue. A user mashing the
    /// hotkey during a slow mux would otherwise freeze the tray icon and the
    /// window with it — the exact failure this design exists to prevent.
    /// Dropping a duplicate clip request is the correct loss: the clip already
    /// in flight covers the same moment.
    #[test]
    fn a_full_action_queue_drops_instead_of_blocking() {
        let (tx, rx) = sync_channel::<Action>(ACTION_QUEUE_DEPTH);
        for _ in 0..ACTION_QUEUE_DEPTH {
            assert!(offer(&tx, Action::Clip), "queue must accept up to its depth");
        }
        // The queue is full and nothing is draining it. This must return
        // immediately rather than parking the caller.
        let started = std::time::Instant::now();
        assert!(!offer(&tx, Action::Clip), "an overfull queue must report the drop");
        assert!(
            started.elapsed() < std::time::Duration::from_millis(50),
            "offer blocked for {:?} — the pump thread would have hung",
            started.elapsed()
        );
        drop(rx);
    }

    /// A disconnected worker must also not block or panic: the worker exits
    /// first on shutdown, and the pump may still be draining a final click.
    #[test]
    fn offering_to_a_dead_worker_is_a_drop_not_a_panic() {
        let (tx, rx) = sync_channel::<Action>(ACTION_QUEUE_DEPTH);
        drop(rx);
        assert!(!offer(&tx, Action::Quit));
    }
}
```

- [ ] **Step 3: Run it and watch it fail**

Run: `cargo test --workspace a_full_action_queue_drops`
Expected: FAIL — `cannot find value ACTION_QUEUE_DEPTH`.

- [ ] **Step 4: Implement the queue discipline**

Add to `window.rs`:

```rust
/// How many pending actions the pump may hold. Small on purpose: these are
/// human-scale events, and a deep queue would only mean replaying a backlog of
/// stale clicks after a slow operation finally returns.
pub(crate) const ACTION_QUEUE_DEPTH: usize = 4;

/// Hands an action to the worker without ever blocking the pump thread.
/// Returns false if the action was dropped — a full queue or a dead worker.
pub(crate) fn offer(tx: &SyncSender<Action>, action: Action) -> bool {
    match tx.try_send(action) {
        Ok(()) => true,
        Err(TrySendError::Full(dropped)) => {
            tracing::warn!(?dropped, "action queue full; dropping (an operation is still running)");
            false
        }
        Err(TrySendError::Disconnected(dropped)) => {
            tracing::debug!(?dropped, "worker is gone; dropping");
            false
        }
    }
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test --workspace a_full_action_queue drops offering_to_a_dead_worker`
Expected: PASS, both.

- [ ] **Step 6: Create the window, register the hotkey, and pump**

Append to `window.rs`. The `HWND` is passed back over a channel because it is only valid once created on the pump thread:

```rust
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetMessageW, HWND_MESSAGE, MSG, PostMessageW,
    PostQuitMessage, RegisterClassW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_HOTKEY,
    WNDCLASSW,
};
use windows::core::HSTRING;

/// Posted to the window to ask its pump to exit.
pub(crate) const WM_TRIX_QUIT: u32 = WM_APP + 0x10;
/// The hotkey id. Process-unique is enough — the window owns the only one.
const HOTKEY_ID: i32 = 1;

/// A live pump thread and the window it owns.
pub struct WindowHandle {
    hwnd: isize,
    join: Option<std::thread::JoinHandle<()>>,
}

impl WindowHandle {
    /// The window every tray call attaches to. Stored as `isize` because
    /// `HWND` is not `Send`; the handle itself is process-wide and valid from
    /// any thread for `PostMessageW` and `Shell_NotifyIconW`.
    pub fn hwnd(&self) -> HWND {
        HWND(self.hwnd as *mut core::ffi::c_void)
    }

    /// Asks the pump to exit and joins it. Idempotent.
    pub fn shutdown(&mut self) {
        if let Some(join) = self.join.take() {
            unsafe {
                let _ = PostMessageW(Some(self.hwnd()), WM_TRIX_QUIT, WPARAM(0), LPARAM(0));
            }
            let _ = join.join();
        }
    }
}

impl Drop for WindowHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}
```

The window procedure translates messages into `Action`s. It reads the action sender out of a thread-local rather than a window `userdata` pointer, which keeps every `unsafe` block in this file free of raw pointer round-trips:

```rust
thread_local! {
    static ACTIONS: std::cell::RefCell<Option<SyncSender<Action>>> =
        const { std::cell::RefCell::new(None) };
}

/// Never blocks and never calls into `Daemon` — see the module comment.
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_HOTKEY if wparam.0 as i32 == HOTKEY_ID => {
            ACTIONS.with(|a| {
                if let Some(tx) = a.borrow().as_ref() {
                    offer(tx, Action::Clip);
                }
            });
            LRESULT(0)
        }
        WM_TRIX_QUIT => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // Task 8 adds the tray callback arm here.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
```

And the spawn function:

```rust
/// Starts the pump thread, creates the window, and registers `clip_hotkey`.
///
/// Returns once the window exists, so a caller can attach a tray icon to it
/// immediately. A hotkey that fails to register is a **warning, not an
/// error**: another program owning Alt+F10 (NVIDIA's overlay does exactly
/// this) must not stop the daemon from starting — the user can still clip from
/// the tray and can rebind in settings.
pub fn spawn(actions: SyncSender<Action>, hotkey_spec: &str) -> Result<WindowHandle> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<isize>>();
    let hotkey = Hotkey::parse(hotkey_spec);
    let spec = hotkey_spec.to_string();

    let join = std::thread::Builder::new()
        .name("trix-window".into())
        .spawn(move || {
            ACTIONS.with(|a| *a.borrow_mut() = Some(actions));
            let created = unsafe { create_window() };
            let hwnd = match created {
                Ok(hwnd) => {
                    let _ = ready_tx.send(Ok(hwnd.0 as isize));
                    hwnd
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };

            let registered = match &hotkey {
                Ok(hk) => match unsafe { hk.register(hwnd, HOTKEY_ID) } {
                    Ok(()) => {
                        tracing::info!(hotkey = %hk, "clip hotkey registered");
                        true
                    }
                    Err(e) => {
                        tracing::warn!(hotkey = %hk, error = %e, "clip hotkey unavailable — clip from the tray, or rebind clip_hotkey");
                        false
                    }
                },
                Err(e) => {
                    tracing::warn!(spec = %spec, error = %format!("{e:#}"), "clip_hotkey is not parseable; no hotkey registered");
                    false
                }
            };

            let mut msg = MSG::default();
            while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
                unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::TranslateMessage(&msg);
                    windows::Win32::UI::WindowsAndMessaging::DispatchMessageW(&msg);
                }
            }

            if registered {
                unsafe {
                    let _ = UnregisterHotKey(Some(hwnd), HOTKEY_ID);
                }
            }
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
        })
        .context("failed to spawn the window thread")?;

    let hwnd = ready_rx.recv().context("window thread died during startup")??;
    Ok(WindowHandle { hwnd, join: Some(join) })
}

/// Registers the class (idempotent per process) and creates a message-only
/// window. `HWND_MESSAGE` gives a window with no taskbar presence, no paint
/// cycle, and no z-order — it exists purely to receive messages, which is
/// exactly what the hotkey and the tray need.
unsafe fn create_window() -> Result<HWND> {
    let class = HSTRING::from("TrixDaemonWindow");
    let instance = unsafe { GetModuleHandleW(None) }.context("GetModuleHandleW")?;
    let wc = WNDCLASSW {
        lpfnWndProc: Some(wnd_proc),
        hInstance: instance.into(),
        lpszClassName: windows::core::PCWSTR(class.as_ptr()),
        ..Default::default()
    };
    // A zero return is "already registered" on the second daemon in a process
    // — harmless, and there is only ever one.
    unsafe { RegisterClassW(&wc) };
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            windows::core::PCWSTR(class.as_ptr()),
            &HSTRING::from("Trix"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance.into()),
            None,
        )
    }
    .context("CreateWindowExW")?;
    Ok(hwnd)
}
```

- [ ] **Step 7: Add `Hotkey::register` to `trix-core`**

`control::start_hotkey` spawns its own thread and pump; this task needs the registration without either. Add to `crates/trix-core/src/control.rs`, on `impl Hotkey`:

```rust
    /// Registers this combination against `hwnd`, so `WM_HOTKEY` arrives at
    /// that window's procedure rather than on a bare thread queue.
    ///
    /// [`start_hotkey`] exists for `trix replay`, which has no window and
    /// wants a channel. The daemon has a window already (for its tray icon)
    /// and must not run a second message pump, so it registers directly.
    ///
    /// # Safety
    /// `hwnd` must belong to the calling thread — `RegisterHotKey` binds the
    /// registration to that thread's queue.
    pub unsafe fn register(&self, hwnd: HWND, id: i32) -> Result<()> {
        unsafe { RegisterHotKey(Some(hwnd), id, self.modifiers | MOD_NOREPEAT, self.vk) }
            .with_context(|| {
                format!(
                    "RegisterHotKey {self} failed — another program already owns this \
                     combination; pick a different clip_hotkey in config.toml"
                )
            })
    }
```

Add `HWND` to `control.rs`'s `windows::Win32::Foundation` import.

- [ ] **Step 8: Run the worker loop in `main.rs`**

In `crates/trix-daemon/src/main.rs`, after `spawn_stats_thread` and **before** `pipe::serve`, start the window and the worker that drains its actions. The worker is where blocking is allowed:

```rust
    let (actions_tx, actions_rx) = std::sync::mpsc::sync_channel(trix_daemon::window::ACTION_QUEUE_DEPTH);
    let hotkey_spec = daemon.config_snapshot().clip_hotkey;
    let mut window = trix_daemon::window::spawn(actions_tx, &hotkey_spec)?;

    let worker_daemon = Arc::clone(&daemon);
    std::thread::Builder::new().name("trix-tray-worker".into()).spawn(move || {
        for action in actions_rx {
            trix_daemon::window::handle_action(&worker_daemon, action);
        }
    })?;
```

Add `handle_action` to `window.rs`. It is the only place in this module that touches `Daemon`:

```rust
/// Runs one action. Called on the worker thread, where blocking is fine.
pub fn handle_action(daemon: &Arc<Daemon>, action: Action) {
    match action {
        Action::Clip => match daemon.clip() {
            Ok(meta) => tracing::info!(clip = %meta.id, "clip saved from the tray or hotkey"),
            Err(e) => tracing::warn!(error = %format!("{e:#}"), "clip failed"),
        },
        Action::Arm | Action::Disarm | Action::ToggleArmed => {
            // Task 8 fills these in with the tray's toggle semantics.
            tracing::debug!(?action, "not wired until the tray lands");
        }
        Action::OpenClipsFolder | Action::Quit => {
            tracing::debug!(?action, "not wired until the tray lands");
        }
    }
}
```

Use whatever `Daemon`'s clip method is actually called — confirm with `grep -n "pub fn clip" crates/trix-daemon/src/state.rs`. If no `config_snapshot` accessor exists, add a small one rather than making the `config` mutex public.

- [ ] **Step 9: Hand-verify the hotkey end to end**

This is the stage-3 requirement and cannot be automated — it needs a real keypress against a real armed daemon.

1. `cargo build --release --workspace`
2. Start `target\release\trix-daemon.exe -v`
3. Confirm the log line `clip hotkey registered` with the right combination
4. `arm` over the pipe
5. Press the hotkey
6. Confirm `clip saved from the tray or hotkey` and a new `.mp4` + `.json` in the clip directory
7. Press it **five times rapidly** during a mux and confirm the log shows dropped actions rather than the daemon hanging
8. `disarm`, press the hotkey again, confirm it reports a clip failure rather than crashing

- [ ] **Step 10: Commit**

```bash
cargo test --workspace && cargo fmt --all -- --check
git add crates/trix-core/src/control.rs crates/trix-daemon/src/window.rs crates/trix-daemon/src/lib.rs crates/trix-daemon/src/main.rs
git commit -m "feat: give the daemon a message-only window and a working clip hotkey"
```

---

### Task 8: The tray icon

Spec §7.2. Icon hollow when idle, filled when armed. Menu: Arm/Disarm, Open Trix, Open clips folder, Quit. Left-click opens the UI — which does not exist until plan 4, so it opens the clips folder for now and is wired to the UI in stage 4.

**Files:**
- Create: `crates/trix-daemon/src/tray.rs`
- Modify: `crates/trix-daemon/src/window.rs` (tray callback arm, action wiring), `crates/trix-daemon/src/main.rs`, `Cargo.toml`
- Test: `crates/trix-daemon/src/tray.rs`, plus hand verification

**Interfaces:**
- Consumes: `WindowHandle::hwnd()` from Task 7; `Action` from Task 7.
- Produces: `tray::Tray::add(hwnd) -> Result<Tray>`, `Tray::set_armed(bool)`, `Tray::show_menu(hwnd, armed)`; `Tray` removes the icon on drop.

- [ ] **Step 1: Add the Shell feature**

Add `"Win32_UI_Shell"` to the `[workspace.dependencies.windows]` feature list in the root `Cargo.toml`.

- [ ] **Step 2: Write the failing test for the icon rasteriser**

The icon is generated at runtime rather than shipped as an `.ico`: two 32×32 bitmaps drawn in Rust need no resource compiler, no asset files, and no build script — which suits a project whose whole pitch is a 1.5 MB binary. The rasteriser is pure and testable; the Win32 calls around it are not. Add to `tray.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// The two states must be visually distinct — an armed daemon that looks
    /// identical to an idle one is the tray icon failing at its only job.
    /// Comparing pixel counts rather than images keeps this a real assertion
    /// without checking in a reference bitmap.
    #[test]
    fn armed_and_idle_icons_differ_and_are_both_drawn() {
        let idle = icon_pixels(false);
        let armed = icon_pixels(true);
        assert_eq!(idle.len(), ICON_SIDE * ICON_SIDE, "one BGRA pixel per cell");
        assert_eq!(armed.len(), idle.len());

        let opaque = |px: &[u32]| px.iter().filter(|p| (*p >> 24) > 0x40).count();
        let (idle_on, armed_on) = (opaque(&idle), opaque(&armed));
        assert!(idle_on > 0, "the idle icon must not be blank");
        assert!(
            armed_on > idle_on * 2,
            "filled must be visibly more than hollow: {armed_on} vs {idle_on}"
        );
    }
}
```

- [ ] **Step 3: Run it and watch it fail**

Run: `cargo test --workspace armed_and_idle_icons_differ`
Expected: FAIL — `cannot find function icon_pixels`.

- [ ] **Step 4: Implement the rasteriser**

```rust
/// Icon edge in pixels. 32 is the large-DPI tray size; Windows downscales to
/// 16 cleanly and asking for a 16 would look soft at 150% scaling.
pub(crate) const ICON_SIDE: usize = 32;

/// A ring (idle) or a filled disc (armed), as premultiplied BGRA.
///
/// Drawn rather than shipped as an `.ico`: two circles need no resource
/// compiler, no asset files, and no build script, and stay correct if the
/// binary is moved. The shape is deliberately the same silhouette in both
/// states so the icon reads as "Trix" either way — only the fill changes.
pub(crate) fn icon_pixels(armed: bool) -> Vec<u32> {
    let side = ICON_SIDE as f32;
    let center = (side - 1.0) / 2.0;
    let outer = side * 0.44;
    let inner = outer * 0.55;
    let mut pixels = vec![0u32; ICON_SIDE * ICON_SIDE];
    for y in 0..ICON_SIDE {
        for x in 0..ICON_SIDE {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let d = (dx * dx + dy * dy).sqrt();
            // One-pixel smoothstep at each edge: an aliased circle at 16 px
            // looks like a defect.
            let edge = |r: f32| (1.0 - (d - r + 0.5)).clamp(0.0, 1.0);
            let coverage = if armed { edge(outer) } else { edge(outer) * (1.0 - edge(inner)) };
            let alpha = (coverage * 255.0) as u32;
            // Premultiplied white; the tray composites over unknown
            // backgrounds and a non-premultiplied icon fringes dark.
            pixels[y * ICON_SIDE + x] = (alpha << 24) | (alpha << 16) | (alpha << 8) | alpha;
        }
    }
    pixels
}
```

- [ ] **Step 5: Run the test**

Run: `cargo test --workspace armed_and_idle_icons_differ`
Expected: PASS.

- [ ] **Step 6: Build the `HICON` and the notify-icon lifecycle**

```rust
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIcon, CreatePopupMenu, DestroyIcon, DestroyMenu, HICON, MF_SEPARATOR,
    MF_STRING, SetForegroundWindow, TPM_RIGHTALIGN, TrackPopupMenu,
};

/// The tray callback message. Anything from `WM_APP` up is ours.
pub const WM_TRIX_TRAY: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 1;

/// Menu command ids, returned by `TrackPopupMenu`.
pub const ID_TOGGLE: usize = 1;
pub const ID_OPEN_UI: usize = 2;
pub const ID_OPEN_FOLDER: usize = 3;
pub const ID_QUIT: usize = 4;

/// A live tray icon. Removing it is not optional: an icon whose process dies
/// without `NIM_DELETE` lingers in the tray until the user hovers it, which
/// looks exactly like a crashed app.
pub struct Tray {
    hwnd: HWND,
    icons: [HICON; 2],
    armed: bool,
}

impl Tray {
    pub fn add(hwnd: HWND) -> Result<Self> {
        let icons = [unsafe { make_icon(false) }?, unsafe { make_icon(true) }?];
        let tray = Self { hwnd, icons, armed: false };
        unsafe { tray.notify(NIM_ADD) }?;
        Ok(tray)
    }

    /// Swaps the icon and the tooltip. Cheap enough to call on every state
    /// change; Windows ignores a `NIM_MODIFY` that changes nothing.
    pub fn set_armed(&mut self, armed: bool) {
        if self.armed == armed {
            return;
        }
        self.armed = armed;
        if let Err(e) = unsafe { self.notify(NIM_MODIFY) } {
            tracing::warn!(error = %e, "could not update the tray icon");
        }
    }

    unsafe fn notify(&self, message: windows::Win32::UI::Shell::NOTIFY_ICON_MESSAGE) -> Result<()> {
        let mut data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: 1,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: WM_TRIX_TRAY,
            hIcon: self.icons[usize::from(self.armed)],
            ..Default::default()
        };
        let tip = if self.armed { "Trix — armed" } else { "Trix — idle" };
        for (i, c) in tip.encode_utf16().enumerate().take(data.szTip.len() - 1) {
            data.szTip[i] = c;
        }
        unsafe { Shell_NotifyIconW(message, &data) }
            .ok()
            .map_err(|e| anyhow!("Shell_NotifyIconW failed: {e}"))
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            let _ = self.notify(NIM_DELETE);
            for icon in self.icons {
                let _ = DestroyIcon(icon);
            }
        }
    }
}

unsafe fn make_icon(armed: bool) -> Result<HICON> {
    let pixels = icon_pixels(armed);
    // CreateIcon takes separate AND and XOR masks; with a full alpha channel
    // in the colour bits the AND mask is ignored, so it is all-zero.
    let and_mask = vec![0u8; ICON_SIDE * ICON_SIDE / 8];
    let xor: Vec<u8> = pixels.iter().flat_map(|p| p.to_le_bytes()).collect();
    unsafe {
        CreateIcon(
            None,
            ICON_SIDE as i32,
            ICON_SIDE as i32,
            1,
            32,
            and_mask.as_ptr(),
            xor.as_ptr(),
        )
    }
    .context("CreateIcon")
}
```

- [ ] **Step 7: Add the popup menu**

```rust
/// Shows the tray menu at the cursor and returns the chosen command id.
///
/// `SetForegroundWindow` before `TrackPopupMenu` is required, not decorative:
/// without it the menu does not dismiss when the user clicks elsewhere and
/// stays stuck on screen. This is a documented Win32 quirk.
pub unsafe fn show_menu(hwnd: HWND, armed: bool) -> Option<usize> {
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        let toggle = if armed { "Disarm" } else { "Arm" };
        let _ = AppendMenuW(menu, MF_STRING, ID_TOGGLE, &HSTRING::from(toggle));
        let _ = AppendMenuW(menu, MF_STRING, ID_OPEN_UI, &HSTRING::from("Open Trix"));
        let _ = AppendMenuW(menu, MF_STRING, ID_OPEN_FOLDER, &HSTRING::from("Open clips folder"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, ID_QUIT, &HSTRING::from("Quit"));

        let mut cursor = windows::Win32::Foundation::POINT::default();
        let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut cursor);
        let _ = SetForegroundWindow(hwnd);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RIGHTALIGN | windows::Win32::UI::WindowsAndMessaging::TPM_RETURNCMD,
            cursor.x,
            cursor.y,
            None,
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);
        match chosen.0 {
            0 => None,
            id => Some(id as usize),
        }
    }
}
```

- [ ] **Step 8: Route tray messages in the window proc**

In `window.rs`'s `wnd_proc`, add an arm before the `_` fallback:

```rust
        WM_TRIX_TRAY => {
            let event = lparam.0 as u32;
            let action = match event {
                // Left click: open the UI. Until plan 4 ships one, that is the
                // clips folder — the thing the user was going to look at.
                WM_LBUTTONUP => Some(Action::OpenClipsFolder),
                WM_RBUTTONUP => {
                    // The menu is modal and pumps its own messages, but it is
                    // driven by the user and returns promptly, so it is the
                    // one thing this thread is allowed to do inline.
                    let armed = ARMED_MIRROR.load(std::sync::atomic::Ordering::Relaxed);
                    match unsafe { crate::tray::show_menu(hwnd, armed) } {
                        Some(crate::tray::ID_TOGGLE) => Some(Action::ToggleArmed),
                        Some(crate::tray::ID_OPEN_UI) => Some(Action::OpenClipsFolder),
                        Some(crate::tray::ID_OPEN_FOLDER) => Some(Action::OpenClipsFolder),
                        Some(crate::tray::ID_QUIT) => Some(Action::Quit),
                        _ => None,
                    }
                }
                _ => None,
            };
            if let Some(action) = action {
                ACTIONS.with(|a| {
                    if let Some(tx) = a.borrow().as_ref() {
                        offer(tx, action);
                    }
                });
            }
            LRESULT(0)
        }
```

`ARMED_MIRROR` is a `static AtomicBool` in `window.rs` that the worker updates after every arm/disarm. The pump reads it instead of taking `Daemon`'s `armed` mutex — that mutex is held across engine startup and teardown, and blocking the pump on it is exactly what the module comment forbids.

- [ ] **Step 9: Finish the action handlers**

Replace the placeholder arms in `handle_action`:

```rust
        Action::ToggleArmed => {
            let armed = ARMED_MIRROR.load(Ordering::Relaxed);
            let result = if armed { daemon.disarm().map(|_| ()) } else { daemon.arm().map(|_| ()) };
            match result {
                Ok(()) => set_armed_mirror(daemon),
                Err(e) => tracing::warn!(error = %format!("{e:#}"), "tray toggle failed"),
            }
        }
        Action::Arm => { /* same as ToggleArmed's arm branch */ }
        Action::Disarm => { /* same as ToggleArmed's disarm branch */ }
        Action::OpenClipsFolder => {
            let dir = daemon.config_snapshot().clip_dir_path();
            // `explorer.exe` returns a non-zero exit code on success, so its
            // status is deliberately ignored rather than logged as a failure.
            let _ = std::process::Command::new("explorer.exe").arg(dir).spawn();
        }
        Action::Quit => {
            // The same path Ctrl+C takes, so a tray Quit finalizes an
            // in-flight mux exactly like a console close does rather than
            // dropping the clip the user just asked for.
            trix_core::control::request_shutdown();
        }
```

`control::request_shutdown()` does not exist yet — add it to `control.rs` beside `shutdown_requested`, setting the same `SHUTDOWN` flag the console handler sets, so the existing shutdown watcher in `main.rs` does the rest:

```rust
/// Requests shutdown from inside the process — the tray's Quit item. Sets the
/// same flag the console handler sets, so the existing watcher performs the
/// same budgeted disarm and the same `mark_finalized`.
pub fn request_shutdown() {
    SHUTDOWN.store(true, Ordering::Release);
}
```

- [ ] **Step 10: Create the tray in `main.rs` and remove it on exit**

After `window::spawn`, add `let mut tray = trix_daemon::tray::Tray::add(window.hwnd())?;`. The tray must be removed before `std::process::exit` — which does not run destructors — so the shutdown watcher needs to drop it. Simplest correct approach: keep the `Tray` in the watcher's reach behind a `Mutex<Option<Tray>>` and `take()` it in the watcher immediately before `mark_finalized()`.

- [ ] **Step 11: Hand-verify the tray**

1. Start the daemon; confirm a **hollow** icon appears in the tray
2. Right-click → **Arm**; confirm the icon becomes **filled** and the tooltip reads "Trix — armed"
3. Right-click again; confirm the menu now reads **Disarm**
4. Left-click; confirm the clips folder opens
5. Right-click → **Open clips folder**; same
6. With the daemon armed, right-click → **Quit**; confirm the icon disappears **immediately** (not on hover), the process exits, and any in-flight clip finished
7. Confirm no orphan icon is left in the tray after the process exits

- [ ] **Step 12: Commit**

```bash
cargo test --workspace && cargo fmt --all -- --check
git add Cargo.toml crates/trix-daemon/src/tray.rs crates/trix-daemon/src/window.rs crates/trix-daemon/src/main.rs crates/trix-core/src/control.rs
git commit -m "feat: add the tray icon, its menu, and armed-state feedback"
```

---

### Task 9: Thumbnails at clip time

Spec §5.3. **This is the one task that touches the capture path certified over seven phases**, which is why it is last: it lands on a daemon Tasks 7–8 have already verified.

The design keeps the gameplay path free. Rather than copying every frame in case a clip is requested, a clip request sets a flag and the **next** frame stages itself. The thumbnail is one frame (~16 ms) after the button press instead of exactly on it — imperceptible, and the alternative is a 9 MB GPU copy 60 times a second forever.

**Files:**
- Create: `crates/trix-core/src/thumb.rs`
- Modify: `crates/trix-core/src/replay.rs`, `crates/trix-core/src/lib.rs`, `Cargo.toml`
- Modify: `PLAN.md` (§5.3 requires amending Decision 1)
- Test: `crates/trix-core/src/thumb.rs`

**Interfaces:**
- Consumes: `frame.as_raw_texture()` (`replay.rs:406`), `library::thumb_path` (`library.rs:29`).
- Produces: `thumb::encode_jpeg(bgra: &[u8], width: u32, height: u32, stride: usize) -> Result<Vec<u8>>`; `SavedClip` gains a `thumbnail: Option<Vec<u8>>`.

- [ ] **Step 1: Add the WIC feature**

Add `"Win32_Graphics_Imaging"` to the `[workspace.dependencies.windows]` feature list.

- [ ] **Step 2: Write the failing test**

The JPEG encoder is a pure function over a byte buffer — no GPU, no capture. Create `crates/trix-core/src/thumb.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// A real JPEG, not just bytes: the SOI/EOI markers are what a `<video>`
    /// poster or an `<img>` in plan 4's grid will actually parse.
    #[test]
    fn encodes_a_bgra_buffer_into_a_real_jpeg() {
        let (w, h) = (64u32, 32u32);
        let stride = (w * 4) as usize;
        // A gradient rather than a solid colour: a solid image would still
        // encode if the stride handling were wrong.
        let mut bgra = vec![0u8; stride * h as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let p = y * stride + x * 4;
                bgra[p] = (x * 4) as u8;
                bgra[p + 1] = (y * 8) as u8;
                bgra[p + 2] = 0x80;
                bgra[p + 3] = 0xFF;
            }
        }

        let jpeg = encode_jpeg(&bgra, w, h, stride).expect("WIC must encode a plain BGRA buffer");
        assert!(jpeg.len() > 256, "suspiciously small for a 64x32 gradient: {}", jpeg.len());
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8], "JPEG SOI marker");
        assert_eq!(&jpeg[jpeg.len() - 2..], &[0xFF, 0xD9], "JPEG EOI marker");
    }

    /// A short buffer must be refused rather than read past the end. This runs
    /// on a socket-reachable path under `panic = "abort"`, so an out-of-bounds
    /// read is a process kill, not an exception.
    #[test]
    fn a_buffer_too_small_for_its_geometry_is_refused() {
        assert!(encode_jpeg(&[0u8; 16], 64, 32, 256).is_err());
    }
}
```

- [ ] **Step 3: Run it and watch it fail**

Run: `cargo test --workspace encodes_a_bgra_buffer`
Expected: FAIL — module does not exist.

- [ ] **Step 4: Implement the encoder**

Prepend to `thumb.rs`:

```rust
//! Clip thumbnails: a staged BGRA frame, WIC-encoded to JPEG.
//!
//! Spec §5.3 takes the thumbnail from the frame at the hotkey press rather
//! than from a decoded MP4 — the frame is already in VRAM, so no decoder is
//! involved and no file is re-read. That requires one staging copy to CPU
//! memory per clip (~9 MB at 1920x1200, freed immediately), which is the
//! **deliberate, documented exception** to PLAN.md's Decision 1 ("frames never
//! touch the CPU"). It is paid once per clip, not once per frame.

use anyhow::{Context as _, Result, bail};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatJpeg, GUID_WICPixelFormat32bppBGRA,
    IWICImagingFactory, WICBitmapEncoderNoCache,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CoCreateInstance, CreateStreamOnHGlobal, STGM_READ,
};

/// JPEG quality. 0.82 is the knee: visually clean in a grid at any thumbnail
/// size, and roughly a third the bytes of 0.95.
const QUALITY: f32 = 0.82;

/// Encodes a top-down BGRA buffer to JPEG bytes.
///
/// `stride` is the row pitch in bytes, which for a staged D3D texture is
/// whatever the driver chose and is frequently larger than `width * 4`.
/// Passing `width * 4` when the real pitch is bigger produces a sheared image,
/// so it is an explicit parameter rather than a derived one.
pub fn encode_jpeg(bgra: &[u8], width: u32, height: u32, stride: usize) -> Result<Vec<u8>> {
    if width == 0 || height == 0 {
        bail!("cannot encode a {width}x{height} thumbnail");
    }
    let needed = stride
        .checked_mul(height as usize)
        .context("thumbnail geometry overflows")?;
    if bgra.len() < needed {
        bail!("thumbnail buffer is {} bytes, need {needed} for {width}x{height}", bgra.len());
    }

    crate::encode::mf::ensure_mf_started()?;
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                .context("WIC factory")?;
        let stream = CreateStreamOnHGlobal(None, true).context("thumbnail stream")?;
        let encoder = factory
            .CreateEncoder(&GUID_ContainerFormatJpeg, std::ptr::null())
            .context("JPEG encoder")?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache).context("encoder init")?;

        let mut frame = None;
        let mut props = None;
        encoder.CreateNewFrame(&mut frame, &mut props).context("encoder frame")?;
        let frame = frame.context("WIC returned no frame")?;
        // Quality is set through the property bag before Initialize, or it is
        // silently ignored.
        if let Some(props) = props {
            set_quality(&props, QUALITY);
        }
        frame.Initialize(props.as_ref()).context("frame init")?;
        frame.SetSize(width, height).context("frame size")?;
        let mut format = GUID_WICPixelFormat32bppBGRA;
        frame.SetPixelFormat(&mut format).context("frame pixel format")?;
        frame
            .WritePixels(height, stride as u32, &bgra[..needed])
            .context("writing thumbnail pixels")?;
        frame.Commit().context("frame commit")?;
        encoder.Commit().context("encoder commit")?;

        read_stream(&stream)
    }
}
```

`set_quality` writes a `VT_R4` VARIANT named `ImageQuality` into the property bag — mirror the hand-built VARIANT technique already in `encode/mf.rs:70` (`variant_u32`), which exists because windows-rs exposes only the raw union. `read_stream` seeks to 0 with `STGM_READ` and reads the `HGLOBAL` back into a `Vec<u8>`.

- [ ] **Step 5: Run the tests**

Run: `cargo test --workspace encodes_a_bgra_buffer a_buffer_too_small`
Expected: PASS, both.

- [ ] **Step 6: Stage the frame in the capture session**

Add to `ReplaySession` (`replay.rs:92`):

```rust
    /// Set when a clip is requested; the next frame stages itself to CPU and
    /// clears it. Deliberately *not* a copy of every frame — a per-frame 9 MB
    /// GPU blit would cost exactly the gameplay impact this project exists to
    /// avoid. The cost is a thumbnail one frame (~16 ms) after the button
    /// press, which nobody can perceive.
    thumb_wanted: bool,
    /// The most recent staged frame, taken on the tick after a clip request.
    thumb_staged: Option<(Vec<u8>, u32, u32, usize)>,
```

In `on_frame_arrived`, after the existing encode call (`replay.rs:407`), add:

```rust
        if std::mem::take(&mut self.thumb_wanted) {
            match self.stage_thumbnail(frame.as_raw_texture()) {
                Ok(staged) => self.thumb_staged = Some(staged),
                // A thumbnail is a nicety; the clip is the product. Never let
                // this fail a capture callback.
                Err(e) => tracing::warn!(error = %format!("{e:#}"), "could not stage a thumbnail"),
            }
        }
```

`stage_thumbnail` creates a `D3D11_USAGE_STAGING` texture with `CPU_ACCESS_READ`, `CopyResource`s the frame into it, `Map`s it, copies the rows into a `Vec<u8>`, and `Unmap`s. Reuse the device already held by `self.converter`.

- [ ] **Step 7: Request and consume it in `save_clip`**

In `save_clip` (`replay.rs:463`), set the flag before the snapshot and read the staged frame after the MP4 is written, so the JPEG lands beside its clip:

```rust
    let thumbnail = capture
        .callback()
        .lock()
        .take_staged_thumbnail()
        .and_then(|(bgra, w, h, stride)| match thumb::encode_jpeg(&bgra, w, h, stride) {
            Ok(jpeg) => Some(jpeg),
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "thumbnail encode failed");
                None
            }
        });
    if let Some(jpeg) = &thumbnail {
        let path = library::thumb_path(clip_dir, &id);
        if let Err(e) = std::fs::write(&path, jpeg) {
            tracing::warn!(path = %path.display(), %e, "thumbnail not written");
        }
    }
```

The flag must be set at least one frame *before* the snapshot for a thumbnail to exist. Set it when the `Clip` command is received in the control loop, not inside `save_clip`, so the staging frame arrives while the snapshot is being taken.

- [ ] **Step 8: Amend PLAN.md**

§5.3 requires this explicitly — "PLAN.md gets amended so it does not read as an accident." Find Decision 1 ("frames never touch the CPU") and add:

```markdown
**Exception (stage 3, spec §5.3):** clip thumbnails stage exactly one frame to
CPU memory per clip — roughly 9 MB at 1920x1200, freed immediately. This is a
deliberate, bounded exception: it is paid once per clip rather than once per
frame, it happens on the frame after the button press so the steady-state
capture path is unchanged, and the alternative (decoding the finished MP4)
would cost a decoder the engine does not otherwise need.
```

- [ ] **Step 9: Verify the gameplay path did not regress**

This is the assertion that matters. Run `scripts/arm-cycle-leak.ps1` — thumbnails allocate 9 MB per clip and must give all of it back:

Run: `pwsh -File scripts/arm-cycle-leak.ps1 -Cycles 6`
Expected: PASS, MB/cycle comparable to the ~0.78 recorded on 2026-08-01.

Then arm, clip ten times, and confirm `dropped` is unchanged from a ten-clip run before this task.

- [ ] **Step 10: Commit**

```bash
cargo test --workspace && cargo fmt --all -- --check
git add Cargo.toml PLAN.md crates/trix-core/src/thumb.rs crates/trix-core/src/lib.rs crates/trix-core/src/replay.rs
git commit -m "feat: write a thumbnail from the frame at clip time"
```

---

### Task 10: The stage-3 gate

Spec §10 stage 3: "tray arm/disarm works, hotkey clips while armed, `paced`/`dropped` counters match the CLI path, and working set stays under 30 MB while armed."

**Files:**
- Create: `scripts/daemon-smoke.ps1`
- Modify: `docs/superpowers/specs/2026-07-26-trix-desktop-ui-design.md` (§4.3 — the `autostart` and `max_library_gb` keys in `config.get`/`config.set`)

- [ ] **Step 1: Write the gate**

Model it on `scripts/protocol-smoke.ps1` and `scripts/arm-cycle-leak.ps1`. It must:

1. **Check binary freshness by timestamp** — extract `Get-NewestSourceFile` from `protocol-smoke.ps1` by AST, as `arm-cycle-leak.ps1` does, so the three gates cannot drift on what "fresh" means
2. **Refuse to run if a daemon is already running** rather than killing it — it may be armed and recording for someone
3. Start the daemon, `arm`, and **sample working set while armed and settled** — fail above 30 MB
4. Clip, and assert the `.mp4`, `.json`, **and `.jpg`** all exist
5. Assert `ring_seconds_used <= ring_seconds_total` — this is the assertion Task 1 makes meaningful, and the one plan 2's gate structurally could not reach
6. Assert the clip's `duration_ms` is within 10% of `replay_seconds * 1000` — tighter than plan 2's 20%, which the GOP pin now earns
7. Compare `paced`/`dropped` against a `trix.exe replay` run of the same length
8. Set `max_library_gb` to a value below the current library size, clip, and assert old non-favorite clips were pruned and a favorite was not
9. Toggle `autostart` on and off and assert the registry value appears and disappears
10. **Print a receipt with a clip id the user can go and play**

Append `$Detail` on FAIL only — plan 2's gate appended it on PASS too, so the healthy receipt read like a failure.

- [ ] **Step 2: Prove the gate can fail**

Run it once green. Then break one thing at a time — comment out the GOP pin, then the thumbnail write, then the ceiling call — and confirm the gate goes red on **that specific check** each time. Restore after each. A gate nobody has seen fail is a gate nobody should trust; this branch has shipped five tests that could not fail.

- [ ] **Step 3: Update the spec's command table**

§4.3's `config.get` row must mention `autostart` reads from the registry rather than the file; `config.set`'s row must note `autostart` writes it. A third-party UI author reads §4.3 and nothing else.

- [ ] **Step 4: Commit**

```bash
git add scripts/daemon-smoke.ps1 docs/superpowers/specs/2026-07-26-trix-desktop-ui-design.md
git commit -m "test: add the stage-3 daemon gate"
```

- [ ] **Step 5: Hand verification by the user — the actual gate**

Machine-green is not stage 3. The user runs `scripts/daemon-smoke.ps1`, then:

1. **Plays the clip** it produced — video and audio in sync
2. **Opens the `.jpg`** and confirms it shows the moment of the press, not a black frame
3. Arms from the **tray**, plays a game for a few minutes, clips with the **hotkey**, and confirms no perceptible fps impact
4. Confirms the daemon's working set while armed, from Task Manager, with a game running

Only then is stage 3 complete.

---

## Self-Review

**Spec coverage.** §5.3 → Task 9. §5.4 → Tasks 4, 5. §6.3 GOP → Task 1. §7.2 tray → Task 8. §7.3 autostart → Tasks 4, 6. §10 stage 3 → Task 10. §7.4 first run is a **UI** concern and belongs to plan 4. `library.export` is explicitly stage 4 per §4.3 and is not here.

**Carried escalations.** Ring overshoot → Task 1. `arm` encoder null → Task 3. Monitor DPI → Task 2. The remaining two (product caps on `replay_seconds`/`bitrate_kbps`, `config.set` stripping comments) are settings-page concerns recorded above as out of scope for plan 3.

**Known soft spots, flagged rather than hidden:**

- **Task 9 Step 6's `stage_thumbnail` is described, not written out.** It needs the exact `D3D11_TEXTURE2D_DESC` the converter's device will accept, which depends on the capture format at runtime. The implementer must read `converter.rs` first and should report back if a staging copy of the raw WGC texture is not directly possible — a `CopyResource` requires matching formats and sizes.
- **Task 8 Step 10's tray teardown across `std::process::exit`** is the one lifecycle in this plan with no test. `exit` does not run destructors, so the `NIM_DELETE` has to happen in the shutdown watcher. Hand-verification step 7 is what actually covers it.
- **Task 3 changes a channel type that `replay.rs` also writes to.** The `grep` in Step 3 is load-bearing; the readiness send site is not at a line number this plan can pin, because plan 2's Task 5 moved it.
- The `windows` crate version is 0.62 and several signatures above (`RegOpenKeyExW`'s `Option` parameters, `CreateIcon`'s pointer arguments, `Shell_NotifyIconW`'s return type) changed shape across recent releases. **Compile early and trust the compiler over this document** where they disagree.
