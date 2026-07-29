//! The request path of `pipe.rs`'s `serve_one`: a client that *does* send
//! requests must get every one of them answered.
//!
//! `tests/events.rs` proves an idle client still receives broadcasts, and
//! `tests/over_cap_error_survives_close.rs` proves an unrecoverable line still
//! gets its diagnostic. Neither of them ever completes a request/response
//! round trip, so neither noticed that the poll the event path introduced
//! (`PeekNamedPipe`, every `EVENT_POLL_MS`) inspects the *OS pipe buffer*
//! while the session reads through a `BufReader` that has already drained it.
//! Two requests written in one `write_all` land in the `BufReader` together:
//! the first is dispatched, the second is invisible to the peek, and the
//! client waits forever for a reply that is never produced.

use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::mpsc::{Sender, SyncSender};
use std::time::{Duration, Instant};

use trix_daemon::pipe::{self, ClientHandler};
use trix_proto::{Request, Response};

/// Answers every request with `ok` and announces the id it dispatched, so a
/// test can tell "the response was lost on the way out" from "the request was
/// never seen at all".
struct EchoHandler {
    dispatched: Sender<u64>,
}

impl ClientHandler for EchoHandler {
    fn client_connected(&self, _out: SyncSender<String>) -> u64 {
        1
    }

    fn client_disconnected(&self, _client: u64) {}

    fn dispatch(&self, _client: u64, request: &Request) -> Response {
        let _ = self.dispatched.send(request.id);
        Response::ok(request.id, serde_json::Value::Null)
    }
}

/// Starts `pipe::serve_at` on `name` in the background and hands back the
/// channel its (only-on-failure) error arrives on. `serve_at` loops forever
/// while it is working, so anything on that channel means the bind failed and
/// the test must say so loudly instead of timing out generically.
fn serve_in_background(
    name: &str,
    handler: Arc<EchoHandler>,
) -> std::sync::mpsc::Receiver<anyhow::Error> {
    let (result_tx, result_rx) = std::sync::mpsc::channel::<anyhow::Error>();
    let serve_name = name.to_string();
    std::thread::Builder::new()
        .name("test-pipe-serve".into())
        .spawn(move || {
            // The test process exiting tears this thread down; there is
            // nothing to join.
            if let Err(e) = pipe::serve_at(&serve_name, handler) {
                let _ = result_tx.send(e);
            }
        })
        .expect("failed to spawn the serve thread");
    result_rx
}

/// Connects a client to `name`, retrying while the serve thread reaches its
/// first `CreateNamedPipeW`. Polls `serve_failure` on every retry so a bind
/// failure is reported directly rather than as a generic timeout. Same shape
/// as the helper in the other two integration tests.
fn connect_client(
    name: &str,
    serve_failure: &std::sync::mpsc::Receiver<anyhow::Error>,
) -> std::fs::File {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match std::fs::OpenOptions::new().read(true).write(true).open(name) {
            Ok(file) => return file,
            Err(_) if Instant::now() < deadline => {
                if let Ok(e) = serve_failure.try_recv() {
                    panic!("pipe::serve_at({name:?}) failed: {e:#}");
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("could not connect to {name}: {e}"),
        }
    }
}

/// Reads `\n`-terminated lines off `source` onto a channel, so the test can
/// wait with a timeout instead of hanging the suite in a blocking pipe read
/// when a response never comes.
fn read_lines_in_background(mut source: std::fs::File) -> std::sync::mpsc::Receiver<String> {
    let (line_tx, line_rx) = std::sync::mpsc::channel::<String>();
    std::thread::Builder::new()
        .name("test-response-reader".into())
        .spawn(move || {
            let mut bytes = Vec::new();
            let mut byte = [0u8; 1];
            while let Ok(1) = source.read(&mut byte) {
                if byte[0] == b'\n' {
                    let line = String::from_utf8_lossy(&bytes).into_owned();
                    bytes.clear();
                    if line_tx.send(line).is_err() {
                        return;
                    }
                    continue;
                }
                bytes.push(byte[0]);
            }
        })
        .expect("failed to spawn the reader thread");
    line_rx
}

fn decode(line: &str) -> Response {
    serde_json::from_str(line)
        .unwrap_or_else(|e| panic!("response line did not decode as JSON: {e}\nline: {line}"))
}

/// Two complete request lines in a single `write_all` — what any client
/// library that batches, or any UI that fires `status` and `library.list`
/// back to back, produces. Both must be answered.
///
/// Lock-step clients (PowerShell's `WriteLine`/`ReadLine`) never exercise
/// this, which is exactly why the defect it guards survived hand verification.
#[test]
fn two_requests_written_at_once_both_get_answered() {
    // A private, pid-qualified name with its own prefix: binding
    // `pipe::PIPE_NAME` would race a real `trix-daemon.exe`, and sharing a
    // prefix with the other test binaries would let them collide under a
    // parallel runner.
    let name = format!(r"\\.\pipe\trix-roundtrip-test-{}", std::process::id());

    let (dispatched_tx, dispatched_rx) = std::sync::mpsc::channel::<u64>();
    let handler = Arc::new(EchoHandler { dispatched: dispatched_tx });
    let result_rx = serve_in_background(&name, handler);

    let mut client = connect_client(&name, &result_rx);

    // One `write_all`, two framed requests. Not two writes: the point is that
    // both lines reach the server's `BufReader` in a single read.
    client
        .write_all(b"{\"id\":1,\"cmd\":\"status\"}\n{\"id\":2,\"cmd\":\"status\"}\n")
        .expect("write both requests to the pipe");

    let lines = read_lines_in_background(client);

    let first = decode(&lines.recv_timeout(Duration::from_secs(5)).expect(
        "the first response must arrive — a timeout here means no request was answered at all",
    ));
    let second = decode(&lines.recv_timeout(Duration::from_secs(5)).expect(
        "the second response must arrive — a timeout here means the session loop cannot see a \
         request already buffered in its `BufReader`, so a client that pipelines two requests \
         hangs forever on the second",
    ));

    let mut ids = [first.id, second.id];
    ids.sort_unstable();
    assert_eq!(ids, [1, 2], "each request must be answered exactly once, correlated by id");
    assert!(first.ok && second.ok, "both responses should report success");

    assert_eq!(dispatched_rx.recv_timeout(Duration::from_secs(1)), Ok(1));
    assert_eq!(dispatched_rx.recv_timeout(Duration::from_secs(1)), Ok(2));
}

/// The open question this settles: a client that writes a complete request and
/// closes its handle in the next instant. The bytes are in the pipe, but by
/// the time the session loop wakes from its poll sleep the client end is gone
/// — and if `PeekNamedPipe` reports that as `ERROR_BROKEN_PIPE` rather than
/// as "N bytes available", the session would break out and discard a request
/// it had already been handed. Task 4's unconditional blocking read would have
/// processed it, so silently dropping it here would be a regression.
///
/// A last write before closing is not exotic: it is what a UI shutting down,
/// or a one-shot `trix-ctl clip && exit`, does every time.
#[test]
fn a_request_written_immediately_before_the_client_closes_is_still_dispatched() {
    let name = format!(r"\\.\pipe\trix-lastwrite-test-{}", std::process::id());

    let (dispatched_tx, dispatched_rx) = std::sync::mpsc::channel::<u64>();
    let handler = Arc::new(EchoHandler { dispatched: dispatched_tx });
    let result_rx = serve_in_background(&name, handler);

    let mut client = connect_client(&name, &result_rx);

    // Let the session settle into its poll loop first, so the write below
    // lands mid-sleep and the next thing the server does is peek a pipe whose
    // client end has already closed — the case under test. Without this the
    // test could accidentally exercise the easy path where the peek happens
    // to run while the client is still open.
    std::thread::sleep(Duration::from_millis(80));

    client.write_all(b"{\"id\":42,\"cmd\":\"status\"}\n").expect("write the request");
    // No read, no shutdown handshake: the handle closes here, immediately.
    drop(client);

    assert_eq!(
        dispatched_rx.recv_timeout(Duration::from_secs(3)),
        Ok(42),
        "a complete request already written must be dispatched even though the client closed \
         before the server could look at it — dropping it loses a command the client had every \
         reason to believe was delivered"
    );
}
