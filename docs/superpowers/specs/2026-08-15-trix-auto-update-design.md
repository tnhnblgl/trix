# Trix — In-App Updates

**Date:** 2026-08-15
**Status:** Approved design, not yet implemented
**Extends** [2026-07-26-trix-desktop-ui-design.md](2026-07-26-trix-desktop-ui-design.md) (the daemon,
the control protocol and the desktop app) and the release process in `scripts/ship-zip.ps1`.

---

## 1. Context

Trix ships as a portable zip. `scripts/ship-zip.ps1` builds
`trix-v<version>-win-x64.zip` containing `trix-ui.exe`, `trix-daemon.exe`, `trix.exe`, `LICENSE` and
`README.txt`; the user creates a GitHub release by hand and attaches it. There is no installer —
`tauri.conf.json` sets `bundle.active` to `false` and the build runs `cargo tauri build --no-bundle`.

Nothing tells an installed copy that a newer one exists. A user still on 0.3.0 has no path to 0.4.0
short of revisiting the GitHub page on a hunch. For a tool distributed as a link passed between
friends, that is the difference between a fix reaching people and a fix existing.

The obvious answer — `tauri-plugin-updater` — does not fit. On Windows that plugin downloads and runs
an NSIS or MSI installer, and Trix has neither. It also has no concept of the piece that makes this
problem interesting: `trix-daemon.exe` is a *running process* that holds the GPU capture stack, the
tray icon and the global hotkey while the update happens. Windows will not overwrite a running
executable, and the daemon's control protocol currently has no way to ask it to stop
(`crates/trix-daemon/src/dispatch.rs` exposes arm, disarm, clip, config and library — nothing that
exits).

So this is a custom updater. It is not much code, but almost all of it is failure handling, because
an updater that fails badly is worse than no updater: it takes a working install with it.

## 2. Scope

**In scope:**

- A check against the GitHub Releases API, once per app launch, plus a manual button
- A banner in the app when a newer version exists
- One-click download, integrity check, and in-place replacement of the shipped files
- A `shutdown` command in the daemon control protocol, and a supervisor that can use it
- A `check_for_updates` config key, defaulting to `true`
- An **Updates** section in Settings
- `SHA256SUMS.txt` as a second release asset, produced by `ship-zip.ps1`

**Explicitly out of scope** (none of it foreclosed by this design):

- **Signature verification.** Decided against for now: integrity is established by HTTPS to GitHub
  plus a published SHA-256. This means the GitHub account is the trust anchor — anyone who can
  publish a release can publish one that every installed Trix will offer to install. Two-factor
  authentication on that account is a requirement of this design, not a suggestion. `verify.rs`
  exists as its own module precisely so a minisign check can be added there later without
  restructuring anything around it.
- **Silent background installation.** The check is automatic; the install is never. See §3.4.
- **Delta or patch updates.** The whole product is 3.2 MB. There is nothing to optimise.
- **Rollback to a previous version.** The replaced binaries are kept as `.old` until the next launch,
  which covers "the update did not start", but there is no UI for going backwards.
- **Release channels.** One channel: whatever `releases/latest` returns.
- **The daemon updating itself.** The daemon has no UI to ask permission with. If Trix is running at
  login without the app open, it updates the next time the app is opened.
- **An installer.** Still planned, still not this change. Nothing here assumes the zip layout beyond
  §4.3, so an installer can replace the swap step later.

## 3. Product behaviour

### 3.1 What the user sees

On launch the app asks GitHub whether there is a newer release. Almost always the answer is no, and
nothing appears — no spinner, no toast, no "you are up to date". Silence is the correct response to
the common case.

When there is a newer version, a single-line banner appears above the clip grid:

```
Trix 0.5.0 is available                      What's new    [ Update ]
```

"What's new" opens the release page in the browser. "Update" starts the install and replaces the
banner with progress:

```
Downloading…  ███████░░░  4.1 / 5.2 MB
```

then `Verifying…`, `Stopping recorder…`, `Installing…`, `Restarting…`. The app closes and reopens on
the new version, with a brief note confirming what happened.

Settings gains an **Updates** section: the current version, a "Check for updates automatically"
toggle, a "Check now" button, and the same banner content when an update is pending.

### 3.2 Failed checks are silent

A check that fails — no network, GitHub unreachable, a rate limit, malformed JSON — logs at `warn`
and does nothing else. It never produces a dialog, a toast, or a red badge. The user did not ask to
check for updates; they asked to open a clip recorder. A background task they did not request must
not interrupt them when it fails.

"Check now" is different. The user asked, so the answer is shown either way: up to date, a version
number, or a plain failure message.

### 3.3 A release whose assets are still uploading is not an update

The user creates the GitHub release by hand, and GitHub publishes the release before the attached
files finish uploading. A check landing in that window sees `tag_name: v0.5.0` and no zip.

That is treated as *no update available*, logged and dismissed — not as an error, and not as an
update the user can click. The alternative is a banner offering a version that cannot be downloaded,
which is the only failure mode here that the user would actually notice and could do nothing about.

### 3.4 The install is always a deliberate act

The check is automatic; the install requires a click, every time. There is no "install updates
automatically" setting, and this is a deliberate refusal rather than an unbuilt feature.

Trix's whole job is to be running, armed and invisible while someone plays a game. A background task
that stops the recorder, replaces three executables and restarts the app is the single most
disruptive thing this codebase could do unprompted, and it would do it precisely when the user is
least able to tolerate it. The same reasoning drives the preflight in §4.4: an update offered while
the recorder is armed is refused, not queued.

### 3.5 Every failure leaves a working install

The design constraint that outranks the others: at no point may a failure leave the user with
something that does not run. Concretely — the old binaries are not deleted until the new ones are in
place and the app has successfully restarted on them; a failed swap is rolled back; and a rollback
that itself fails reports exactly which file is where, by name, rather than pretending it succeeded.

## 4. Architecture

### 4.1 The updater lives in the app, not the daemon

`trix-ui` owns all of it. Two reasons, and the second is the real one.

The daemon is the process that must stay cheap while a game runs. It is the reason the product
exists. Adding an HTTP client and a TLS stack to it costs binary size and attack surface in exactly
the process where neither is affordable.

More importantly, the app already owns the daemon's lifecycle. `crates/trix-ui/src/daemon.rs` finds
`trix-daemon.exe` beside itself and spawns it under a supervisor. Stopping the daemon, replacing it,
and starting it again is a lifecycle operation, and it belongs where lifecycle already lives. A
daemon that updated itself would have to stop and replace the process it is currently executing from.

### 4.2 HTTP client: `ureq` with `native-tls`

`ureq` is blocking, which matches how the app already talks to the daemon — `commands.rs` wraps
every call in `tauri::async_runtime::spawn_blocking`, so the updater uses the same pattern rather
than introducing a second concurrency model.

`native-tls` rather than `rustls`: on Windows it resolves to schannel, which uses the certificate
store the OS already maintains. That is both smaller — roughly 200 KB against the ~2 MB a bundled
root store costs — and more correct on machines with a corporate trust root. On a product whose
entire zip is 3.2 MB, the size difference is a feature, not a rounding error.

This is the first outbound connection Trix has ever made. It sends an HTTP GET with a
`User-Agent: trix/<version>` header, which the GitHub API requires, and nothing else. No identifiers,
no telemetry, no request body.

### 4.3 Files

```
crates/trix-ui/src/update/
  mod.rs        Public surface, the UpdateState machine, Tauri commands
  check.rs      GitHub API query, version comparison, asset selection
  download.rs   Streaming download, progress reporting, host allowlist
  verify.rs     SHA256SUMS parsing and comparison  (the seam for signing)
  swap.rs       Preflight, staging, the rename dance, rollback, cleanup
```

Modified:

```
crates/trix-ui/src/daemon.rs          stop() on the supervisor
crates/trix-ui/src/main.rs            post-update cleanup at startup; --post-update arg
crates/trix-daemon/src/dispatch.rs    the shutdown command
crates/trix-cli/src/main.rs           the restart-ui subcommand (§4.6)
crates/trix-core/src/config.rs        check_for_updates
crates/trix-ui/web/src/…              the banner and the Settings section
scripts/ship-zip.ps1                  emit SHA256SUMS.txt
docs/ship/README.txt                  an UPDATES section
```

New:

```
scripts/update-smoke.ps1              offline end-to-end swap rehearsal (§7.3)
```

### 4.4 The install sequence

Run by `trix-ui.exe`, in this order, each step abandoning the update cleanly if it fails.

**1. Preflight.** Ask the daemon for `status`; refuse if it is armed, with "Stop recording before
updating." Then confirm the install directory is writable by creating and deleting a probe file. A
portable app dropped into `C:\Program Files` is not writable without elevation, and discovering that
halfway through the swap is how an install gets destroyed. Refusing here costs nothing.

**2. Download** to `<install_dir>\.trix-update\download\`. Deliberately not `%TEMP%`: the swap
depends on renaming files into place, and a rename is only cheap and atomic *within a volume*. A
portable install on `D:\` with `%TEMP%` on `C:\` would turn every move into a copy, with a
correspondingly wider window to fail in. The staging directory is removed on success and on failure.

**3. Verify.** SHA-256 of the downloaded zip against its line in `SHA256SUMS.txt`. Then extract,
which validates each entry's CRC-32 as a side effect.

**4. Sanity-check the payload.** The extracted tree must contain `trix.exe`, `trix-daemon.exe` and
`trix-ui.exe`, and the staged `trix.exe --version` must report the version being installed. This is
the same question `ship-zip.ps1:257` asks before it will write a zip, and asking it again before
installing costs one process spawn. Note that the zip has a single top-level directory —
`ship-zip.ps1` passes `includeBaseDirectory: true` — so extraction produces
`staged\trix-v0.5.0-win-x64\`, and the code resolves that one directory rather than assuming a flat
layout.

**5. Stop the daemon.** Send `shutdown` (§5), then wait for the control pipe to disappear, polling
every 250 ms up to 15 seconds. The generous timeout is not arbitrary: disarming releases the capture
stack, and the Intel driver stack has been measured taking about ten seconds to let go. If the pipe
is still there at 15 seconds, terminate the process by the PID the `shutdown` reply carried. If that
also fails, abort — nothing has been modified yet.

**6. Swap.** For each of the three executables, in order: rename `X.exe` to `X.exe.old`, then move the
staged `X.exe` into place. Windows forbids overwriting a running executable but permits *renaming*
one, which is the entire mechanism — `trix-ui.exe` renames itself out of the way while running from
the renamed file. `LICENSE` and `README.txt` are not locked and are simply overwritten. No other file
in the directory is touched.

Each completed step is recorded. Any failure undoes them in reverse: move the new file back to
staging, rename `X.exe.old` back to `X.exe`. If an undo step *also* fails, stop and report the exact
filenames and their exact current locations, so the user can finish by hand — see §3.5.

**7. Restart** via the mechanism in §4.6.

**8. Clean up on next launch.** The newly started `trix-ui.exe` deletes `*.old` and `.trix-update\`
at startup, which is possible now that nothing holds them open. Deletion failures here are logged and
ignored: a stale `trix-ui.exe.old` is litter, not a fault, and the next launch tries again.

### 4.5 Hosts and bounds

Every URL the updater fetches — including redirect targets, since GitHub redirects asset downloads to
its object storage — must have a host in a compiled-in allowlist: `api.github.com`, `github.com`,
`objects.githubusercontent.com`. A redirect anywhere else aborts the download. Responses are capped
at 64 MB; the product is 3.2 MB, so the bound is pure headroom against a response that never ends.

### 4.6 Restarting without racing single-instance

The app registers `tauri-plugin-single-instance`. If the old `trix-ui.exe` spawned the new one
directly and then exited, the new process would start while the old one was still alive, and the
plugin would hand its arguments to the dying instance and exit — leaving nothing running.

Rather than sleep and hope, the relaunch is delegated to `trix.exe`, which is already in the folder,
is already documented as the interface "for scripting and diagnostics", and is not running during any
of this. A new subcommand:

```
trix.exe restart-ui --wait-pid <pid>
```

waits for that process to exit, then starts `trix-ui.exe` from beside itself. The old app spawns it
*after* the swap — so it is the new `trix.exe` doing the work — and then exits immediately. There is
no race to lose, because the helper's only job is to observe that the old process is gone.

### 4.7 State

```rust
enum UpdateState {
    Idle,
    Checking,
    UpToDate,
    Available { version: String, notes_url: String, size: u64 },
    Downloading { received: u64, total: u64 },
    Verifying,
    Installing,
    Restarting,
    Failed { message: String },
}
```

The frontend reads this over the existing Tauri command/event channel. `Failed` carries a message
written for a user rather than a developer, and always names the manual fallback.

## 5. Daemon and protocol

One new command:

```
shutdown  ->  { ok: true, pid: <u32> }
```

The reply is written **before** the daemon begins shutting down. A daemon that exits first can never
answer, and the caller would be left inferring success from a dropped pipe — indistinguishable from a
crash. After replying, the daemon disarms if armed, releases the capture stack, tears down the tray
icon and exits.

The `pid` in the reply exists so the app can terminate a daemon that hangs. The app cannot rely on
holding a process handle: when "Start with Windows" is on, the daemon was started at login by the
shell, not by the supervisor.

`shutdown` is a live command in the sense used by `config_set` — it requires no re-arm and no
engine restart, because there is nothing left to restart.

This command is useful beyond updates. Today the app can start the daemon and never stop it; the
only way to stop it is the tray menu.

## 6. Configuration

One new key in `config.toml`:

```toml
check_for_updates = true
```

It lives in the daemon's config like every other setting, reached through the existing
`config.get` / `config.set` plumbing, so the Settings page needs no new persistence mechanism. The
daemon stores and returns it without acting on it — the app is the only reader.

No timestamp is persisted and there is no 24-hour throttle. The check runs once per app launch and on
demand. A user who opens Trix twenty times in a day makes twenty requests, comfortably inside
GitHub's unauthenticated limit of sixty per hour, and skipping the throttle removes a persistence
layer, a clock dependency and a class of "why didn't it check" bug.

## 7. Testing

### 7.1 Unit

- **`check.rs`** — version comparison across a table: newer, identical, older, a tag missing its `v`
  prefix, a malformed tag, a prerelease-looking tag. Asset selection against a captured
  `releases/latest` response, including the §3.3 case where the release exists but carries no assets,
  and the case where the zip is present but `SHA256SUMS.txt` is not.
- **`verify.rs`** — `SHA256SUMS.txt` parsing, a matching digest, a mismatched digest, and a file with
  no line for the asset.
- **`download.rs`** — the host allowlist accepts each permitted host and rejects a lookalike
  (`github.com.evil.example`); the size bound trips.
- **`swap.rs`** — against temporary directories of dummy files: a clean swap, a failure injected at
  each step rolling back to the original layout, and cleanup removing `.old` files.
- **Daemon** — `shutdown` replies before exiting; `shutdown` while armed disarms first; the reply
  carries the process's own PID.

Every daemon test uses a scratch `%APPDATA%` **and** a scratch `clip_dir`, as the existing suite does.

### 7.2 Frontend

The banner renders only in `Available`; progress renders in `Downloading`; `Failed` shows the message
and the manual-download link. The Settings toggle round-trips through `config.set`.

### 7.3 `scripts/update-smoke.ps1`

The swap is the part that can destroy an install, and it is the part unit tests can only approximate.
This script rehearses it end to end with no network: it builds a throwaway "0.0.1" zip from the
current binaries, lays out a scratch install directory in `%TEMP%`, points the updater at the local
file, and asserts the three executables were replaced, the `.old` files exist, and cleanup removes
them.

It never touches the developer's real install, real `%APPDATA%` or real clip library — the same
isolation rule `daemon-smoke.ps1` follows.

### 7.4 Manual

One real 0.4.0 → 0.4.1 update against an actual GitHub release, on a machine other than the
development one. This is the only way to exercise TLS, redirects to object storage, SmartScreen and
antivirus behaviour together, and none of those can be faked convincingly.

## 8. Release process changes

`ship-zip.ps1` already computes the zip's SHA-256 for its summary. It gains one step: write
`SHA256SUMS.txt` beside the zip, in the standard `<hash>  <filename>` format, and name both files in
its closing instructions. Releases now carry two assets.

The script's existing gates are unchanged. The version agreement check between `Cargo.toml` and
`tauri.conf.json` becomes more load-bearing than before, since the version it stamps is now what
every installed copy compares itself against.

## 9. Risks

- **The GitHub account is the trust anchor.** Accepted deliberately (§2). Two-factor authentication
  on that account is now a security control of the shipped product.
- **Antivirus heuristics.** A process that downloads an executable and replaces executables beside
  itself is a recognisable malware shape, and Trix is unsigned. Doing it only on an explicit click,
  never in the background, is the mitigation available without a code-signing certificate. If this
  proves to be a real problem in the field, code signing is the answer, and it costs money.
- **Install directories that are not writable.** Caught in preflight; the fallback is the manual
  download link.
- **A partial swap.** Rolled back; an unrecoverable rollback reports filenames and locations rather
  than a generic failure.
- **The daemon refusing to stop.** Force-terminated after 15 seconds; if even that fails, the update
  aborts before modifying anything.

## 10. Definition of done

- The app checks on launch, silently when there is nothing to report.
- A newer release produces a banner; the release page opens from it.
- One click downloads, verifies, stops the daemon, swaps three executables, and restarts on the new
  version.
- Every failure path leaves a runnable install, and says what to do next.
- A release with no assets attached yet reads as "no update", not as an error.
- `shutdown` exists in the protocol, replies before exiting, and reports its PID.
- Settings has an Updates section; `check_for_updates` persists.
- `ship-zip.ps1` emits `SHA256SUMS.txt` and says to upload it.
- `README.txt` states what Trix contacts, when, and how to turn it off.
- `cargo test --workspace` and `npm test` pass; `update-smoke.ps1` passes.
- One real update performed on a second machine.
