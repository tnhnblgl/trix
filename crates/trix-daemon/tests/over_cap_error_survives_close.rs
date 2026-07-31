//! Regression test for the over-cap error response (`pipe.rs`'s `serve_one`):
//! the client must actually receive the `LineError::TooLong` diagnostic
//! before the connection ends, not race a `DisconnectNamedPipe` that
//! discards it. See the comment at the end of `serve_one` in `pipe.rs` for
//! why the fix is to let the server's handle close on drop instead of
//! calling `DisconnectNamedPipe` explicitly.

use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::SyncSender;
use std::time::{Duration, Instant};

use trix_daemon::pipe::{self, ClientHandler};
use trix_proto::{MAX_LINE_BYTES, Request, Response};

/// Never actually dispatches anything in this test — the client disconnects
/// (or is refused) before a well-formed request could arrive.
struct StubHandler;

impl ClientHandler for StubHandler {
    fn client_connected(&self, _out: SyncSender<String>) -> u64 {
        1
    }

    fn client_disconnected(&self, _client: u64) {}

    fn dispatch(&self, _client: u64, request: &Request) -> Response {
        Response::ok(request.id, serde_json::Value::Null)
    }

    /// No registry behind this handler, so nothing can ever evict. Stated
    /// rather than inherited: `eviction_flag` is a required method precisely so
    /// a handler that *does* have a registry cannot forget it by accident.
    fn eviction_flag(&self, _client: u64) -> Option<Arc<AtomicBool>> {
        None
    }
}

/// Connects a client to the daemon's control pipe, retrying briefly. The
/// background thread that calls `pipe::serve_at` needs a moment to reach its
/// first `CreateNamedPipeW` before `name` exists for a client to open.
///
/// Polls `serve_failure` on every retry rather than only waiting out the
/// deadline: `serve_at` only returns (and sends) on error, so a value showing
/// up there means the pipe will never appear — reporting that error directly
/// is a better failure than the generic "could not connect" this loop would
/// otherwise take up to 5 seconds to reach.
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

/// Reads one `\n`-terminated line by hand. Deliberately not
/// `pipe::read_line_capped` — this is standing in for an arbitrary client,
/// not exercising the server's own reader.
fn read_line(source: &mut impl Read) -> Option<String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match source.read(&mut byte).expect("reading the response line failed") {
            0 => {
                return if line.is_empty() {
                    None
                } else {
                    Some(String::from_utf8_lossy(&line).into_owned())
                };
            }
            _ => {
                if byte[0] == b'\n' {
                    return Some(String::from_utf8_lossy(&line).into_owned());
                }
                line.push(byte[0]);
            }
        }
    }
}

#[test]
fn an_over_cap_line_gets_its_error_before_the_stream_ends() {
    // A private name, not `pipe::PIPE_NAME`: binding the real production pipe
    // here would race an actually-running `trix-daemon.exe` for
    // `FILE_FLAG_FIRST_PIPE_INSTANCE` and, if that daemon won, this test would
    // silently exercise its already-built binary instead of this crate's
    // code. The pid keeps concurrent test binaries (or a stale leftover) from
    // colliding on the same name.
    let name = format!(r"\\.\pipe\trix-control-test-{}", std::process::id());

    // `serve_at` only returns on error — it loops forever on success — so
    // anything arriving on `result_rx` always means the bind/serve failed.
    // Sending it (rather than swallowing it with `let _ =`) turns that
    // failure into a loud assertion instead of a silent fallthrough to
    // whatever else happens to be listening on the name.
    let (result_tx, result_rx) = std::sync::mpsc::channel::<anyhow::Error>();
    let serve_name = name.clone();
    std::thread::Builder::new()
        .name("test-pipe-serve".into())
        .spawn(move || {
            // The test process exiting tears this thread down; `serve_at`
            // loops forever on success, so there is nothing to join.
            if let Err(e) = pipe::serve_at(&serve_name, Arc::new(StubHandler)) {
                let _ = result_tx.send(e);
            }
        })
        .expect("failed to spawn the serve thread");

    let mut client = connect_client(&name, &result_rx);

    // MAX_LINE_BYTES + 1 bytes, no newline: exactly enough to trip
    // `read_line_capped`'s cap without ever completing a frame.
    let payload = vec![b'x'; MAX_LINE_BYTES + 1];
    client.write_all(&payload).expect("write to the pipe");

    let text = read_line(&mut client)
        .expect("the client must see the diagnostic line, not an immediate EOF/broken pipe");

    let response: Response = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("response line did not decode as JSON: {e}\nline: {text}"));

    assert_eq!(response.id, 0, "an unrecoverable line must reply with the reserved id");
    assert!(!response.ok, "an over-cap line must be reported as an error");
    assert_eq!(
        response.error.as_deref(),
        Some(format!("line exceeded {MAX_LINE_BYTES} bytes").as_str()),
        "the client must see the diagnostic, not lose it to the connection closing"
    );

    // The connection then ends cleanly: the next read is EOF (0 bytes), not
    // an error — proving the diagnostic above was itself read intact rather
    // than raced by the close.
    let mut trailing = [0u8; 1];
    let read_after = client.read(&mut trailing).expect("read after the response should not error");
    assert_eq!(read_after, 0, "the stream should end cleanly once the error line has been read");
}
