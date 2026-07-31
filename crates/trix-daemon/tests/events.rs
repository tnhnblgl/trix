//! The unsolicited-event path (`pipe.rs`'s `serve_one`): a client that
//! connects and never sends a byte must still receive a broadcast.
//!
//! This is the whole reason the session loop polls with `PeekNamedPipe` every
//! `EVENT_POLL_MS` instead of blocking in `read_line_capped`. A synchronous
//! named pipe serializes I/O on the file object, so a client parked in a read
//! cannot be written to from another thread — an idle UI would not see
//! `clip_saved` until it happened to send its next command, which spec §4.4
//! forbids. Only a real pipe can exercise that, hence an integration test.

use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Sender, SyncSender};
use std::time::{Duration, Instant};

use trix_daemon::clients::Clients;
use trix_daemon::pipe::{self, ClientHandler};
use trix_proto::{Event, Request, Response};

/// Registers connections in a real [`Clients`] registry — the production
/// broadcast path — and announces each registration so the test can broadcast
/// only once the server has actually taken the client on.
struct EventHandler {
    clients: Clients,
    connected: Sender<u64>,
}

impl ClientHandler for EventHandler {
    fn client_connected(&self, out: SyncSender<String>) -> u64 {
        let id = self.clients.register(out);
        let _ = self.connected.send(id);
        id
    }

    fn client_disconnected(&self, client: u64) {
        self.clients.unregister(client);
    }

    /// Never called in this test: the client under test sends nothing at all.
    fn dispatch(&self, _client: u64, request: &Request) -> Response {
        Response::ok(request.id, serde_json::Value::Null)
    }

    /// Forwarded to the registry, exactly as the production handler
    /// (`dispatch.rs`'s `impl ClientHandler for Daemon`) does.
    ///
    /// This handler is the one place in the test suite that drives a *real*
    /// `Clients`, and until `eviction_flag` became a required method it had
    /// silently inherited the `None` default — so its registry could evict a
    /// client's entry while the connection stayed open forever, event-deaf.
    /// Returning `None` here would have satisfied the compiler and preserved
    /// that hole; forwarding is what makes this harness match the thing it is
    /// standing in for.
    fn eviction_flag(&self, client: u64) -> Option<Arc<AtomicBool>> {
        self.clients.eviction_flag(client)
    }
}

/// Connects a client to `name`, retrying briefly while the background thread
/// reaches its first `CreateNamedPipeW`. Polls `serve_failure` on every retry
/// so a bind failure is reported directly instead of as a generic timeout.
/// Same shape as `over_cap_error_survives_close.rs`'s helper.
fn connect_client(name: &str, serve_failure: &std::sync::mpsc::Receiver<anyhow::Error>) -> std::fs::File {
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

#[test]
fn an_idle_client_that_never_sends_a_request_still_receives_an_event() {
    // A private name, not `pipe::PIPE_NAME`: binding the real production pipe
    // here would race an actually-running `trix-daemon.exe` for
    // `FILE_FLAG_FIRST_PIPE_INSTANCE` and, if that daemon won, this test would
    // silently exercise its already-built binary instead of this crate's code —
    // passing either way. The pid also keeps this from colliding with the
    // over-cap test under a parallel runner.
    let name = format!(r"\\.\pipe\trix-events-test-{}", std::process::id());

    // `serve_at` only returns on error — it loops forever on success — so
    // anything arriving on `result_rx` always means the bind/serve failed.
    // Sending it turns that failure into a loud assertion instead of a silent
    // fallthrough to whatever else happens to be listening on the name.
    let (result_tx, result_rx) = std::sync::mpsc::channel::<anyhow::Error>();
    let (connected_tx, connected_rx) = std::sync::mpsc::channel::<u64>();
    let handler = Arc::new(EventHandler { clients: Clients::default(), connected: connected_tx });

    let serve_name = name.clone();
    let serve_handler = Arc::clone(&handler);
    std::thread::Builder::new()
        .name("test-pipe-serve".into())
        .spawn(move || {
            // The test process exiting tears this thread down; `serve_at`
            // loops forever on success, so there is nothing to join.
            if let Err(e) = pipe::serve_at(&serve_name, serve_handler) {
                let _ = result_tx.send(e);
            }
        })
        .expect("failed to spawn the serve thread");

    let mut client = connect_client(&name, &result_rx);

    // Wait for the *server* to have registered the connection. Opening the file
    // only proves the client end exists; broadcasting before `client_connected`
    // ran would send to an empty registry and prove nothing.
    connected_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the server should register the connection");

    // Read on its own thread so a failure to deliver shows up as a 2 s timeout
    // rather than hanging the suite forever on a blocking pipe read.
    let (line_tx, line_rx) = std::sync::mpsc::channel::<String>();
    std::thread::Builder::new()
        .name("test-event-reader".into())
        .spawn(move || {
            let mut bytes = Vec::new();
            let mut byte = [0u8; 1];
            while let Ok(1) = client.read(&mut byte) {
                if byte[0] == b'\n' {
                    break;
                }
                bytes.push(byte[0]);
            }
            let _ = line_tx.send(String::from_utf8_lossy(&bytes).into_owned());
        })
        .expect("failed to spawn the reader thread");

    // The client above has not written a single byte, and never will.
    handler.clients.broadcast(&Event::new("clip_saved", serde_json::Value::Null));

    let text = line_rx.recv_timeout(Duration::from_secs(2)).expect(
        "an idle client must receive a broadcast without sending a request first — a timeout \
         here means the session loop is parked in a blocking read",
    );

    // Asserted on the decoded `event` field rather than the raw bytes: this
    // test is about delivery, not about JSON key order.
    let event: Event = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("event line did not decode as JSON: {e}\nline: {text}"));
    assert_eq!(event.event, "clip_saved");
}
