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

/// Answered for commands the protocol defines but this build does not yet
/// implement — `config.*`, `monitors.list`, `encoders.list`,
/// `stats.subscribe`. Task 7 replaces the remaining arms.
const NOT_IMPLEMENTED: &str = "not implemented in this build";

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

    /// `client` is unused until `stats.subscribe` (Task 7) needs to know which
    /// connection asked.
    fn dispatch(&self, _client: ClientId, request: &Request) -> Response {
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
            // `Daemon::clip_dir_for` — so there is nothing to check here, and
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
            Ok(_) => Response::err(request.id, NOT_IMPLEMENTED),
            Err(error) => Response::err(request.id, error),
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
        (Daemon::new(config), dir)
    }

    /// Everything below runs against an idle daemon: arming needs real
    /// hardware, so the armed branches are covered by the hand verification in
    /// this task and the end-to-end gate in Task 8.
    fn idle() -> Daemon {
        Daemon::new(Config::default())
    }

    #[test]
    fn status_is_answered_without_an_engine() {
        let response = idle().dispatch(1, &request(1, "status"));
        assert_eq!(response.id, 1);
        assert!(response.ok, "status must answer on an idle daemon: {:?}", response.error);
        let data = response.data.expect("status carries data");
        assert_eq!(data.get("armed"), Some(&Value::Bool(false)));
    }

    /// The wording names the step the caller missed, rather than an internal
    /// "no engine" condition — a user reads this string.
    #[test]
    fn clip_while_idle_names_the_missing_step() {
        let response = idle().dispatch(1, &request(9, "clip"));
        assert_eq!(response.id, 9);
        assert!(!response.ok);
        assert_eq!(response.error.as_deref(), Some("not armed — send arm first"));
    }

    /// Idempotent and quiet: a UI syncing its toggle on reconnect sends this
    /// against an already-idle daemon all the time.
    #[test]
    fn disarm_while_idle_succeeds_and_broadcasts_nothing() {
        let daemon = idle();
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
        let daemon = idle();
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
    #[test]
    fn a_traversal_id_off_the_wire_is_refused_without_touching_a_file() {
        let (daemon, dir) = with_two_clips("traversal");
        let survivor = trix_core::library::mp4_path(&dir, "20260726_100000");

        for cmd in ["library.delete", "library.rename", "library.favorite", "library.reveal"] {
            let request = request_with(
                7,
                cmd,
                &[
                    ("clip_id", "../../../boot.ini".into()),
                    ("title", "x".into()),
                    ("favorite", true.into()),
                ],
            );
            let response = daemon.dispatch(1, &request);
            assert!(!response.ok, "{cmd} accepted a traversal id");
            assert!(
                response.error.unwrap_or_default().contains("boot.ini"),
                "the error should quote the id it refused, for {cmd}"
            );
        }
        assert!(survivor.exists(), "a refused id must not have touched the filesystem");

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

    /// The protocol defines more commands than this build answers; the ones it
    /// does not must say so rather than parse-error, so a UI can tell "you are
    /// talking to an older daemon" from "you sent nonsense".
    #[test]
    fn a_command_this_build_does_not_answer_says_so() {
        let daemon = idle();
        for cmd in ["config.get", "monitors.list", "encoders.list"] {
            let response = daemon.dispatch(1, &request(5, cmd));
            assert!(!response.ok, "{cmd} should not report success yet");
            assert_eq!(response.error.as_deref(), Some(NOT_IMPLEMENTED), "for {cmd}");
        }
    }

    #[test]
    fn an_unknown_command_keeps_its_id() {
        let response = idle().dispatch(1, &request(77, "launch_missiles"));
        assert_eq!(response.id, 77, "the caller needs its id back to match the reply");
        assert!(!response.ok);
        assert!(
            response.error.unwrap_or_default().contains("launch_missiles"),
            "the error should name what was not understood"
        );
    }
}
