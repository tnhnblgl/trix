//! Does one command actually round-trip over the real control pipe?
//!
//! `pipe.rs`'s unit tests prove the framing and the correlation against
//! in-memory channels, and they pass whether or not the transport underneath
//! works at all. These two tests are the other half: a real
//! `\\.\pipe\trix-control`, a real daemon, and the exact handle layout
//! `daemon.rs::connect` uses — one `File` opened read+write, a `try_clone` for
//! the reader thread, the request written from another thread while that reader
//! is parked.
//!
//! That layout is the whole point. A named pipe opened without
//! `FILE_FLAG_OVERLAPPED` carries `FO_SYNCHRONOUS_IO` on its *file object*, and
//! `try_clone` is `DuplicateHandle` — a second handle onto that same file
//! object. The I/O manager serializes every operation on it, so a blocking read
//! pending on one handle stalls a write issued on the other, and the app
//! deadlocks on its first command: the write cannot finish until the read does,
//! the read cannot finish until the daemon replies, and the daemon cannot reply
//! until it receives the request. Nothing short of a real pipe reproduces that.
//!
//! [`the_naive_layout_deadlocks_the_write_behind_the_read`] pins that platform
//! behaviour, and is the reason `src/pipe_reader.rs` exists.
//! [`a_request_written_while_the_reader_is_parked_is_still_answered`] is the fix
//! working. It pulls the app's own reader in with `#[path]` rather than
//! reimplementing it, so the day somebody reverts the fix this test fails
//! instead of passing against a copy that is still correct.
//!
//! Both are ignored by default because they need a daemon; `cargo test
//! --workspace` stays green and unattended. Run them with:
//!
//! ```text
//! cargo test -p trix-ui --test pipe_roundtrip -- --ignored --nocapture
//! ```
//!
//! `status` is the only command either one sends, deliberately: it is
//! read-only, so these tests can never touch the user's real clip library or
//! config.

#[path = "../src/pipe_reader.rs"]
mod pipe_reader;

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use serde_json::Map;
use trix_proto::{Request, Response, encode_line};

use pipe_reader::PeekingPipeReader;

/// Same constant as `daemon.rs::PIPE_PATH`, spelled out rather than shared:
/// these tests exist to check that the *published* socket path works for an
/// outside client, and importing the constant from the code under test would
/// make a typo in it invisible here.
const PIPE_PATH: &str = r"\\.\pipe\trix-control";

/// How long a test waits for the write to complete, and then for the reply.
/// Generous next to a `status` the daemon answers out of memory in
/// microseconds — if either wait runs out, the transport is wedged, not slow.
const WAIT: Duration = Duration::from_secs(5);

/// Kills the daemon *only* if a test started it.
///
/// A daemon the developer already had running is theirs: it may be armed, and
/// killing it would throw away a replay ring nobody asked to lose.
struct DaemonGuard(Option<Child>);

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// One connect attempt, exactly as `daemon.rs::connect` makes it.
fn open_pipe() -> std::io::Result<File> {
    OpenOptions::new().read(true).write(true).open(PIPE_PATH)
}

/// `open_pipe`, retrying while every instance is busy.
///
/// The daemon creates the next pipe instance only *after* `ConnectNamedPipe`
/// hands it the previous one, so there is a sub-millisecond window after any
/// connect in which the name exists with no free instance and the open fails
/// with `ERROR_PIPE_BUSY` (231). These tests connect twice in a row — once to
/// find out whether a daemon is there, once for real — and land in that window
/// almost every time. The app needs no such helper: its supervisor already
/// treats a failed connect as "try again shortly".
fn connect(within: Duration) -> std::io::Result<File> {
    const ERROR_PIPE_BUSY: i32 = 231;
    let deadline = Instant::now() + within;
    loop {
        match open_pipe() {
            Ok(pipe) => return Ok(pipe),
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(e),
        }
    }
}

fn daemon_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/release/trix-daemon.exe")
}

/// The daemon these tests run against, started at most once per test binary.
///
/// `cargo test` runs the two tests on threads of their own, and without this
/// they would race: both would find no daemon, both would spawn one — the
/// second losing the `FILE_FLAG_FIRST_PIPE_INSTANCE` bind and exiting — and
/// whichever test finished first would kill the survivor out from under the
/// other. A `Weak` rather than a `OnceLock` because this slot owns a *process*:
/// statics are never dropped, so a `OnceLock` would leave a daemon (and its
/// tray icon, and its hotkey registration) running after the test binary exits.
/// Here the child dies with the last test that was using it.
static DAEMON: Mutex<Option<Weak<DaemonGuard>>> = Mutex::new(None);

fn ensure_daemon() -> Arc<DaemonGuard> {
    // Poisoning is ignored on purpose: it only means some other test panicked,
    // which says nothing about whether this slot's contents are usable.
    let mut slot = DAEMON.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(daemon) = slot.as_ref().and_then(Weak::upgrade) {
        return daemon;
    }
    let daemon = Arc::new(start_daemon());
    *slot = Some(Arc::downgrade(&daemon));
    daemon
}

/// Returns once `PIPE_PATH` is connectable, starting a daemon if there is none.
fn start_daemon() -> DaemonGuard {
    if connect(Duration::from_secs(1)).is_ok() {
        return DaemonGuard(None);
    }

    let exe = daemon_exe();
    assert!(
        exe.exists(),
        "no daemon is running and {} does not exist -- build it first: \
         cargo build --release -p trix-daemon",
        exe.display()
    );

    // stdout/stderr to null: the daemon's `tracing` output would otherwise be
    // interleaved into the test's own, and none of it is evidence of anything
    // these tests assert.
    let child = Command::new(&exe)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("could not start trix-daemon.exe");
    let guard = DaemonGuard(Some(child));

    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if connect(Duration::from_secs(1)).is_ok() {
            return guard;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("trix-daemon did not publish {PIPE_PATH} within 15s");
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

/// Waits for the daemon's answer to `id`, skipping anything unsolicited — an
/// event is not a reply, and the daemon may send one at any moment.
fn await_response(lines: &Receiver<String>, id: u64) -> Response {
    let deadline = Instant::now() + WAIT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let line = lines.recv_timeout(left).unwrap_or_else(|_| {
            panic!(
                "the daemon never answered the `status` request within {}s -- the write \
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
    assert!(response.ok, "the daemon rejected `status`: {:?}", response.error);
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
#[ignore = "needs a running trix-daemon"]
fn the_naive_layout_deadlocks_the_write_behind_the_read() {
    let _daemon = ensure_daemon();

    let pipe = connect(WAIT).expect("could not open the control pipe");
    let read_half = pipe.try_clone().expect("could not duplicate the pipe handle");
    let write_half = Arc::new(pipe);

    // A plain blocking read on the duplicated handle: the reader thread is
    // inside `ReadFile` and stays there until something arrives.
    let _lines = spawn_reader(read_half);
    // Long enough that the reader is certainly parked.
    std::thread::sleep(Duration::from_millis(200));

    let wrote = write_status(&write_half, 1);
    assert!(
        wrote.recv_timeout(WAIT).is_err(),
        "the `status` write completed in under {}s with a blocking read pending on a duplicate \
         of the same handle. That is the serialization `src/pipe_reader.rs` exists to work \
         around, and this platform no longer appears to do it -- re-check whether the wrapper \
         is still needed before trusting this",
        WAIT.as_secs()
    );
}

/// The fix, working: the app's own reader, the app's own handle layout, a real
/// daemon, and a command that comes back.
#[test]
#[ignore = "needs a running trix-daemon"]
fn a_request_written_while_the_reader_is_parked_is_still_answered() {
    let _daemon = ensure_daemon();

    // Exactly `daemon.rs::connect`: one read+write handle, one duplicate for
    // the reader thread, and `PeekingPipeReader` between that duplicate and the
    // `BufReader`.
    let pipe = connect(WAIT).expect("could not open the control pipe");
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
