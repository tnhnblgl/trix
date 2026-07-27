# Trix Control Protocol Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A headless `trix-daemon.exe` that owns the replay ring and answers the control protocol over a user-ACL'd named pipe, so any UI — ours or a third party's — can arm, clip, and browse the library without linking the engine.

**Architecture:** `trix-proto` defines the wire types as pure serde. `trix-core` grows an `EngineHandle` that drives the existing, verified replay loop from a command channel instead of only from the hotkey, plus a `library` module that owns the on-disk clip format. `trix-daemon` is a new binary: a named-pipe server, a command dispatcher, and an in-RAM library cache. The CLI keeps talking to `trix-core` directly and never learns the daemon exists.

**Tech Stack:** Rust 2024 · windows-rs 0.62 (named pipes, SDDL, token SIDs) · serde / serde\_json · Media Foundation (unchanged) · Windows.Graphics.Capture (unchanged)

**Stage:** This is **plan 2 of 4** for [2026-07-26-trix-desktop-ui-design.md](../specs/2026-07-26-trix-desktop-ui-design.md). Plan 1 (workspace split) is merged and verified. This plan delivers spec §10 stage 2. Plans 3 (tray, hotkey, thumbnails, disk ceiling) and 4 (Tauri UI, export) follow.

**Branch:** `feat/control-protocol`, forked from `master`.

---

## Global Constraints

Every task's requirements implicitly include this section.

- **Windows-only. Rust edition 2024.** All dependencies come from `[workspace.dependencies]` in the root `Cargo.toml`; no per-crate version literals.
- **Exactly one new third-party dependency is permitted: `serde_json`.** Nothing else. New `windows` crate *features* are fine — that is not a new dependency. If a task appears to need another crate, stop and escalate.
- **`trix-proto` must contain no `windows` dependency, no `unsafe`, and no engine types.** It is what a third-party UI author consumes. It must build on a non-Windows host.
- **`trix-core` may depend on `trix-proto`** (for `ClipMeta`) — one definition of the on-disk metadata, used by both the CLI's clip path and the daemon's library. The one forbidden edge is `trix-ui → trix-core`, and `trix-ui` does not exist yet.
- **The CLI binary stays `trix.exe`** and its argument surface stays exactly as it is. `crates/trix-cli/src/main.rs`'s `cli_surface_is_unchanged` test must keep passing untouched.
- **The capture callback is not to be modified.** `ReplaySession::on_frame_arrived`, the fps pacer, `evict`, `pump_audio_to`, and `snapshot_clip` carry seven phases of verification. This plan changes what *drives* the control loop, never what happens per frame.
- **`trix replay`'s console output stays byte-identical** except for the clip path, which now points at the clip directory. The `replay buffer running: …` banner and the `clip saved: …` line keep their exact formats.
- **Transport is the named pipe `\\.\pipe\trix-control`**, `PIPE_REJECT_REMOTE_CLIENTS`, DACL granting the current user's SID and nobody else. Never a TCP socket.
- **Framing is newline-delimited JSON**, UTF-8, one message per line, `\n`-terminated. No length prefix, no schema compiler.
- **Malformed input never drops a connection.** A bad line, an unknown `cmd`, or a missing argument gets an error *response*; the socket stays open. Only EOF closes a client.
- **Clip ids arriving from the socket are untrusted.** Any command that turns an `id` into a filesystem path validates it first. This is the plan's one genuine security boundary.
- **`panic = "abort"` stays.** Decided 2026-07-27 with measurements: `unwind` costs 685 KB on a 1.49 MB binary (+44%), and buys less than it appears to — a panic in the capture callback aborts either way, because `windows-capture` invokes the handler across an `extern "system"` boundary that Rust refuses to unwind through. The one thing it would protect is socket-facing code, which this plan handles by construction instead:
  - **No `unwrap`, `expect`, or indexing panic is permitted on any path reachable from a socket message.** Parsing, id handling, path building, library mutation, and serialization all return `Result` and become an error response. `encode_line` returns `Result` for exactly this reason.
  - `catch_unwind` does not catch under `abort` — there is no recovery net. Not panicking is the whole strategy.
  - A reviewer should treat a panicking call on a socket-reachable path as a Critical finding.
- **Out of scope, deliberately** — do not build these, they are plans 3 and 4: thumbnails / `.jpg` sidecars, `max_library_gb` enforcement, GOP pinning, `library.export`, the tray icon, daemon-side hotkey registration, autostart, `trim_mode`, first-run setup.

---

## Spec gaps resolved by this plan

Two things the spec describes but does not fully specify. Both are decided here; both are called out again in Task 8's documentation step.

1. **`stats` subscription has no command.** Spec §4.4 says `stats` is "emitted only while a client has subscribed", but §4.3's command table has no way to subscribe. This plan adds **`stats.subscribe {enabled: bool}`** and amends the spec's table.
2. **A line too malformed to yield an `id`** cannot be answered by the `{"id":N,...}` response shape. This plan reserves **`id: 0`** for "the request could not be parsed far enough to recover its id", and documents that clients must not use id 0.

---

## File Structure

```
Cargo.toml                                  MODIFY  add serde_json + members + default-members
crates/trix-proto/Cargo.toml                CREATE
crates/trix-proto/src/lib.rs                CREATE  crate docs + re-exports
crates/trix-proto/src/message.rs            CREATE  Request/Response/Event/ClipMeta + framing
crates/trix-proto/src/command.rs            CREATE  typed Command + parse from Request
crates/trix-core/Cargo.toml                 MODIFY  + trix-proto, + serde_json
crates/trix-core/src/lib.rs                 MODIFY  + pub mod engine; + pub mod library;
crates/trix-core/src/config.rs              MODIFY  + clip_dir, + Serialize, + Clone, + save()
crates/trix-core/src/library.rs             CREATE  clip ids, sidecars, scan, id validation
crates/trix-core/src/engine.rs              CREATE  EngineHandle / EngineCommand / EngineStatus
crates/trix-core/src/replay.rs              MODIFY  command channel drives the loop; save_clip returns metadata
crates/trix-core/src/control.rs             MODIFY  single-instance becomes an RAII guard
crates/trix-core/src/probe.rs               MODIFY  extract monitors()/encoders() behind the printers
crates/trix-core/src/encode/h264.rs         MODIFY  keep the MFT friendly name
crates/trix-cli/src/main.rs                 MODIFY  bind the single-instance guard
crates/trix-daemon/Cargo.toml               CREATE
crates/trix-daemon/src/main.rs              CREATE  startup, logging, accept loop
crates/trix-daemon/src/pipe.rs              CREATE  SDDL, CreateNamedPipeW, per-client threads
crates/trix-daemon/src/clients.rs           CREATE  client registry + event broadcast
crates/trix-daemon/src/state.rs             CREATE  Daemon: config, engine, library cache
crates/trix-daemon/src/dispatch.rs          CREATE  Command -> Response
scripts/protocol-smoke.ps1                  CREATE  the stage-2 gate artifact
PLAN.md                                     MODIFY  module layout + progress log
docs/superpowers/specs/…-desktop-ui-design.md MODIFY §4.3 gains stats.subscribe; §4.2 gains id 0
```

---

## Task 1: The `trix-proto` wire crate

**Files:**
- Create: `crates/trix-proto/Cargo.toml`
- Create: `crates/trix-proto/src/lib.rs`
- Create: `crates/trix-proto/src/message.rs`
- Create: `crates/trix-proto/src/command.rs`
- Modify: `Cargo.toml` (root)

**Interfaces:**
- Consumes: nothing.
- Produces: `trix_proto::{Request, Response, Event, ClipMeta, Command, encode_line, decode_request, MAX_LINE_BYTES, RESERVED_ID}`.

**Design note for the implementer — read before writing code.** The obvious design is one `#[serde(tag = "cmd")]` enum. It is wrong here: an unknown or misspelled `cmd` fails deserialization of the *whole line*, which destroys the `id`, and the protocol requires an error response carrying that id. So parsing is two-stage — `Request` is deliberately loose (`id`, `cmd`, and a flattened bag of arguments), and `Command::parse` types it afterwards, once the id is safely in hand.

- [ ] **Step 1: Add `serde_json` to the workspace**

In `Cargo.toml`, inside `[workspace.dependencies]`, keeping the list alphabetical (it goes directly after the `serde` line):

```toml
serde_json = "1"
```

And add the crate to `members`, which becomes:

```toml
members = ["crates/trix-core", "crates/trix-cli", "crates/trix-proto"]
```

- [ ] **Step 2: Write the failing test**

Create `crates/trix-proto/src/message.rs` containing **only** this test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// The exact bytes from spec §4.2. If these drift, every third-party UI
    /// written against the published spec breaks silently.
    #[test]
    fn response_matches_the_spec_examples_byte_for_byte() {
        let ok = Response::ok(7, serde_json::json!({"armed": true, "encoder": "NVENC H.264"}));
        assert_eq!(
            serde_json::to_string(&ok).unwrap(),
            r#"{"id":7,"ok":true,"data":{"armed":true,"encoder":"NVENC H.264"}}"#
        );

        let err = Response::err(7, "no hardware encoder available");
        assert_eq!(
            serde_json::to_string(&err).unwrap(),
            r#"{"id":7,"ok":false,"error":"no hardware encoder available"}"#,
            "the data key must be omitted on failure, not serialized as null"
        );
    }
}
```

- [ ] **Step 3: Run it to verify it fails**

Run: `cargo test -p trix-proto`
Expected: FAIL — `error: failed to load manifest` / `no such package`, because `crates/trix-proto/Cargo.toml` does not exist yet.

- [ ] **Step 4: Create the manifest**

`crates/trix-proto/Cargo.toml`:

```toml
[package]
name = "trix-proto"
version.workspace = true
edition.workspace = true

[dependencies]
serde.workspace = true
serde_json.workspace = true
```

Note what is absent: no `windows`, no `anyhow`, no `tracing`. This crate is what a third-party UI author compiles.

- [ ] **Step 5: Write the message types**

Prepend to `crates/trix-proto/src/message.rs`, above the test module:

```rust
//! The three things that travel over the control socket, plus the on-disk
//! clip metadata that rides inside several of them.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Longest accepted protocol line, including the newline. A client that opens
/// the pipe and never sends a `\n` must not be able to grow the daemon's read
/// buffer without bound.
pub const MAX_LINE_BYTES: usize = 1024 * 1024;

/// Response id used when a line was too malformed to recover its real id.
/// Clients must not send requests with this id.
pub const RESERVED_ID: u64 = 0;

/// A request as it arrives on the wire. Arguments are flattened alongside
/// `id` and `cmd`: `{"id":4,"cmd":"library.list","offset":0,"limit":50}`.
///
/// Untyped on purpose — see [`crate::Command::parse`]. An unknown `cmd` has to
/// produce an error *response*, and that needs the id, so the id is recovered
/// before anything can fail on the command itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub cmd: String,
    #[serde(flatten)]
    pub args: Map<String, Value>,
}

/// Exactly one per request. `data` and `error` are mutually exclusive and the
/// unused one is omitted entirely rather than sent as null.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    pub fn ok(id: u64, data: Value) -> Self {
        Self { id, ok: true, data: Some(data), error: None }
    }

    pub fn err(id: u64, error: impl Into<String>) -> Self {
        Self { id, ok: false, data: None, error: Some(error.into()) }
    }
}

/// Unsolicited. Never carries an id — an event is not a reply to anything.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub event: String,
    pub data: Value,
}

impl Event {
    pub fn new(event: impl Into<String>, data: Value) -> Self {
        Self { event: event.into(), data }
    }
}

/// One clip's metadata: the `.json` sidecar's contents verbatim, and the
/// payload of `clip_saved` and `library.list`.
///
/// Field order here is the field order on disk — keep it matching the spec.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipMeta {
    pub id: String,
    pub title: String,
    /// RFC 3339 with a local UTC offset, e.g. `2026-07-26T14:30:12+03:00`.
    pub created: String,
    pub duration_ms: u64,
    pub bytes: u64,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub encoder: String,
    pub has_audio: bool,
    #[serde(default)]
    pub favorite: bool,
}

/// Serializes one message and appends its newline.
///
/// Returns `Result` rather than panicking on purpose: the workspace builds
/// with `panic = "abort"`, so an unwrap here would take an armed replay ring
/// down with it.
pub fn encode_line<T: Serialize>(value: &T) -> serde_json::Result<String> {
    let mut line = serde_json::to_string(value)?;
    line.push('\n');
    Ok(line)
}

/// Parses one line into a request. `Err` means the line was not a request
/// object at all and no id could be recovered; the caller answers with
/// [`RESERVED_ID`].
pub fn decode_request(line: &str) -> Result<Request, String> {
    serde_json::from_str(line).map_err(|e| e.to_string())
}
```

- [ ] **Step 6: Create the crate root**

First create `crates/trix-proto/src/command.rs` as an empty file — `lib.rs` declares it and the crate will not compile without it. It is filled in at Step 10.

`crates/trix-proto/src/lib.rs`:

```rust
//! Wire types for the Trix control protocol.
//!
//! Pure serde — no windows-rs, no unsafe, no engine types. The daemon exposes
//! every capability it has over a named pipe speaking newline-delimited JSON;
//! this crate is the convenience layer for clients that happen to be written
//! in Rust. A UI in C#, Python, or TypeScript reimplements these definitions
//! and is in no way second-class.
//!
//! See `docs/superpowers/specs/2026-07-26-trix-desktop-ui-design.md` §4.

pub mod command;
pub mod message;

pub use command::{Command, DEFAULT_LIST_LIMIT, MAX_LIST_LIMIT};
pub use message::{
    ClipMeta, Event, MAX_LINE_BYTES, RESERVED_ID, Request, Response, decode_request, encode_line,
};
```

- [ ] **Step 7: Run the test to verify it passes**

Run: `cargo test -p trix-proto`
Expected: PASS — `test message::tests::response_matches_the_spec_examples_byte_for_byte ... ok`.

- [ ] **Step 8: Write the failing command-parse tests**

Append to `crates/trix-proto/src/command.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::decode_request;

    fn parse(line: &str) -> (u64, Result<Command, String>) {
        let req = decode_request(line).expect("line should decode as a request");
        (req.id, Command::parse(&req))
    }

    #[test]
    fn parses_the_spec_example() {
        let (id, cmd) = parse(r#"{"id":7,"cmd":"arm"}"#);
        assert_eq!(id, 7);
        assert_eq!(cmd.unwrap(), Command::Arm);
    }

    /// The whole reason parsing is two-stage: an unknown command must still
    /// leave the id available for the error response.
    #[test]
    fn unknown_command_fails_but_keeps_the_id() {
        let (id, cmd) = parse(r#"{"id":42,"cmd":"launch_missiles"}"#);
        assert_eq!(id, 42);
        assert!(cmd.unwrap_err().contains("launch_missiles"));
    }

    #[test]
    fn library_list_defaults_and_clamps() {
        let (_, cmd) = parse(r#"{"id":1,"cmd":"library.list"}"#);
        assert_eq!(cmd.unwrap(), Command::LibraryList { offset: 0, limit: DEFAULT_LIST_LIMIT });

        let (_, cmd) = parse(r#"{"id":1,"cmd":"library.list","offset":40,"limit":99999}"#);
        assert_eq!(
            cmd.unwrap(),
            Command::LibraryList { offset: 40, limit: MAX_LIST_LIMIT },
            "an unbounded limit would let one request materialize the entire library"
        );
    }

    /// A wrong-type page argument must be refused, not quietly replaced by the
    /// default — otherwise a UI's paging bug looks like an empty library
    /// rather than a mistake it can see and fix.
    #[test]
    fn a_wrong_type_page_argument_is_an_error_not_a_silent_default() {
        for line in [
            r#"{"id":1,"cmd":"library.list","offset":-5}"#,
            r#"{"id":1,"cmd":"library.list","offset":1.5}"#,
            r#"{"id":1,"cmd":"library.list","limit":"ten"}"#,
            r#"{"id":1,"cmd":"library.list","limit":null}"#,
        ] {
            let (_, cmd) = parse(line);
            let err = cmd.expect_err(&format!("{line} should be refused"));
            assert!(err.contains("library.list"), "error should name the command: {err}");
        }

        let (_, cmd) = parse(r#"{"id":1,"cmd":"library.list","offset":-5}"#);
        assert!(
            cmd.unwrap_err().contains("offset"),
            "the error should name which argument was wrong"
        );
    }

    #[test]
    fn missing_arguments_are_named_in_the_error() {
        let (_, cmd) = parse(r#"{"id":1,"cmd":"library.rename","id_":"x"}"#);
        let err = cmd.unwrap_err();
        assert!(err.contains("library.rename"), "error should name the command: {err}");
        assert!(err.contains("clip_id"), "error should name the missing argument: {err}");
    }

    #[test]
    fn stats_subscribe_carries_its_flag() {
        let (_, cmd) = parse(r#"{"id":1,"cmd":"stats.subscribe","enabled":true}"#);
        assert_eq!(cmd.unwrap(), Command::StatsSubscribe { enabled: true });
    }

    /// A request with no id cannot be answered in the normal shape at all.
    #[test]
    fn a_line_without_an_id_is_not_a_request() {
        assert!(decode_request(r#"{"cmd":"arm"}"#).is_err());
        assert!(decode_request("not json at all").is_err());
    }
}
```

- [ ] **Step 9: Run them to verify they fail**

Run: `cargo test -p trix-proto`
Expected: FAIL — `cannot find type Command in this scope` / `cannot find value DEFAULT_LIST_LIMIT`.

- [ ] **Step 10: Write the command parser**

Prepend to `crates/trix-proto/src/command.rs`:

```rust
//! The typed command set, parsed out of a [`Request`]'s loose argument bag.

use serde_json::Value;

use crate::message::Request;

/// Page size when `library.list` omits `limit`.
pub const DEFAULT_LIST_LIMIT: usize = 50;
/// Hard ceiling on `library.list`'s page size.
pub const MAX_LIST_LIMIT: usize = 500;

/// Every command the daemon answers. `library.export` is deliberately absent —
/// it arrives with trim support.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Status,
    Arm,
    Disarm,
    Clip,
    ConfigGet,
    ConfigSet(serde_json::Map<String, Value>),
    LibraryList { offset: usize, limit: usize },
    LibraryDelete { clip_id: String },
    LibraryRename { clip_id: String, title: String },
    LibraryFavorite { clip_id: String, favorite: bool },
    LibraryReveal { clip_id: String },
    MonitorsList,
    EncodersList,
    StatsSubscribe { enabled: bool },
}

impl Command {
    /// `Err` is the text of the error response — it is shown to whoever is
    /// driving the socket, so it names the command and the missing argument.
    pub fn parse(req: &Request) -> Result<Self, String> {
        let cmd = req.cmd.as_str();
        Ok(match cmd {
            "status" => Self::Status,
            "arm" => Self::Arm,
            "disarm" => Self::Disarm,
            "clip" => Self::Clip,
            "config.get" => Self::ConfigGet,
            "config.set" => {
                let values = match req.args.get("values") {
                    Some(Value::Object(map)) => map.clone(),
                    Some(_) => return Err("config.set: \"values\" must be an object".into()),
                    // Bare form: every argument other than id/cmd is a config key.
                    None => req.args.clone(),
                };
                if values.is_empty() {
                    return Err("config.set requires at least one key to set".into());
                }
                Self::ConfigSet(values)
            }
            "library.list" => Self::LibraryList {
                offset: usize_arg(req, "offset", 0)?,
                limit: usize_arg(req, "limit", DEFAULT_LIST_LIMIT)?.min(MAX_LIST_LIMIT),
            },
            "library.delete" => Self::LibraryDelete { clip_id: str_arg(req, "clip_id")? },
            "library.rename" => Self::LibraryRename {
                clip_id: str_arg(req, "clip_id")?,
                title: str_arg(req, "title")?,
            },
            "library.favorite" => Self::LibraryFavorite {
                clip_id: str_arg(req, "clip_id")?,
                favorite: bool_arg(req, "favorite")?,
            },
            "library.reveal" => Self::LibraryReveal { clip_id: str_arg(req, "clip_id")? },
            "monitors.list" => Self::MonitorsList,
            "encoders.list" => Self::EncodersList,
            "stats.subscribe" => Self::StatsSubscribe { enabled: bool_arg(req, "enabled")? },
            other => return Err(format!("unknown command {other:?}")),
        })
    }
}

fn str_arg(req: &Request, key: &str) -> Result<String, String> {
    req.args
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{} requires a string {key:?}", req.cmd))
}

fn bool_arg(req: &Request, key: &str) -> Result<bool, String> {
    req.args
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("{} requires a boolean {key:?}", req.cmd))
}

/// An absent argument means `default`; a present one that is not a
/// non-negative whole number is an error, matching [`str_arg`] and
/// [`bool_arg`]. Silently defaulting a bad page number would let a UI's paging
/// bug read as an empty library instead of a mistake.
///
/// `try_from` rather than `as`: on a 32-bit target `as` would wrap a huge
/// value into a small one instead of rejecting it.
fn usize_arg(req: &Request, key: &str, default: usize) -> Result<usize, String> {
    match req.args.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| {
                format!("{} requires {key:?} to be a non-negative whole number", req.cmd)
            }),
    }
}
```

**Note on the argument name `clip_id`, not `id`:** `id` is already the request's correlation id at the top level of the same JSON object, and the arguments are flattened into it. A clip argument called `id` would collide with it. The spec's `{id}` shorthand in §4.3 becomes `clip_id` on the wire; Task 8 amends the spec.

- [ ] **Step 11: Run all the tests**

Run: `cargo test -p trix-proto`
Expected: PASS, 7 tests, zero warnings.

- [ ] **Step 12: Verify the crate is genuinely platform-free**

Run: `cargo tree -p trix-proto`
Expected: the tree contains `serde`, `serde_json`, `itoa`, `memchr`, `ryu` and nothing named `windows`. If `windows` appears, a dependency leaked in — fix before committing.

- [ ] **Step 13: Commit**

```bash
git add Cargo.toml Cargo.lock crates/trix-proto
git commit -m "feat: add trix-proto, the control protocol's wire types"
```

---

## Task 2: Clip directory, ids, and JSON sidecars

**Files:**
- Modify: `crates/trix-core/Cargo.toml`
- Modify: `crates/trix-core/src/lib.rs`
- Modify: `crates/trix-core/src/config.rs`
- Create: `crates/trix-core/src/library.rs`
- Modify: `crates/trix-core/src/replay.rs:416-471` (`clip_path`, `save_clip`)
- Modify: `crates/trix-core/src/encode/h264.rs:40-98, 257-320`

**Interfaces:**
- Consumes: `trix_proto::ClipMeta` (Task 1).
- Produces:
  - `library::{allocate_clip_id, mp4_path, sidecar_path, thumb_path, write_sidecar, read_sidecar, scan, is_valid_id, now_rfc3339_local, format_rfc3339}`
  - `Config::clip_dir_path() -> PathBuf`, `Config::save() -> Result<()>`, `Config: Clone + Serialize`
  - `H264Encoder::name(&self) -> &str`
  - `replay::save_clip(&CaptureControl<…>, &Path, &str) -> Result<Option<ClipMeta>>`

This task changes where `trix replay` writes clips — from the working directory to `%USERPROFILE%\Videos\Trix` — and starts writing a sidecar next to every clip. Both are spec §5.1/§5.2 requirements, and both are behaviour the CLI gains too, deliberately: one clip-writing code path, not two.

- [ ] **Step 1: Write the failing tests**

Create `crates/trix-core/src/library.rs` containing **only**:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Istanbul is UTC+3; Windows reports that as a Bias of -180 minutes.
    #[test]
    fn rfc3339_renders_a_positive_offset() {
        assert_eq!(
            format_rfc3339(2026, 7, 26, 14, 30, 12, 180),
            "2026-07-26T14:30:12+03:00"
        );
    }

    #[test]
    fn rfc3339_renders_a_negative_and_a_zero_offset() {
        assert_eq!(format_rfc3339(2026, 1, 2, 3, 4, 5, -300), "2026-01-02T03:04:05-05:00");
        assert_eq!(format_rfc3339(2026, 1, 2, 3, 4, 5, 0), "2026-01-02T03:04:05+00:00");
        assert_eq!(
            format_rfc3339(2026, 1, 2, 3, 4, 5, 330),
            "2026-01-02T03:04:05+05:30",
            "half-hour zones are real (India, Newfoundland) and must not truncate"
        );
    }

    /// Clip ids arrive over the control socket and get concatenated into
    /// paths. This is the check that keeps them inside the clip directory.
    #[test]
    fn id_validation_rejects_anything_that_could_escape_the_clip_dir() {
        assert!(is_valid_id("20260726_143012"));
        assert!(is_valid_id("20260726_143012_2"));

        for bad in [
            "",
            "..",
            "../../Windows/System32/config/SAM",
            r"..\..\secrets",
            "20260726_143012/../x",
            r"C:\Windows\System32",
            r"\\server\share\x",
            "20260726",
            "2026072_143012",
            "20260726_14301",
            "20260726_143012_",
            "20260726_143012_a",
            "20260726_1430l2",
            "20260726_143012.mp4",
        ] {
            assert!(!is_valid_id(bad), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn ids_do_not_collide_within_one_second() {
        let dir = std::env::temp_dir().join(format!("trix-lib-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let _ = std::fs::remove_file(dir.join("20260726_143012.mp4"));

        std::fs::write(dir.join("20260726_143012.mp4"), b"x").unwrap();
        let next = next_free_id(&dir, "20260726_143012");
        assert_eq!(next, "20260726_143012_2");

        std::fs::write(dir.join("20260726_143012_2.mp4"), b"x").unwrap();
        assert_eq!(next_free_id(&dir, "20260726_143012"), "20260726_143012_3");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn scan_skips_orphaned_sidecars_and_adopts_bare_mp4s() {
        let dir = std::env::temp_dir().join(format!("trix-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // A complete clip.
        std::fs::write(dir.join("20260726_100000.mp4"), b"video").unwrap();
        write_sidecar(&dir, &sample_meta("20260726_100000")).unwrap();
        // A sidecar whose mp4 was deleted in Explorer: pruned, not listed.
        write_sidecar(&dir, &sample_meta("20260726_090000")).unwrap();
        // An mp4 with no sidecar: adopted so hand-dropped files still appear.
        std::fs::write(dir.join("20260726_110000.mp4"), b"video").unwrap();

        let clips = scan(&dir).unwrap();
        let ids: Vec<&str> = clips.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["20260726_110000", "20260726_100000"], "newest first");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn sample_meta(id: &str) -> trix_proto::ClipMeta {
        trix_proto::ClipMeta {
            id: id.to_string(),
            title: format!("clip_{id}"),
            created: "2026-07-26T10:00:00+03:00".into(),
            duration_ms: 15_000,
            bytes: 5,
            width: 1920,
            height: 1080,
            fps: 60,
            encoder: "test".into(),
            has_audio: true,
            favorite: false,
        }
    }
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p trix-core library`
Expected: FAIL to compile — `cannot find function format_rfc3339`, `unresolved import trix_proto`.

- [ ] **Step 3: Wire the dependency and the module**

In `crates/trix-core/Cargo.toml`, add to `[dependencies]`:

```toml
serde_json.workspace = true
trix-proto = { path = "../trix-proto" }
```

In `crates/trix-core/src/lib.rs`, add one line to the module list, keeping it alphabetical:

```rust
pub mod library;
```

`pub mod engine;` is added by Task 3, alongside the file it declares. Never declare a module in one task and create it in another — the tree must compile at every commit.

`crates/trix-core/tests/public_api.rs` guards the modules the daemon consumes. Add `library` to it in the same style as the existing entries, so demoting `pub mod library;` breaks the build rather than only the daemon crate.

- [ ] **Step 4: Write the library module**

Prepend to `crates/trix-core/src/library.rs`:

```rust
//! The clip library's on-disk format.
//!
//! One `.mp4` and one `.json` sidecar sharing a stem, flat in one directory.
//! Not a database: a crash mid-write costs one clip's metadata instead of the
//! library, deleting an `.mp4` in Explorer leaves an orphan that the next scan
//! prunes rather than a phantom row, and SQLite would be a C dependency and a
//! ~1 MB binary bump against a 1.5 MB engine. See spec §5.2.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use trix_proto::ClipMeta;
use windows::Win32::System::SystemInformation::{
    GetLocalTime, GetTimeZoneInformation, TIME_ZONE_ID_DAYLIGHT, TIME_ZONE_INFORMATION,
};

pub fn mp4_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.mp4"))
}

pub fn sidecar_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.json"))
}

/// Thumbnails are written by the daemon starting in stage 3; the path shape
/// is fixed here so deletion already removes them.
pub fn thumb_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.jpg"))
}

/// True if `id` is a syntactically valid clip id: `YYYYMMDD_HHMMSS`, with an
/// optional `_N` collision suffix.
///
/// Ids arrive from the control socket and are concatenated into filesystem
/// paths. Everything that turns an id into a path calls this first — a
/// whitelist of digits and two underscores cannot express `..`, a separator,
/// a drive letter, or a UNC prefix.
pub fn is_valid_id(id: &str) -> bool {
    let Some((date, rest)) = id.split_once('_') else { return false };
    let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if date.len() != 8 || !all_digits(date) {
        return false;
    }
    match rest.split_once('_') {
        None => rest.len() == 6 && all_digits(rest),
        Some((time, suffix)) => time.len() == 6 && all_digits(time) && all_digits(suffix),
    }
}

/// Picks an unused id for a clip being saved now, creating `dir` if needed.
pub fn allocate_clip_id(dir: &Path) -> Result<String> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("could not create clip directory {}", dir.display()))?;
    let now = unsafe { GetLocalTime() };
    let stem = format!(
        "{:04}{:02}{:02}_{:02}{:02}{:02}",
        now.wYear, now.wMonth, now.wDay, now.wHour, now.wMinute, now.wSecond
    );
    Ok(next_free_id(dir, &stem))
}

/// Two clips in the same second get `_2`, `_3`, … rather than one silently
/// overwriting the other.
fn next_free_id(dir: &Path, stem: &str) -> String {
    if !mp4_path(dir, stem).exists() {
        return stem.to_string();
    }
    for n in 2u32.. {
        let candidate = format!("{stem}_{n}");
        if !mp4_path(dir, &candidate).exists() {
            return candidate;
        }
    }
    unreachable!("u32 range is not exhaustible in one second")
}

/// Writes the sidecar to a temp file and renames it into place, so a crash
/// mid-write cannot leave half-parsed JSON beside a good MP4.
pub fn write_sidecar(dir: &Path, meta: &ClipMeta) -> Result<()> {
    let final_path = sidecar_path(dir, &meta.id);
    let temp_path = final_path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(meta).context("serializing clip metadata")?;
    std::fs::write(&temp_path, json)
        .with_context(|| format!("writing {}", temp_path.display()))?;
    // Windows rename fails if the destination exists; rewriting a sidecar
    // (rename, favorite) is a normal operation.
    let _ = std::fs::remove_file(&final_path);
    std::fs::rename(&temp_path, &final_path)
        .with_context(|| format!("renaming into {}", final_path.display()))
}

pub fn read_sidecar(path: &Path) -> Result<ClipMeta> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Every clip in `dir`, newest first.
///
/// An `.mp4` with no sidecar gets one synthesized from the filesystem, so a
/// file dropped in by hand still shows up. A `.json` with no `.mp4` is an
/// orphan and is skipped. A malformed sidecar is warned about and its clip is
/// adopted as if the sidecar were missing — one bad file never hides a clip.
pub fn scan(dir: &Path) -> Result<Vec<ClipMeta>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        // No directory yet simply means no clips yet.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };

    let mut clips = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("mp4") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|s| s.to_str()) else { continue };
        if !is_valid_id(id) {
            continue;
        }
        let sidecar = sidecar_path(dir, id);
        let meta = match read_sidecar(&sidecar) {
            Ok(meta) => meta,
            Err(e) if sidecar.exists() => {
                tracing::warn!(clip = id, error = %e, "unreadable sidecar, adopting the clip");
                adopt(dir, id)
            }
            Err(_) => adopt(dir, id),
        };
        clips.push(meta);
    }
    // Ids are zero-padded timestamps, so lexicographic order is chronological.
    clips.sort_by(|a, b| b.id.cmp(&a.id));
    Ok(clips)
}

/// Best-effort metadata for an `.mp4` with no usable sidecar. The fields that
/// need a demuxer to recover are left at zero rather than guessed.
fn adopt(dir: &Path, id: &str) -> ClipMeta {
    let bytes = std::fs::metadata(mp4_path(dir, id)).map(|m| m.len()).unwrap_or(0);
    ClipMeta {
        id: id.to_string(),
        title: format!("clip_{id}"),
        created: created_from_id(id),
        duration_ms: 0,
        bytes,
        width: 0,
        height: 0,
        fps: 0,
        encoder: String::new(),
        has_audio: false,
        favorite: false,
    }
}

/// `20260726_143012` -> `2026-07-26T14:30:12` with no offset claimed, since
/// an adopted file's original time zone is unknowable.
fn created_from_id(id: &str) -> String {
    let d = &id[..8];
    let t = &id[9..15];
    format!("{}-{}-{}T{}:{}:{}", &d[..4], &d[4..6], &d[6..8], &t[..2], &t[2..4], &t[4..6])
}

/// Local-time RFC 3339 stamp for a clip being saved now.
pub fn now_rfc3339_local() -> String {
    let now = unsafe { GetLocalTime() };
    let mut tz = TIME_ZONE_INFORMATION::default();
    // Windows defines UTC = local + Bias, so the ISO offset is the negation,
    // and the active seasonal bias has to be folded in or half the year is
    // reported an hour out.
    let id = unsafe { GetTimeZoneInformation(&mut tz) };
    let bias = tz.Bias
        + if id == TIME_ZONE_ID_DAYLIGHT { tz.DaylightBias } else { tz.StandardBias };
    format_rfc3339(
        now.wYear, now.wMonth, now.wDay, now.wHour, now.wMinute, now.wSecond, -bias,
    )
}

/// Split out from [`now_rfc3339_local`] so the offset arithmetic is testable
/// without running in a particular time zone.
pub fn format_rfc3339(
    year: u16,
    month: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    offset_minutes: i32,
) -> String {
    let sign = if offset_minutes < 0 { '-' } else { '+' };
    let abs = offset_minutes.unsigned_abs();
    format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}{sign}{:02}:{:02}",
        abs / 60,
        abs % 60,
    )
}
```

The test module references `next_free_id`, which is private — that is fine, the tests are in the same file.

- [ ] **Step 5: Run the library tests**

Run: `cargo test -p trix-core library`
Expected: PASS, 5 tests.

- [ ] **Step 6: Keep the encoder's friendly name**

In `crates/trix-core/src/encode/h264.rs`, the MFT's friendly name is read at activation and thrown away after logging. `status` and every clip's metadata need it.

Change `activate_hardware_encoder`'s signature and its final lines (currently `h264.rs:257` and `h264.rs:312-319`):

```rust
fn activate_hardware_encoder(device: &ID3D11Device) -> Result<(IMFTransform, String)> {
```

```rust
        let name = allocated_string(
            &activate.cast::<IMFAttributes>()?,
            &MFT_FRIENDLY_NAME_Attribute,
        )
        .unwrap_or_else(|| "<unnamed>".into());
        tracing::info!(encoder = %name, "hardware H.264 MFT activated");

        let transform = activate.ActivateObject::<IMFTransform>().context("ActivateObject")?;
        Ok((transform, name))
```

Add the field to the struct (`h264.rs:40`):

```rust
pub struct H264Encoder {
    transform: IMFTransform,
    events: IMFMediaEventGenerator,
    /// NeedInput credits granted by the MFT that we haven't spent yet.
    input_credits: u32,
    /// The MFT's friendly name, e.g. "Intel® Quick Sync Video H.264 Encoder MFT".
    /// Reported in `status` and stamped into every clip's metadata.
    name: String,
    pub frames_in: u64,
    pub packets_out: u64,
}
```

In `H264Encoder::new` (`h264.rs:62` and `h264.rs:96`):

```rust
            let (transform, name) = activate_hardware_encoder(device)?;
```

```rust
            Ok(Self { transform, events, input_credits: 0, name, frames_in: 0, packets_out: 0 })
```

And the accessor, in the same `impl` block:

```rust
    pub fn name(&self) -> &str {
        &self.name
    }
```

- [ ] **Step 7: Add `clip_dir` to the config**

In `crates/trix-core/src/config.rs`, change the derive on `Config` (line 18) to add `Clone` and `Serialize` — the daemon clones the config onto the engine thread and serializes it for `config.get`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
```

and the import on line 3:

```rust
use serde::{Deserialize, Serialize};
```

Add the field, after `stats_seconds`:

```rust
    /// Directory clips are written to. Empty (the default) resolves to
    /// `%USERPROFILE%\Videos\Trix`.
    pub clip_dir: String,
```

and to `Default::default()`:

```rust
            clip_dir: String::new(),
```

`deny_unknown_fields` is on, so this is purely additive: an existing `config.toml` without `clip_dir` still loads, and one written by the new code still loads in an old binary only if the key is absent — which is why the default is the empty string rather than a written-out path.

Add to `impl Config`:

```rust
    /// Resolved clip directory (spec §5.1). A flat, timestamped folder —
    /// manual arming means there is no game name to fold on, and a flat
    /// directory sorts correctly in Explorer for people who never open the UI.
    pub fn clip_dir_path(&self) -> PathBuf {
        let configured = self.clip_dir.trim();
        if !configured.is_empty() {
            return PathBuf::from(configured);
        }
        std::env::var_os("USERPROFILE")
            .map(|home| PathBuf::from(home).join("Videos").join("Trix"))
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// Writes the config back to `%APPDATA%\trix\config.toml`, creating the
    /// directory. Used by `config.set`.
    pub fn save(&self) -> Result<()> {
        let path = Self::path().context("APPDATA is not set")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("serializing config")?;
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))
    }
```

with `use anyhow::{Context as _, Result};` at the top of the file.

`toml::to_string_pretty` needs the `toml` crate's serialization support, which is on by default in `toml = "0.8"` — no manifest change.

- [ ] **Step 8: Write the failing config test**

Append to `crates/trix-core/src/config.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_dir_defaults_under_the_user_profile() {
        let config = Config::default();
        let path = config.clip_dir_path();
        assert!(path.ends_with(r"Videos\Trix"), "unexpected default: {}", path.display());
    }

    #[test]
    fn an_explicit_clip_dir_wins() {
        let config = Config { clip_dir: r"D:\Clips".into(), ..Config::default() };
        assert_eq!(config.clip_dir_path(), PathBuf::from(r"D:\Clips"));
    }

    /// `deny_unknown_fields` means a config written by a newer build must not
    /// be silently ignored — and a config from before `clip_dir` existed must
    /// still load.
    #[test]
    fn a_config_without_clip_dir_still_loads() {
        let config: Config = toml::from_str("fps = 30\nreplay_seconds = 20\n").unwrap();
        assert_eq!(config.fps, 30);
        assert_eq!(config.replay_seconds, 20);
        assert_eq!(config.clip_dir, "");
    }

    #[test]
    fn config_round_trips_through_toml() {
        let original = Config { clip_dir: r"D:\Clips".into(), fps: 30, ..Config::default() };
        let text = toml::to_string_pretty(&original).unwrap();
        let parsed: Config = toml::from_str(&text).unwrap();
        assert_eq!(parsed.fps, 30);
        assert_eq!(parsed.clip_dir, r"D:\Clips");
    }
}
```

- [ ] **Step 9: Run the config tests**

Run: `cargo test -p trix-core config`
Expected: PASS, 4 tests.

- [ ] **Step 10: Make `save_clip` produce metadata**

In `crates/trix-core/src/replay.rs`, delete `clip_path()` (lines 416-422) and replace `save_clip` (lines 445-471) with the following.

Note the return type. `ClipMeta` is the on-disk *and* wire format and must not grow fields to suit a console line, so `save_clip` returns a private struct carrying the metadata plus the three numbers only the CLI's output cares about. The engine handle hands its callers `saved.meta` and nothing else.

```rust
/// What one save produced: the metadata that goes on the wire and on disk,
/// plus the numbers only the console line cares about.
pub(crate) struct SavedClip {
    pub meta: ClipMeta,
    pub path: PathBuf,
    pub audio_secs: f64,
    pub packets: usize,
    pub mux_ms: u128,
}

/// Saves a clip and its sidecar. `None` means nothing is buffered yet — a
/// legitimate outcome moments after arming, not an error.
fn save_clip(
    capture: &CaptureControl<ReplaySession, anyhow::Error>,
    clip_dir: &Path,
    encoder_name: &str,
) -> Result<Option<SavedClip>> {
    let started = Instant::now();
    let snapshot = capture.callback().lock().snapshot_clip()?;
    let Some(snapshot) = snapshot else { return Ok(None) };

    let id = library::allocate_clip_id(clip_dir)?;
    let path = library::mp4_path(clip_dir, &id);
    write_clip(&snapshot, &path)?;

    let last = &snapshot.video[snapshot.video.len() - 1];
    let video_100ns = last.pts_100ns + last.duration_100ns - snapshot.base_pts;
    let audio_secs =
        snapshot.audio_pcm.len() as f64 / (SAMPLE_RATE * ENCODER_BLOCK_ALIGN) as f64;
    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

    let meta = ClipMeta {
        id: id.clone(),
        title: format!("clip_{id}"),
        created: library::now_rfc3339_local(),
        duration_ms: (video_100ns / 10_000).max(0) as u64,
        bytes,
        width: snapshot.settings.width,
        height: snapshot.settings.height,
        fps: snapshot.settings.fps,
        encoder: encoder_name.to_string(),
        has_audio: snapshot.settings.with_audio,
        favorite: false,
    };
    // A clip with no sidecar is still a clip — the next scan adopts it — so a
    // sidecar failure is logged, not propagated over the freshly written MP4.
    if let Err(e) = library::write_sidecar(clip_dir, &meta) {
        tracing::warn!(clip = %id, error = %format!("{e:#}"), "clip saved without a sidecar");
    }

    tracing::debug!(clip = %id, bytes, "clip written");

    Ok(Some(SavedClip {
        meta,
        path,
        audio_secs,
        packets: snapshot.video.len(),
        mux_ms: started.elapsed().as_millis(),
    }))
}

/// The console line `trix replay` has printed since Phase 5b. Kept in exactly
/// this shape — the verification workflow greps for it.
fn print_clip_line(saved: &SavedClip) {
    println!(
        "clip saved: {} ({:.1} s video / {:.1} s audio, {} packets, {:.1} MB, muxed in {} ms)",
        saved.path.display(),
        saved.meta.duration_ms as f64 / 1000.0,
        saved.audio_secs,
        saved.packets,
        saved.meta.bytes as f64 / (1024.0 * 1024.0),
        saved.mux_ms,
    );
}
```

Add to the imports at the top of `replay.rs` (`path::PathBuf` is already imported; add `Path` beside it):

```rust
use std::path::{Path, PathBuf};
use trix_proto::ClipMeta;
use crate::library;
```

- [ ] **Step 11: Update `save_clip`'s three call sites**

All three are in `run_session` (`replay.rs:600`, `replay.rs:623`) and take the new arguments. The clip directory and encoder name come from the config and the session:

```rust
    let clip_dir = config.clip_dir_path();
```

right after the `monitor` lines, and the encoder name read once after the capture starts:

```rust
    let encoder_name = capture.callback().lock().encoder.name().to_string();
```

Each call site becomes:

```rust
                match save_clip(&capture, &clip_dir, &encoder_name) {
                    Ok(Some(saved)) => print_clip_line(&saved),
                    Ok(None) => println!("nothing buffered yet — try again in a moment"),
                    Err(e) => {
                        tracing::error!("clip failed: {e:#}");
                        println!("clip failed: {e}");
                    }
                }
```

The `--auto-clip` path currently uses `save_clip(&capture)?` and must keep propagating its error (that is how a failed verification run fails loudly):

```rust
                        if let Some(saved) = save_clip(&capture, &clip_dir, &encoder_name)? {
                            print_clip_line(&saved);
                        }
```

- [ ] **Step 12: Build and run the whole suite**

Run: `cargo build --release`
Expected: success, zero warnings.

Run: `cargo test`
Expected: PASS — the 20 existing tests plus this task's 9.

- [ ] **Step 13: Verify with a real artifact**

**Check the session is unlocked first** — `Get-Process LogonUI` must find nothing. WGC silently stops delivering video frames on a locked workstation and the run will produce a 1-frame clip while still exiting 0.

Run: `./target/release/trix.exe replay --auto-clip 8 --exit-after 12`

Expected:
- The `clip saved:` line now shows a path under `%USERPROFILE%\Videos\Trix\`, named `20260727_HHMMSS.mp4` — no `clip_` prefix.
- `Videos\Trix\20260727_HHMMSS.json` exists beside it and contains all eleven `ClipMeta` keys, with `encoder` naming the real MFT, `width`/`height` matching the monitor, and `duration_ms` within a few hundred ms of 8000.
- `dropped=0` in the closing perf line.

- [ ] **Step 14: Commit**

```bash
git add crates/trix-core crates/trix-cli Cargo.toml Cargo.lock
git commit -m "feat: write clips to the library directory with JSON sidecars"
```

---

## Task 3: A controllable replay engine

**Files:**
- Create: `crates/trix-core/src/engine.rs`
- Modify: `crates/trix-core/src/replay.rs:482-655` (`run`, `run_rebuild_loop`, `run_session`)
- Modify: `crates/trix-core/src/control.rs:66-78` (single-instance guard)
- Modify: `crates/trix-cli/src/main.rs:84, 91`

**Interfaces:**
- Consumes: `library`, `ClipMeta`, `H264Encoder::name` (Task 2).
- Produces:
  - `engine::{EngineHandle, EngineCommand, EngineStatus}`
  - `EngineHandle::spawn(Config) -> Result<EngineHandle>`, `.status() -> EngineStatus`, `.clip() -> Result<Option<ClipMeta>>`, `.stop(self) -> Result<()>`
  - `control::SingleInstance` (RAII), `control::acquire_single_instance() -> Result<SingleInstance>`

**This is the highest-risk task in the plan.** `replay.rs` carries seven phases of verification. The change is deliberately narrow: the control loop's *input* becomes a command channel instead of a hotkey receiver, and both callers feed that channel. The capture callback, the pacer, eviction, audio pumping, snapshotting, and the rebuild policy are not touched.

- [ ] **Step 1: Turn the single-instance mutex into a guard**

In `crates/trix-core/src/control.rs`, replace `acquire_single_instance` (lines 61-78) with:

```rust
/// Holds the single-instance mutex. Dropping it releases the slot — which is
/// what lets the daemon take the encoder on `arm` and give it back on
/// `disarm`, instead of blocking the CLI for the daemon's whole lifetime.
#[must_use = "dropping the guard immediately releases the single-instance slot"]
pub struct SingleInstance(HANDLE);

impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

// SAFETY: a mutex HANDLE is process-wide and has no thread affinity; the guard
// is moved onto the daemon's state and dropped from whichever thread disarms.
unsafe impl Send for SingleInstance {}
unsafe impl Sync for SingleInstance {}

/// Refuses a second concurrent capture session: two would fight over the
/// hardware encoder and the clip hotkey. Session-local (`Local\`) so each
/// logged-in user gets their own slot.
pub fn acquire_single_instance() -> Result<SingleInstance> {
    let name = windows::core::HSTRING::from("Local\\trix-capture-single-instance");
    unsafe {
        let handle = CreateMutexW(None, false, &name).context("CreateMutexW")?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(handle);
            bail!(
                "another trix capture session (record or replay) is already \
                 running — stop it before starting a new one"
            );
        }
        Ok(SingleInstance(handle))
    }
}
```

Add `CloseHandle` and `HANDLE` to the `windows::Win32::Foundation` import on line 16.

**The `#[must_use]` is load-bearing.** `crates/trix-cli/src/main.rs` currently writes `control::acquire_single_instance()?;`, which under the new signature would create the mutex and drop it on the same line, silently disabling the guard. The attribute makes that a compiler warning rather than a runtime surprise.

- [ ] **Step 2: Bind the guard in the CLI**

`crates/trix-cli/src/main.rs`, lines 84 and 91, both become:

```rust
            let _single = control::acquire_single_instance()?;
```

`_single` rather than `_`: a binding named exactly `_` drops immediately.

- [ ] **Step 3: Verify the guard binding is not accidentally dropped**

Run: `cargo build 2>&1 | Select-String -Pattern "must_use|unused"`
Expected: no output. If `unused_must_use` fires, a call site is still discarding the guard.

- [ ] **Step 4: Write the failing engine test**

Create `crates/trix-core/src/engine.rs` containing only:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// The daemon's `status` response is built from these fields. Nothing else
    /// in the suite would notice one being dropped from the struct, and the
    /// daemon crate would only fail to compile — after this crate is green.
    #[test]
    fn status_defaults_are_the_disarmed_shape() {
        let status = EngineStatus::default();
        assert!(status.encoder.is_empty());
        assert_eq!(status.ring_seconds_used, 0.0);
        assert_eq!(status.ring_seconds_total, 0);
        assert_eq!((status.frames, status.dropped, status.paced), (0, 0, 0));
    }
}
```

- [ ] **Step 5: Run it to verify it fails**

Run: `cargo test -p trix-core engine`
Expected: FAIL — `cannot find type EngineStatus in this scope`.

- [ ] **Step 6: Write the engine module**

Prepend to `crates/trix-core/src/engine.rs`:

```rust
//! Driving the replay ring from something other than a terminal.
//!
//! [`crate::replay::run`] owns a loop that waits on the clip hotkey. The
//! daemon needs the same loop waiting on a control socket instead, so the
//! loop's input is a channel of [`EngineCommand`] and both callers feed it:
//! the CLI forwards hotkey presses, the daemon forwards `clip` requests.
//!
//! Everything downstream of that channel — the capture session, the fps
//! pacer, eviction, the rebuild policy — is the code that Phases 0–7
//! certified, unchanged.

use std::sync::{
    Arc, Mutex,
    mpsc::{Sender, channel},
};
use std::thread::JoinHandle;

use anyhow::{Context as _, Result, anyhow};
use trix_proto::ClipMeta;

use crate::{config::Config, replay};

/// What the control loop accepts. Both variants are terminal for the caller:
/// `Clip` always answers on its reply channel, `Stop` always ends the loop.
pub enum EngineCommand {
    /// Save a clip now. `Ok(None)` means nothing is buffered yet.
    Clip { reply: Sender<Result<Option<ClipMeta>>> },
    /// Leave the rebuild loop and shut the capture session down.
    Stop,
}

/// Live view of the engine, refreshed on the control loop's 250 ms tick.
/// Read under a mutex that the capture callback never touches.
#[derive(Debug, Clone, Default)]
pub struct EngineStatus {
    pub encoder: String,
    pub monitor_index: u32,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Seconds of footage currently held in the ring.
    pub ring_seconds_used: f64,
    /// Configured `replay_seconds` — what the ring fills toward.
    pub ring_seconds_total: u32,
    pub frames: u64,
    pub dropped: u64,
    pub paced: u64,
}

/// An armed engine running on its own thread.
pub struct EngineHandle {
    tx: Sender<EngineCommand>,
    status: Arc<Mutex<EngineStatus>>,
    join: Option<JoinHandle<Result<()>>>,
}

impl EngineHandle {
    /// Starts capture and returns once the first session is live, so a caller
    /// answering an `arm` command can report "no hardware encoder available"
    /// synchronously instead of optimistically claiming success.
    pub fn spawn(config: Config) -> Result<Self> {
        let (tx, rx) = channel();
        let (ready_tx, ready_rx) = channel::<Result<()>>();
        let status = Arc::new(Mutex::new(EngineStatus {
            monitor_index: config.monitor_index,
            fps: config.fps,
            ring_seconds_total: config.replay_seconds,
            ..EngineStatus::default()
        }));
        let thread_status = Arc::clone(&status);

        let join = std::thread::Builder::new()
            .name("trix-engine".into())
            .spawn(move || {
                replay::run_driven(&config, rx, thread_status, Some(ready_tx))
            })
            .context("failed to spawn the engine thread")?;

        // A failure before the first session starts arrives here; a failure
        // after it is handled by the rebuild loop and surfaces on stop().
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self { tx, status, join: Some(join) }),
            Ok(Err(e)) => Err(e),
            // The channel closed without a message: the thread ended before it
            // could report. Its own Result is the better error.
            Err(_) => match join.join() {
                Ok(Err(e)) => Err(e),
                Ok(Ok(())) => Err(anyhow!("engine thread ended before capture started")),
                Err(_) => Err(anyhow!("engine thread panicked during startup")),
            },
        }
    }

    pub fn status(&self) -> EngineStatus {
        self.status.lock().expect("engine status mutex poisoned").clone()
    }

    /// Saves a clip, blocking until the mux completes. `Ok(None)` means
    /// nothing is buffered yet.
    pub fn clip(&self) -> Result<Option<ClipMeta>> {
        let (reply, answer) = channel();
        self.tx
            .send(EngineCommand::Clip { reply })
            .map_err(|_| anyhow!("engine thread is gone"))?;
        answer.recv().map_err(|_| anyhow!("engine thread died while saving the clip"))?
    }

    /// Stops capture and joins the thread, surfacing whatever the rebuild loop
    /// finally returned.
    pub fn stop(mut self) -> Result<()> {
        let _ = self.tx.send(EngineCommand::Stop);
        match self.join.take() {
            Some(join) => join.join().map_err(|_| anyhow!("engine thread panicked"))?,
            None => Ok(()),
        }
    }
}

impl Drop for EngineHandle {
    /// A handle dropped without `stop()` must still stop capture — otherwise
    /// the encoder stays held and the next `arm` fails.
    fn drop(&mut self) {
        if let Some(join) = self.join.take() {
            let _ = self.tx.send(EngineCommand::Stop);
            let _ = join.join();
        }
    }
}
```

**Note:** `unwrap_err_or_else` in `spawn` is not a real method — the implementer writes the equivalent match. It is spelled out that way here only to show the intent: if the ready channel closed without a message, the thread's own `Result` is the better error.

- [ ] **Step 7: Generalize the replay control loop**

In `crates/trix-core/src/replay.rs`:

**First, resolve who prints.** `EngineCommand::Clip`'s reply carries `ClipMeta`, because that is what the daemon needs, but the CLI's console line needs `SavedClip`. Rather than widening the reply or giving the command a `print` flag, the *control loop* prints: it builds a `SavedClip` either way, sends `saved.meta` on the reply channel, and prints the console line when the options say to. `ReplayOptions` gains one field:

```rust
pub struct ReplayOptions {
    /// Testing hook: save a clip automatically N seconds after start.
    pub auto_clip_secs: Option<u64>,
    /// Testing hook: exit after N seconds instead of running forever.
    pub exit_after_secs: Option<u64>,
    /// Print the `clip saved:` console line. True for the CLI; false for the
    /// daemon, which reports clips as `clip_saved` events instead.
    pub print_clips: bool,
}
```

That makes the hotkey forwarder trivial — it never reads a reply, so it drops the receiver and the loop's `let _ = reply.send(…)` absorbs the closed channel. `run` keeps its exact signature and its existing behaviour:

```rust
pub fn run(config: &Config, options: ReplayOptions) -> Result<()> {
    if config.gpu_priority_low() {
        crate::capture::lower_gpu_priority();
    }
    let hotkey = control::Hotkey::parse(&config.clip_hotkey)
        .context("invalid clip_hotkey in config.toml")?;
    let hotkey_rx = control::start_hotkey(&hotkey)?;

    let (tx, rx) = channel();
    // The hotkey thread speaks `()`; the control loop speaks commands. One
    // forwarder bridges them. It discards each reply — the loop itself prints
    // the console line for the CLI.
    std::thread::Builder::new()
        .name("trix-hotkey-forward".into())
        .spawn(move || {
            while hotkey_rx.recv().is_ok() {
                let (reply, _discard) = channel();
                if tx.send(EngineCommand::Clip { reply }).is_err() {
                    return; // control loop is gone — session over
                }
            }
        })
        .context("failed to spawn the hotkey forwarder")?;

    let status = Arc::new(Mutex::new(EngineStatus::default()));
    let mut ready = None;
    let result = run_driven_inner(config, &rx, &status, &mut ready, Some(&hotkey), options);
    control::mark_finalized();
    result
}
```

`run_driven` is the daemon's entry point:

```rust
/// Runs the replay engine driven by a command channel rather than a hotkey.
/// The daemon's engine thread body.
pub fn run_driven(
    config: &Config,
    commands: Receiver<EngineCommand>,
    status: Arc<Mutex<EngineStatus>>,
    ready: Option<Sender<Result<()>>>,
) -> Result<()> {
    if config.gpu_priority_low() {
        crate::capture::lower_gpu_priority();
    }
    let mut ready = ready;
    let options = ReplayOptions {
        auto_clip_secs: None,
        exit_after_secs: None,
        print_clips: false,
    };
    let result = run_driven_inner(config, &commands, &status, &mut ready, None, options);
    control::mark_finalized();
    result
}
```

`run_rebuild_loop` is renamed `run_driven_inner` and changes only in its parameters:

- `hotkey_rx: &Receiver<()>` → `commands: &Receiver<EngineCommand>`
- gains `status: &Arc<Mutex<EngineStatus>>` and `ready: &mut Option<Sender<Result<()>>>`, both threaded straight through to `run_session`
- gains `hotkey: Option<&control::Hotkey>` — it is used only in the banner
- its `while hotkey_rx.try_recv().is_ok() {}` drain becomes `while commands.try_recv().is_ok() {}`; a `Clip` dropped during a rebuild answers nothing, and the sender's `recv()` returns `Err`, which `EngineHandle::clip` already reports as "engine thread died". Change that message to `"capture is rebuilding — try again in a moment"` so a user who clips during a display change gets the truth.

`run_session` changes in four places and nowhere else:

1. Signature: `hotkey_rx: &Receiver<()>` → `commands: &Receiver<EngineCommand>`; add `status`, `ready`, and `hotkey: Option<&control::Hotkey>`.
2. The banner (`replay.rs:578`) keeps its exact format; with no hotkey it drops the trailing clause:

```rust
    match hotkey {
        Some(hotkey) => println!(
            "replay buffer running: {}x{} at {} fps, {} kbps, last {} s kept — {} to clip, Ctrl+C to quit",
            width, height, config.fps, config.bitrate_kbps, config.replay_seconds, hotkey,
        ),
        None => tracing::info!(
            width, height, fps = config.fps, kbps = config.bitrate_kbps,
            replay_secs = config.replay_seconds, "replay buffer running"
        ),
    }
```

3. Immediately after `start_free_threaded` succeeds and `encoder_name` is read, signal readiness once:

```rust
    if let Some(tx) = ready.take() {
        let _ = tx.send(Ok(()));
    }
```

and on every early-return error path *before* that point, send the error instead — the simplest correct shape is to wrap the setup in a closure returning `Result` and, on `Err`, `ready.take()` and forward it before propagating.

4. The `recv_timeout` arm switches on the command:

```rust
        match commands.recv_timeout(Duration::from_millis(250)) {
            Ok(EngineCommand::Clip { reply }) => {
                let result = save_clip(&capture, &clip_dir, &encoder_name);
                if options.print_clips {
                    match &result {
                        Ok(Some(saved)) => print_clip_line(saved),
                        Ok(None) => println!("nothing buffered yet — try again in a moment"),
                        Err(e) => {
                            tracing::error!("clip failed: {e:#}");
                            println!("clip failed: {e}");
                        }
                    }
                }
                let _ = reply.send(result.map(|opt| opt.map(|saved| saved.meta)));
            }
            Ok(EngineCommand::Stop) => {
                break;
            }
            Err(RecvTimeoutError::Timeout) => { /* unchanged */ }
            Err(RecvTimeoutError::Disconnected) => break,
        }
```

5. In the existing `Timeout` branch, inside the block that already locks the handler for `idle_pump`, refresh the status snapshot — the lock is already held, so this costs nothing extra:

```rust
                {
                    let handler = capture.callback();
                    let mut session = handler.lock();
                    session.idle_pump();
                    session.report_if_due();
                    if let Ok(mut status) = status.lock() {
                        status.encoder = encoder_name.clone();
                        status.monitor_index = config.monitor_index;
                        status.width = width;
                        status.height = height;
                        status.fps = config.fps;
                        status.ring_seconds_total = config.replay_seconds;
                        status.ring_seconds_used = session.ring_span_secs();
                        status.frames = session.frames;
                        status.dropped = session.frames_dropped;
                        status.paced = session.frames_paced;
                    }
                }
```

with a small accessor on `ReplaySession`:

```rust
    /// Seconds of footage the ring currently holds.
    fn ring_span_secs(&self) -> f64 {
        match (self.video_ring.front(), self.video_ring.back()) {
            (Some(front), Some(back)) => {
                (back.pts_100ns - front.pts_100ns) as f64 / 10_000_000.0
            }
            _ => 0.0,
        }
    }
```

- [ ] **Step 8: Update `ReplayOptions` construction in the CLI**

`crates/trix-cli/src/main.rs:94` gains the new field:

```rust
                replay::ReplayOptions {
                    auto_clip_secs: auto_clip,
                    exit_after_secs: exit_after,
                    print_clips: true,
                },
```

- [ ] **Step 9: Run the full suite**

Run: `cargo test`
Expected: PASS. The existing `record_defaults_match_the_pre_split_binary` and `cli_surface_is_unchanged` tests must be untouched and still green.

- [ ] **Step 10: Verify the CLI path is behaviourally unchanged**

Run: `cargo build --release`, then (session unlocked, screen active):

`./target/release/trix.exe replay --auto-clip 8 --exit-after 12`

Expected — the same shape as Task 2's run:
- the `replay buffer running: …` banner appears with its exact former wording, including `Alt+F10 to clip, Ctrl+C to quit`
- the `clip saved:` line prints once with the library path
- `dropped=0`, latency `p50<=1.0ms`, and a closing perf line
- the sidecar exists and parses

A missing or reworded banner means the refactor changed CLI-visible behaviour — fix it rather than accepting it.

- [ ] **Step 11: Commit**

```bash
git add crates/trix-core crates/trix-cli
git commit -m "feat: drive the replay loop from a command channel"
```

---

## Task 4: The daemon and its named pipe

**Files:**
- Create: `crates/trix-daemon/Cargo.toml`
- Create: `crates/trix-daemon/src/main.rs`
- Create: `crates/trix-daemon/src/pipe.rs`
- Create: `crates/trix-daemon/src/clients.rs`
- Modify: `Cargo.toml` (root — members, default-members, windows features)

**Interfaces:**
- Consumes: `trix_proto::{Request, Response, Event, decode_request, encode_line, MAX_LINE_BYTES, RESERVED_ID}`.
- Produces: `pipe::{PIPE_NAME, serve}`, `clients::{Clients, ClientId}`, and a `trix-daemon.exe` that answers `status`.

At the end of this task the daemon answers exactly one real command. That is deliberate: it makes the transport — security descriptor, framing, error handling, multi-client fan-out — independently verifiable before any engine state is involved.

- [ ] **Step 1: Extend the workspace**

Root `Cargo.toml`:

```toml
members = ["crates/trix-core", "crates/trix-cli", "crates/trix-daemon", "crates/trix-proto"]
default-members = ["crates/trix-cli"]
```

`default-members` keeps bare `cargo run` and `cargo build` pointed at `trix.exe` now that the workspace has two binaries.

Add these to the `features` list under `[workspace.dependencies.windows]`, keeping it alphabetical:

```toml
    "Win32_Security",
    "Win32_Security_Authorization",
    "Win32_Storage_FileSystem",
    "Win32_System_Memory",
    "Win32_System_Pipes",
```

New *features* of an existing dependency — not a new dependency.

- [ ] **Step 2: Create the manifest**

`crates/trix-daemon/Cargo.toml`:

```toml
[package]
name = "trix-daemon"
version.workspace = true
edition.workspace = true

[[bin]]
name = "trix-daemon"
path = "src/main.rs"

[dependencies]
anyhow.workspace = true
serde_json.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
trix-core = { path = "../trix-core" }
trix-proto = { path = "../trix-proto" }
windows.workspace = true
```

- [ ] **Step 3: Write the failing test**

Create `crates/trix-daemon/src/pipe.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// The pipe name is part of the published protocol — third-party UIs hard
    /// code it. It does not get to drift.
    #[test]
    fn pipe_name_is_the_published_one() {
        assert_eq!(PIPE_NAME, r"\\.\pipe\trix-control");
    }

    /// The DACL is the whole security story: `D:P` protects it from inheriting
    /// a permissive default, and the single ACE grants the current user alone.
    #[test]
    fn the_security_descriptor_grants_only_this_user() {
        let sid = current_user_sid_string().expect("current user must have a SID");
        assert!(sid.starts_with("S-1-"), "unexpected SID form: {sid}");
        assert_eq!(sddl_for(&sid), format!("D:P(A;;GA;;;{sid})"));
    }

    #[test]
    fn a_line_longer_than_the_cap_is_refused_rather_than_buffered() {
        let mut source = std::io::Cursor::new(vec![b'x'; MAX_LINE_BYTES + 10]);
        let mut line = String::new();
        let result = read_line_capped(&mut source, &mut line);
        assert!(matches!(result, Err(LineError::TooLong)));
    }

    #[test]
    fn a_short_line_reads_back_without_its_newline() {
        let mut source = std::io::Cursor::new(b"{\"id\":1}\n".to_vec());
        let mut line = String::new();
        assert!(matches!(read_line_capped(&mut source, &mut line), Ok(true)));
        assert_eq!(line, r#"{"id":1}"#);
    }
}
```

- [ ] **Step 4: Run it to verify it fails**

Run: `cargo test -p trix-daemon`
Expected: FAIL — `cannot find value PIPE_NAME`, `cannot find function read_line_capped`.

- [ ] **Step 5: Write the pipe module**

Prepend to `crates/trix-daemon/src/pipe.rs`:

```rust
//! The control socket: `\\.\pipe\trix-control`.
//!
//! A named pipe rather than a localhost TCP socket because a listening port is
//! reachable by any local process — including any web page the user has open —
//! while pipe ACLs give real per-user isolation for free. The accepted tradeoff
//! is that a plain browser page cannot be a Trix UI; anything with a runtime
//! connects to a named pipe in one line. See spec §4.1.

use std::io::{BufReader, Read, Write};
use std::os::windows::io::FromRawHandle;
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow, bail};
use trix_proto::MAX_LINE_BYTES;
use windows::Win32::Foundation::{CloseHandle, ERROR_PIPE_CONNECTED, HANDLE, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{HSTRING, PWSTR};

/// The control socket's well-known name. Part of the published protocol.
pub const PIPE_NAME: &str = r"\\.\pipe\trix-control";

const PIPE_BUFFER_BYTES: u32 = 64 * 1024;

/// `D:P` — a *protected* DACL, so it does not inherit the permissive default —
/// carrying one ACE granting `GA` (all access) to this user and nobody else.
fn sddl_for(sid: &str) -> String {
    format!("D:P(A;;GA;;;{sid})")
}

/// The current process token's user SID in string form (`S-1-5-21-…`).
fn current_user_sid_string() -> Result<String> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
            .context("OpenProcessToken")?;
        let token = OwnedHandle(token);

        let mut needed = 0u32;
        // First call is expected to fail with ERROR_INSUFFICIENT_BUFFER; it is
        // how the required size is learned.
        let _ = GetTokenInformation(token.0, TokenUser, None, 0, &mut needed);
        if needed == 0 {
            bail!("GetTokenInformation reported a zero-length TokenUser");
        }
        let mut buffer = vec![0u8; needed as usize];
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            needed,
            &mut needed,
        )
        .context("GetTokenInformation(TokenUser)")?;

        let user = &*(buffer.as_ptr() as *const TOKEN_USER);
        let mut sid_string = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut sid_string).context("ConvertSidToStringSidW")?;
        let owned = sid_string.to_string().context("SID string was not valid UTF-16")?;
        let _ = LocalFree(Some(windows::Win32::Foundation::HLOCAL(sid_string.0.cast())));
        Ok(owned)
    }
}

/// A `PSECURITY_DESCRIPTOR` allocated by `Convert…W`, freed on drop.
struct LocalSecurityDescriptor(PSECURITY_DESCRIPTOR);

impl Drop for LocalSecurityDescriptor {
    fn drop(&mut self) {
        unsafe {
            let _ = LocalFree(Some(windows::Win32::Foundation::HLOCAL(self.0.0)));
        }
    }
}

/// A `HANDLE` closed on drop. Used for the token and for pipe instances that
/// fail before ownership moves into a `File`.
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn user_only_security_descriptor() -> Result<LocalSecurityDescriptor> {
    let sid = current_user_sid_string()?;
    let sddl = HSTRING::from(sddl_for(&sid));
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            &sddl,
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
        .context("ConvertStringSecurityDescriptorToSecurityDescriptorW")?;
    }
    Ok(LocalSecurityDescriptor(descriptor))
}

/// Creates one pipe instance. `first` adds `FILE_FLAG_FIRST_PIPE_INSTANCE`,
/// which is how a second daemon is refused: the flag fails if the name already
/// has an instance.
fn create_instance(
    security: &LocalSecurityDescriptor,
    first: bool,
) -> Result<std::fs::File> {
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: security.0.0,
        bInheritHandle: false.into(),
    };
    let open_mode =
        PIPE_ACCESS_DUPLEX | if first { FILE_FLAG_FIRST_PIPE_INSTANCE } else { Default::default() };

    let handle = unsafe {
        CreateNamedPipeW(
            &HSTRING::from(PIPE_NAME),
            open_mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            PIPE_BUFFER_BYTES,
            PIPE_BUFFER_BYTES,
            0,
            Some(&mut attributes),
        )
    };
    if handle.is_invalid() {
        let error = windows::core::Error::from_win32();
        if first {
            bail!(
                "could not create {PIPE_NAME} ({error}) — another trix daemon is \
                 probably already running"
            );
        }
        return Err(anyhow!("CreateNamedPipeW: {error}"));
    }
    Ok(unsafe { std::fs::File::from_raw_handle(handle.0.cast()) })
}

/// Why a read ended.
#[derive(Debug)]
pub enum LineError {
    /// The client sent more than [`MAX_LINE_BYTES`] without a newline.
    TooLong,
    Io(std::io::Error),
}

/// Reads one `\n`-terminated line, refusing to buffer past the cap.
///
/// `std::io::BufRead::read_line` grows without bound, which would let one
/// client that opens the pipe and never sends a newline exhaust the daemon's
/// memory.
pub fn read_line_capped<R: Read>(source: &mut R, line: &mut String) -> Result<bool, LineError> {
    line.clear();
    let mut bytes = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match source.read(&mut byte) {
            Ok(0) => {
                if bytes.is_empty() {
                    return Ok(false); // clean EOF
                }
                break; // final line without a trailing newline
            }
            Ok(_) => {
                if byte[0] == b'\n' {
                    break;
                }
                if bytes.len() >= MAX_LINE_BYTES {
                    return Err(LineError::TooLong);
                }
                bytes.push(byte[0]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(LineError::Io(e)),
        }
    }
    // A lone \r from a CRLF-writing client is not part of the JSON.
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    *line = String::from_utf8_lossy(&bytes).into_owned();
    Ok(true)
}
```

**Byte-at-a-time reads are correct but slow.** Wrap the `File` in a `BufReader` at the call site — `read_line_capped(&mut buffered, &mut line)` — so each syscall still fills 8 KB while the cap is enforced per line.

- [ ] **Step 6: Write the accept loop**

Append to `crates/trix-daemon/src/pipe.rs`:

```rust
/// Accepts clients forever, handing each to `handle` on its own thread.
///
/// One thread per client, blocking IO: a control socket sees a handful of
/// connections, and overlapped IO would be a large amount of unsafe code
/// bought for nothing.
pub fn serve<H>(handler: Arc<H>) -> Result<()>
where
    H: ClientHandler + Send + Sync + 'static,
{
    let security = user_only_security_descriptor()?;
    let mut first = true;
    loop {
        let instance = create_instance(&security, first)?;
        first = false;

        // ERROR_PIPE_CONNECTED means the client connected between creation and
        // this call — already connected, not an error.
        let connected = unsafe { ConnectNamedPipe(HANDLE(handle_of(&instance)), None) };
        if let Err(e) = connected {
            if e.code() != ERROR_PIPE_CONNECTED.to_hresult() {
                tracing::warn!(error = %e, "ConnectNamedPipe failed, retrying");
                continue;
            }
        }

        let handler = Arc::clone(&handler);
        std::thread::Builder::new()
            .name("trix-client".into())
            .spawn(move || {
                if let Err(e) = serve_one(instance, handler) {
                    tracing::debug!(error = %format!("{e:#}"), "client session ended");
                }
            })
            .context("failed to spawn a client thread")?;
    }
}
```

`handle_of` is a one-line helper: `fn handle_of(file: &std::fs::File) -> *mut std::ffi::c_void { file.as_raw_handle().cast() }`, with `use std::os::windows::io::AsRawHandle;`.

The `ClientHandler` trait is the seam Task 5 fills:

```rust
/// What the transport needs from the daemon: turn one request into one
/// response, and learn each client's id so events can be routed to it.
pub trait ClientHandler {
    fn client_connected(&self, out: std::sync::mpsc::Sender<String>) -> u64;
    fn client_disconnected(&self, client: u64);
    fn dispatch(&self, client: u64, request: &trix_proto::Request) -> trix_proto::Response;
}
```

And one client session:

```rust
/// One connected client: a read loop, and a writer thread fed by a channel.
///
/// The writer is a separate thread so an event broadcast to every client never
/// blocks behind one slow reader — a UI that stops draining its socket must
/// not be able to stall the daemon.
fn serve_one<H: ClientHandler>(instance: std::fs::File, handler: Arc<H>) -> Result<()> {
    let (out_tx, out_rx) = std::sync::mpsc::channel::<String>();
    let mut writer = instance.try_clone().context("cloning the pipe handle")?;
    let writer_thread = std::thread::Builder::new()
        .name("trix-client-out".into())
        .spawn(move || {
            for line in out_rx {
                if writer.write_all(line.as_bytes()).is_err() || writer.flush().is_err() {
                    return; // client hung up; the read loop will notice too
                }
            }
        })
        .context("failed to spawn a client writer thread")?;

    let client = handler.client_connected(out_tx.clone());
    let mut reader = BufReader::new(instance.try_clone().context("cloning the pipe handle")?);
    let mut line = String::new();

    loop {
        match read_line_capped(&mut reader, &mut line) {
            Ok(false) => break, // clean EOF
            Ok(true) => {}
            Err(LineError::TooLong) => {
                // The one case where a bad line does close the connection:
                // past the cap the remaining bytes cannot be resynchronized to
                // a frame boundary, so there is nothing to recover to.
                let response = Response::err(
                    RESERVED_ID,
                    format!("line exceeded {MAX_LINE_BYTES} bytes"),
                );
                if let Ok(text) = encode_line(&response) {
                    let _ = out_tx.send(text);
                }
                break;
            }
            Err(LineError::Io(e)) => {
                tracing::debug!(error = %e, "client read failed");
                break;
            }
        }

        if line.trim().is_empty() {
            continue; // a blank keepalive line is not a request
        }

        let response = match decode_request(&line) {
            Ok(request) => handler.dispatch(client, &request),
            // No id could be recovered, so the reply carries the reserved one.
            Err(e) => Response::err(RESERVED_ID, e),
        };
        match encode_line(&response) {
            Ok(text) => {
                if out_tx.send(text).is_err() {
                    break; // writer thread is gone
                }
            }
            Err(e) => tracing::error!(error = %e, "could not serialize a response"),
        }
    }

    handler.client_disconnected(client);
    drop(out_tx);
    let _ = writer_thread.join();
    unsafe {
        let _ = DisconnectNamedPipe(HANDLE(handle_of(&instance)));
    }
    Ok(())
}
```

- [ ] **Step 7: Write the client registry**

`crates/trix-daemon/src/clients.rs`:

```rust
//! Who is connected, and where events go.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;

use trix_proto::{Event, encode_line};

struct Client {
    id: u64,
    out: Sender<String>,
    /// Set by `stats.subscribe`. With nobody subscribed, nothing measures
    /// anything — which is what makes the `stats_seconds = 0` default
    /// coherent (spec §4.4).
    stats: bool,
}

#[derive(Default)]
pub struct Clients {
    inner: Mutex<Vec<Client>>,
    next_id: AtomicU64,
}

impl Clients {
    pub fn register(&self, out: Sender<String>) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        self.lock().push(Client { id, out, stats: false });
        id
    }

    pub fn unregister(&self, id: u64) {
        self.lock().retain(|client| client.id != id);
    }

    pub fn set_stats(&self, id: u64, enabled: bool) {
        if let Some(client) = self.lock().iter_mut().find(|c| c.id == id) {
            client.stats = enabled;
        }
    }

    /// True while at least one client wants `stats`. The stats thread checks
    /// this before doing any work.
    pub fn any_stats_subscribers(&self) -> bool {
        self.lock().iter().any(|client| client.stats)
    }

    pub fn broadcast(&self, event: &Event) {
        self.send_to(event, |_| true);
    }

    pub fn broadcast_stats(&self, event: &Event) {
        self.send_to(event, |client| client.stats);
    }

    fn send_to(&self, event: &Event, want: impl Fn(&Client) -> bool) {
        let Ok(line) = encode_line(event) else {
            tracing::error!(event = %event.event, "could not serialize an event");
            return;
        };
        // A failed send means that client's writer thread has exited; drop it
        // here rather than letting the registry accumulate dead entries.
        self.lock()
            .retain(|client| !want(client) || client.out.send(line.clone()).is_ok());
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Client>> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
```

The mutex is recovered from poisoning rather than unwrapping. Under `panic = "abort"` a poisoned lock cannot occur at all, so this costs nothing today — but `inner.lock().unwrap()` is a panicking call on a socket-reachable path, and the Global Constraints forbid those outright. Write it this way and the code stays correct if the profile ever changes.

Tests in the same file: a registered client receives a broadcast; an unregistered one does not; `broadcast_stats` reaches only subscribers; and a client whose receiver has been dropped is evicted from the registry after one failed broadcast.

- [ ] **Step 8: Write `main.rs`**

`crates/trix-daemon/src/main.rs` initializes `tracing_subscriber` exactly as the CLI does (`trix=info` default, `-v`/`RUST_LOG` honoured), loads the config, installs the shutdown handler, builds the handler, logs `listening on \\.\pipe\trix-control`, and calls `pipe::serve`. For this task the handler answers only `status`:

```json
{"armed":false,"encoder":null,"monitor_index":0,"ring_seconds_used":0.0,
 "ring_seconds_total":15,"version":"0.1.0","clip_dir":"C:\\Users\\…\\Videos\\Trix"}
```

`version` is `env!("CARGO_PKG_VERSION")`. Every other command returns the `Command::parse` error or `"not available until the engine is wired"` — Task 5 replaces that.

- [ ] **Step 9: Run the tests**

Run: `cargo test -p trix-daemon`
Expected: PASS, 6 tests (4 in `pipe`, 2 in `clients`).

- [ ] **Step 10: Verify the transport by hand**

Build: `cargo build --release`

Start the daemon in one terminal: `./target/release/trix-daemon.exe`
Expected: `listening on \\.\pipe\trix-control`.

In a second terminal:

```powershell
$pipe = New-Object System.IO.Pipes.NamedPipeClientStream('.', 'trix-control', 'InOut')
$pipe.Connect(5000)
$writer = New-Object System.IO.StreamWriter($pipe); $writer.AutoFlush = $true
$reader = New-Object System.IO.StreamReader($pipe)

$writer.WriteLine('{"id":1,"cmd":"status"}');        $reader.ReadLine()
$writer.WriteLine('{"id":2,"cmd":"launch_missiles"}'); $reader.ReadLine()
$writer.WriteLine('this is not json');                 $reader.ReadLine()
$writer.WriteLine('{"id":3,"cmd":"status"}');        $reader.ReadLine()
$pipe.Dispose()
```

Expected, in order:
1. `{"id":1,"ok":true,"data":{"armed":false,…,"version":"0.1.0",…}}`
2. `{"id":2,"ok":false,"error":"unknown command \"launch_missiles\""}`
3. `{"id":0,"ok":false,"error":"expected value at line 1 column 1"}` — id 0, the reserved value
4. `{"id":3,"ok":true,…}` — **the connection survived both bad lines**, which is the point of the exercise

Then start a second daemon: it must exit with the "another trix daemon is probably already running" message rather than silently stealing the name.

- [ ] **Step 11: Commit**

```bash
git add Cargo.toml Cargo.lock crates/trix-daemon
git commit -m "feat: add the trix-daemon control socket"
```

---

## Task 5: Arm, disarm, clip, and events

**Files:**
- Create: `crates/trix-daemon/src/state.rs`
- Create: `crates/trix-daemon/src/dispatch.rs`
- Modify: `crates/trix-daemon/src/main.rs`

**Interfaces:**
- Consumes: `EngineHandle` (Task 3), `Clients` + `ClientHandler` (Task 4).
- Produces: `state::Daemon`, and working `status` / `arm` / `disarm` / `clip`.

- [ ] **Step 1: Write the daemon state**

`crates/trix-daemon/src/state.rs`:

```rust
//! Everything the daemon owns, and the rules for changing it.

use std::sync::Mutex;

use anyhow::{Result, bail};
use trix_core::{config::Config, control::SingleInstance, engine::EngineHandle};
use trix_proto::ClipMeta;

use crate::clients::Clients;

/// The engine plus the single-instance slot it holds. Armed as a unit,
/// disarmed as a unit — dropping this releases the encoder *and* the slot, so
/// `trix replay` from a terminal works again the moment the daemon disarms.
struct Armed {
    engine: EngineHandle,
    _slot: SingleInstance,
}

pub struct Daemon {
    pub config: Mutex<Config>,
    pub clients: Clients,
    armed: Mutex<Option<Armed>>,
    /// The library, scanned once at startup and updated incrementally.
    /// `library.list` never touches the disk after that (spec §5.2).
    pub library: Mutex<Vec<ClipMeta>>,
}
```

with `arm()`, `disarm()`, `clip()`, and `status()` methods. `arm` is idempotent — arming an armed daemon returns the current state rather than erroring, because a UI reconnecting after a restart will call it. The lock ordering rule, which goes in a comment: `armed` is never held while `clients` is locked, so a broadcast can never deadlock against an arm.

- [ ] **Step 2: Write the failing tests**

In `state.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Disarming a daemon that was never armed is a no-op, not an error — a
    /// UI that reconnects and syncs its toggle must not see a spurious failure.
    #[test]
    fn disarm_when_idle_is_not_an_error() {
        let daemon = Daemon::new(Config::default());
        assert!(daemon.disarm().is_ok());
        assert!(!daemon.status().armed);
    }

    /// The status shape the UI binds to. Arming needs real hardware, so this
    /// covers only the disarmed branch — the armed branch is covered by the
    /// end-to-end gate in Task 8.
    #[test]
    fn idle_status_reports_config_not_engine_state() {
        let config = Config { replay_seconds: 30, monitor_index: 1, ..Config::default() };
        let daemon = Daemon::new(config);
        let status = daemon.status();
        assert!(!status.armed);
        assert_eq!(status.ring_seconds_total, 30);
        assert_eq!(status.monitor_index, 1);
        assert_eq!(status.ring_seconds_used, 0.0);
        assert!(status.encoder.is_none(), "a disarmed daemon has not chosen an encoder yet");
    }
}
```

- [ ] **Step 3: Run them to verify they fail, then implement**

Run: `cargo test -p trix-daemon state`
Expected: FAIL — `cannot find type Daemon`.

Then implement `Daemon`. `arm` acquires the single-instance slot *before* spawning the engine, so the "already running" message wins over a confusing encoder error when a CLI session holds the slot. On `EngineHandle::spawn` failing, the slot is dropped and the error text goes back verbatim — that is where `no hardware encoder on the capture adapter` reaches the UI.

- [ ] **Step 4: Wire the dispatcher**

`crates/trix-daemon/src/dispatch.rs` implements `ClientHandler` for `Arc<Daemon>`: `Command::parse` then one match arm per command. This task fills in `Status`, `Arm`, `Disarm`, `Clip`; everything else returns `"not implemented in this build"` until Tasks 6 and 7.

Events broadcast here, not inside `Daemon`, so the state type stays free of transport concerns:

| Trigger | Event | Data |
|---|---|---|
| `arm` succeeds (and was idle) | `armed` | the same object `status` returns |
| `disarm` succeeds (and was armed) | `disarmed` | `{}` |
| `clip` succeeds | `clip_saved` | the `ClipMeta` |
| `arm`/`clip` fails | `error` | `{"cmd":"…","error":"…"}` |

`clip_saved` must also fire for clips saved by something other than a `clip` command — nothing else can trigger one yet, but Task 3's hotkey path and plan 3's tray both will. Note it in a comment rather than building for it now.

`clip` on a disarmed daemon is an error response: `"not armed — send arm first"`.

- [ ] **Step 5: Build and run the suite**

Run: `cargo test`
Expected: PASS, all crates.

- [ ] **Step 6: Verify by hand**

Start the daemon, then in PowerShell (reusing the connection block from Task 4):

```powershell
$writer.WriteLine('{"id":1,"cmd":"arm"}');   $reader.ReadLine()
Start-Sleep -Seconds 8
$writer.WriteLine('{"id":2,"cmd":"clip"}');  $reader.ReadLine()
$writer.WriteLine('{"id":3,"cmd":"status"}'); $reader.ReadLine()
$writer.WriteLine('{"id":4,"cmd":"disarm"}'); $reader.ReadLine()
```

Expected:
- `arm` returns `ok:true` with the real encoder name, within about a second
- after 8 seconds `status` shows `ring_seconds_used` near 8 (not 0, not 15)
- `clip` returns a full `ClipMeta`, and `Videos\Trix\<id>.mp4` + `.json` exist
- `disarm` returns `ok:true`, and `trix.exe replay --exit-after 3` then runs — proving the single-instance slot was released
- while armed, `trix.exe replay` refuses with the existing "another trix capture session" message

Keep the clip. It is the stage-2 gate artifact and Task 8 references it.

- [ ] **Step 7: Commit**

```bash
git add crates/trix-daemon
git commit -m "feat: arm, disarm, and clip over the control socket"
```

---

## Task 6: The clip library over the socket

**Files:**
- Modify: `crates/trix-daemon/src/state.rs`
- Modify: `crates/trix-daemon/src/dispatch.rs`

**Interfaces:**
- Consumes: `library::{scan, is_valid_id, mp4_path, sidecar_path, thumb_path, write_sidecar}` (Task 2).
- Produces: `library.list` / `delete` / `rename` / `favorite` / `reveal`.

- [ ] **Step 1: Write the failing tests**

In `state.rs`. `fixture()` builds a `Daemon` whose `clip_dir` is a fresh temp directory holding three complete clips (`20260726_100000`, `_110000`, `_120000`), with the library already scanned into the cache:

```rust
    fn fixture(name: &str) -> (Daemon, PathBuf) {
        let dir = std::env::temp_dir().join(format!("trix-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for id in ["20260726_100000", "20260726_110000", "20260726_120000"] {
            std::fs::write(trix_core::library::mp4_path(&dir, id), b"video").unwrap();
            trix_core::library::write_sidecar(&dir, &meta(id)).unwrap();
        }
        let config = Config {
            clip_dir: dir.to_string_lossy().into_owned(),
            ..Config::default()
        };
        let daemon = Daemon::new(config);
        daemon.rescan_library().unwrap();
        (daemon, dir)
    }

    #[test]
    fn list_pages_newest_first_and_clamps_past_the_end() {
        let (daemon, dir) = fixture("list");

        let page = daemon.list(0, 2);
        assert_eq!(page.total, 3, "total is the unpaged count — the UI needs it for \"3 of 47\"");
        let ids: Vec<&str> = page.clips.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["20260726_120000", "20260726_110000"]);

        let page = daemon.list(2, 50);
        assert_eq!(page.clips.len(), 1, "a short final page, not an error");

        let page = daemon.list(99, 50);
        assert!(page.clips.is_empty(), "an offset past the end is empty, not a panic");
        assert_eq!(page.total, 3);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The security boundary. Every id-taking command runs through the same
    /// check; if one forgets, this catches it.
    #[test]
    fn every_id_command_rejects_a_traversal_attempt() {
        let (daemon, dir) = fixture("traversal");
        let canary = dir.join("canary.mp4");
        std::fs::write(&canary, b"must survive").unwrap();

        for evil in [
            "../../../Windows/System32/config/SAM",
            r"..\..\secrets",
            "x/../y",
            "canary",
            r"C:\Windows\System32\drivers\etc\hosts",
        ] {
            assert!(daemon.delete(evil).is_err(), "delete accepted {evil:?}");
            assert!(daemon.rename(evil, "x").is_err(), "rename accepted {evil:?}");
            assert!(daemon.set_favorite(evil, true).is_err(), "favorite accepted {evil:?}");
            assert!(daemon.reveal(evil).is_err(), "reveal accepted {evil:?}");
        }
        assert!(canary.exists(), "a rejected id must not have touched the filesystem");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rename_edits_the_title_and_never_moves_the_file() {
        let (daemon, dir) = fixture("rename");
        let id = "20260726_110000";

        daemon.rename(id, "Ace on Ascent").unwrap();

        let on_disk = trix_core::library::read_sidecar(
            &trix_core::library::sidecar_path(&dir, id),
        )
        .unwrap();
        assert_eq!(on_disk.title, "Ace on Ascent");
        assert_eq!(on_disk.id, id, "the id is the file stem and never changes");
        assert!(
            trix_core::library::mp4_path(&dir, id).exists(),
            "renaming edits metadata only — no collision logic, no broken handles"
        );

        let cached = daemon.list(0, 50);
        let entry = cached.clips.iter().find(|c| c.id == id).unwrap();
        assert_eq!(entry.title, "Ace on Ascent", "the cache must not go stale");

        assert!(daemon.rename(id, "   ").is_err(), "a blank title is not a rename");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn delete_removes_all_three_sidecar_files_and_the_cache_entry() {
        let (daemon, dir) = fixture("delete");
        let id = "20260726_100000";
        // Nothing writes thumbnails yet; deletion must already handle one.
        std::fs::write(trix_core::library::thumb_path(&dir, id), b"jpeg").unwrap();

        daemon.delete(id).unwrap();

        assert!(!trix_core::library::mp4_path(&dir, id).exists());
        assert!(!trix_core::library::sidecar_path(&dir, id).exists());
        assert!(!trix_core::library::thumb_path(&dir, id).exists());
        assert_eq!(daemon.list(0, 50).total, 2);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An id that is valid syntax but names no clip is a clean error, not a
    /// panic and not a silent success.
    #[test]
    fn an_unknown_but_wellformed_id_is_a_clean_error() {
        let (daemon, dir) = fixture("unknown");

        let err = daemon.delete("20991231_235959").unwrap_err().to_string();
        assert!(err.contains("20991231_235959"), "the error should name the clip: {err}");
        assert!(daemon.rename("20991231_235959", "x").is_err());
        assert_eq!(daemon.list(0, 50).total, 3, "a failed delete changed nothing");

        std::fs::remove_dir_all(&dir).unwrap();
    }
```

`meta(id)` is the same helper shape as Task 2's `sample_meta`. `Daemon::rescan_library` and `Daemon::list` returning `{clips, total, offset}` are part of this task's surface.

- [ ] **Step 2: Run them to verify they fail, then implement**

Run: `cargo test -p trix-daemon library`
Expected: FAIL — the methods do not exist.

Implementation notes the plan fixes rather than leaves to taste:

- **Scan once at startup**, in `main.rs`, into `Daemon::library`. `library.list` reads only the cache (spec §5.2). Log the clip count and the scan duration — plan 3 owes a measurement at 5,000 clips and this is the instrument.
- **`library.list`** returns `{"clips":[…],"total":N,"offset":O}`. `total` is the unpaged count; without it a UI cannot render "3 of 47".
- **`library.delete`** removes `.mp4`, `.json`, and `.jpg`, tolerating a missing `.jpg` (nothing writes one yet) and a missing `.json`. A missing `.mp4` is an error — that is the clip.
- **`library.rename`** edits `title` in the cache and rewrites the sidecar. The filename never moves. Reject an empty or whitespace-only title.
- **`library.favorite`** is the same path with the `favorite` flag.
- **`library.reveal`** runs `explorer.exe /select,<path>`. Build it with `std::process::Command::new("explorer.exe").arg("/select,").arg(path)` — never a formatted shell string. Explorer returns a nonzero exit code on success, so do not treat the status as failure; check only that the process spawned.
- Each mutation broadcasts nothing in this task; the UI that issued the command has the response. Plan 4 decides whether other clients need `library_changed`.

- [ ] **Step 3: Run the suite and verify by hand**

Run: `cargo test`

Then, with the daemon running and at least two clips in the library:

```powershell
$writer.WriteLine('{"id":1,"cmd":"library.list","offset":0,"limit":10}'); $reader.ReadLine()
$writer.WriteLine('{"id":2,"cmd":"library.rename","clip_id":"<id>","title":"Ace"}'); $reader.ReadLine()
$writer.WriteLine('{"id":3,"cmd":"library.favorite","clip_id":"<id>","favorite":true}'); $reader.ReadLine()
$writer.WriteLine('{"id":4,"cmd":"library.reveal","clip_id":"<id>"}'); $reader.ReadLine()
$writer.WriteLine('{"id":5,"cmd":"library.delete","clip_id":"../../../boot.ini"}'); $reader.ReadLine()
```

Expected: the list is newest-first with a `total`; the renamed clip's `.json` on disk shows the new title while the filename is unchanged; Explorer opens with the clip selected; the traversal attempt returns `ok:false` with an id-validation error and **no** file is touched.

- [ ] **Step 4: Commit**

```bash
git add crates/trix-daemon
git commit -m "feat: serve the clip library over the control socket"
```

---

## Task 7: Config, hardware enumeration, and stats

**Files:**
- Modify: `crates/trix-core/src/probe.rs`
- Modify: `crates/trix-daemon/src/state.rs`
- Modify: `crates/trix-daemon/src/dispatch.rs`
- Modify: `crates/trix-daemon/src/main.rs`

**Interfaces:**
- Consumes: `Config::save` (Task 2), `EngineStatus` (Task 3), `Clients::broadcast_stats` (Task 4).
- Produces: `probe::{monitors, encoders, MonitorInfo, EncoderInfo}`, and `config.get` / `config.set` / `monitors.list` / `encoders.list` / `stats.subscribe`.

- [ ] **Step 1: Extract the probe's data from its printing**

`probe.rs` today enumerates and prints in one pass. The settings dropdowns need the data. Split each function:

```rust
/// One desktop-attached monitor, in the same index order `monitor_index` uses.
#[derive(Debug, Clone, Serialize)]
pub struct MonitorInfo {
    pub index: u32,
    pub name: String,
    pub width: i32,
    pub height: i32,
    pub left: i32,
    pub top: i32,
    pub adapter: String,
}

/// One encoder MFT, as offered to the settings UI.
#[derive(Debug, Clone, Serialize)]
pub struct EncoderInfo {
    pub codec: String,
    pub name: String,
    pub hardware: bool,
}

pub fn monitors() -> Result<Vec<MonitorInfo>>;
pub fn encoders() -> Result<Vec<EncoderInfo>>;
```

`print_monitors` and `print_encoders` are then thin loops over those. **`trix probe`'s stdout must not change** — same lines, same order, same spacing. Verify by capturing the output before and after and diffing:

```powershell
./target/release/trix.exe probe > before.txt   # on the previous commit
./target/release/trix.exe probe > after.txt
Compare-Object (Get-Content before.txt) (Get-Content after.txt)
```

Expected: no output from `Compare-Object` except the VRAM line if a background process shifted it — there is none; the fields are static.

Note that `monitors()` must produce the same index space `config.monitor_index` uses, which `replay.rs:549` derives via `Monitor::from_index(index + 1)`. If DXGI's enumeration order and windows-capture's disagree, the settings UI would silently point at the wrong screen. Assert they agree by comparing `monitors()[i]`'s width/height against `Monitor::from_index(i+1)` for every index, in a test that skips gracefully when only one monitor is attached.

- [ ] **Step 2: Implement `config.get` / `config.set`**

`config.get` returns the effective config as JSON, plus `clip_dir_resolved` so a UI can show the real path when `clip_dir` is empty.

`config.set` takes a map of keys, applies the known ones, rejects unknown ones by name, persists via `Config::save()`, and returns:

```json
{"accepted": {"fps": 30}, "requires_rearm": ["fps"]}
```

The re-arm set is fixed and belongs in one named constant with a comment: `fps`, `bitrate_kbps`, `max_bitrate_kbps`, `rate_control`, `replay_seconds`, `monitor_index`, `gpu_priority` — everything baked into `RecorderSettings` or the capture session at arm time. `clip_dir`, `stats_seconds`, and `clip_hotkey` take effect immediately.

Applying to a *live* engine is out of scope: `config.set` writes the file and reports which keys need a re-arm; the UI decides whether to prompt. Do not silently re-arm.

- [ ] **Step 3: Implement `stats.subscribe`**

`stats.subscribe {enabled: true}` flips the client's flag in the registry. A stats thread in `main.rs` wakes on the config's `stats_seconds` interval (defaulting to 1 second when it is 0 *and* at least one subscriber exists) and broadcasts `stats` with `EngineStatus`'s counters plus the memory numbers from `trix_core::stats`.

This is what makes the `stats_seconds = 0` default coherent: with no UI attached, nothing measures anything (spec §4.4).

The thread must not run while nothing is subscribed and nothing is armed — check both before doing any work.

- [ ] **Step 4: Tests**

- `config.set` with an unknown key names it in the error and changes nothing on disk
- `config.set {"fps": 30}` reports `requires_rearm: ["fps"]`
- `config.set {"clip_dir": "…"}` reports an empty `requires_rearm`
- `config.get` round-trips through `Config` without losing a key — assert the returned object's key set equals `Config::default()`'s serialized key set plus `clip_dir_resolved`, so a future config key added without touching the daemon still appears
- the monitor index-space agreement test from Step 1

- [ ] **Step 5: Run the suite and verify by hand**

```powershell
$writer.WriteLine('{"id":1,"cmd":"config.get"}');                       $reader.ReadLine()
$writer.WriteLine('{"id":2,"cmd":"monitors.list"}');                    $reader.ReadLine()
$writer.WriteLine('{"id":3,"cmd":"encoders.list"}');                    $reader.ReadLine()
$writer.WriteLine('{"id":4,"cmd":"config.set","values":{"fps":30}}');   $reader.ReadLine()
$writer.WriteLine('{"id":5,"cmd":"config.set","values":{"nope":1}}');   $reader.ReadLine()
$writer.WriteLine('{"id":6,"cmd":"stats.subscribe","enabled":true}');   $reader.ReadLine()
```

Expected: `monitors.list` matches what `trix probe` prints; `encoders.list` names the real MFTs; `config.set` writes `%APPDATA%\trix\config.toml` and the unknown key is refused by name; after subscribing and arming, `stats` event lines arrive unprompted on the same connection.

Then set `fps` back to 60.

- [ ] **Step 6: Commit**

```bash
git add crates/trix-core crates/trix-daemon
git commit -m "feat: serve config, hardware enumeration, and stats"
```

---

## Task 8: The stage-2 gate and documentation

**Files:**
- Create: `scripts/protocol-smoke.ps1`
- Modify: `PLAN.md`
- Modify: `docs/superpowers/specs/2026-07-26-trix-desktop-ui-design.md`

- [ ] **Step 1: Write the gate script**

`scripts/protocol-smoke.ps1` drives spec §10 stage 2 end to end and exits nonzero on any failure. It must:

1. Refuse to run if `Get-Process LogonUI` finds anything — a locked session makes WGC stop delivering video frames while the run still exits 0 with a one-frame clip. This has already cost one full task cycle on this project; the check is not optional.
2. Start `trix-daemon.exe` if it is not running, and remember whether it started it (so it stops only its own).
3. Connect, `arm`, assert `ok:true` and a non-empty encoder name.
4. Wait 10 seconds, `status`, assert `ring_seconds_used` is between 5 and `replay_seconds`.
5. `clip`, assert `ok:true`, capture the returned id.
6. Assert `<clip_dir>\<id>.mp4` exists and is over 100 KB, and `<id>.json` parses with all eleven keys, `duration_ms` within 20% of `replay_seconds * 1000`, and `encoder` matching what `arm` reported.
7. `library.list`, assert the new id is present and first.
8. `disarm`, assert `ok:true`.
9. Assert `trix.exe replay --exit-after 3` now succeeds — the single-instance slot came back.
10. Print a PASS/FAIL summary naming the clip path, so the user has something to play.

- [ ] **Step 2: Run the gate**

Run: `powershell -ExecutionPolicy Bypass -File scripts/protocol-smoke.ps1`
Expected: every check PASS, and a clip path printed.

**Play the clip.** It must have video and audio in sync, exactly like a CLI clip. This is the hand-verification the stage is gated on; a green script is not the gate.

- [ ] **Step 3: Update PLAN.md**

§1.4's module layout gains `trix-proto` and `trix-daemon` with one line each. §5's progress log gains a "Control protocol (2026-07-27) — Stage 2 of the desktop UI spec" entry recording the gate result with the real numbers, the `clip_dir` behaviour change for the CLI, and the panic decision:

> `panic = "abort"` kept for the daemon, decided with measurements rather than by default. `unwind` costs 685 KB against a 1,565,696-byte binary (+44%) and cannot protect the capture path anyway — `windows-capture` calls the frame handler across an `extern "system"` boundary, and Rust aborts rather than unwinding through foreign frames regardless of the profile setting. The exposure it would have covered is socket-facing code, which is instead required to be panic-free by construction.

- [ ] **Step 4: Amend the spec**

Three edits to `docs/superpowers/specs/2026-07-26-trix-desktop-ui-design.md`:

1. §4.2 — document `id: 0` as reserved for responses to unparseable lines.
2. §4.3 — add the `stats.subscribe` row, and change the `{id}` argument shorthand to `{clip_id}` with a sentence saying why (`id` is taken by the request's correlation id in the same flattened object).
3. §4.3 — mark `library.export` as arriving in stage 4 so the table is not read as a description of this build.

- [ ] **Step 5: Commit**

```bash
git add scripts PLAN.md docs
git commit -m "docs: record the stage 2 protocol gate"
```

---

## Verification summary

What the user hand-verifies before plan 3 begins, per spec §10 stage 2:

| # | Check | How |
|---|---|---|
| 1 | `trix replay` is unchanged apart from where clips land | Task 3 Step 10 — banner wording, `clip saved:` format, `dropped=0` |
| 2 | A scripted client drives `arm` → `clip` → `library.list` → `disarm` | `scripts/protocol-smoke.ps1` |
| 3 | The clip is on disk with a valid sidecar | Same script, steps 6–7 |
| 4 | **The clip plays, with audio in sync** | By hand — the only check no script can make |
| 5 | The pipe's DACL names this user and nobody else | `(Get-Acl \\.\pipe\trix-control).Sddl` while the daemon runs — expect `D:P(A;;GA;;;S-1-5-21-…)` with one ACE and no `BU`/`WD`/`AU` entry. If a second local account exists, additionally `runas /user:<other>` a client and confirm the connect is denied. |
| 6 | A malformed client cannot take the daemon down | Task 4 Step 10 — bad lines, then a good one on the same connection |
