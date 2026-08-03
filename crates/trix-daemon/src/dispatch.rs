//! One request in, one response out — plus the events those requests cause.
//!
//! Events are broadcast here rather than inside [`Daemon`] for two reasons.
//! The state type stays free of transport concerns, and — the load-bearing
//! one — every broadcast happens *after* the state method has returned and
//! released the `armed` lock, which is what makes the lock ordering documented
//! in `state.rs` true by construction rather than by discipline.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::SyncSender;

use serde_json::{Map, Value};
use trix_proto::{ClipMeta, Command, Event, Request, Response};

use crate::clients::ClientId;
use crate::pipe::ClientHandler;
use crate::state::Daemon;

/// What `clip` answers when the ring holds no footage yet. Byte-identical to
/// what `trix replay` prints for the same condition (`trix-core`'s
/// `replay.rs`): the daemon and the CLI must not describe the same situation
/// two different ways.
const NOTHING_BUFFERED: &str = "nothing buffered yet — try again in a moment";

/// Implemented for `Daemon` rather than for `Arc<Daemon>` as the brief's sketch
/// had it: `pipe::serve` already takes an `Arc<H>`, so implementing the trait
/// on the `Arc` would make the daemon an `Arc<Arc<Daemon>>` at the call site
/// for no gain.
impl ClientHandler for Daemon {
    fn client_connected(&self, out: SyncSender<String>) -> ClientId {
        self.clients.register(out)
    }

    fn client_disconnected(&self, client: ClientId) {
        self.clients.unregister(client);
    }

    /// Lets the transport see an eviction the registry has already performed,
    /// so the connection ends rather than lingering as an event-deaf socket.
    fn eviction_flag(&self, client: ClientId) -> Option<Arc<AtomicBool>> {
        self.clients.eviction_flag(client)
    }

    /// `client` identifies the connection that sent this request. Only
    /// `stats.subscribe` cares — it is a per-connection flag, not daemon state,
    /// because two UIs must be able to disagree about whether they want a
    /// 1 Hz event stream.
    fn dispatch(&self, client: ClientId, request: &Request) -> Response {
        match Command::parse(request) {
            Ok(Command::Status) => Response::ok(request.id, self.status().to_json()),
            Ok(Command::Arm) => arm(self, request.id),
            Ok(Command::Disarm) => disarm(self, request.id),
            Ok(Command::Clip) => clip(self, request.id),
            Ok(Command::LibraryList { offset, limit }) => {
                library_list(self, request.id, offset, limit)
            }
            // Every one of these takes a clip id straight off the wire.
            // `Daemon` validates it before it can reach a path — see
            // `Daemon::paths_for` — so there is nothing to check here, and
            // deliberately so: a check in the dispatcher is one a second caller
            // of the same method would not get.
            Ok(Command::LibraryDelete { clip_id }) => {
                acknowledge(request.id, &clip_id, self.delete(&clip_id))
            }
            Ok(Command::LibraryRename { clip_id, title }) => {
                updated_clip(request.id, self.rename(&clip_id, &title))
            }
            Ok(Command::LibraryFavorite { clip_id, favorite }) => {
                updated_clip(request.id, self.set_favorite(&clip_id, favorite))
            }
            Ok(Command::LibraryReveal { clip_id }) => {
                acknowledge(request.id, &clip_id, self.reveal(&clip_id))
            }
            Ok(Command::ConfigGet) => match self.config_json() {
                Ok(data) => Response::ok(request.id, data),
                Err(e) => Response::err(request.id, format!("{e:#}")),
            },
            Ok(Command::ConfigSet(values)) => config_set(self, request.id, &values),
            // `monitors_true_pixels`, not `monitors`: DXGI's sizes are
            // DPI-virtualised and a settings dropdown must offer the resolution
            // clips actually come out at. `trix probe` keeps using `monitors`,
            // because its stdout is a frozen contract.
            Ok(Command::MonitorsList) => match trix_core::probe::monitors_true_pixels() {
                Ok(found) => named_array(request.id, "monitors", serde_json::to_value(&found)),
                Err(e) => Response::err(request.id, format!("{e:#}")),
            },
            Ok(Command::EncodersList) => match trix_core::probe::encoders() {
                Ok(found) => named_array(request.id, "encoders", serde_json::to_value(&found)),
                Err(e) => Response::err(request.id, format!("{e:#}")),
            },
            Ok(Command::StatsSubscribe { enabled }) => {
                self.clients.set_stats(client, enabled);
                let mut fields = Map::new();
                fields.insert("enabled".to_string(), Value::Bool(enabled));
                Response::ok(request.id, Value::Object(fields))
            }
            // No catch-all arm. Every `Command` variant is answered here now,
            // so the match is exhaustive and the compiler — not a reviewer —
            // is what stops a command added to `trix-proto` later from
            // silently falling through to a generic error.
            Err(error) => Response::err(request.id, error),
        }
    }
}

/// `{"accepted":{…},"requires_rearm":[…]}` (spec §4.3).
fn config_set(daemon: &Daemon, id: u64, values: &Map<String, Value>) -> Response {
    match daemon.set_config(values) {
        Ok(update) => {
            let mut fields = Map::new();
            fields.insert("accepted".to_string(), Value::Object(update.accepted));
            fields.insert(
                "requires_rearm".to_string(),
                Value::Array(update.requires_rearm.into_iter().map(Value::from).collect()),
            );
            Response::ok(id, Value::Object(fields))
        }
        // No `error` event. Spec §4.4 broadcasts one for `arm` and `clip`,
        // whose failure changes what every other client can expect to happen
        // next; a refused settings write concerns only the client that sent it.
        Err(e) => Response::err(id, format!("{e:#}")),
    }
}

/// `{"monitors":[…]}` and `{"encoders":[…]}` — the same "named array" shape
/// `library.list` uses for `clips`, so a client never has to tell a bare array
/// from an object.
///
/// Takes the already-serialized list rather than the `Vec`: a generic
/// `T: Serialize` parameter would need `serde` as a direct dependency of this
/// crate, which it does not otherwise have. `serde_json::to_value` is called at
/// the two call sites, where the concrete type is known and inference does the
/// work.
///
/// The enumerations themselves run on the calling client's own session thread,
/// and both talk to real hardware — DXGI for monitors, `MFTEnumEx` for
/// encoders. That is deliberate: a settings page opens rarely, the calls take
/// milliseconds, and caching them would mean serving a stale monitor list to
/// the one user who just plugged a screen in. A slow enumeration costs that one
/// connection its own latency and nothing else, because `pipe::serve` gives
/// every client a thread.
fn named_array(id: u64, key: &str, serialized: serde_json::Result<Value>) -> Response {
    match serialized {
        Ok(value) => {
            let mut fields = Map::new();
            fields.insert(key.to_string(), value);
            Response::ok(id, Value::Object(fields))
        }
        // `MonitorInfo` and `EncoderInfo` are strings, integers and bools, so
        // this cannot actually happen — but `to_value` returns a `Result` and a
        // `panic = "abort"` build has no room for an `unwrap` on a path a
        // socket message reaches directly.
        Err(e) => {
            tracing::error!(list = key, error = %e, "could not serialize a hardware list");
            Response::err(id, format!("the {key} list could not be serialized"))
        }
    }
}

fn arm(daemon: &Daemon, id: u64) -> Response {
    match daemon.arm() {
        Ok(outcome) => {
            let data = outcome.status.to_json();
            if outcome.newly_armed {
                daemon.clients.broadcast(&Event::new("armed", data.clone()));
            }
            Response::ok(id, data)
        }
        // `{e:#}` so anyhow's context chain survives — this is where
        // "no hardware encoder on the capture adapter" and "another trix
        // capture session … is already running" reach the UI verbatim.
        Err(e) => fail(daemon, id, "arm", &format!("{e:#}")),
    }
}

fn disarm(daemon: &Daemon, id: u64) -> Response {
    match daemon.disarm() {
        Ok(was_armed) => {
            if was_armed {
                daemon.clients.broadcast(&Event::new("disarmed", Value::Object(Map::new())));
            }
            Response::ok(id, Value::Object(Map::new()))
        }
        // No `error` event here: spec §4.4 broadcasts one for `arm` and `clip`,
        // which are the commands whose failure changes what the other clients
        // can expect to happen next.
        Err(e) => Response::err(id, format!("{e:#}")),
    }
}

fn clip(daemon: &Daemon, id: u64) -> Response {
    match daemon.clip() {
        Ok(Some(meta)) => match serde_json::to_value(&meta) {
            Ok(data) => {
                // Broadcast, not just answered: `clip_saved` is how a client
                // that did not send this `clip` learns a clip exists. Nothing
                // else saves one in this build, but the hotkey path already in
                // `trix-core`'s engine and plan 3's tray both will, and both
                // belong here — this is the only place that turns a saved clip
                // into an event.
                daemon.clients.broadcast(&Event::new("clip_saved", data.clone()));
                Response::ok(id, data)
            }
            // `ClipMeta` is strings, integers and bools, so this cannot
            // actually happen — but `to_value` returns a `Result` and a
            // `panic = "abort"` build has no room for an `unwrap` on a path a
            // socket message reaches.
            Err(e) => {
                tracing::error!(error = %e, "could not serialize the saved clip's metadata");
                Response::err(id, "the clip was saved but its metadata could not be serialized")
            }
        },
        // The ring exists but holds nothing yet — a few hundred ms after
        // arming, or right after a display rebuild. Reported as an error
        // response so a UI does not have to special-case a successful `clip`
        // with no clip in it, but deliberately *not* as a `clip_saved`.
        Ok(None) => fail(daemon, id, "clip", NOTHING_BUFFERED),
        Err(e) => fail(daemon, id, "clip", &format!("{e:#}")),
    }
}

/// `{"clips":[…],"total":N,"offset":O}` (spec §4.3).
///
/// `total` and `offset` are built with `Value::from`, and the clips with the
/// same matched `to_value` the `clip` arm above uses — `serde_json::json!`
/// would hide an `unwrap` on a path a socket message reaches directly.
fn library_list(daemon: &Daemon, id: u64, offset: usize, limit: usize) -> Response {
    let page = daemon.list(offset, limit);
    let mut clips = Vec::with_capacity(page.clips.len());
    for meta in &page.clips {
        match serde_json::to_value(meta) {
            Ok(value) => clips.push(value),
            // `ClipMeta` is strings, integers and bools, so this is
            // unreachable in practice — but the whole page fails rather than
            // silently returning a short one, because a client that trusts
            // `total` would otherwise render a hole it cannot see.
            Err(e) => {
                tracing::error!(clip = %meta.id, error = %e, "could not serialize clip metadata");
                return Response::err(id, "the clip library could not be serialized");
            }
        }
    }
    let mut fields = Map::new();
    fields.insert("clips".to_string(), Value::Array(clips));
    fields.insert("total".to_string(), Value::from(page.total));
    fields.insert("offset".to_string(), Value::from(page.offset));
    Response::ok(id, Value::Object(fields))
}

/// The response for `library.rename` and `library.favorite`: the clip as it now
/// stands, so a UI can re-render the one row it changed instead of re-listing
/// the page it is on.
fn updated_clip(id: u64, result: anyhow::Result<ClipMeta>) -> Response {
    match result {
        Ok(meta) => match serde_json::to_value(&meta) {
            Ok(data) => Response::ok(id, data),
            Err(e) => {
                tracing::error!(error = %e, "could not serialize the updated clip's metadata");
                Response::err(id, "the clip was updated but its metadata could not be serialized")
            }
        },
        Err(e) => Response::err(id, format!("{e:#}")),
    }
}

/// The response for `library.delete` and `library.reveal`, which have no data
/// to return: `{"clip_id":"…"}` names what the daemon acted on, so a UI that
/// pipelined several deletes can drop the right row without keeping its own
/// request-id-to-clip table.
///
/// No `error` event on failure. Spec §4.4 broadcasts one for `arm` and `clip`,
/// whose failure changes what every other client can expect to happen next; a
/// library command that failed concerns only the client that sent it.
fn acknowledge(id: u64, clip_id: &str, result: anyhow::Result<()>) -> Response {
    match result {
        Ok(()) => {
            let mut fields = Map::new();
            fields.insert("clip_id".to_string(), Value::from(clip_id));
            Response::ok(id, Value::Object(fields))
        }
        Err(e) => Response::err(id, format!("{e:#}")),
    }
}

/// One error response and the matching `error` event, so a UI watching the
/// socket sees the failure even if another client asked for it.
fn fail(daemon: &Daemon, id: u64, cmd: &str, error: &str) -> Response {
    daemon.clients.broadcast(&Event::new("error", error_data(cmd, error)));
    Response::err(id, error)
}

/// `{"cmd":"…","error":"…"}` (spec §4.4). Built by hand for the same reason
/// `DaemonStatus::to_json` is — `serde_json::json!` hides an `unwrap`.
fn error_data(cmd: &str, error: &str) -> Value {
    let mut fields = Map::new();
    fields.insert("cmd".to_string(), Value::from(cmd));
    fields.insert("error".to_string(), Value::from(error));
    Value::Object(fields)
}

#[cfg(test)]
mod tests {
    use super::*;
    use trix_core::config::Config;

    fn request(id: u64, cmd: &str) -> Request {
        Request { id, cmd: cmd.to_string(), args: Map::new() }
    }

    fn request_with(id: u64, cmd: &str, args: &[(&str, Value)]) -> Request {
        let mut map = Map::new();
        for (key, value) in args {
            map.insert((*key).to_string(), value.clone());
        }
        Request { id, cmd: cmd.to_string(), args: map }
    }

    /// A daemon over a temp clip directory holding two bare `.mp4`s, which
    /// `library::scan` adopts. Deliberately not the default config: that points
    /// at the developer's real `Videos\Trix`, which would make the assertions
    /// below depend on how many clips happen to be sitting there — and would
    /// aim a `library.delete` test at real footage.
    fn with_two_clips(name: &str) -> (Daemon, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("trix-dispatch-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for id in ["20260726_100000", "20260726_110000"] {
            std::fs::write(trix_core::library::mp4_path(&dir, id), b"video").unwrap();
        }
        let config = Config { clip_dir: dir.to_string_lossy().into_owned(), ..Config::default() };
        // `new_at(.., None)` rather than `new`: `new` would resolve
        // `config_path` from the developer's real `%APPDATA%	rix\config.toml`.
        // These tests never call `config.set`, so nothing would have been
        // written -- but nothing here needs a config path at all, and the
        // safest place for that to be true is the constructor.
        (Daemon::new_at(config, None), dir)
    }

    /// Everything below runs against an idle daemon: arming needs real
    /// hardware, so the armed branches are covered by the hand verification in
    /// this task and the end-to-end gate in Task 8.
    ///
    /// Idle does not mean "over the real machine". This used to be
    /// `Daemon::new(Config::default())`, which resolves `config_path` from
    /// `Config::path()` — the developer's live `%APPDATA%\trix\config.toml` —
    /// and runs the startup library scan against their real `Videos\Trix`. So
    /// every `cargo test --workspace` walked the user's actual footage, and the
    /// `config.set` tests below were pointed at their actual settings; they
    /// happen to reject before writing, which is one careless test away from
    /// rewriting them for real. Both seams to avoid that already existed in
    /// this crate (`Daemon::new_at`, `with_scratch_config`) and this helper
    /// simply did not use them.
    ///
    /// `config_path` is `None`, so there is nowhere for a `config.set` to
    /// write at all. `clip_dir` names a scratch path under `%TEMP%`, which the
    /// constructor's clip-directory preflight creates — the same
    /// best-effort create that gives a real user an existing `Videos\Trix` on
    /// first launch. It is left behind empty, exactly as `with_two_clips` and
    /// `with_scratch_config` leave theirs; what matters is that it is scratch
    /// rather than the developer's real clip folder.
    fn idle(name: &str) -> Daemon {
        let dir = std::env::temp_dir().join(format!("trix-idle-{name}-{}", std::process::id()));
        let config = Config { clip_dir: dir.to_string_lossy().into_owned(), ..Config::default() };
        Daemon::new_at(config, None)
    }

    /// A daemon whose `config.set` writes to a scratch file rather than the
    /// developer's real `%APPDATA%\trix\config.toml`. Without this seam, every
    /// `config.set` test below would rewrite the settings of whoever ran
    /// `cargo test` — and the `fps = 30` one would leave them capturing at 30.
    ///
    /// `clip_dir` points into the scratch directory too, so the startup library
    /// scan does not walk the developer's real clip folder.
    fn with_scratch_config(name: &str) -> (Daemon, std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("trix-config-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let config = Config { clip_dir: dir.to_string_lossy().into_owned(), ..Config::default() };
        (Daemon::new_at(config, Some(path.clone())), path, dir)
    }

    #[test]
    fn status_is_answered_without_an_engine() {
        let response = idle("status").dispatch(1, &request(1, "status"));
        assert_eq!(response.id, 1);
        assert!(response.ok, "status must answer on an idle daemon: {:?}", response.error);
        let data = response.data.expect("status carries data");
        assert_eq!(data.get("armed"), Some(&Value::Bool(false)));
    }

    /// The wording names the step the caller missed, rather than an internal
    /// "no engine" condition — a user reads this string.
    #[test]
    fn clip_while_idle_names_the_missing_step() {
        let response = idle("clip").dispatch(1, &request(9, "clip"));
        assert_eq!(response.id, 9);
        assert!(!response.ok);
        assert_eq!(response.error.as_deref(), Some("not armed — send arm first"));
    }

    /// Idempotent and quiet: a UI syncing its toggle on reconnect sends this
    /// against an already-idle daemon all the time.
    #[test]
    fn disarm_while_idle_succeeds_and_broadcasts_nothing() {
        let daemon = idle("disarm");
        let (tx, rx) = std::sync::mpsc::sync_channel(crate::clients::OUTBOUND_QUEUE_DEPTH);
        daemon.client_connected(tx);

        let response = daemon.dispatch(1, &request(3, "disarm"));
        assert!(response.ok, "disarming an idle daemon is a no-op, not a failure");
        assert!(
            rx.try_recv().is_err(),
            "no transition happened, so no `disarmed` event should have gone out"
        );
    }

    /// A failed command tells every connected client, not just the one that
    /// sent it (spec §4.4).
    #[test]
    fn a_failed_clip_broadcasts_an_error_event() {
        let daemon = idle("error-event");
        let (tx, rx) = std::sync::mpsc::sync_channel(crate::clients::OUTBOUND_QUEUE_DEPTH);
        daemon.client_connected(tx);

        daemon.dispatch(1, &request(4, "clip"));

        let line = rx.try_recv().expect("a failed clip must broadcast an `error` event");
        let event: Event = serde_json::from_str(line.trim_end())
            .unwrap_or_else(|e| panic!("event line did not decode: {e}\nline: {line}"));
        assert_eq!(event.event, "error");
        assert_eq!(event.data.get("cmd").and_then(Value::as_str), Some("clip"));
        assert_eq!(
            event.data.get("error").and_then(Value::as_str),
            Some("not armed — send arm first")
        );
    }

    /// The published `library.list` payload (spec §4.3). A UI binds to these
    /// three keys, and to `total` being the *unpaged* count — a page of 1 out
    /// of 2 must still say 2 or "1 of 2" cannot be rendered.
    #[test]
    fn library_list_answers_with_the_paged_wire_shape() {
        let (daemon, dir) = with_two_clips("list");

        let request = request_with(1, "library.list", &[("offset", 0.into()), ("limit", 1.into())]);
        let response = daemon.dispatch(1, &request);
        assert!(response.ok, "library.list must be answered now: {:?}", response.error);
        let data = response.data.expect("library.list carries data");

        assert_eq!(data.get("total").and_then(Value::as_u64), Some(2));
        assert_eq!(data.get("offset").and_then(Value::as_u64), Some(0));
        let clips = data.get("clips").and_then(Value::as_array).expect("clips is an array");
        assert_eq!(clips.len(), 1, "the page honours limit while total does not");
        assert_eq!(
            clips.first().and_then(|c| c.get("id")).and_then(Value::as_str),
            Some("20260726_110000"),
            "newest first"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The security boundary as it is actually reached — from a socket message,
    /// through `Command::parse`, into the dispatcher. `state.rs` proves the
    /// method rejects a hostile id; this proves nothing in the wiring hands one
    /// through by a different route.
    ///
    /// `../../../boot.ini` alone would make the final "untouched" assertion
    /// unfalsifiable: that id could never resolve to `20260726_100000.mp4`
    /// under any bug, so the check could not fail no matter what the wiring
    /// did. `canary` is the id that makes it mean something — like the
    /// `state.rs` traversal test, it is not a valid clip id, but it *is*
    /// exactly what `library::mp4_path` would target for it if validation
    /// were ever skipped on this path.
    #[test]
    fn a_traversal_id_off_the_wire_is_refused_without_touching_a_file() {
        let (daemon, dir) = with_two_clips("traversal");
        let canary = dir.join("canary.mp4");
        std::fs::write(&canary, b"must survive").unwrap();

        for cmd in ["library.delete", "library.rename", "library.favorite", "library.reveal"] {
            for evil in ["../../../boot.ini", "canary"] {
                let request = request_with(
                    7,
                    cmd,
                    &[("clip_id", evil.into()), ("title", "x".into()), ("favorite", true.into())],
                );
                let response = daemon.dispatch(1, &request);
                assert!(!response.ok, "{cmd} accepted {evil:?}");
                let error = response.error.unwrap_or_default();
                assert!(
                    error.contains("is not a valid clip id"),
                    "{cmd} on {evil:?} should be refused at the validation boundary, not fall \
                     through to a lookup: {error}"
                );
                assert!(
                    error.contains(evil),
                    "the error should quote the id it refused, for {cmd} on {evil:?}: {error}"
                );
            }
        }
        assert!(canary.exists(), "a refused id must not have touched the filesystem");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Rename answers with the clip as it now stands, so a UI can repaint one
    /// row from the response rather than re-listing the page.
    #[test]
    fn rename_answers_with_the_updated_clip() {
        let (daemon, dir) = with_two_clips("rename");

        let request = request_with(
            2,
            "library.rename",
            &[("clip_id", "20260726_100000".into()), ("title", "Ace on Ascent".into())],
        );
        let response = daemon.dispatch(1, &request);
        assert!(response.ok, "rename should succeed: {:?}", response.error);
        let data = response.data.expect("rename carries the updated clip");
        assert_eq!(data.get("title").and_then(Value::as_str), Some("Ace on Ascent"));
        assert_eq!(
            data.get("id").and_then(Value::as_str),
            Some("20260726_100000"),
            "the id is the file stem and a rename never changes it"
        );

        let blank = request_with(
            3,
            "library.rename",
            &[("clip_id", "20260726_100000".into()), ("title", "   ".into())],
        );
        assert!(!daemon.dispatch(1, &blank).ok, "a whitespace title is not a rename");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The published `config.get` payload: every key `Config` has, plus
    /// `clip_dir_resolved`.
    ///
    /// The expected key set is *derived* from `Config::default()` rather than
    /// written out here, and that is the whole point. A config key added to
    /// `Config` later has to reach a settings page without anyone remembering
    /// the daemon exists; if `config.get` ever grows a hand-written field list,
    /// this fails the day the next key is added rather than the day a user
    /// notices it missing from the UI.
    #[test]
    fn config_get_returns_every_config_key_plus_the_resolved_clip_dir() {
        let daemon = idle("config-get");
        let response = daemon.dispatch(1, &request(1, "config.get"));
        assert!(response.ok, "config.get must be answered now: {:?}", response.error);
        let data = response.data.expect("config.get carries data");
        let object = data.as_object().expect("config.get answers with an object");

        let defaults = serde_json::to_value(Config::default()).unwrap();
        let mut expected: Vec<&str> =
            defaults.as_object().unwrap().keys().map(String::as_str).collect();
        expected.push("clip_dir_resolved");
        expected.push("config_file_exists");
        expected.sort_unstable();

        let mut actual: Vec<&str> = object.keys().map(String::as_str).collect();
        actual.sort_unstable();
        assert_eq!(
            actual, expected,
            "config.get must round-trip every Config key, plus clip_dir_resolved and nothing else"
        );

        // `clip_dir_resolved` is `clip_dir` put through `Config::clip_dir_path`,
        // which is the one thing about this payload a UI cannot compute for
        // itself. Asserted against the daemon's own config rather than a
        // literal, so it stays true whatever `idle` points the scratch dir at.
        let clip_dir = object.get("clip_dir").and_then(Value::as_str).unwrap_or_default();
        let resolved = object.get("clip_dir_resolved").and_then(Value::as_str).unwrap_or_default();
        assert!(!clip_dir.is_empty(), "this fixture configures a clip_dir");
        assert_eq!(resolved, clip_dir, "a configured clip_dir resolves to itself");

        // The case that actually needs the resolver — an empty `clip_dir`,
        // which is the shipping default — is asserted on `Config` directly.
        // Routing it through a `Daemon` would mean constructing one over the
        // default clip dir, i.e. scanning the developer's real `Videos\Trix` on
        // every `cargo test --workspace`, which is exactly what `idle` exists
        // to stop. The claim is unchanged; only what has to be built to make it
        // is.
        let default_resolved = Config::default().clip_dir_path().to_string_lossy().into_owned();
        assert!(
            default_resolved.ends_with(r"Videos\Trix"),
            "an empty clip_dir must resolve to the default: {default_resolved}"
        );
    }

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

    /// The refusal boundary. A key the daemon does not know, and a value it
    /// cannot apply, are both named in the error — a settings page has to be
    /// able to point at the field — and neither writes anything.
    ///
    /// The all-or-nothing case is the one worth the extra assertion: a client
    /// that sent five keys and got one error must not have to guess which two
    /// landed.
    #[test]
    fn config_set_refuses_what_it_cannot_apply_and_writes_nothing() {
        let (daemon, path, dir) = with_scratch_config("refuse");

        let unknown = daemon.dispatch(1, &request_with(5, "config.set", &[("nope", 1.into())]));
        assert!(!unknown.ok, "an unknown key is not a setting");
        let error = unknown.error.unwrap_or_default();
        assert!(error.contains("nope"), "the error must name the key it refused: {error}");
        assert!(!path.exists(), "a refused config.set must not have written the file");

        let wrong_type =
            daemon.dispatch(1, &request_with(6, "config.set", &[("fps", "sixty".into())]));
        assert!(!wrong_type.ok, "fps is a number");
        let error = wrong_type.error.unwrap_or_default();
        assert!(error.contains("fps"), "the error must name the key that would not take: {error}");
        assert!(!path.exists(), "a refused config.set must not have written the file");

        let mixed = daemon
            .dispatch(1, &request_with(7, "config.set", &[("fps", 30.into()), ("nope", 1.into())]));
        assert!(!mixed.ok);
        assert!(!path.exists(), "one bad key must refuse the whole request, not half of it");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Type coercion is not the only thing that can make a setting absurd.
    /// `{"replay_seconds": 4294967295}` deserializes cleanly and would be a
    /// 136-year retention window at the next `arm` — `evict` would never evict
    /// and the ring would grow until the process was OOM-killed. Two socket
    /// lines must not be able to do that, so every numeric key carries a sanity
    /// range and an out-of-range value is refused *by name, with the range*,
    /// having written nothing.
    #[test]
    fn config_set_refuses_an_out_of_range_value_for_every_bounded_key() {
        let (daemon, path, dir) = with_scratch_config("range");

        // One real write first, so the "unchanged" assertions below compare a
        // file that exists against itself rather than against absence — the
        // weaker check the refusal tests above already make.
        let seed = daemon.dispatch(1, &request_with(1, "config.set", &[("fps", 30.into())]));
        assert!(seed.ok, "the seed write must land: {:?}", seed.error);
        let before = std::fs::read(&path).unwrap();

        for (key, bad) in [
            ("fps", 0u64),
            ("fps", 481),
            ("bitrate_kbps", 0),
            ("bitrate_kbps", 200_001),
            ("max_bitrate_kbps", 200_001),
            ("replay_seconds", 0),
            ("replay_seconds", 601),
            ("replay_seconds", u64::from(u32::MAX)),
            ("monitor_index", 64),
            ("stats_seconds", 86_401),
        ] {
            let response = daemon.dispatch(1, &request_with(2, "config.set", &[(key, bad.into())]));
            assert!(!response.ok, "config.set accepted {key} = {bad}");
            let error = response.error.unwrap_or_default();
            assert!(error.contains(key), "the error must name the key: {error}");
            assert!(
                error.contains(&bad.to_string()),
                "the error must quote the value it refused: {error}"
            );
            assert_eq!(
                std::fs::read(&path).unwrap(),
                before,
                "a refused {key} = {bad} must leave the file byte-unchanged"
            );
        }

        // Mixed: one key the daemon would happily take, one it will not. All
        // or nothing across the whole map — a partial write is worse than a
        // rejection, because a settings page cannot tell which half landed.
        let mixed = daemon.dispatch(
            1,
            &request_with(
                3,
                "config.set",
                &[("bitrate_kbps", 12_000.into()), ("replay_seconds", 9_999.into())],
            ),
        );
        assert!(!mixed.ok, "one out-of-range key refuses the whole request");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "the valid half of a mixed request must not reach the disk either"
        );
        let after = daemon.dispatch(1, &request(4, "config.get")).data.unwrap();
        assert_eq!(
            after.get("bitrate_kbps").and_then(Value::as_u64),
            Some(8_000),
            "nor the daemon's own memory"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The bounds are inclusive at both ends, and the two documented zeroes
    /// (`max_bitrate_kbps` = auto, `stats_seconds` = no periodic log line, both
    /// of them defaults) stay legal. A bound that quietly refused the shipping
    /// default would be worse than no bound at all.
    #[test]
    fn config_set_accepts_both_ends_of_every_range() {
        let (daemon, path, dir) = with_scratch_config("bounds");

        for (key, edge) in [
            ("fps", 1u64),
            ("fps", 480),
            ("bitrate_kbps", 1),
            ("bitrate_kbps", 200_000),
            ("max_bitrate_kbps", 0),
            ("max_bitrate_kbps", 200_000),
            ("replay_seconds", 1),
            ("replay_seconds", 600),
            ("monitor_index", 0),
            ("monitor_index", 63),
            ("stats_seconds", 0),
            ("stats_seconds", 86_400),
        ] {
            let response =
                daemon.dispatch(1, &request_with(5, "config.set", &[(key, edge.into())]));
            assert!(
                response.ok,
                "config.set refused {key} = {edge}, which is on the boundary: {:?}",
                response.error
            );
            let data = response.data.expect("an accepted config.set carries data");
            assert_eq!(
                data.pointer(&format!("/accepted/{key}")).and_then(Value::as_u64),
                Some(edge),
                "the boundary value must be reported back as accepted, not silently clamped"
            );
        }
        assert!(path.exists(), "an accepted config.set writes the file");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// `fps` is baked into `RecorderSettings` when the engine is spawned, so
    /// setting it changes the file and nothing that is already capturing. The
    /// response says so; it does not re-arm, because tearing down a live replay
    /// ring is not something a slider gets to do on its own.
    #[test]
    fn config_set_of_an_engine_key_asks_for_a_rearm() {
        let (daemon, path, dir) = with_scratch_config("rearm");

        let response = daemon.dispatch(1, &request_with(4, "config.set", &[("fps", 30.into())]));
        assert!(response.ok, "config.set must be answered now: {:?}", response.error);
        let data = response.data.expect("config.set carries data");
        assert_eq!(data.pointer("/accepted/fps").and_then(Value::as_u64), Some(30));
        assert_eq!(
            data.get("requires_rearm").and_then(Value::as_array).map(Vec::as_slice),
            Some([Value::from("fps")].as_slice()),
            "fps only takes effect at the next arm, and the UI has to be told"
        );

        // Persisted, not merely remembered — a setting that does not survive a
        // restart is a bug a user finds tomorrow.
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("fps = 30"), "config.set must write the file: {saved}");
        // And visible to the very next config.get, from the same daemon.
        let after = daemon.dispatch(1, &request(5, "config.get")).data.unwrap();
        assert_eq!(after.get("fps").and_then(Value::as_u64), Some(30));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// `clip_dir` is read when a clip is written, not when the engine starts,
    /// so it takes effect immediately and the re-arm list stays empty. A UI
    /// that prompted "restart capture?" for this would be wrong.
    #[test]
    fn config_set_of_a_live_key_needs_no_rearm() {
        let (daemon, path, dir) = with_scratch_config("live");
        let clips = dir.join("Clips");

        let response = daemon.dispatch(
            1,
            &request_with(
                8,
                "config.set",
                &[("clip_dir", Value::from(clips.to_string_lossy().as_ref()))],
            ),
        );
        assert!(response.ok, "config.set must be answered now: {:?}", response.error);
        let data = response.data.expect("config.set carries data");
        assert!(
            data.get("requires_rearm").and_then(Value::as_array).is_some_and(Vec::is_empty),
            "clip_dir is read per clip and needs no re-arm: {data}"
        );
        assert!(path.exists(), "an accepted config.set writes the file");

        std::fs::remove_dir_all(&dir).unwrap();
    }

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

    /// The contract behind the tray's "Change clips folder…", both ways round:
    /// an accepted directory exists afterwards, and a refused one leaves
    /// *nothing* changed.
    ///
    /// The refusal half is the one that matters. `clip_dir` is the only setting
    /// whose value can be wrong in a way the daemon cannot detect later — a
    /// mistyped path or an unplugged drive is a perfectly well-formed string —
    /// and accepting one would point every future clip at a folder that
    /// swallows it. So the check is a real write, and on failure the old
    /// directory must still be in force in memory *and* on disk.
    #[test]
    fn config_set_creates_the_clip_directory_and_refuses_one_it_cannot_use() {
        let (daemon, path, dir) = with_scratch_config("clipdir");

        // Accepted: a nested path that does not exist yet is created.
        let clips = dir.join("Recordings").join("Trix");
        let response = daemon.dispatch(
            1,
            &request_with(
                1,
                "config.set",
                &[("clip_dir", Value::from(clips.to_string_lossy().as_ref()))],
            ),
        );
        assert!(response.ok, "a usable directory must be accepted: {:?}", response.error);
        assert!(clips.is_dir(), "config.set must create the directory it accepted");
        let left: Vec<_> = std::fs::read_dir(&clips).unwrap().map(|e| e.unwrap().path()).collect();
        assert!(left.is_empty(), "the writability probe must not survive: {left:?}");
        let saved = std::fs::read_to_string(&path).unwrap();

        // Refused: a directory whose parent is a file can never be created.
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, b"not a directory").unwrap();
        let bad = blocker.join("clips");
        let response = daemon.dispatch(
            1,
            &request_with(
                2,
                "config.set",
                &[("clip_dir", Value::from(bad.to_string_lossy().as_ref()))],
            ),
        );
        assert!(!response.ok, "a directory that cannot be created is not a usable clip_dir");
        let error = response.error.unwrap_or_default();
        assert!(error.contains("clip"), "the error must say what it refused: {error}");

        // The old directory still stands — in memory…
        let after = daemon.dispatch(1, &request(3, "config.get")).data.unwrap();
        assert_eq!(
            after.get("clip_dir").and_then(Value::as_str),
            Some(clips.to_string_lossy().as_ref()),
            "a refused clip_dir must leave the working directory alone"
        );
        // …and on disk. A settings page that showed the old value while the
        // file held the new one would "fix itself" into the bad path at the
        // next restart.
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            saved,
            "a refused config.set must not have rewritten the file"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The settings dropdowns' data source, over the wire. Named-array shape
    /// (`{"monitors":[…]}`) so a client never has to tell a bare array from an
    /// object, and `index` really is a `config.monitor_index` value.
    ///
    /// Renamed from `..._answers_with_the_data_the_probe_prints`, which
    /// over-claimed: nothing here compares anything to what `trix probe`
    /// prints. The body was always fine — it asserts the wire shape a dropdown
    /// binds to — but a reader auditing coverage stops at the name, and a name
    /// that promises a cross-check nobody wrote is how a reviewer concludes
    /// something is covered when it is not.
    #[test]
    fn monitors_list_answers_with_the_wire_shape_a_dropdown_binds() {
        let response = idle("monitors").dispatch(1, &request(2, "monitors.list"));
        assert!(response.ok, "monitors.list must be answered now: {:?}", response.error);
        let data = response.data.expect("monitors.list carries data");
        let monitors =
            data.get("monitors").and_then(Value::as_array).expect("monitors is an array");
        assert!(!monitors.is_empty(), "a machine running this test has a desktop attached");

        let first = monitors.first().expect("at least one monitor");
        for key in ["index", "name", "width", "height", "left", "top", "adapter"] {
            assert!(first.get(key).is_some(), "a settings dropdown binds {key:?}: {first}");
        }
        assert_eq!(
            first.get("index").and_then(Value::as_u64),
            Some(0),
            "the first entry is monitor_index 0 — these are config values, not ordinals"
        );
    }

    /// The encoder dropdown. Every entry has to be nameable and its hardware
    /// flag has to be there, because "software fallback" is exactly the thing a
    /// user needs to see before they wonder why their game got slower.
    ///
    /// Renamed from `encoders_list_names_the_real_mfts` for the reason given on
    /// the monitors test above: the list does come from `MFTEnumEx`, but this
    /// test asserts JSON key presence and says nothing about *which* MFTs, so
    /// the old name promised a cross-check that is not here.
    #[test]
    fn encoders_list_answers_with_the_wire_shape_a_dropdown_binds() {
        let response = idle("encoders").dispatch(1, &request(3, "encoders.list"));
        assert!(response.ok, "encoders.list must be answered now: {:?}", response.error);
        let data = response.data.expect("encoders.list carries data");
        let encoders =
            data.get("encoders").and_then(Value::as_array).expect("encoders is an array");
        assert!(!encoders.is_empty(), "a Windows machine offers at least a software H.264 MFT");

        for encoder in encoders {
            assert!(
                encoder.get("name").and_then(Value::as_str).is_some_and(|n| !n.is_empty()),
                "an encoder with no name is not selectable: {encoder}"
            );
            assert!(
                encoder.get("codec").and_then(Value::as_str).is_some_and(|c| c.trim() == c),
                "codec labels reach the UI unpadded: {encoder}"
            );
            assert!(
                encoder.get("hardware").and_then(Value::as_bool).is_some(),
                "the hardware flag is what a settings page warns on: {encoder}"
            );
        }
    }

    /// `stats` is per-connection, not daemon state: two UIs must be able to
    /// disagree about whether they want a 1 Hz event stream. This is the one
    /// command that needs to know *which* client sent it.
    #[test]
    fn stats_subscribe_flips_only_the_asking_clients_flag() {
        let daemon = idle("stats-subscribe");
        let (subscriber_tx, subscriber_rx) =
            std::sync::mpsc::sync_channel(crate::clients::OUTBOUND_QUEUE_DEPTH);
        let subscriber = daemon.client_connected(subscriber_tx);
        let (bystander_tx, bystander_rx) =
            std::sync::mpsc::sync_channel(crate::clients::OUTBOUND_QUEUE_DEPTH);
        daemon.client_connected(bystander_tx);
        assert!(!daemon.clients.any_stats_subscribers(), "nothing measures anything yet");

        let on = daemon.dispatch(
            subscriber,
            &request_with(10, "stats.subscribe", &[("enabled", true.into())]),
        );
        assert!(on.ok, "stats.subscribe must be answered now: {:?}", on.error);
        assert_eq!(
            on.data.as_ref().and_then(|d| d.get("enabled")),
            Some(&Value::Bool(true)),
            "the response echoes the state the client is now in"
        );
        assert!(daemon.clients.any_stats_subscribers(), "the stats thread may now do work");

        daemon.clients.broadcast_stats(&Event::new("stats", Value::Object(Map::new())));
        assert!(subscriber_rx.try_recv().is_ok(), "the subscriber receives stats");
        assert!(bystander_rx.try_recv().is_err(), "a client that never asked must not");

        let off = daemon.dispatch(
            subscriber,
            &request_with(11, "stats.subscribe", &[("enabled", false.into())]),
        );
        assert!(off.ok);
        assert!(
            !daemon.clients.any_stats_subscribers(),
            "unsubscribing is the same command in reverse, and it stops the measuring"
        );
    }

    /// Malformed input never drops a connection — it gets an error *response*
    /// and the socket stays open. The commands this build added are the newest
    /// place that could get wrong, so they are where it is checked.
    #[test]
    fn a_new_command_with_bad_arguments_answers_with_an_error() {
        let daemon = idle("bad-args");

        let empty = daemon.dispatch(1, &request(11, "config.set"));
        assert_eq!(empty.id, 11, "the caller needs its id back to match the reply");
        assert!(!empty.ok, "config.set with nothing to set is not a request");
        assert!(empty.error.unwrap_or_default().contains("config.set"));

        let not_an_object =
            daemon.dispatch(1, &request_with(12, "config.set", &[("values", 5.into())]));
        assert!(!not_an_object.ok);
        assert!(
            not_an_object.error.unwrap_or_default().contains("values"),
            "the error should name the argument that was the wrong shape"
        );

        let no_flag = daemon.dispatch(1, &request(13, "stats.subscribe"));
        assert!(!no_flag.ok);
        let error = no_flag.error.unwrap_or_default();
        assert!(error.contains("enabled"), "the error should name the missing argument: {error}");

        assert!(
            daemon.dispatch(1, &request(14, "status")).ok,
            "three bad requests must leave the daemon answering the fourth"
        );
    }

    #[test]
    fn an_unknown_command_keeps_its_id() {
        let response = idle("unknown-cmd").dispatch(1, &request(77, "launch_missiles"));
        assert_eq!(response.id, 77, "the caller needs its id back to match the reply");
        assert!(!response.ok);
        assert!(
            response.error.unwrap_or_default().contains("launch_missiles"),
            "the error should name what was not understood"
        );
    }
}
