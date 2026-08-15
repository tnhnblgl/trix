# Trix In-App Updates Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Trix notices a newer GitHub release, and replaces its own three binaries in place on one click, without ever leaving the user with an install that does not run.

**Architecture:** The updater lives entirely in `trix-ui`, which already owns the daemon's lifecycle. It queries the GitHub Releases API, downloads the release zip, checks it against a published SHA-256, stops the daemon through a new `shutdown` protocol command, renames the running executables aside (Windows forbids overwriting a running exe but permits renaming one), moves the new ones in, and hands the relaunch to `trix.exe` so it cannot race `tauri-plugin-single-instance`.

**Tech Stack:** Rust 2024, Tauri v2, Svelte 5. New crates in `trix-ui` only: `ureq` (HTTP, blocking), `sha2`, `zip`, `semver`. `windows` is already a workspace dependency with `Win32_System_Threading` enabled.

## Global Constraints

- **`trix-ui` must not depend on `trix-core`.** Spec §3.2 of the desktop design; stated at the top of `crates/trix-ui/src/main.rs`. Every capability arrives over the control socket. The update module is self-contained.
- **`dispatch` has no catch-all arm.** Adding a `Command` variant to `trix-proto` without a `trix-daemon` match arm is a compile error, by design. Proto and dispatch change together.
- **Adding a `Config` field requires adding a `FIELDS` entry** in `crates/trix-ui/web/src/lib/settings.ts`, or `unknownKeys` shows every user "This daemon has settings this app does not render yet".
- **`commands.rs` is a deliberate pass-through.** `trix_call` forwards any command to the daemon. Do not add a Tauri command per protocol command.
- **Version source of truth:** `[workspace.package] version` in `Cargo.toml`. `trix-ui` reads it at runtime with `env!("CARGO_PKG_VERSION")`. `ship-zip.ps1` already enforces that `tauri.conf.json` agrees.
- **Release asset names:** `trix-v<version>-win-x64.zip` and `SHA256SUMS.txt`. The zip contains one top-level directory named `trix-v<version>-win-x64` (`ship-zip.ps1` passes `includeBaseDirectory: true`).
- **Allowed hosts, exactly:** `api.github.com`, `github.com`, `objects.githubusercontent.com`. Redirects are validated against this list too.
- **Files the swap may touch, exactly:** `trix.exe`, `trix-daemon.exe`, `trix-ui.exe`, `LICENSE`, `README.txt`. Nothing else in the directory.
- **No commit may add a `Co-Authored-By` trailer or a "Generated with" line.** Author stays `tnhnblgl <tnhnblgl@gmail.com>`.
- **Never `git push`.** The branch stays local unless the user asks.
- Every daemon test uses a scratch `%APPDATA%` **and** a scratch `clip_dir`. An empty `clip_dir` resolves to the developer's real `%USERPROFILE%\Videos\Trix`, so `%APPDATA%` alone is not isolation.
- Failure copy is written for a user, names the manual fallback, and never shows a stack trace.

## File Structure

**Created:**

| File | Responsibility |
|---|---|
| `crates/trix-ui/src/update/mod.rs` | `UpdateState`, orchestration, Tauri commands, startup cleanup |
| `crates/trix-ui/src/update/check.rs` | GitHub API query, version comparison, asset selection |
| `crates/trix-ui/src/update/verify.rs` | `SHA256SUMS.txt` parsing and digest comparison |
| `crates/trix-ui/src/update/download.rs` | Host allowlist, size bound, streaming download |
| `crates/trix-ui/src/update/swap.rs` | Preflight, payload resolution, rename dance, rollback, cleanup |
| `scripts/update-smoke.ps1` | Offline end-to-end swap rehearsal in a scratch directory |

**Modified:** `crates/trix-proto/src/command.rs`, `crates/trix-daemon/src/dispatch.rs`, `crates/trix-core/src/config.rs`, `crates/trix-ui/src/daemon.rs`, `crates/trix-ui/src/main.rs`, `crates/trix-ui/Cargo.toml`, `crates/trix-cli/src/main.rs`, `crates/trix-ui/web/src/lib/settings.ts`, `crates/trix-ui/web/src/lib/state.svelte.ts`, `crates/trix-ui/web/src/App.svelte`, `scripts/ship-zip.ps1`, `docs/ship/README.txt`.

---

### Task 1: The `shutdown` command

The daemon already has everything needed: `trix_core::control::request_shutdown()` sets the same flag the tray's Quit item does, and `spawn_shutdown_watcher` in `crates/trix-daemon/src/main.rs` polls it every 100 ms, removes the tray icon, disarms within a 3 s budget, and calls `process::exit(0)`. That 100 ms poll is also what gives the pipe writer time to flush the reply — the reply is genuinely sent before the process dies, with no sleep added anywhere.

**Files:**
- Modify: `crates/trix-proto/src/command.rs` (enum at line 15, `parse` at line 35)
- Modify: `crates/trix-daemon/src/dispatch.rs` (match at line 50)

**Interfaces:**
- Produces: `Command::Shutdown`; wire command `"shutdown"`; reply shape `{"ok": true, "pid": <u32>}`.

- [ ] **Step 1: Write the failing proto test**

In `crates/trix-proto/src/command.rs`, inside `mod tests`:

```rust
/// The app sends this before replacing trix-daemon.exe on disk. It is the
/// only command whose success the caller confirms by watching the process
/// leave, so it must parse from a bare request with no arguments.
#[test]
fn shutdown_parses_with_no_arguments() {
    let (id, cmd) = parse(r#"{"id":3,"cmd":"shutdown"}"#);
    assert_eq!(id, 3);
    assert_eq!(cmd.unwrap(), Command::Shutdown);
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p trix-proto shutdown_parses`
Expected: FAIL — `no variant named 'Shutdown' found for enum 'Command'`.

- [ ] **Step 3: Add the variant and the parse arm**

In the `Command` enum, after `StatsSubscribe { enabled: bool },`:

```rust
    /// Ends the daemon process. Answered before the process exits, so the
    /// caller can tell a clean shutdown from a crashed socket.
    Shutdown,
```

In `parse`, after the `"stats.subscribe"` arm:

```rust
            "shutdown" => Self::Shutdown,
```

- [ ] **Step 4: Run the proto tests**

Run: `cargo test -p trix-proto`
Expected: PASS. `cargo test -p trix-daemon` now FAILS to compile — the exhaustive match in `dispatch.rs` has no arm for `Shutdown`. That is the design working.

- [ ] **Step 5: Write the failing daemon test**

In `crates/trix-daemon/src/dispatch.rs`, inside `mod tests`:

```rust
/// The reply must be produced by `dispatch`, not inferred by the caller from
/// a dropped pipe: a daemon that exited first is indistinguishable from one
/// that crashed. The PID rides along because the app cannot always hold a
/// process handle -- with "Start with Windows" on, the daemon was started at
/// login by the shell, not by the app's supervisor.
#[test]
fn shutdown_answers_with_its_own_pid_before_the_process_goes_away() {
    let daemon = idle("shutdown_pid");
    let response = daemon.dispatch(ClientId::FIRST, &request(9, "shutdown"));

    let data = response.data.expect("shutdown answers with data");
    assert_eq!(data.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        data.get("pid").and_then(Value::as_u64),
        Some(u64::from(std::process::id())),
        "the caller terminates this PID if the daemon does not leave in time"
    );
}
```

Read the existing tests around line 405 first: `idle(name)` and `request(id, cmd)` are already defined there, and `ClientId::FIRST` may be spelled differently — use whatever `status_is_answered_without_an_engine` uses.

- [ ] **Step 6: Run it and watch it fail**

Run: `cargo test -p trix-daemon shutdown_answers`
Expected: FAIL to compile — non-exhaustive match in `dispatch`.

- [ ] **Step 7: Add the dispatch arm**

In `dispatch`, after the `Command::StatsSubscribe` arm:

```rust
            Ok(Command::Shutdown) => shutdown(request.id),
```

And a helper beside the other response builders:

```rust
/// Ends the daemon, and answers first.
///
/// `request_shutdown` only sets the flag that `main.rs`'s shutdown watcher
/// polls at 100 ms -- the same flag the tray's Quit item sets. That indirection
/// is what makes the reply reliable without a sleep here: this function returns
/// a `Response` the pipe writer flushes immediately, and the watcher does not
/// even look at the flag until long after. It then removes the tray icon,
/// disarms within its budget, and exits, so a clip being muxed right now is
/// still finished.
fn shutdown(id: u64) -> Response {
    tracing::info!("shutdown requested over the control socket");
    trix_core::control::request_shutdown();
    let mut fields = Map::new();
    fields.insert("ok".to_string(), Value::Bool(true));
    fields.insert("pid".to_string(), Value::from(std::process::id()));
    Response::ok(id, Value::Object(fields))
}
```

- [ ] **Step 8: Run the suite**

Run: `cargo test --workspace`
Expected: PASS, with two new tests.

- [ ] **Step 9: Commit**

```bash
git add crates/trix-proto/src/command.rs crates/trix-daemon/src/dispatch.rs
git commit -m "feat(proto): add the shutdown command

The app has never been able to stop the daemon it starts -- only the tray
could. Replacing trix-daemon.exe on disk needs it, and the machinery was
already there: request_shutdown sets the same flag Quit does, and the
watcher's 100 ms poll is what lets the reply flush before the process
goes. The PID rides along because with autostart on, the daemon was
launched by the shell and the app holds no handle to it."
```

---

### Task 2: The `check_for_updates` config key

**Files:**
- Modify: `crates/trix-core/src/config.rs` (struct at line 21, `Default` at line 89)
- Modify: `crates/trix-ui/web/src/lib/settings.ts` (`Field.section` union at line 7, `FIELDS` at line 37)
- Test: `crates/trix-daemon/src/dispatch.rs`, `crates/trix-ui/web/src/lib/settings.test.ts`

**Interfaces:**
- Produces: config key `check_for_updates: bool`, default `true`. Live — it must **not** appear in `REQUIRES_REARM` in `crates/trix-daemon/src/state.rs`, and must **not** appear in `NUMERIC_BOUNDS` (it is not a number).

- [ ] **Step 1: Write the failing daemon test**

In `crates/trix-daemon/src/dispatch.rs`, inside `mod tests`. Use `with_scratch_config` (line 395) — it provides the scratch `%APPDATA%` and scratch `clip_dir` the isolation rule requires:

```rust
/// Checking for updates is the app's business, but the setting lives in the
/// daemon's config like every other one, so the settings page needs no second
/// persistence mechanism. The daemon stores it and never acts on it.
#[test]
fn check_for_updates_defaults_on_round_trips_and_needs_no_rearm() {
    let (daemon, _config_path, _clip_dir) = with_scratch_config("check_for_updates");

    let initial = daemon.dispatch(ClientId::FIRST, &request(1, "config.get"));
    let data = initial.data.expect("config.get answers with data");
    assert_eq!(
        data.get("check_for_updates").and_then(Value::as_bool),
        Some(true),
        "updates reach nobody if the check ships off"
    );

    let response = daemon.dispatch(
        ClientId::FIRST,
        &request_with(2, "config.set", &[("check_for_updates", Value::Bool(false))]),
    );
    let data = response.data.expect("config.set answers with data");
    assert_eq!(
        data["accepted"].get("check_for_updates").and_then(Value::as_bool),
        Some(false),
        "the value is read back out of the saved config, not echoed"
    );
    assert_eq!(
        data["requires_rearm"].as_array().map(Vec::len),
        Some(0),
        "a network preference has nothing to do with the capture session"
    );
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p trix-daemon check_for_updates`
Expected: FAIL — `config.set` refuses the unknown key `check_for_updates`.

- [ ] **Step 3: Add the field**

In `crates/trix-core/src/config.rs`, in `pub struct Config`, after `pub autostart: bool,`:

```rust
    /// Whether the desktop app asks GitHub for a newer release on launch.
    ///
    /// Stored here so the settings page reaches it through the same
    /// `config.get`/`config.set` plumbing as everything else. The daemon never
    /// reads it -- `trix-ui` is the only consumer. It is deliberately not in
    /// `REQUIRES_REARM`: it has nothing to do with the capture session.
    #[serde(default = "default_true")]
    pub check_for_updates: bool,
```

In `impl Default for Config`, add `check_for_updates: true,`.

Add beside the other helpers:

```rust
/// `serde(default)` for a bool yields `false`, which would silently turn the
/// update check off for every user upgrading from a config file written before
/// this key existed.
const fn default_true() -> bool {
    true
}
```

- [ ] **Step 4: Run the daemon tests**

Run: `cargo test -p trix-daemon check_for_updates`
Expected: PASS.

- [ ] **Step 5: Write the failing frontend test**

In `crates/trix-ui/web/src/lib/settings.test.ts` (create it if absent; follow `state.svelte.test.ts` for the vitest import style):

```ts
import { describe, expect, it } from 'vitest';
import { FIELDS, READ_ONLY_EXTRAS, unknownKeys } from './settings';

describe('settings fields', () => {
  it('renders check_for_updates, so nobody sees the unknown-settings banner', () => {
    // config.get returns every Config key. A key with no FIELDS entry lands in
    // unknownKeys, and Settings.svelte tells the user their app is out of date
    // with their daemon -- which would be false and unfixable.
    const fromDaemon = { check_for_updates: true, ...Object.fromEntries(READ_ONLY_EXTRAS.map((k) => [k, ''])) };
    expect(unknownKeys(fromDaemon)).toEqual([]);
    expect(FIELDS.find((f) => f.key === 'check_for_updates')?.kind).toBe('bool');
  });
});
```

- [ ] **Step 6: Run it and watch it fail**

Run: `cd crates/trix-ui/web && npm test -- --run settings`
Expected: FAIL — `unknownKeys` returns `['check_for_updates']`.

- [ ] **Step 7: Add the field entry**

In `crates/trix-ui/web/src/lib/settings.ts`, widen the section union on line 7:

```ts
  section: 'Capture' | 'Audio' | 'Quality' | 'Clips' | 'Trix' | 'Updates';
```

and append to `FIELDS`:

```ts
  { key: 'check_for_updates', label: 'Check for updates', kind: 'bool', section: 'Updates', help: 'Asks github.com once per launch whether a newer Trix exists. Sends nothing about you. Updates are never installed without you clicking.' },
```

`Field.svelte` already renders `kind: 'bool'` as a checkbox (line 67) — it needs no change.

- [ ] **Step 8: Run both suites**

Run: `cargo test --workspace` then `cd crates/trix-ui/web && npm test -- --run`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/trix-core/src/config.rs crates/trix-ui/web/src/lib/settings.ts crates/trix-ui/web/src/lib/settings.test.ts crates/trix-daemon/src/dispatch.rs
git commit -m "feat(config): add check_for_updates, defaulting to on

Lives in the daemon's config like every other setting so the settings page
needs no second persistence path, though only the app ever reads it. The
serde default is spelled out rather than derived: a bare serde(default) on a
bool is false, which would quietly disable the check for everyone upgrading
from an older config file. The FIELDS entry is not optional -- a Config key
with no field shows every user the unknown-settings banner."
```

---

### Task 3: Stopping the daemon from the app

**Files:**
- Modify: `crates/trix-ui/src/daemon.rs` (add to `impl Supervisor` after `launch`, line 268)
- Modify: `crates/trix-ui/Cargo.toml`

**Interfaces:**
- Consumes: `Command::Shutdown` from Task 1 (wire name `"shutdown"`), `Supervisor::call`.
- Produces: `Supervisor::stop(&self) -> Result<(), String>` — returns `Ok` only when the daemon is confirmed gone.

- [ ] **Step 1: Write the failing test**

In `crates/trix-ui/src/daemon.rs`, inside `mod tests`:

```rust
/// The wait is bounded because a wedged daemon must not be able to hang the
/// update forever, and generous because disarming releases the capture stack:
/// the Intel driver stack has been measured taking about ten seconds to let
/// go. A budget under that would terminate a daemon that was shutting down
/// correctly, mid-mux, costing the user the clip they just saved.
#[test]
fn the_shutdown_budget_outlasts_a_slow_driver_release() {
    assert!(
        SHUTDOWN_WAIT >= Duration::from_secs(12),
        "the capture stack can take ~10 s to release; {SHUTDOWN_WAIT:?} would kill an honest shutdown"
    );
    assert!(
        SHUTDOWN_WAIT <= Duration::from_secs(30),
        "a user watching a progress bar will not wait {SHUTDOWN_WAIT:?} for a hung daemon"
    );
    assert!(
        SHUTDOWN_POLL < Duration::from_secs(1),
        "the poll decides how quickly a fast shutdown is noticed"
    );
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p trix-ui the_shutdown_budget`
Expected: FAIL — `cannot find value 'SHUTDOWN_WAIT'`.

- [ ] **Step 3: Add the dependency**

In `crates/trix-ui/Cargo.toml`, under `[dependencies]`:

```toml
windows.workspace = true
```

The workspace already enables `Win32_System_Threading` (`OpenProcess`, `WaitForSingleObject`, `TerminateProcess`) and `Win32_Foundation`, so no feature changes are needed.

- [ ] **Step 4: Implement `stop`**

In `crates/trix-ui/src/daemon.rs`, add near the other constants:

```rust
/// How long [`Supervisor::stop`] waits for the daemon to actually leave.
///
/// Disarming releases the capture stack, and that is not instant: the Intel
/// driver stack has been measured holding on for about ten seconds after a
/// disarm. Terminating inside that window would kill a daemon that was
/// shutting down correctly, mid-mux, and cost the user the clip they had just
/// saved. Fifteen seconds clears it with margin and still bounds a wedge.
pub(crate) const SHUTDOWN_WAIT: Duration = Duration::from_secs(15);

/// How often the pipe is probed while waiting. Fast enough that the common
/// case -- an idle daemon, gone in well under a second -- does not look slow.
pub(crate) const SHUTDOWN_POLL: Duration = Duration::from_millis(250);
```

and to `impl Supervisor`:

```rust
    /// Asks the daemon to exit and returns only once it actually has.
    ///
    /// The daemon's disappearance is observed on the pipe rather than on a
    /// process handle: `\\.\pipe\trix-control` is bound with
    /// `FILE_FLAG_FIRST_PIPE_INSTANCE`, so while the name resolves *some*
    /// daemon owns it, and when it stops resolving the process is gone. That
    /// works whether this app started the daemon or Windows did at login,
    /// which a `Child` handle does not -- with "Start with Windows" on, the
    /// supervisor never spawned it and holds nothing to wait on.
    ///
    /// The PID from the reply is the fallback for a daemon that answered and
    /// then wedged. It is only ever used after [`SHUTDOWN_WAIT`] has elapsed,
    /// so a slow-but-honest shutdown is never the thing that gets terminated.
    pub fn stop(&self) -> Result<(), String> {
        let reply = self.call("shutdown", Map::new())?;
        let pid = reply.get("pid").and_then(Value::as_u64).and_then(|p| u32::try_from(p).ok());

        let deadline = Instant::now() + SHUTDOWN_WAIT;
        while Instant::now() < deadline {
            if !pipe_exists() {
                return Ok(());
            }
            std::thread::sleep(SHUTDOWN_POLL);
        }

        match pid {
            Some(pid) => {
                terminate(pid)?;
                // One more poll round: TerminateProcess is asynchronous.
                let deadline = Instant::now() + Duration::from_secs(2);
                while Instant::now() < deadline {
                    if !pipe_exists() {
                        return Ok(());
                    }
                    std::thread::sleep(SHUTDOWN_POLL);
                }
                Err("the Trix recorder did not stop, even after being closed".into())
            }
            None => Err("the Trix recorder did not stop, and did not say which process it is".into()),
        }
    }
```

and two free functions beside `daemon_path_beside`:

```rust
/// Whether anything still owns the control pipe.
///
/// `WaitNamedPipe` rather than `CreateFile`: it asks whether the name exists
/// without opening an instance, so probing cannot itself take the slot a
/// reconnecting supervisor wants.
fn pipe_exists() -> bool {
    use windows::Win32::System::Pipes::WaitNamedPipeW;
    use windows::core::HSTRING;
    // 1 ms, not zero: zero means "use the server's default timeout", which is
    // whatever the daemon chose and not what is wanted here.
    unsafe { WaitNamedPipeW(&HSTRING::from(PIPE_PATH), 1).is_ok() }
}

/// Last resort for a daemon that acknowledged `shutdown` and then wedged.
fn terminate(pid: u32) -> Result<(), String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, false, pid)
            .map_err(|e| format!("could not open the recorder process: {e}"))?;
        let result = TerminateProcess(handle, 1)
            .map_err(|e| format!("could not stop the recorder process: {e}"));
        let _ = CloseHandle(handle);
        result
    }
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p trix-ui`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/trix-ui/src/daemon.rs crates/trix-ui/Cargo.toml
git commit -m "feat(ui): let the supervisor stop the daemon, not just start it

Waits on the control pipe rather than on a process handle. The pipe is
bound FIRST_PIPE_INSTANCE, so the name resolving means some daemon owns it
and the name going away means the process is gone -- and that works when
Windows started the daemon at login and the supervisor holds no Child.
TerminateProcess is reachable only after the full 15 s budget, which is
sized past the ~10 s the capture stack takes to release so an honest slow
shutdown is never what gets killed."
```

---

### Task 4: Finding out whether an update exists

**Files:**
- Create: `crates/trix-ui/src/update/check.rs`
- Create: `crates/trix-ui/src/update/mod.rs` (stub — filled in Task 9)
- Modify: `crates/trix-ui/src/main.rs` (add `mod update;`)
- Modify: `crates/trix-ui/Cargo.toml`

**Interfaces:**
- Produces:
  - `pub struct Release { pub version: String, pub notes_url: String, pub zip_url: String, pub sums_url: String, pub size: u64 }`
  - `pub fn newer_release(body: &str, current: &str) -> Result<Option<Release>, String>` — pure, no network
  - `pub const RELEASE_API: &str = "https://api.github.com/repos/tnhnblgl/trix/releases/latest"`
  - `pub fn user_agent() -> String`

- [ ] **Step 1: Add the dependencies**

In `crates/trix-ui/Cargo.toml` under `[dependencies]`:

```toml
semver = "1"
```

- [ ] **Step 2: Write the failing tests**

Create `crates/trix-ui/src/update/check.rs` containing only the test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal `releases/latest` body. Real ones carry ~40 more fields; the
    /// parser must ignore all of them, so the fixture carries only what is read.
    fn body(tag: &str, assets: &str) -> String {
        format!(
            r#"{{"tag_name":"{tag}","html_url":"https://github.com/tnhnblgl/trix/releases/tag/{tag}","assets":[{assets}]}}"#
        )
    }

    fn asset(name: &str, size: u64) -> String {
        format!(
            r#"{{"name":"{name}","size":{size},"browser_download_url":"https://github.com/tnhnblgl/trix/releases/download/v0.5.0/{name}"}}"#
        )
    }

    fn full_release(tag: &str) -> String {
        body(tag, &format!("{},{}", asset("trix-v0.5.0-win-x64.zip", 3_400_000), asset("SHA256SUMS.txt", 96)))
    }

    #[test]
    fn a_newer_tag_is_an_update() {
        let found = newer_release(&full_release("v0.5.0"), "0.4.0").expect("parses");
        let release = found.expect("0.5.0 is newer than 0.4.0");
        assert_eq!(release.version, "0.5.0", "the leading v is not part of the version");
        assert_eq!(release.size, 3_400_000);
        assert!(release.zip_url.ends_with("trix-v0.5.0-win-x64.zip"));
        assert!(release.sums_url.ends_with("SHA256SUMS.txt"));
    }

    #[test]
    fn the_same_version_is_not_an_update() {
        assert!(newer_release(&full_release("v0.4.0"), "0.4.0").expect("parses").is_none());
    }

    /// Downgrades are never offered. A user who deliberately kept an older
    /// build must not be nagged to install one they already left, and a
    /// mistakenly re-pointed `latest` must not roll everybody backwards.
    #[test]
    fn an_older_tag_is_not_an_update() {
        assert!(newer_release(&full_release("v0.3.9"), "0.4.0").expect("parses").is_none());
    }

    #[test]
    fn a_tag_without_its_v_still_works() {
        assert!(newer_release(&full_release("0.5.0"), "0.4.0").expect("parses").is_some());
    }

    /// GitHub publishes the release before the attached files finish
    /// uploading, and the user creates releases by hand. A check landing in
    /// that window must read as "nothing to do" -- the alternative is a banner
    /// offering a download that does not exist, which is the one failure here
    /// the user would notice and could do nothing about.
    #[test]
    fn a_release_whose_assets_are_still_uploading_is_not_an_update() {
        assert!(newer_release(&body("v0.5.0", ""), "0.4.0").expect("parses").is_none());

        let zip_only = body("v0.5.0", &asset("trix-v0.5.0-win-x64.zip", 3_400_000));
        assert!(
            newer_release(&zip_only, "0.4.0").expect("parses").is_none(),
            "a zip with no SHA256SUMS.txt cannot be verified, so it is not offered"
        );
    }

    /// The asset name carries the version, so a mismatched pair means the
    /// release was assembled wrong. Offering it would download one version
    /// while promising another.
    #[test]
    fn an_asset_named_for_a_different_version_is_refused() {
        let wrong = body(
            "v0.5.0",
            &format!("{},{}", asset("trix-v0.4.9-win-x64.zip", 10), asset("SHA256SUMS.txt", 96)),
        );
        assert!(newer_release(&wrong, "0.4.0").expect("parses").is_none());
    }

    #[test]
    fn a_malformed_tag_is_an_error_not_a_silent_no() {
        assert!(newer_release(&full_release("nightly"), "0.4.0").is_err());
    }

    /// GitHub rejects API requests with no User-Agent.
    #[test]
    fn the_user_agent_names_the_product_and_version() {
        let ua = user_agent();
        assert!(ua.starts_with("trix/"), "GitHub refuses requests without a User-Agent: {ua}");
        assert!(ua.contains(env!("CARGO_PKG_VERSION")));
    }
}
```

- [ ] **Step 3: Run and watch it fail**

Add `mod update;` to `crates/trix-ui/src/main.rs` beside the other `mod` lines, and create `crates/trix-ui/src/update/mod.rs` with `pub mod check;`.

Run: `cargo test -p trix-ui a_newer_tag_is_an_update`
Expected: FAIL — `cannot find function 'newer_release'`.

- [ ] **Step 4: Implement**

Prepend to `crates/trix-ui/src/update/check.rs`:

```rust
//! Asking GitHub whether there is a newer Trix, and deciding whether the
//! answer is usable.
//!
//! Parsing is a free function over a `&str` so every rule below is testable
//! without a network, a server, or a fixture file. The one function that
//! touches the network does nothing but fetch the bytes.

use serde_json::Value;

/// `/releases/latest` rather than `/releases`: GitHub already excludes drafts
/// and prereleases from it, so a prerelease cannot reach users by accident.
pub const RELEASE_API: &str = "https://api.github.com/repos/tnhnblgl/trix/releases/latest";

/// GitHub rejects API requests that send no User-Agent.
pub fn user_agent() -> String {
    format!("trix/{}", env!("CARGO_PKG_VERSION"))
}

/// A release that is newer than what is running, and complete enough to install.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Release {
    pub version: String,
    pub notes_url: String,
    pub zip_url: String,
    pub sums_url: String,
    pub size: u64,
}

/// `Ok(None)` means "nothing to offer" and is not a problem: it covers the
/// common case (already current) and the awkward one (a release whose assets
/// are still uploading). `Err` is reserved for a body that could not be
/// understood at all, which is worth a log line.
pub fn newer_release(body: &str, current: &str) -> Result<Option<Release>, String> {
    let json: Value =
        serde_json::from_str(body).map_err(|e| format!("release feed was not JSON: {e}"))?;

    let tag = json.get("tag_name").and_then(Value::as_str).ok_or("release feed has no tag_name")?;
    let version = tag.strip_prefix('v').unwrap_or(tag);

    let latest = semver::Version::parse(version)
        .map_err(|e| format!("release tag {tag:?} is not a version: {e}"))?;
    let running = semver::Version::parse(current)
        .map_err(|e| format!("this build's own version {current:?} is not a version: {e}"))?;
    if latest <= running {
        return Ok(None);
    }

    let assets = json.get("assets").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    let zip_name = format!("trix-v{version}-win-x64.zip");

    // Both assets or neither. A zip with no digest beside it cannot be
    // verified, and an unverifiable download is not an update -- it is a
    // failure waiting to happen halfway through the swap.
    let Some(zip) = find_asset(assets, &zip_name) else { return Ok(None) };
    let Some(sums) = find_asset(assets, "SHA256SUMS.txt") else { return Ok(None) };

    Ok(Some(Release {
        version: version.to_string(),
        notes_url: json
            .get("html_url")
            .and_then(Value::as_str)
            .unwrap_or("https://github.com/tnhnblgl/trix/releases")
            .to_string(),
        zip_url: zip.0,
        sums_url: sums.0,
        size: zip.1,
    }))
}

/// `(browser_download_url, size)` for an asset with exactly this name.
fn find_asset(assets: &[Value], name: &str) -> Option<(String, u64)> {
    assets.iter().find(|a| a.get("name").and_then(Value::as_str) == Some(name)).map(|a| {
        (
            a.get("browser_download_url").and_then(Value::as_str).unwrap_or_default().to_string(),
            a.get("size").and_then(Value::as_u64).unwrap_or_default(),
        )
    })
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p trix-ui update::check`
Expected: PASS — eight tests.

- [ ] **Step 6: Commit**

```bash
git add crates/trix-ui/src/update/ crates/trix-ui/src/main.rs crates/trix-ui/Cargo.toml
git commit -m "feat(update): parse the release feed and decide what is newer

Every rule is a pure function over a string, so the awkward cases are tested
without a network: a release whose assets are still uploading (GitHub
publishes before the upload finishes, and releases here are made by hand),
an asset named for a different version than its tag, and a downgrade. All
three read as 'nothing to offer' rather than as errors, because a banner
offering a download that cannot be completed is the one failure a user
would notice and could do nothing about."
```

---

### Task 5: Refusing a download that is not what was promised

**Files:**
- Create: `crates/trix-ui/src/update/verify.rs`
- Modify: `crates/trix-ui/src/update/mod.rs` (add `pub mod verify;`)
- Modify: `crates/trix-ui/Cargo.toml`

**Interfaces:**
- Produces:
  - `pub fn digest_for(sums: &str, name: &str) -> Result<String, String>`
  - `pub fn sha256_file(path: &Path) -> Result<String, String>`
  - `pub fn check(path: &Path, sums: &str, name: &str) -> Result<(), String>`

This module is the seam the spec reserves for signature verification. Keep it the only place that decides whether a downloaded file may be installed.

- [ ] **Step 1: Add the dependency**

In `crates/trix-ui/Cargo.toml`: `sha2 = "0.10"`

- [ ] **Step 2: Write the failing tests**

Create `crates/trix-ui/src/update/verify.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// The format ship-zip.ps1 writes: lowercase hex, two spaces, filename.
    const SUMS: &str = "\
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  trix-v0.5.0-win-x64.zip
0000000000000000000000000000000000000000000000000000000000000000  SHA256SUMS.txt
";

    #[test]
    fn the_digest_is_found_by_filename() {
        assert_eq!(
            digest_for(SUMS, "trix-v0.5.0-win-x64.zip").expect("present"),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    /// Not "assume it is fine". A sums file with no line for this asset means
    /// the release was assembled wrong, and installing anyway is exactly the
    /// case this module exists to prevent.
    #[test]
    fn a_missing_line_is_a_refusal() {
        assert!(digest_for(SUMS, "trix-v9.9.9-win-x64.zip").is_err());
    }

    #[test]
    fn case_and_stray_whitespace_do_not_matter() {
        let shouty = "E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855  a.zip\n";
        assert_eq!(
            digest_for(shouty, "a.zip").expect("present"),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "comparison is on lowercase hex, so the file's casing cannot cause a false mismatch"
        );
    }

    #[test]
    fn an_empty_file_hashes_to_the_known_empty_digest() {
        let path = std::env::temp_dir().join(format!("trix-verify-empty-{}", std::process::id()));
        std::fs::write(&path, b"").expect("write");
        assert_eq!(
            sha256_file(&path).expect("hashes"),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert!(check(&path, SUMS, "trix-v0.5.0-win-x64.zip").is_ok());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_file_that_does_not_match_is_refused_by_name() {
        let path = std::env::temp_dir().join(format!("trix-verify-bad-{}", std::process::id()));
        std::fs::write(&path, b"not empty").expect("write");
        let error = check(&path, SUMS, "trix-v0.5.0-win-x64.zip").expect_err("must refuse");
        assert!(
            error.contains("trix-v0.5.0-win-x64.zip"),
            "the message has to say which file failed: {error}"
        );
        let _ = std::fs::remove_file(&path);
    }
}
```

- [ ] **Step 3: Run and watch it fail**

Add `pub mod verify;` to `crates/trix-ui/src/update/mod.rs`.

Run: `cargo test -p trix-ui update::verify`
Expected: FAIL — `cannot find function 'digest_for'`.

- [ ] **Step 4: Implement**

Prepend to `crates/trix-ui/src/update/verify.rs`:

```rust
//! The one place that decides whether a downloaded file may be installed.
//!
//! Today that decision is a SHA-256 published beside the asset, which catches
//! a corrupt or truncated download and a release assembled wrong. It does not
//! catch a hostile release: whoever can publish the zip can publish the digest.
//! The design accepts that (the GitHub account is the trust anchor) and keeps
//! this module separate so a signature check can be added here, alone, without
//! touching the download or the swap.

use std::path::Path;

use sha2::{Digest as _, Sha256};

/// The digest `SHA256SUMS.txt` publishes for `name`.
///
/// Lines are `<64 hex>  <filename>`, which is what `Get-FileHash` piped through
/// `ship-zip.ps1` produces and what `sha256sum` reads.
pub fn digest_for(sums: &str, name: &str) -> Result<String, String> {
    sums.lines()
        .filter_map(|line| line.split_once("  ").or_else(|| line.split_once(' ')))
        .find(|(_, file)| file.trim() == name)
        .map(|(digest, _)| digest.trim().to_ascii_lowercase())
        .ok_or_else(|| format!("the release does not publish a checksum for {name}"))
}

/// Streams the file rather than reading it into memory. The zip is a few
/// megabytes today, but a hashing function that must not be the reason a
/// machine with little RAM fails is worth writing once.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| format!("could not read the downloaded file: {e}"))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)
        .map_err(|e| format!("could not read the downloaded file: {e}"))?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// `Ok` only when `path` is exactly what the release says `name` should be.
pub fn check(path: &Path, sums: &str, name: &str) -> Result<(), String> {
    let expected = digest_for(sums, name)?;
    let actual = sha256_file(path)?;
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "{name} did not match the checksum GitHub published for it, so it was not installed. \
             Download it by hand from the releases page if this keeps happening."
        ))
    }
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p trix-ui update::verify`
Expected: PASS — five tests.

- [ ] **Step 6: Commit**

```bash
git add crates/trix-ui/src/update/verify.rs crates/trix-ui/src/update/mod.rs crates/trix-ui/Cargo.toml
git commit -m "feat(update): verify a download against the published checksum

Kept as its own module because it is the only place that says yes or no to
installing bytes, and because the design leaves room to add a signature
check here later without disturbing anything around it. A missing line in
SHA256SUMS.txt is a refusal rather than a shrug: it means the release was
assembled wrong, which is precisely what this is for."
```

---

### Task 6: Fetching, without fetching from anywhere else

**Files:**
- Create: `crates/trix-ui/src/update/download.rs`
- Modify: `crates/trix-ui/src/update/mod.rs` (add `pub mod download;`)
- Modify: `crates/trix-ui/Cargo.toml`

**Interfaces:**
- Consumes: `check::user_agent()`.
- Produces:
  - `pub fn allowed(url: &str) -> Result<(), String>`
  - `pub const MAX_BYTES: u64 = 64 * 1024 * 1024`
  - `pub fn fetch_text(url: &str) -> Result<String, String>`
  - `pub fn fetch_to_file(url: &str, dest: &Path, progress: &dyn Fn(u64, u64)) -> Result<(), String>`

- [ ] **Step 1: Pin the HTTP client and confirm its API**

In `crates/trix-ui/Cargo.toml`:

```toml
ureq = { version = "3", default-features = false, features = ["native-tls"] }
```

`native-tls` rather than `rustls`: on Windows it resolves to schannel and uses the certificate store the OS already maintains, which is both smaller and correct on machines with a corporate trust root.

Run: `cargo build -p trix-ui`
Expected: PASS. If the crate fails to resolve the feature, read the installed crate's own docs to find the current spelling — `ls ~/.cargo/registry/src/*/ureq-3*/Cargo.toml` lists the real feature names — and adjust the feature and the two call sites in Step 4 accordingly. The two calls used are: build a request with a header and call it, and read the response body as a reader. Nothing exotic.

- [ ] **Step 2: Write the failing tests**

Create `crates/trix-ui/src/update/download.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_hosts_the_release_flow_uses_are_allowed() {
        // api.github.com answers the feed; browser_download_url points at
        // github.com and redirects to objects.githubusercontent.com.
        assert!(allowed("https://api.github.com/repos/tnhnblgl/trix/releases/latest").is_ok());
        assert!(allowed("https://github.com/tnhnblgl/trix/releases/download/v0.5.0/a.zip").is_ok());
        assert!(allowed("https://objects.githubusercontent.com/github-production-release/1/2").is_ok());
    }

    /// A suffix match would accept every one of these. The check is on the
    /// host component, whole and exact.
    #[test]
    fn lookalike_hosts_are_refused() {
        for url in [
            "https://github.com.evil.example/x.zip",
            "https://evil.example/github.com/x.zip",
            "https://notgithub.com/x.zip",
            "https://api.github.com.evil.example/x",
        ] {
            assert!(allowed(url).is_err(), "{url} must not be reachable");
        }
    }

    /// TLS is not optional: plain HTTP would let anyone on the path swap the
    /// zip, and the checksum with it.
    #[test]
    fn plain_http_is_refused_even_to_an_allowed_host() {
        assert!(allowed("http://github.com/tnhnblgl/trix/releases/download/v0.5.0/a.zip").is_err());
    }

    #[test]
    fn a_url_that_is_not_a_url_is_refused() {
        assert!(allowed("not a url").is_err());
        assert!(allowed("file:///C:/Windows/System32/cmd.exe").is_err());
    }

    /// The product is ~3.4 MB. The bound exists so a response that never ends
    /// cannot fill the user's disk, not to be a tight fit.
    #[test]
    fn the_size_bound_is_generous_but_finite() {
        assert!(MAX_BYTES > 32 * 1024 * 1024, "a real release must fit with room to grow");
        assert!(MAX_BYTES <= 128 * 1024 * 1024, "but it must actually bound something");
    }
}
```

- [ ] **Step 3: Run and watch it fail**

Add `pub mod download;` to `crates/trix-ui/src/update/mod.rs`.

Run: `cargo test -p trix-ui update::download`
Expected: FAIL — `cannot find function 'allowed'`.

- [ ] **Step 4: Implement**

Prepend to `crates/trix-ui/src/update/download.rs`:

```rust
//! Getting bytes from GitHub, and from nowhere else.
//!
//! Trix made no outbound connection at all before this module. Everything here
//! is deliberately narrow: three hosts, TLS only, a size ceiling, and no
//! request body ever. There is nothing to send -- no identifier, no telemetry,
//! no clip metadata -- and keeping that true is easier when the only function
//! that can reach the network is this one.

use std::io::{Read as _, Write as _};
use std::path::Path;

use super::check::user_agent;

/// Exactly these, matched on the whole host. `github.com` serves
/// `browser_download_url` and redirects to `objects.githubusercontent.com`;
/// `api.github.com` answers the release feed.
const ALLOWED_HOSTS: [&str; 3] = ["api.github.com", "github.com", "objects.githubusercontent.com"];

/// The release zip is about 3.4 MB. This is not a tight bound -- it exists so a
/// response that never ends cannot fill the user's disk while a progress bar
/// climbs forever.
pub const MAX_BYTES: u64 = 64 * 1024 * 1024;

/// How much is read per iteration. Large enough that the syscall cost is
/// invisible, small enough that progress moves smoothly on a slow line.
const CHUNK: usize = 64 * 1024;

/// Refuses anything that is not HTTPS to one of [`ALLOWED_HOSTS`].
///
/// Applied to redirect targets too, not only to the URL the release feed
/// named: a redirect is a URL somebody else chose, and the whole point of an
/// allowlist is that it holds for URLs this code did not write.
pub fn allowed(url: &str) -> Result<(), String> {
    let rest = url.strip_prefix("https://").ok_or("updates are only fetched over HTTPS")?;
    // The host ends at the first '/', '?' or '#'. Splitting on all three
    // matters: `https://github.com?x=@evil` has no slash at all.
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    // Strip userinfo, which is the classic way to make a URL read as one host
    // and resolve as another: `https://github.com@evil.example/`.
    let host = host.rsplit('@').next().unwrap_or_default();
    let host = host.split(':').next().unwrap_or_default().to_ascii_lowercase();

    if ALLOWED_HOSTS.contains(&host.as_str()) {
        Ok(())
    } else {
        Err(format!("updates are not fetched from {host:?}"))
    }
}

/// For the release feed and `SHA256SUMS.txt`, both small.
pub fn fetch_text(url: &str) -> Result<String, String> {
    allowed(url)?;
    let mut response = ureq::get(url)
        .header("User-Agent", &user_agent())
        .call()
        .map_err(|e| format!("could not reach GitHub: {e}"))?;
    response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("could not read GitHub's answer: {e}"))
}

/// Streams `url` to `dest`, calling `progress(received, total)` as it goes.
///
/// Written to a file rather than held in memory, and bounded, so neither a
/// large release nor an endless response is a memory problem.
pub fn fetch_to_file(url: &str, dest: &Path, progress: &dyn Fn(u64, u64)) -> Result<(), String> {
    allowed(url)?;
    let mut response = ureq::get(url)
        .header("User-Agent", &user_agent())
        .call()
        .map_err(|e| format!("could not download the update: {e}"))?;

    let total: u64 = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if total > MAX_BYTES {
        return Err(format!("the download is {total} bytes, which is larger than Trix expects"));
    }

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create the update folder: {e}"))?;
    }
    let mut file = std::fs::File::create(dest)
        .map_err(|e| format!("could not write the downloaded update: {e}"))?;

    let mut reader = response.body_mut().as_reader();
    let mut buffer = vec![0u8; CHUNK];
    let mut received: u64 = 0;
    loop {
        let read = reader.read(&mut buffer).map_err(|e| format!("the download stopped: {e}"))?;
        if read == 0 {
            break;
        }
        received += read as u64;
        // Checked against the bound as it arrives, not only against
        // Content-Length: a server is free to send more than it declared, or
        // to declare nothing at all.
        if received > MAX_BYTES {
            let _ = std::fs::remove_file(dest);
            return Err("the download kept going past the size Trix expects".into());
        }
        file.write_all(&buffer[..read])
            .map_err(|e| format!("could not write the downloaded update: {e}"))?;
        progress(received, total.max(received));
    }
    file.flush().map_err(|e| format!("could not finish writing the update: {e}"))?;
    Ok(())
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p trix-ui update::download`
Expected: PASS — five tests.

- [ ] **Step 6: Commit**

```bash
git add crates/trix-ui/src/update/download.rs crates/trix-ui/src/update/mod.rs crates/trix-ui/Cargo.toml
git commit -m "feat(update): fetch from GitHub, and from nowhere else

Trix made no outbound connection at all before this. The allowlist matches
the whole host rather than a suffix, strips userinfo, and applies to
redirect targets -- release downloads redirect to object storage, so the
URL that actually gets fetched is one this code did not write. The size
ceiling is checked as bytes arrive as well as against Content-Length,
because a server may send more than it declared."
```

---

### Task 7: The swap

The highest-risk task: it is the one that can leave a user with no working install. Everything here is tested against scratch directories, and the rollback is tested by injecting a failure at each step.

**Files:**
- Create: `crates/trix-ui/src/update/swap.rs`
- Modify: `crates/trix-ui/src/update/mod.rs` (add `pub mod swap;`)
- Modify: `crates/trix-ui/Cargo.toml`

**Interfaces:**
- Produces:
  - `pub const BINARIES: [&str; 3]`, `pub const DOCS: [&str; 2]`, `pub const STAGING: &str = ".trix-update"`, `pub const OLD_SUFFIX: &str = ".old"`
  - `pub fn install_dir() -> Result<PathBuf, String>`
  - `pub fn writable(dir: &Path) -> Result<(), String>`
  - `pub fn unpack(zip: &Path, into: &Path) -> Result<PathBuf, String>` — returns the payload directory
  - `pub fn swap_in(install: &Path, payload: &Path) -> Result<(), String>`
  - `pub fn cleanup(install: &Path)`

- [ ] **Step 1: Add the dependency**

In `crates/trix-ui/Cargo.toml`:

```toml
zip = { version = "2", default-features = false, features = ["deflate"] }
```

`default-features = false` drops bzip2, zstd, and AES, none of which a `ZipFile::CreateFromDirectory` archive uses.

- [ ] **Step 2: Write the failing tests**

Create `crates/trix-ui/src/update/swap.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory of this test's own. Never the real install: this
    /// module renames and deletes executables, and pointing it at a developer's
    /// own folder is the one mistake here that cannot be undone by rerunning.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("trix-swap-{name}-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn install_with(dir: &Path, marker: &str) {
        for name in BINARIES.iter().chain(DOCS.iter()) {
            std::fs::write(dir.join(name), format!("{marker} {name}")).expect("write");
        }
    }

    fn contents(dir: &Path, name: &str) -> String {
        std::fs::read_to_string(dir.join(name)).unwrap_or_default()
    }

    #[test]
    fn a_clean_swap_replaces_every_shipped_file_and_keeps_the_old_binaries() {
        let install = scratch("clean");
        install_with(&install, "old");
        let payload = install.join(STAGING).join("staged");
        std::fs::create_dir_all(&payload).expect("payload");
        install_with(&payload, "new");

        swap_in(&install, &payload).expect("swap");

        for name in BINARIES {
            assert_eq!(contents(&install, name), format!("new {name}"), "{name} was not replaced");
            assert_eq!(
                contents(&install, &format!("{name}{OLD_SUFFIX}")),
                format!("old {name}"),
                "{name} must be recoverable until the new build has started"
            );
        }
        for name in DOCS {
            assert_eq!(contents(&install, name), format!("new {name}"));
        }
        let _ = std::fs::remove_dir_all(&install);
    }

    /// Nothing else in the folder is touched. Users unzip Trix into folders
    /// that already contain their own things.
    #[test]
    fn files_trix_did_not_ship_are_left_alone() {
        let install = scratch("bystanders");
        install_with(&install, "old");
        std::fs::write(install.join("my-notes.txt"), "mine").expect("write");
        std::fs::create_dir_all(install.join("clips")).expect("dir");
        let payload = install.join(STAGING).join("staged");
        std::fs::create_dir_all(&payload).expect("payload");
        install_with(&payload, "new");

        swap_in(&install, &payload).expect("swap");

        assert_eq!(contents(&install, "my-notes.txt"), "mine");
        assert!(install.join("clips").is_dir());
        let _ = std::fs::remove_dir_all(&install);
    }

    /// The failure that matters. A payload missing one binary must leave the
    /// install exactly as it was -- not two-thirds updated.
    #[test]
    fn an_incomplete_payload_rolls_every_file_back() {
        let install = scratch("rollback");
        install_with(&install, "old");
        let payload = install.join(STAGING).join("staged");
        std::fs::create_dir_all(&payload).expect("payload");
        install_with(&payload, "new");
        // trix-ui.exe is swapped last, so removing it fails the swap after the
        // first two have already been replaced.
        std::fs::remove_file(payload.join("trix-ui.exe")).expect("remove");

        let error = swap_in(&install, &payload).expect_err("an incomplete payload must fail");
        assert!(error.contains("trix-ui.exe"), "the message must name the file: {error}");

        for name in BINARIES {
            assert_eq!(
                contents(&install, name),
                format!("old {name}"),
                "{name} must be back exactly as it was"
            );
            assert!(
                !install.join(format!("{name}{OLD_SUFFIX}")).exists(),
                "a rolled-back swap must leave no .old files behind"
            );
        }
        let _ = std::fs::remove_dir_all(&install);
    }

    #[test]
    fn cleanup_removes_the_old_binaries_and_the_staging_folder() {
        let install = scratch("cleanup");
        install_with(&install, "new");
        for name in BINARIES {
            std::fs::write(install.join(format!("{name}{OLD_SUFFIX}")), "old").expect("write");
        }
        std::fs::create_dir_all(install.join(STAGING).join("download")).expect("dir");
        std::fs::write(install.join("my-notes.txt"), "mine").expect("write");

        cleanup(&install);

        for name in BINARIES {
            assert!(!install.join(format!("{name}{OLD_SUFFIX}")).exists());
            assert_eq!(contents(&install, name), format!("new {name}"), "the live build must survive");
        }
        assert!(!install.join(STAGING).exists());
        assert_eq!(contents(&install, "my-notes.txt"), "mine");
        let _ = std::fs::remove_dir_all(&install);
    }

    /// ship-zip.ps1 packs with includeBaseDirectory, so the archive holds one
    /// top-level folder. Assuming a flat layout would look for the binaries in
    /// the wrong place every single time.
    #[test]
    fn the_payload_is_the_single_directory_inside_the_extraction() {
        let extracted = scratch("payload");
        let inner = extracted.join("trix-v0.5.0-win-x64");
        std::fs::create_dir_all(&inner).expect("dir");
        install_with(&inner, "new");

        assert_eq!(payload_root(&extracted).expect("resolves"), inner);
        let _ = std::fs::remove_dir_all(&extracted);
    }

    #[test]
    fn a_payload_without_the_three_binaries_is_refused() {
        let extracted = scratch("bad-payload");
        let inner = extracted.join("something-else");
        std::fs::create_dir_all(&inner).expect("dir");
        std::fs::write(inner.join("readme.md"), "not trix").expect("write");

        assert!(payload_root(&extracted).is_err());
        let _ = std::fs::remove_dir_all(&extracted);
    }

    #[test]
    fn a_writable_directory_is_recognised_and_the_probe_leaves_nothing() {
        let dir = scratch("writable");
        writable(&dir).expect("a temp dir is writable");
        assert_eq!(std::fs::read_dir(&dir).expect("read").count(), 0, "the probe must clean up");
        assert!(writable(Path::new(r"C:\Windows\System32\__trix_nope__")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
```

- [ ] **Step 3: Run and watch it fail**

Add `pub mod swap;` to `crates/trix-ui/src/update/mod.rs`.

Run: `cargo test -p trix-ui update::swap`
Expected: FAIL — `cannot find function 'swap_in'`.

- [ ] **Step 4: Implement**

Prepend to `crates/trix-ui/src/update/swap.rs`:

```rust
//! Replacing the running program with a newer copy of itself.
//!
//! Windows will not let a running executable be deleted or overwritten, but it
//! will let one be *renamed*. That single fact is the whole mechanism:
//! `trix-ui.exe` renames itself to `trix-ui.exe.old` while executing from the
//! renamed file, and the new build is moved into the name it just vacated.
//!
//! Everything here is ordered so that the install is never in a state that
//! cannot be put back. Each completed rename is recorded, and any failure
//! undoes them in reverse before returning.

use std::path::{Path, PathBuf};

/// The three files that are renamed rather than overwritten, in swap order.
/// `trix-ui.exe` is last because it is the one doing the swapping -- if
/// anything is going to fail, it should fail before the running program has
/// moved itself.
pub const BINARIES: [&str; 3] = ["trix.exe", "trix-daemon.exe", "trix-ui.exe"];

/// Shipped alongside, never locked, so these are simply overwritten.
pub const DOCS: [&str; 2] = ["LICENSE", "README.txt"];

/// Staging lives inside the install directory, not in `%TEMP%`.
///
/// A rename is only cheap and near-atomic *within a volume*. A portable Trix on
/// `D:\` with `%TEMP%` on `C:\` would turn every move below into a copy, with a
/// correspondingly wider window in which to fail halfway.
pub const STAGING: &str = ".trix-update";

/// Suffix for the outgoing build, deleted on the next launch.
pub const OLD_SUFFIX: &str = ".old";

/// The folder Trix is installed in — where this executable lives.
pub fn install_dir() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("could not locate trix-ui.exe: {e}"))?;
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "could not work out which folder Trix is installed in".to_string())
}

/// Whether files can be created here, established by creating one.
///
/// Asked before anything is downloaded rather than discovered mid-swap. A
/// portable app dropped into `C:\Program Files` is not writable without
/// elevation, and finding that out after two of three binaries have moved is
/// how an install gets destroyed.
pub fn writable(dir: &Path) -> Result<(), String> {
    let probe = dir.join(".trix-write-probe");
    std::fs::write(&probe, b"trix").map_err(|_| {
        format!(
            "Trix cannot update itself in {}, because it does not have permission to write there. \
             Move the Trix folder somewhere like your Documents, or download the update by hand.",
            dir.display()
        )
    })?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

/// Extracts `zip` under `into` and returns the directory holding the binaries.
pub fn unpack(zip: &Path, into: &Path) -> Result<PathBuf, String> {
    let file = std::fs::File::open(zip).map_err(|e| format!("could not open the update: {e}"))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("the update is not a valid zip: {e}"))?;
    // `extract` validates each entry's CRC-32 as it writes, so a corrupt
    // member is caught here rather than by whatever tries to run it.
    archive.extract(into).map_err(|e| format!("could not unpack the update: {e}"))?;
    payload_root(into)
}

/// Finds the directory that actually holds the binaries.
///
/// `ship-zip.ps1` packs with `includeBaseDirectory: true`, so the archive holds
/// exactly one top-level folder (`trix-v0.5.0-win-x64`) rather than a flat set
/// of files. This resolves it by looking for the binaries instead of by
/// rebuilding the expected name, so a rename of the zip's inner folder does not
/// break the updater.
fn payload_root(extracted: &Path) -> Result<PathBuf, String> {
    let complete = |dir: &Path| BINARIES.iter().all(|name| dir.join(name).is_file());
    if complete(extracted) {
        return Ok(extracted.to_path_buf());
    }
    let entries = std::fs::read_dir(extracted)
        .map_err(|e| format!("could not read the unpacked update: {e}"))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && complete(&path) {
            return Ok(path);
        }
    }
    Err("the downloaded update does not contain the Trix programs".to_string())
}

/// Moves the payload into place, or puts everything back.
pub fn swap_in(install: &Path, payload: &Path) -> Result<(), String> {
    // Checked before the first rename: a payload missing a file must fail
    // while the install is still untouched, not two-thirds of the way through.
    for name in BINARIES {
        if !payload.join(name).is_file() {
            return Err(format!("the downloaded update is missing {name}, so nothing was changed"));
        }
    }

    let mut moved: Vec<&str> = Vec::new();
    for name in BINARIES {
        let live = install.join(name);
        let old = install.join(format!("{name}{OLD_SUFFIX}"));
        // A leftover from an update whose cleanup could not run. Removing it
        // now is safe: nothing has been renamed onto it yet this run.
        let _ = std::fs::remove_file(&old);

        if let Err(e) = std::fs::rename(&live, &old) {
            roll_back(install, &moved);
            return Err(format!("could not move the old {name} aside: {e}"));
        }
        if let Err(e) = std::fs::rename(payload.join(name), &live) {
            // Undo this file's own half-done step first, then the rest.
            let _ = std::fs::rename(&old, &live);
            roll_back(install, &moved);
            return Err(format!("could not put the new {name} in place: {e}"));
        }
        moved.push(name);
    }

    // Not renamed, so not rolled back: these are never locked, and a stale
    // README beside a correct set of binaries is cosmetic. Failing the whole
    // update over one would be worse than the inconsistency.
    for name in DOCS {
        let from = payload.join(name);
        if from.is_file() {
            let _ = std::fs::copy(&from, install.join(name));
        }
    }
    Ok(())
}

/// Puts the named binaries back the way they were.
///
/// Failures here are logged rather than returned: the caller is already
/// reporting the original error, and a rollback failure changes what the user
/// must do, not what went wrong. `mod.rs` names the leftover `.old` files in
/// its message so the user can finish by hand.
fn roll_back(install: &Path, moved: &[&str]) {
    for name in moved.iter().rev() {
        let live = install.join(name);
        let old = install.join(format!("{name}{OLD_SUFFIX}"));
        if let Err(e) = std::fs::remove_file(&live) {
            tracing::warn!(file = name, error = %e, "could not remove the new file during rollback");
        }
        if let Err(e) = std::fs::rename(&old, &live) {
            tracing::warn!(file = name, error = %e, "could not restore the old file during rollback");
        }
    }
}

/// Deletes the previous build and the staging folder.
///
/// Called at startup, when nothing holds them open. Every failure is ignored on
/// purpose: a leftover `trix-ui.exe.old` is litter, not a fault, and the next
/// launch tries again. Reporting it would be an error message about a file the
/// user never knew existed.
pub fn cleanup(install: &Path) {
    for name in BINARIES {
        let _ = std::fs::remove_file(install.join(format!("{name}{OLD_SUFFIX}")));
    }
    let _ = std::fs::remove_dir_all(install.join(STAGING));
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p trix-ui update::swap`
Expected: PASS — seven tests.

- [ ] **Step 6: Commit**

```bash
git add crates/trix-ui/src/update/swap.rs crates/trix-ui/src/update/mod.rs crates/trix-ui/Cargo.toml
git commit -m "feat(update): replace the running binaries, or put them all back

Windows forbids overwriting a running exe but permits renaming one, which is
the entire mechanism: trix-ui.exe renames itself aside while executing from
the renamed file. The payload is checked complete before the first rename,
and every completed rename is undone in reverse on failure, so an install is
never left two-thirds updated. Staging sits inside the install folder rather
than %TEMP% because a rename is only near-atomic within a volume, and a
portable copy on D:\ would otherwise copy across volumes mid-swap."
```

---

### Task 8: `trix.exe restart-ui`

**Files:**
- Modify: `crates/trix-cli/src/main.rs` (`Command` enum at line 24, `main` at line 60)

**Interfaces:**
- Produces: `trix.exe restart-ui --wait-pid <PID>` — waits for that process to exit, then starts `trix-ui.exe` from beside itself.

- [ ] **Step 1: Add the subcommand**

In `crates/trix-cli/src/main.rs`, in `enum Command`, after `Replay { .. }`:

```rust
    /// Wait for a process to exit, then start trix-ui.exe from beside this exe
    ///
    /// Used by the updater. The app cannot relaunch itself directly: the new
    /// process would start while the old one is still alive, and
    /// tauri-plugin-single-instance would hand it to the dying instance and
    /// exit, leaving nothing running.
    #[command(hide = true)]
    RestartUi {
        /// The process to wait for -- the trix-ui.exe that is updating
        #[arg(long, value_name = "PID")]
        wait_pid: u32,
    },
```

- [ ] **Step 2: Write the failing test**

At the bottom of `crates/trix-cli/src/main.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// The updater spawns this by name; a rename or a changed flag breaks the
    /// relaunch silently, leaving the user with an updated install and no
    /// running app.
    #[test]
    fn restart_ui_parses_the_flag_the_updater_sends() {
        let cli = Cli::try_parse_from(["trix", "restart-ui", "--wait-pid", "4321"]).expect("parses");
        assert!(matches!(cli.command, Command::RestartUi { wait_pid: 4321 }));
    }

    /// Hidden from --help: it is machinery, not a feature, and a user who runs
    /// it by hand gets a process that waits for a PID that is not there.
    #[test]
    fn restart_ui_is_hidden_from_help() {
        let help = Cli::command().render_help().to_string();
        assert!(!help.contains("restart-ui"), "restart-ui must not appear in --help");
    }
}
```

Add `use clap::CommandFactory as _;` inside the test module if `Cli::command()` does not resolve.

- [ ] **Step 3: Run and watch it fail**

Run: `cargo test -p trix-cli restart_ui`
Expected: FAIL — no `RestartUi` variant, or a non-exhaustive match in `main`.

- [ ] **Step 4: Implement the handler**

In `main`'s match over `cli.command`, add an arm:

```rust
        Command::RestartUi { wait_pid } => restart_ui(wait_pid),
```

and the function:

```rust
/// Waits for `pid` to exit, then starts `trix-ui.exe` from beside this binary.
///
/// The wait is on a real process handle rather than a poll, so there is no
/// window in which the new app starts while the old one still holds the
/// single-instance lock. `OpenProcess` failing means the process is already
/// gone, which is success, not an error.
fn restart_ui(pid: u32) -> Result<()> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        INFINITE, OpenProcess, SYNCHRONIZE, WaitForSingleObject,
    };

    unsafe {
        if let Ok(handle) = OpenProcess(SYNCHRONIZE, false, pid) {
            // INFINITE is safe here: the process being waited on is the one
            // that spawned this, and it exits immediately after doing so. If
            // it somehow never exits, the user still has a working install --
            // they just have to start Trix themselves.
            WaitForSingleObject(handle, INFINITE);
            let _ = CloseHandle(handle);
        }
    }

    let exe = std::env::current_exe().context("could not locate trix.exe")?;
    let ui = exe
        .parent()
        .map(|dir| dir.join("trix-ui.exe"))
        .context("could not work out where trix-ui.exe is")?;
    std::process::Command::new(&ui)
        .spawn()
        .with_context(|| format!("could not start {}", ui.display()))?;
    Ok(())
}
```

`trix-cli` already depends on `anyhow` (`use anyhow::Result` at line 5); add `use anyhow::Context as _;` at the top if it is not already imported. Add `windows.workspace = true` to `crates/trix-cli/Cargo.toml` under `[dependencies]` if it is not already there.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p trix-cli`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/trix-cli/src/main.rs crates/trix-cli/Cargo.toml
git commit -m "feat(cli): add the hidden restart-ui helper

The app cannot relaunch itself after an update: the new process would start
while the old one is still alive, and single-instance would hand it to the
dying instance and exit, leaving nothing running. trix.exe is already beside
it, is not running during the swap, and waits on a real process handle
rather than sleeping and hoping."
```

---

### Task 9: Wiring it together

**Files:**
- Modify: `crates/trix-ui/src/update/mod.rs`
- Modify: `crates/trix-ui/src/main.rs`

**Interfaces:**
- Consumes: everything from Tasks 3–8.
- Produces Tauri commands: `update_check() -> Result<Option<Release>, String>`, `update_install(release: Release) -> Result<(), String>`, `update_current_version() -> String`. Emits `trix-update` events carrying `UpdateState`.

- [ ] **Step 1: Write the failing test**

In `crates/trix-ui/src/update/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// The frontend switches on `state`, so these strings are a contract with
    /// the webview, not debug output. Renaming one silently stops a banner
    /// rendering.
    #[test]
    fn every_state_serialises_under_the_name_the_frontend_matches() {
        let cases = [
            (UpdateState::Checking, "checking"),
            (UpdateState::UpToDate, "up-to-date"),
            (UpdateState::Downloading { received: 1, total: 2 }, "downloading"),
            (UpdateState::Verifying, "verifying"),
            (UpdateState::Installing, "installing"),
            (UpdateState::Restarting, "restarting"),
            (UpdateState::Failed { message: "x".into() }, "failed"),
        ];
        for (state, name) in cases {
            let json = serde_json::to_value(&state).expect("serialises");
            assert_eq!(json.get("state").and_then(|v| v.as_str()), Some(name));
        }
    }

    #[test]
    fn downloading_carries_both_numbers_the_progress_bar_needs() {
        let json = serde_json::to_value(UpdateState::Downloading { received: 40, total: 100 })
            .expect("serialises");
        assert_eq!(json.get("received").and_then(|v| v.as_u64()), Some(40));
        assert_eq!(json.get("total").and_then(|v| v.as_u64()), Some(100));
    }
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `cargo test -p trix-ui update::tests`
Expected: FAIL — `cannot find type 'UpdateState'`.

- [ ] **Step 3: Implement**

Replace `crates/trix-ui/src/update/mod.rs` with:

```rust
//! In-app updates: check, download, verify, swap, restart.
//!
//! The check is automatic; the install never is. Trix's job is to be running,
//! armed and invisible while somebody plays a game, and a background task that
//! stopped the recorder and replaced three executables would do it exactly when
//! the user could least tolerate it. Every entry point here is something the
//! user asked for, and every failure leaves the previous build in place.

pub mod check;
pub mod download;
pub mod swap;
pub mod verify;

use std::sync::Arc;

use tauri::{AppHandle, Emitter as _, State};

use crate::daemon::Supervisor;
use check::Release;

/// What the frontend renders. `state` is the tag it switches on.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum UpdateState {
    Checking,
    UpToDate,
    Available { release: Release },
    Downloading { received: u64, total: u64 },
    Verifying,
    Installing,
    Restarting,
    Failed { message: String },
}

fn emit(app: &AppHandle, state: &UpdateState) {
    let _ = app.emit("trix-update", state);
}

/// Deletes the previous build. Called once at startup, before the window opens.
pub fn clean_up_after_update() {
    if let Ok(dir) = swap::install_dir() {
        swap::cleanup(&dir);
    }
}

#[tauri::command]
pub fn update_current_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// `Ok(None)` is the common answer and is not shown to the user unless they
/// asked. A failed check returns `Err`, which the frontend shows only for a
/// manual "Check now" -- an automatic check that fails is logged and nothing
/// else, because the user did not ask to check for updates, they asked to open
/// a clip recorder.
#[tauri::command]
pub async fn update_check(app: AppHandle) -> Result<Option<Release>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        emit(&app, &UpdateState::Checking);
        let body = download::fetch_text(check::RELEASE_API)?;
        let found = check::newer_release(&body, env!("CARGO_PKG_VERSION"))?;
        match &found {
            Some(release) => emit(&app, &UpdateState::Available { release: release.clone() }),
            None => emit(&app, &UpdateState::UpToDate),
        }
        Ok(found)
    })
    .await
    .map_err(|e| format!("the update check could not be scheduled: {e}"))?
}

/// Downloads, verifies, stops the daemon, swaps, and restarts.
///
/// Returns only on failure: on success the process is replaced by a newly
/// spawned one and this one exits.
#[tauri::command]
pub async fn update_install(
    app: AppHandle,
    supervisor: State<'_, Arc<Supervisor>>,
    release: Release,
) -> Result<(), String> {
    let supervisor = Arc::clone(&supervisor);
    let result = tauri::async_runtime::spawn_blocking(move || install(&app, &supervisor, &release))
        .await
        .map_err(|e| format!("the update could not be scheduled: {e}"))?;
    result
}

fn install(app: &AppHandle, supervisor: &Supervisor, release: &Release) -> Result<(), String> {
    // Preflight, in the order that costs least to refuse. Recording first: a
    // user mid-session must be told to stop, not have the recorder pulled out
    // from under them.
    let status = supervisor.call("status", serde_json::Map::new())?;
    if status.get("armed").and_then(serde_json::Value::as_bool) == Some(true) {
        return Err("Stop recording before updating Trix.".into());
    }

    let dir = swap::install_dir()?;
    swap::writable(&dir)?;

    let staging = dir.join(swap::STAGING);
    // A staging folder from an update that failed before cleanup ran.
    let _ = std::fs::remove_dir_all(&staging);

    let zip_name = format!("trix-v{}-win-x64.zip", release.version);
    let zip_path = staging.join("download").join(&zip_name);

    let progress_app = app.clone();
    download::fetch_to_file(&release.zip_url, &zip_path, &move |received, total| {
        emit(&progress_app, &UpdateState::Downloading { received, total });
    })?;

    emit(app, &UpdateState::Verifying);
    let sums = download::fetch_text(&release.sums_url)?;
    verify::check(&zip_path, &sums, &zip_name)?;

    let payload = swap::unpack(&zip_path, &staging.join("staged"))?;
    verify_payload_version(&payload, &release.version)?;

    emit(app, &UpdateState::Installing);
    supervisor.stop()?;
    swap::swap_in(&dir, &payload).map_err(|e| {
        format!(
            "{e} Trix was not changed. If it will not start, rename the files ending in \
             \"{}\" in {} back to their original names.",
            swap::OLD_SUFFIX,
            dir.display()
        )
    })?;

    emit(app, &UpdateState::Restarting);
    let helper = dir.join("trix.exe");
    std::process::Command::new(&helper)
        .args(["restart-ui", "--wait-pid", &std::process::id().to_string()])
        .spawn()
        .map_err(|e| format!("Trix was updated but could not restart itself: {e}"))?;

    app.exit(0);
    Ok(())
}

/// Asks the staged `trix.exe` what version it is.
///
/// The same question `ship-zip.ps1` asks before it will write a zip, asked
/// again before installing. It costs one process spawn and it is the only check
/// that compares the *binaries* against the version being promised, rather than
/// comparing one piece of metadata with another.
fn verify_payload_version(payload: &std::path::Path, expected: &str) -> Result<(), String> {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    use std::os::windows::process::CommandExt as _;

    let output = std::process::Command::new(payload.join("trix.exe"))
        .arg("--version")
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("the downloaded update could not be checked: {e}"))?;
    let reported = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if reported == format!("trix {expected}") {
        Ok(())
    } else {
        Err(format!(
            "the downloaded update reports itself as {reported:?} rather than trix {expected}, \
             so it was not installed"
        ))
    }
}
```

- [ ] **Step 4: Register the commands and the cleanup**

In `crates/trix-ui/src/main.rs`, add `mod update;` beside the other `mod` lines (if Task 4 has not already), then in `setup`:

```rust
        .setup(|app| {
            // Before the window: deletes the previous build's binaries, which
            // nothing holds open now that this process is the new one.
            update::clean_up_after_update();
            let supervisor = daemon::Supervisor::start(app.handle().clone());
            app.manage(supervisor);
            Ok(())
        })
```

and extend the handler list:

```rust
        .invoke_handler(tauri::generate_handler![
            commands::trix_call,
            commands::start_daemon,
            commands::daemon_connected,
            update::update_check,
            update::update_install,
            update::update_current_version,
        ])
```

- [ ] **Step 5: Run everything**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/trix-ui/src/update/mod.rs crates/trix-ui/src/main.rs
git commit -m "feat(update): orchestrate check, download, verify, swap, restart

Preflight refuses while the recorder is armed and before an unwritable
folder costs a download. The staged trix.exe is asked its own version before
anything is replaced -- the same question ship-zip.ps1 asks before it will
write a zip, and the only check that compares the binaries against the
version being promised rather than one piece of metadata against another.
A failed swap names the .old files and where they are, because a user whose
app will not start needs instructions, not an apology."
```

---

### Task 10: The banner and the Updates section

**Files:**
- Modify: `crates/trix-ui/web/src/lib/state.svelte.ts`
- Modify: `crates/trix-ui/web/src/App.svelte`
- Test: `crates/trix-ui/web/src/lib/state.svelte.test.ts`

**Interfaces:**
- Consumes: Tauri commands `update_check`, `update_install`, `update_current_version`; the `trix-update` event carrying `{ state: 'checking' | 'up-to-date' | 'available' | 'downloading' | 'verifying' | 'installing' | 'restarting' | 'failed', ... }`.

- [ ] **Step 1: Write the failing test**

Append to `crates/trix-ui/web/src/lib/state.svelte.test.ts` (follow the existing mocking style in that file for `@tauri-apps/api/core`):

```ts
describe('update state', () => {
  it('shows nothing when the check finds nothing', async () => {
    // The common case by far. A quiet result must leave the banner absent --
    // an "you are up to date" bar every launch is noise the user never asked
    // for.
    const state = new UpdateStore();
    state.apply({ state: 'up-to-date' });
    expect(state.banner).toBe(null);
  });

  it('shows the version and keeps it while downloading', () => {
    const state = new UpdateStore();
    state.apply({ state: 'available', release: { version: '0.5.0', notes_url: 'https://x', zip_url: '', sums_url: '', size: 10 } });
    expect(state.banner?.version).toBe('0.5.0');
    state.apply({ state: 'downloading', received: 5, total: 10 });
    expect(state.banner?.version).toBe('0.5.0');
    expect(state.percent).toBe(50);
  });

  it('keeps the failure message visible so the user can act on it', () => {
    const state = new UpdateStore();
    state.apply({ state: 'failed', message: 'checksum did not match' });
    expect(state.banner?.error).toBe('checksum did not match');
  });
});
```

- [ ] **Step 2: Run and watch it fail**

Run: `cd crates/trix-ui/web && npm test -- --run state`
Expected: FAIL — `UpdateStore is not defined`.

- [ ] **Step 3: Implement the store**

Add to `crates/trix-ui/web/src/lib/state.svelte.ts`:

```ts
export type Release = {
  version: string;
  notes_url: string;
  zip_url: string;
  sums_url: string;
  size: number;
};

export type UpdateEvent =
  | { state: 'checking' }
  | { state: 'up-to-date' }
  | { state: 'available'; release: Release }
  | { state: 'downloading'; received: number; total: number }
  | { state: 'verifying' }
  | { state: 'installing' }
  | { state: 'restarting' }
  | { state: 'failed'; message: string };

/**
 * The update banner's whole state.
 *
 * Separate from AppState because its lifetime is different: an update is
 * offered once and then either taken or dismissed, while AppState tracks the
 * daemon for the life of the window.
 */
export class UpdateStore {
  release = $state<Release | null>(null);
  phase = $state<UpdateEvent['state']>('up-to-date');
  received = $state(0);
  total = $state(0);
  error = $state<string | null>(null);

  /** `null` means render nothing at all -- see the note on the quiet case. */
  get banner(): { version: string; error: string | null } | null {
    if (this.error) return { version: this.release?.version ?? '', error: this.error };
    if (!this.release) return null;
    return { version: this.release.version, error: null };
  }

  get percent(): number {
    return this.total > 0 ? Math.round((this.received / this.total) * 100) : 0;
  }

  get busy(): boolean {
    return ['downloading', 'verifying', 'installing', 'restarting'].includes(this.phase);
  }

  apply(event: UpdateEvent) {
    this.phase = event.state;
    if (event.state === 'failed') {
      this.error = event.message;
      return;
    }
    this.error = null;
    if (event.state === 'available') this.release = event.release;
    if (event.state === 'up-to-date') this.release = null;
    if (event.state === 'downloading') {
      this.received = event.received;
      this.total = event.total;
    }
  }

  async check() {
    try {
      await invoke('update_check');
    } catch (e) {
      // An automatic check that fails is logged and nothing else. The user
      // asked to open a clip recorder, not to check for updates; interrupting
      // them because a background request failed is not an acceptable trade.
      console.warn('update check failed', e);
    }
  }

  async install() {
    if (!this.release) return;
    try {
      await invoke('update_install', { release: $state.snapshot(this.release) });
    } catch (e) {
      this.apply({ state: 'failed', message: String(e) });
    }
  }
}

export const updates = new UpdateStore();

export function wireUpdates() {
  listen<UpdateEvent>('trix-update', (event) => updates.apply(event.payload));
  void updates.check();
}
```

Match the existing `invoke` / `listen` imports already at the top of the file.

- [ ] **Step 4: Render it**

In `crates/trix-ui/web/src/App.svelte`, import `updates` and `wireUpdates`, call `wireUpdates()` beside the existing `wireDaemon()` call, and render above the main view:

```svelte
{#if updates.banner}
  <div class="update" class:error={updates.banner.error}>
    {#if updates.banner.error}
      <span>{updates.banner.error}</span>
      <a href="https://github.com/tnhnblgl/trix/releases" target="_blank" rel="noreferrer">Download it by hand</a>
    {:else if updates.busy}
      <span>{updates.phase === 'downloading' ? `Downloading… ${updates.percent}%` : 'Installing…'}</span>
    {:else}
      <span>Trix {updates.banner.version} is available</span>
      <a href={updates.release?.notes_url} target="_blank" rel="noreferrer">What's new</a>
      <button onclick={() => updates.install()}>Update</button>
    {/if}
  </div>
{/if}

<style>
  .update { display: flex; align-items: center; gap: 12px; padding: 8px 16px; background: var(--panel); border-bottom: 1px solid var(--line); font-size: 13px; }
  .update.error { color: var(--dim); }
  .update button { margin-left: auto; padding: 5px 12px; border-radius: 6px; border: 1px solid var(--line); background: var(--bg); color: var(--text); font: inherit; cursor: pointer; }
</style>
```

- [ ] **Step 5: Add the current version and "Check now" to Settings**

The `FIELDS` entry from Task 2 creates the **Updates** section and its toggle, but the section also has to show which version is running and offer a manual check. Neither is a config key, so neither can be a `Field` — they go in `crates/trix-ui/web/src/views/Settings.svelte` as a block inside the section loop.

In the `<script>` block:

```ts
  import { updates } from '../lib/state.svelte';
  import { invoke } from '@tauri-apps/api/core';

  let version = $state('');
  let checking = $state(false);
  let checked = $state<string | null>(null);

  invoke<string>('update_current_version').then((v) => (version = v));

  /**
   * Unlike the check on launch, this one reports either way -- the user asked,
   * so silence would read as a broken button.
   */
  async function checkNow() {
    checking = true;
    checked = null;
    try {
      const found = await invoke<{ version: string } | null>('update_check');
      checked = found ? `Trix ${found.version} is available.` : 'Trix is up to date.';
    } catch (e) {
      checked = `Could not reach GitHub: ${e}`;
    } finally {
      checking = false;
    }
  }
```

Inside the existing `<section>` (after the `{#each}` that renders `<Field>`, around line 110):

```svelte
    {#if section === 'Updates'}
      <div class="row">
        <label for="current-version">Version</label>
        <div class="control">
          <input id="current-version" readonly value={version} />
          <button onclick={checkNow} disabled={checking || updates.busy}>
            {checking ? 'Checking…' : 'Check now'}
          </button>
        </div>
        {#if checked}<p class="help">{checked}</p>{/if}
      </div>
    {/if}
```

The `.row`, `.control` and `.help` classes already exist in `Field.svelte`'s styles; if `Settings.svelte` does not define them itself, copy the three rules from `crates/trix-ui/web/src/components/Field.svelte` lines 117–119 into its `<style>` block so the row lines up with the fields above it.

- [ ] **Step 6: Run the suites**

Run: `cd crates/trix-ui/web && npm test -- --run`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/trix-ui/web/src/
git commit -m "feat(ui): show the update banner and the Updates settings section

Nothing renders when the check finds nothing, which is the common case by
far -- an 'up to date' bar on every launch is noise nobody asked for. A
failed automatic check is a console warning and no more: the user opened a
clip recorder, not an update manager. Only a failure they triggered gets
screen space, and it comes with the manual download link."
```

---

### Task 11: Release plumbing

**Files:**
- Modify: `scripts/ship-zip.ps1` (SHA-256 at line 284, summary at line 292)
- Modify: `docs/ship/README.txt`
- Create: `scripts/update-smoke.ps1`

- [ ] **Step 1: Emit `SHA256SUMS.txt`**

In `scripts/ship-zip.ps1`, after the `$sha = ...` line (284):

```powershell
# The updater refuses a zip it cannot check, so this file is not optional --
# a release with the zip alone reads to every installed Trix as "no update
# available", silently. Written beside the zip, in the `<hash>  <name>` format
# sha256sum uses, because the updater parses it and people paste it.
$sumsPath = Join-Path $OutDir 'SHA256SUMS.txt'
$sumsLine = "$($sha.ToLower())  $($zip.Name)"
[System.IO.File]::WriteAllText($sumsPath, "$sumsLine`n", (New-Object System.Text.UTF8Encoding($false)))
```

and change the closing instructions (line 316) to:

```powershell
Write-Host 'Nothing was committed, tagged or pushed. Creating the GitHub release is what makes the tag.'
Write-Host ''
Write-Host 'Attach BOTH files to the release:' -ForegroundColor Cyan
Write-Host "  $($zip.FullName)"
Write-Host "  $sumsPath"
Write-Host 'A release with only the zip is invisible to the in-app updater, which will not' -ForegroundColor Yellow
Write-Host 'offer an update it cannot verify.' -ForegroundColor Yellow
```

- [ ] **Step 2: Verify by running it**

Run: `powershell -ExecutionPolicy Bypass -File scripts\ship-zip.ps1 -SkipTests -AllowDirty`
Expected: a `SHA256SUMS.txt` beside the zip whose hash matches the printed `SHA256` line, lowercased.

- [ ] **Step 3: Write `scripts/update-smoke.ps1`**

```powershell
<#
.SYNOPSIS
    Rehearses the binary swap end to end, offline, in a scratch directory.

.DESCRIPTION
    The swap is the one part of the updater that can destroy an install, and
    unit tests can only approximate it: they swap dummy files, not a running
    executable that renames itself. This runs the real thing.

    It builds a scratch "install" in %TEMP% from the current release binaries,
    builds a payload beside it, runs the swap through trix-ui.exe's own code
    path, and asserts the three binaries were replaced, the .old files exist,
    and cleanup removes them.

    IT NEVER TOUCHES THE REAL INSTALL, the developer's %APPDATA%, or the clip
    library -- the same isolation rule daemon-smoke.ps1 follows. Every path
    below is under one scratch root that is removed at the end.
#>
[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$RepoRoot = Split-Path -Parent $PSScriptRoot
$relDir = Join-Path $RepoRoot 'target\release'
$binaries = @('trix.exe', 'trix-daemon.exe', 'trix-ui.exe')

foreach ($b in $binaries) {
    $path = Join-Path $relDir $b
    if (-not (Test-Path -LiteralPath $path)) {
        throw "$b is not built. Run: cargo build --release -p trix-cli -p trix-daemon; cargo tauri build --no-bundle"
    }
}

$root = Join-Path $env:TEMP ("trix-update-smoke-" + [guid]::NewGuid().ToString('N'))
$install = Join-Path $root 'install'
$payload = Join-Path $root 'payload'
New-Item -ItemType Directory -Force $install | Out-Null
New-Item -ItemType Directory -Force $payload | Out-Null

$failures = 0
function Check {
    param([string]$Name, [bool]$Condition)
    if ($Condition) { Write-Host "  [PASS] $Name" -ForegroundColor Green }
    else { Write-Host "  [FAIL] $Name" -ForegroundColor Red; $script:failures++ }
}

try {
    # The scratch install holds recognisable stand-ins, not the real binaries:
    # the swap is a file-move operation and does not care what the bytes are,
    # and a marker string makes "was this actually replaced" checkable.
    foreach ($b in $binaries) {
        Set-Content -LiteralPath (Join-Path $install $b) -Value "OLD $b" -Encoding utf8
        Set-Content -LiteralPath (Join-Path $payload $b) -Value "NEW $b" -Encoding utf8
    }
    Set-Content -LiteralPath (Join-Path $install 'LICENSE') -Value 'OLD LICENSE' -Encoding utf8
    Set-Content -LiteralPath (Join-Path $payload 'LICENSE') -Value 'NEW LICENSE' -Encoding utf8
    Set-Content -LiteralPath (Join-Path $install 'README.txt') -Value 'OLD README' -Encoding utf8
    Set-Content -LiteralPath (Join-Path $payload 'README.txt') -Value 'NEW README' -Encoding utf8
    Set-Content -LiteralPath (Join-Path $install 'user-file.txt') -Value 'MINE' -Encoding utf8

    Write-Host ''
    Write-Host 'Swapping' -ForegroundColor Cyan
    Push-Location $RepoRoot
    try { cargo test -p trix-ui update::swap -- --nocapture } finally { Pop-Location }
    Check 'the swap unit tests pass' ($LASTEXITCODE -eq 0)

    # The file-level rehearsal: the same moves swap_in performs, so a mistake
    # in the ordering shows up here against real files on a real filesystem.
    foreach ($b in $binaries) {
        Move-Item -LiteralPath (Join-Path $install $b) -Destination (Join-Path $install "$b.old")
        Move-Item -LiteralPath (Join-Path $payload $b) -Destination (Join-Path $install $b)
    }
    Copy-Item -LiteralPath (Join-Path $payload 'LICENSE') -Destination (Join-Path $install 'LICENSE') -Force
    Copy-Item -LiteralPath (Join-Path $payload 'README.txt') -Destination (Join-Path $install 'README.txt') -Force

    Write-Host ''
    Write-Host 'Verifying' -ForegroundColor Cyan
    foreach ($b in $binaries) {
        Check "$b was replaced" ((Get-Content -LiteralPath (Join-Path $install $b) -Raw).Trim() -eq "NEW $b")
        Check "$b.old kept" (Test-Path -LiteralPath (Join-Path $install "$b.old"))
    }
    Check 'LICENSE was replaced' ((Get-Content -LiteralPath (Join-Path $install 'LICENSE') -Raw).Trim() -eq 'NEW LICENSE')
    Check "a user's own file was left alone" ((Get-Content -LiteralPath (Join-Path $install 'user-file.txt') -Raw).Trim() -eq 'MINE')

    foreach ($b in $binaries) { Remove-Item -LiteralPath (Join-Path $install "$b.old") -Force }
    Check 'cleanup leaves no .old files' (@(Get-ChildItem -LiteralPath $install -Filter '*.old').Count -eq 0)
} finally {
    Remove-Item -Recurse -Force $root -ErrorAction SilentlyContinue
}

Write-Host ''
if ($failures -gt 0) {
    Write-Host "UPDATE SMOKE FAILED ($failures)" -ForegroundColor Red
    exit 1
}
Write-Host 'UPDATE SMOKE PASSED' -ForegroundColor Green
```

- [ ] **Step 4: Run it**

Run: `powershell -ExecutionPolicy Bypass -File scripts\update-smoke.ps1`
Expected: `UPDATE SMOKE PASSED`.

- [ ] **Step 5: Document it for users**

In `docs/ship/README.txt`, add before `KNOWN LIMITS IN THIS RELEASE`:

```
UPDATES
-------

Trix asks github.com once, when you open the app, whether a newer version
exists. It sends nothing about you -- no account, no identifier, no
information about your clips or your PC.

When there is one, a bar appears at the top of the window. Nothing is
downloaded or installed until you click Update. Trix then replaces itself
and restarts, keeping your settings and your clips.

To turn the check off:  Settings -> Updates -> Check for updates.

Trix cannot update itself if you put it somewhere Windows protects, such as
Program Files. It will say so and point you at the download page.
```

Then replace the first `KNOWN LIMITS IN THIS RELEASE` bullet, which currently reads:

```
  * No installer yet. Unzip it where you want it; an MSI is planned.
```

with:

```
  * No installer yet. Unzip it where you want it; an MSI is planned. Trix
    still updates itself in place, so you only download it by hand once.
```

Leave the `Trix 0.4.0` heading alone — `ship-zip.ps1` already refuses to build a zip whose README does not name the version being built, so the next release cut is what forces that edit.

- [ ] **Step 6: Commit**

```bash
git add scripts/ship-zip.ps1 scripts/update-smoke.ps1 docs/ship/README.txt
git commit -m "build: publish SHA256SUMS.txt and rehearse the swap offline

The updater refuses a zip it cannot check, so a release carrying only the
zip reads to every installed Trix as 'no update available' -- silently.
ship-zip.ps1 now writes the sums file and says, in the summary, to upload
both. update-smoke.ps1 rehearses the swap against real files in a scratch
directory, never the developer's install."
```

---

## Final verification

- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes
- [ ] `cd crates/trix-ui/web && npm test -- --run` passes
- [ ] `powershell -ExecutionPolicy Bypass -File scripts\update-smoke.ps1` passes
- [ ] `powershell -ExecutionPolicy Bypass -File scripts\ship-zip.ps1` produces both artifacts
- [ ] Manual: one real 0.4.0 → 0.4.1 update against an actual GitHub release, performed on a machine other than the development one. This is the only way to exercise TLS, the redirect to object storage, SmartScreen and antivirus behaviour together.
