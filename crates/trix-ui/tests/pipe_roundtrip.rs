//! Does one command actually round-trip over a real Windows named pipe?
//!
//! `pipe.rs`'s unit tests prove the framing and the correlation against
//! in-memory channels, and they pass whether or not the transport underneath
//! works at all. These two tests are the other half: a real named pipe and the
//! exact handle layout `daemon.rs::connect` uses — one `File` opened
//! read+write, a `try_clone` for the reader thread, the request written from
//! another thread while that reader is parked.
//!
//! That layout is the whole point. A named pipe opened without
//! `FILE_FLAG_OVERLAPPED` carries `FO_SYNCHRONOUS_IO` on its *file object*, and
//! `try_clone` is `DuplicateHandle` — a second handle onto that same file
//! object. The I/O manager serializes every operation on it, so a blocking read
//! pending on one handle stalls a write issued on the other, and the app
//! deadlocks on its first command: the write cannot finish until the read does,
//! the read cannot finish until the server replies, and the server cannot reply
//! until it receives the request. Nothing short of a real pipe reproduces that.
//!
//! The server on the other end is [`StubDaemon`], forty lines of
//! `CreateNamedPipeW` in this file, and it is deliberately *not*
//! `trix-daemon.exe`. The serialization under test is a property of the
//! **client's** file object, so the server's identity contributes nothing to
//! either result — while starting the real daemon would load the developer's
//! `%APPDATA%\trix\config.toml`, scan their real clip library, register their
//! real global clip hotkey and put a tray icon in their notification area, all
//! to answer one `status`. Worse, the only way to stop it again is
//! `Child::kill`, which is `TerminateProcess` and runs no destructors, so a
//! test run could leave a ghost tray icon behind. Every integration test in
//! `trix-daemon/tests/` binds a private pipe name for the same family of
//! reasons; this follows them.
//!
//! [`the_naive_layout_deadlocks_the_write_behind_the_read`] pins the platform
//! behaviour, and is the reason `src/pipe_reader.rs` exists.
//! [`a_request_written_while_the_reader_is_parked_is_still_answered`] is the fix
//! working. It pulls the app's own reader in with `#[path]` rather than
//! reimplementing it, so gutting `PeekingPipeReader` fails this test instead of
//! leaving it passing against a mirror that is still correct.
//!
//! Note what that does not reach: `daemon.rs::connect` is where the wrapper is
//! actually put on the socket, and this test builds its own handle layout
//! rather than asking for that one. Revert
//! `BufReader::new(PeekingPipeReader::new(read_half))` to
//! `BufReader::new(read_half)` and both tests here still pass. Nothing
//! automated guards that line.
//!
//! Neither test is `#[ignore]`d: with no daemon to find, no config to read and
//! no library to scan, both belong in a plain `cargo test --workspace`, which
//! is the only run anybody is guaranteed to do.

#[path = "../src/pipe_reader.rs"]
mod pipe_reader;

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{Map, json};
use trix_proto::{RESERVED_ID, Request, Response, decode_request, encode_line};
use windows::Win32::Foundation::{ERROR_PIPE_CONNECTED, HANDLE};
use windows::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, NAMED_PIPE_MODE, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT, PeekNamedPipe,
};
use windows::core::HSTRING;

use pipe_reader::PeekingPipeReader;

/// How long a test waits for the write to complete, and then for the reply.
/// Generous next to a `status` the stub answers out of a literal in
/// microseconds — if either wait runs out, the transport is wedged, not slow.
const WAIT: Duration = Duration::from_secs(5);

/// How often the stub looks for a request, and how quickly it notices it has
/// been told to stop. The same 25 ms the daemon and `pipe_reader.rs` both use.
const STUB_POLL: Duration = Duration::from_millis(25);

/// The same byte-mode, no-remote-clients pipe the daemon creates
/// (`trix-daemon/src/pipe.rs::PIPE_MODE`). Byte mode matters here: newline
/// framing and the partial reads `PeekingPipeReader` performs are only correct
/// against a stream, so a message-mode stub would be testing a pipe the app
/// never talks to.
const PIPE_MODE: NAMED_PIPE_MODE = NAMED_PIPE_MODE(
    PIPE_TYPE_BYTE.0 | PIPE_READMODE_BYTE.0 | PIPE_WAIT.0 | PIPE_REJECT_REMOTE_CLIENTS.0,
);

const PIPE_BUFFER_BYTES: u32 = 64 * 1024;

/// A private pipe name for one test. Qualified by pid *and* by the caller's
/// label so the two tests in this binary cannot collide when cargo runs them on
/// threads of their own, and so neither can ever be `\\.\pipe\trix-control` —
/// binding that would fight a real `trix-daemon.exe` for the live socket.
fn private_pipe_name(label: &str) -> String {
    format!(r"\\.\pipe\trix-ui-{label}-test-{}", std::process::id())
}

/// One raw HANDLE value borrowed from a `&File` for the duration of one call.
fn handle_of(file: &File) -> HANDLE {
    HANDLE(file.as_raw_handle().cast())
}

/// Bytes the OS is holding on `pipe` right now. `Err` means the pipe is gone.
fn available(pipe: &File) -> Result<u32, windows::core::Error> {
    let mut available = 0u32;
    // SAFETY: the handle is borrowed from `pipe`, which owns it and keeps it
    // open for the whole call; the only non-`None` pointer argument is a live
    // local.
    unsafe {
        PeekNamedPipe(handle_of(pipe), None, 0, None, Some(&mut available), None)?;
    }
    Ok(available)
}

/// A single-client control-socket server, bound to a private pipe name.
///
/// It answers any decodable request with an `ok` carrying an `armed` field, so
/// the round-trip test can assert it got a *reply to its request* rather than
/// merely some bytes. That is the entire protocol surface these tests need: the
/// question under test is whether the request ever reaches the far end, not
/// what the far end does with it.
///
/// The worker stops on [`Self::stop`], which it checks once per [`STUB_POLL`].
/// A blocking read would be simpler and wrong — nothing would ever unblock it,
/// because the client handles outlive the test body inside the reader thread.
struct StubDaemon {
    stop: Arc<AtomicBool>,
    stopped: Receiver<()>,
    name: String,
}

impl StubDaemon {
    /// Binds `name` and starts serving. The instance is created on *this*
    /// thread, before the worker starts, so a bind failure surfaces as a panic
    /// in the test body rather than as a connect timeout thirty lines later.
    fn start(label: &str) -> Self {
        let name = private_pipe_name(label);
        let instance = create_instance(&name);
        let stop = Arc::new(AtomicBool::new(false));
        let (stopped_tx, stopped) = mpsc::channel();
        let worker_stop = Arc::clone(&stop);
        std::thread::Builder::new()
            .name("stub-daemon".into())
            .spawn(move || {
                serve_one(instance, &worker_stop);
                let _ = stopped_tx.send(());
            })
            .expect("could not spawn the stub daemon thread");
        Self { stop, stopped, name }
    }

    /// One connect attempt, exactly as `daemon.rs::connect` makes it.
    fn open(&self) -> std::io::Result<File> {
        OpenOptions::new().read(true).write(true).open(&self.name)
    }

    /// `open`, retrying until the worker has reached `ConnectNamedPipe`.
    ///
    /// The instance exists before `start` returns, but a client that opens it
    /// in the window before the worker accepts can still see `ERROR_PIPE_BUSY`
    /// (231), so every error is retried until the deadline. The app needs no
    /// such helper: its supervisor already treats a failed connect as "try
    /// again shortly".
    fn connect(&self) -> File {
        let deadline = Instant::now() + WAIT;
        loop {
            match self.open() {
                Ok(pipe) => return pipe,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Err(e) => panic!("could not connect to the stub daemon at {}: {e}", self.name),
            }
        }
    }
}

impl Drop for StubDaemon {
    /// Ends the worker, and with it the connection.
    ///
    /// This is what bounds the threads
    /// [`the_naive_layout_deadlocks_the_write_behind_the_read`] deliberately
    /// leaves stuck. Closing the server's handle breaks the pipe, which
    /// completes the client's pending `ReadFile` with an error; that releases
    /// the file object, which lets the write queued behind it fail too. Both
    /// client threads then fall out of their loops on their own. Nothing is
    /// left running past this drop except in the failure case below.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Bounded, not infinite: if the worker is still parked in
        // `ConnectNamedPipe` because a test panicked before it ever connected,
        // it cannot see the flag, and joining would turn a clear test failure
        // into a hung suite. In that case one thread and one private pipe
        // instance outlive this drop and die with the test binary.
        let _ = self.stopped.recv_timeout(WAIT);
    }
}

/// Creates one pipe instance on `name`, with default security.
///
/// The daemon builds a user-only DACL here (`trix-daemon/src/pipe.rs`) and
/// `trix-daemon/tests` has a test for it. This is not that test: nothing
/// reaches this pipe but the client fifteen lines below it, so the descriptor
/// is left at the default and `Win32_Security` stays out of the dev-dependency.
fn create_instance(name: &str) -> File {
    // SAFETY: the name is a live `HSTRING` for the duration of the call, and
    // the security-attributes argument is `None`, so no pointer outlives it.
    let handle = unsafe {
        CreateNamedPipeW(
            &HSTRING::from(name),
            PIPE_ACCESS_DUPLEX,
            PIPE_MODE,
            PIPE_UNLIMITED_INSTANCES,
            PIPE_BUFFER_BYTES,
            PIPE_BUFFER_BYTES,
            0,
            None,
        )
    };
    assert!(
        !handle.is_invalid(),
        "could not create the stub pipe {name}: {}",
        windows::core::Error::from_thread()
    );
    // SAFETY: `CreateNamedPipeW` returned a valid, unowned handle, and this is
    // the only thing that ever takes ownership of it.
    unsafe { File::from_raw_handle(handle.0.cast()) }
}

/// Accepts one client and answers its requests until `stop` is set or the
/// client goes away.
///
/// Read-then-write on one thread, like the daemon's `serve_one` and for the
/// same reason: the server's own pipe instance is synchronous too, so a reply
/// written from a second thread while this one held a read would queue behind
/// it exactly the way the bug under test does.
fn serve_one(instance: File, stop: &AtomicBool) {
    // ERROR_PIPE_CONNECTED means the client got there between the create and
    // this call — already connected, not a failure.
    // SAFETY: the handle is borrowed from `instance`, which outlives the call.
    let connected = unsafe { ConnectNamedPipe(handle_of(&instance), None) };
    if let Err(e) = connected
        && e.code() != ERROR_PIPE_CONNECTED.to_hresult()
    {
        return;
    }

    let Ok(reader) = instance.try_clone() else { return };
    let Ok(mut writer) = instance.try_clone() else { return };
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    while !stop.load(Ordering::Acquire) {
        // The OS is asked only when the reader has nothing of its own: a
        // `BufReader` drains the whole pipe buffer on its first read, so a peek
        // after that would correctly report zero and park this loop while it
        // was holding a request. Same ordering as the daemon's session loop.
        if reader.buffer().is_empty() {
            match available(&instance) {
                Ok(0) => {
                    std::thread::sleep(STUB_POLL);
                    continue;
                }
                Ok(_) => {}
                Err(_) => return, // the client is gone
            }
        }

        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if line.trim().is_empty() {
            continue;
        }

        let response = match decode_request(&line) {
            // A `status` payload in the shape `daemon.rs::on_connected` reads:
            // enough for the round-trip test to prove it is holding the answer
            // to its own request, and no more.
            Ok(Request { id, .. }) => Response::ok(id, json!({ "armed": false })),
            Err(e) => Response::err(RESERVED_ID, e),
        };
        let Ok(text) = encode_line(&response) else { return };
        if writer.write_all(text.as_bytes()).is_err() || writer.flush().is_err() {
            return;
        }
    }
}

/// Reads lines off `reader` on its own thread, exactly as
/// `Connection::start`'s reader thread does, and publishes each one.
fn spawn_reader<R: Read + Send + 'static>(reader: R) -> Receiver<String> {
    let (lines_tx, lines_rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            if lines_tx.send(line.clone()).is_err() {
                break;
            }
        }
    });
    lines_rx
}

/// Writes one `status` request on its own thread and reports whether the write
/// ever came back.
///
/// On its own thread because a write that never completes is the failure under
/// test: on the main thread it would hang `cargo test` with no output instead of
/// failing it.
fn write_status(pipe: &Arc<File>, id: u64) -> Receiver<Result<(), String>> {
    let request = Request { id, cmd: "status".to_string(), args: Map::new() };
    let line = encode_line(&request).expect("could not encode the status request");
    let (wrote_tx, wrote_rx) = mpsc::channel();
    let writer = Arc::clone(pipe);
    std::thread::spawn(move || {
        let result = (&*writer).write_all(line.as_bytes()).and_then(|()| (&*writer).flush());
        let _ = wrote_tx.send(result.map_err(|e| e.to_string()));
    });
    wrote_rx
}

/// Waits for the answer to `id`, skipping anything unsolicited — an event is
/// not a reply, and a server may send one at any moment.
fn await_response(lines: &Receiver<String>, id: u64) -> Response {
    let deadline = Instant::now() + WAIT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let line = lines.recv_timeout(left).unwrap_or_else(|_| {
            panic!(
                "the stub never answered the `status` request within {}s -- the write \
                 completed, so the reply is what is missing",
                WAIT.as_secs()
            )
        });
        if let Ok(response) = serde_json::from_str::<Response>(line.trim_end())
            && response.id == id
        {
            return response;
        }
    }
}

fn assert_is_a_status_payload(response: Response) {
    assert!(response.ok, "the stub rejected `status`: {:?}", response.error);
    let data = response.data.expect("an ok `status` response carries data");
    assert!(
        data.get("armed").is_some(),
        "a `status` payload should describe the engine, got: {data}"
    );
    println!("status round-tripped while the reader was parked: {data}");
}

/// The bug, pinned.
///
/// This is a characterization test, not a regression test: it asserts that
/// Windows still behaves the way `src/pipe_reader.rs` says it does. It passed
/// before the fix and it passes after, because it never touches the fix — it
/// reads with a plain `BufReader<File>`, which is what `daemon.rs` used to hand
/// `Connection::start`. If it ever starts failing, the write it expects to be
/// stuck is completing, and the peek-then-read wrapper has become removable.
#[test]
fn the_naive_layout_deadlocks_the_write_behind_the_read() {
    let stub = StubDaemon::start("naive");

    let pipe = stub.connect();
    let read_half = pipe.try_clone().expect("could not duplicate the pipe handle");
    let write_half = Arc::new(pipe);

    // A plain blocking read on the duplicated handle: the reader thread is
    // inside `ReadFile` and stays there until something arrives.
    let _lines = spawn_reader(read_half);
    // Long enough that the reader is certainly parked.
    std::thread::sleep(Duration::from_millis(200));

    let wrote = write_status(&write_half, 1);
    // Matched rather than tested with `is_err()`: `Disconnected` is also an
    // `Err`, so a writer thread that panicked would have made this assertion
    // pass instantly having proven nothing at all.
    match wrote.recv_timeout(WAIT) {
        Err(RecvTimeoutError::Timeout) => {}
        Err(RecvTimeoutError::Disconnected) => {
            panic!("the writer thread died before reporting, so this test proved nothing")
        }
        Ok(result) => panic!(
            "the `status` write completed in under {}s ({result:?}) with a blocking read pending \
             on a duplicate of the same handle. That is the serialization `src/pipe_reader.rs` \
             exists to work around, and this platform no longer appears to do it -- re-check \
             whether the wrapper is still needed before trusting this",
            WAIT.as_secs()
        ),
    }

    // Dropping `stub` here is what unwedges the two threads this test leaves
    // stuck; see `StubDaemon::drop`. Explicit rather than implicit because the
    // cleanup is the point, not an afterthought.
    drop(stub);
}

/// The fix, working: the app's own reader, the app's own handle layout, and a
/// command that comes back.
#[test]
fn a_request_written_while_the_reader_is_parked_is_still_answered() {
    let stub = StubDaemon::start("roundtrip");

    // Exactly `daemon.rs::connect`: one read+write handle, one duplicate for
    // the reader thread, and `PeekingPipeReader` between that duplicate and the
    // `BufReader`.
    let pipe = stub.connect();
    let read_half = pipe.try_clone().expect("could not duplicate the pipe handle");
    let write_half = Arc::new(pipe);

    let lines = spawn_reader(PeekingPipeReader::new(read_half));
    // Long enough that the reader is certainly parked in `read_line` — the
    // state the app is always in between commands, and the state that made this
    // a deadlock rather than a race.
    std::thread::sleep(Duration::from_millis(200));

    let wrote = write_status(&write_half, 1).recv_timeout(WAIT).unwrap_or_else(|_| {
        panic!(
            "the `status` write never completed in {}s while the reader thread was parked in \
             `read_line` -- the pipe is opened without FILE_FLAG_OVERLAPPED, so the I/O manager \
             is serializing the write behind a read the reader should not be holding",
            WAIT.as_secs()
        )
    });
    assert!(wrote.is_ok(), "writing the status request failed: {wrote:?}");

    assert_is_a_status_payload(await_response(&lines, 1));
}
