//! Regression test for the over-cap error response (`pipe.rs`'s `serve_one`):
//! the client must actually receive the `LineError::TooLong` diagnostic
//! before the connection ends, not race a `DisconnectNamedPipe` that
//! discards it. See the comment at the end of `serve_one` in `pipe.rs` for
//! why the fix is to let the server's handle close on drop instead of
//! calling `DisconnectNamedPipe` explicitly.

use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use trix_daemon::pipe::{self, ClientHandler};
use trix_proto::{MAX_LINE_BYTES, Request, Response};

/// Never actually dispatches anything in this test — the client disconnects
/// (or is refused) before a well-formed request could arrive.
struct StubHandler;

impl ClientHandler for StubHandler {
    fn client_connected(&self, _out: Sender<String>) -> u64 {
        1
    }

    fn client_disconnected(&self, _client: u64) {}

    fn dispatch(&self, _client: u64, request: &Request) -> Response {
        Response::ok(request.id, serde_json::Value::Null)
    }
}

/// Connects a client to the daemon's control pipe, retrying briefly. The
/// background thread that calls `pipe::serve` needs a moment to reach its
/// first `CreateNamedPipeW` before `PIPE_NAME` exists for a client to open.
fn connect_client() -> std::fs::File {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match std::fs::OpenOptions::new().read(true).write(true).open(pipe::PIPE_NAME) {
            Ok(file) => return file,
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("could not connect to {}: {e}", pipe::PIPE_NAME),
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
            0 => return if line.is_empty() { None } else { Some(String::from_utf8_lossy(&line).into_owned()) },
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
    std::thread::Builder::new()
        .name("test-pipe-serve".into())
        .spawn(|| {
            // The test process exiting tears this thread down; `serve` loops
            // forever on success, so there is nothing to join.
            let _ = pipe::serve(Arc::new(StubHandler));
        })
        .expect("failed to spawn the serve thread");

    let mut client = connect_client();

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
