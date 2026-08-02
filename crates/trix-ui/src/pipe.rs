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
//! (`PIPE_TYPE_BYTE | PIPE_READMODE_BYTE`) with newline framing, so nothing in
//! this file needs to know it is talking to a pipe at all.
//!
//! That is not the same as saying a plain `File` is a complete client, which
//! this comment used to claim and which is the opposite of true. The pipe is
//! opened without `FILE_FLAG_OVERLAPPED`, so a blocking read parked on one
//! handle stalls a write issued on any other handle onto the same file object —
//! and `Connection::start` is handed exactly that pair. Reading through a plain
//! `File` deadlocks the app on its first command. `pipe_reader.rs` is what
//! makes the transport underneath this module work, and carries the long
//! version of why.

use std::collections::HashMap;
use std::io::{BufRead, Write};
// Only the test harness constructs a `BufReader` (`Connection::start` is
// generic over any `R: BufRead`); importing it unconditionally warns on a
// normal build.
#[cfg(test)]
use std::io::BufReader;
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
    pub fn call(
        &self,
        cmd: &str,
        args: Map<String, Value>,
        timeout: Duration,
    ) -> Result<Value, String> {
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

        // `close()` sets `closed` *before* it clears `pending`. If this
        // re-check reads `false`, then `close()`'s store has not happened
        // yet, so its clear has not either — and since our insert has
        // already completed, that clear will drop our sender and wake
        // `recv_timeout` immediately with a disconnect. If this re-check
        // reads `true`, we bail out right here. There is no third case.
        // Without this, a `close()` that runs entirely between the entry
        // check above and the insert leaves our sender in `pending` with
        // nothing left to ever drop it, and this caller would serve out the
        // full `timeout` before line ~157 finally notices `closed`.
        if self.closed.load(Ordering::Acquire) {
            self.forget(id);
            return Err("not connected to the Trix daemon".to_string());
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
        let id_a =
            serde_json::from_str::<Value>(line_a.trim_end()).unwrap()["id"].as_u64().unwrap();

        let b = Arc::clone(&h.conn);
        let second = std::thread::spawn(move || b.call("status", Map::new(), FAST));
        let line_b = h.sent.recv_timeout(FAST).expect("second request");
        let id_b =
            serde_json::from_str::<Value>(line_b.trim_end()).unwrap()["id"].as_u64().unwrap();
        assert_ne!(id_a, id_b, "each request gets a fresh id");

        // Answered backwards on purpose.
        h.to_client
            .send(
                format!("{{\"id\":{id_b},\"ok\":true,\"data\":{{\"who\":\"status\"}}}}\n")
                    .into_bytes(),
            )
            .unwrap();
        h.to_client
            .send(
                format!("{{\"id\":{id_a},\"ok\":true,\"data\":{{\"who\":\"list\"}}}}\n")
                    .into_bytes(),
            )
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
            .send(
                format!("{{\"id\":{id},\"ok\":false,\"error\":\"the replay ring is empty\"}}\n")
                    .into_bytes(),
            )
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

        h.to_client.send(b"{\"event\":\"armed\",\"data\":{\"armed\":true}}\n".to_vec()).unwrap();
        let event = h.events.recv_timeout(FAST).expect("the event should be routed");
        assert_eq!(event.event, "armed");

        h.to_client
            .send(format!("{{\"id\":{id},\"ok\":true,\"data\":{{}}}}\n").into_bytes())
            .unwrap();
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

    /// A daemon that dies mid-write leaves a final line with no trailing
    /// `\n`. That line fails JSON parsing and is dropped by `deliver`, and
    /// the next `read_line` then sees the real EOF — the connection must
    /// close cleanly rather than panic or wedge.
    #[test]
    fn a_truncated_final_line_closes_the_connection_cleanly() {
        let h = harness();
        h.to_client.send(b"{\"id\":1,\"ok\":tr".to_vec()).unwrap(); // no trailing \n
        drop(h.to_client); // EOF right behind the partial line
        h.closed.recv_timeout(FAST).expect("on_close should fire after the truncated line");
        assert!(h.conn.is_closed(), "the connection should report itself closed");
    }
}
