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
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context as _, Result, anyhow, bail};
use trix_proto::{MAX_LINE_BYTES, RESERVED_ID, Response, decode_request, encode_line};
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
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT, PeekNamedPipe,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{HSTRING, PWSTR};

/// The control socket's well-known name. Part of the published protocol.
pub const PIPE_NAME: &str = r"\\.\pipe\trix-control";

const PIPE_BUFFER_BYTES: u32 = 64 * 1024;

/// The pipe mode word every instance is created with.
///
/// Hoisted out of the [`CreateNamedPipeW`] call so that
/// `PIPE_REJECT_REMOTE_CLIENTS` — half of the transport's security story, and a
/// binding constraint of spec §4.1 — is something a test can assert on. Unlike
/// the DACL it cannot be read back off a live handle, so a named constant is
/// the only place the claim can be pinned; see
/// `the_pipe_mode_rejects_remote_clients` below, which fails the day someone
/// edits these flags.
const PIPE_MODE: windows::Win32::System::Pipes::NAMED_PIPE_MODE =
    windows::Win32::System::Pipes::NAMED_PIPE_MODE(
        PIPE_TYPE_BYTE.0 | PIPE_READMODE_BYTE.0 | PIPE_WAIT.0 | PIPE_REJECT_REMOTE_CLIENTS.0,
    );

/// How often an idle connection wakes to flush queued events. A UI blocked in
/// `read_line_capped` cannot be written to — a synchronous pipe serializes I/O
/// on the file object — so the loop must never park indefinitely.
///
/// 25 ms is below the threshold where a person perceives a UI as lagging, and
/// costs one `PeekNamedPipe` per connection per interval: a few dozen cheap
/// syscalls a second against a 60 fps hardware encode. The alternative,
/// overlapped I/O, buys latency nobody can perceive for a few hundred lines of
/// unsafe FFI in the one place where a bug is a hang or an abort.
const EVENT_POLL_MS: u64 = 25;

/// How long the accept loop waits before retrying after a failure that is not
/// the first-instance bind. Long enough that a persistently failing
/// `CreateNamedPipeW`/`ConnectNamedPipe` cannot spin a core at 100% emitting a
/// `warn!` per iteration, short enough that a UI reconnecting after a transient
/// blip does not notice.
const ACCEPT_RETRY_MS: u64 = 250;

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
        // Freed unconditionally, before the `?` below propagates any error —
        // otherwise a `to_string` failure would return without ever freeing
        // the string `ConvertSidToStringSidW` allocated.
        let owned = sid_string.to_string().context("SID string was not valid UTF-16");
        let _ = LocalFree(Some(windows::Win32::Foundation::HLOCAL(sid_string.0.cast())));
        owned
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

/// A `HANDLE` closed on drop. Used for the process token handle obtained in
/// `current_user_sid_string`.
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
    name: &str,
    security: &LocalSecurityDescriptor,
    first: bool,
) -> Result<std::fs::File> {
    // `CreateNamedPipeW`'s last parameter is `Option<*const SECURITY_ATTRIBUTES>`
    // (a borrow, not an out-param), so `attributes` is never mutated after
    // construction — no `mut` binding, matching the project's zero-warnings bar.
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: security.0.0,
        bInheritHandle: false.into(),
    };
    let open_mode =
        PIPE_ACCESS_DUPLEX | if first { FILE_FLAG_FIRST_PIPE_INSTANCE } else { Default::default() };

    let handle = unsafe {
        CreateNamedPipeW(
            &HSTRING::from(name),
            open_mode,
            PIPE_MODE,
            PIPE_UNLIMITED_INSTANCES,
            PIPE_BUFFER_BYTES,
            PIPE_BUFFER_BYTES,
            0,
            Some(&attributes as *const _),
        )
    };
    if handle.is_invalid() {
        // `windows::core::Error::from_win32` does not exist on this crate
        // version (0.62's `Error` only has `from_thread`/`from_hresult`;
        // `from_win32` lives on `HRESULT`, not `Error`) — `from_thread` reads
        // the same `GetLastError()` value `CreateNamedPipeW` just set.
        let error = windows::core::Error::from_thread();
        if first {
            bail!(
                "could not create {name} ({error}) — another trix daemon is \
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

/// One raw HANDLE value, viewed from a `&File` for the duration of one call.
/// `File` owns and closes the underlying handle; this just borrows its value
/// for Win32 calls (`ConnectNamedPipe`) that take a bare `HANDLE` rather than
/// a `File`.
fn handle_of(file: &std::fs::File) -> *mut std::ffi::c_void {
    use std::os::windows::io::AsRawHandle;
    file.as_raw_handle().cast()
}

/// True when the client has sent at least one byte *that the OS still holds*,
/// so a read will not block. `Err` means the pipe is gone — the caller ends the
/// session.
///
/// Only ever a partial answer: this peeks the OS pipe buffer, and the session
/// reads through a `BufReader` that drains that buffer wholesale on its first
/// read. Bytes already sitting in the reader are invisible here, which is why
/// `serve_one` checks `BufReader::buffer()` before it calls this at all.
///
/// The `Err` case is narrower than it looks, and that was measured rather than
/// assumed: with a request written and the client handle then closed,
/// `PeekNamedPipe` still reports the queued bytes (`Ok`, `available = 25` for a
/// 25-byte request) and only fails with `ERROR_BROKEN_PIPE` once they have been
/// read. So a closing client's last command is not lost to this call — `Err`
/// means the pipe is both broken *and* drained.
fn request_pending(instance: &std::fs::File) -> Result<bool, windows::core::Error> {
    let mut available = 0u32;
    unsafe {
        PeekNamedPipe(HANDLE(handle_of(instance)), None, 0, None, Some(&mut available), None)?;
    }
    Ok(available > 0)
}

/// Accepts clients forever on the published [`PIPE_NAME`], handing each to
/// `handler` on its own thread. Thin delegation to [`serve_at`] — production
/// callers want this one.
///
/// One thread per client, blocking IO: a control socket sees a handful of
/// connections, and overlapped IO would be a large amount of unsafe code
/// bought for nothing.
pub fn serve<H>(handler: Arc<H>) -> Result<()>
where
    H: ClientHandler + Send + Sync + 'static,
{
    serve_at(PIPE_NAME, handler)
}

/// Accepts clients forever on `name`, handing each to `handler` on its own
/// thread.
///
/// Exists as a separate entry point so tests can bind a private pipe name
/// instead of claiming the live [`PIPE_NAME`] — production callers want
/// [`serve`].
pub fn serve_at<H>(name: &str, handler: Arc<H>) -> Result<()>
where
    H: ClientHandler + Send + Sync + 'static,
{
    let security = user_only_security_descriptor()?;
    let mut first = true;
    let retry = std::time::Duration::from_millis(ACCEPT_RETRY_MS);
    loop {
        // Only the *first* instance failing is fatal: that is the
        // single-instance check (`FILE_FLAG_FIRST_PIPE_INSTANCE`), and another
        // daemon owning the name must abort startup. Every later failure is
        // transient by nature — handle exhaustion, nonpaged pool pressure — and
        // propagating it would return out of `serve`, out of `main`, and
        // terminate a process that may be holding a live replay ring. Losing
        // the user's next clip because one `CreateNamedPipeW` blipped is not a
        // trade worth making; log it, wait, and try again.
        let instance = match create_instance(name, &security, first) {
            Ok(instance) => instance,
            Err(e) if first => return Err(e),
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "could not create a pipe instance, retrying");
                std::thread::sleep(retry);
                continue;
            }
        };
        // Logged here, after the first instance is actually bound, rather
        // than by the caller before calling `serve` — `FILE_FLAG_FIRST_PIPE_INSTANCE`
        // makes that first `create_instance` the single-instance check, and a
        // second daemon must not claim to be listening right before failing it.
        if first {
            tracing::info!("listening on {name}");
        }
        first = false;

        // ERROR_PIPE_CONNECTED means the client connected between creation and
        // this call — already connected, not an error.
        let connected = unsafe { ConnectNamedPipe(HANDLE(handle_of(&instance)), None) };
        if let Err(e) = connected
            && e.code() != ERROR_PIPE_CONNECTED.to_hresult()
        {
            // The same sleep closes the other half of the problem: without it,
            // a persistently failing `ConnectNamedPipe` spins this loop at full
            // CPU emitting one `warn!` per iteration.
            tracing::warn!(error = %e, "ConnectNamedPipe failed, retrying");
            std::thread::sleep(retry);
            continue;
        }

        let handler = Arc::clone(&handler);
        let spawned = std::thread::Builder::new().name("trix-client".into()).spawn(move || {
            if let Err(e) = serve_one(instance, handler) {
                tracing::debug!(error = %format!("{e:#}"), "client session ended");
            }
        });
        // Same reasoning as the `create_instance` failure above: a thread that
        // cannot be spawned is one client that does not get served, not a
        // reason to kill an armed capture. The closure — and with it this pipe
        // instance's handle — is dropped by the failed `spawn`, so the instance
        // is closed rather than leaked.
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "could not spawn a client thread, dropping the connection");
            std::thread::sleep(retry);
        }
    }
}

/// What the transport needs from the daemon: turn one request into one
/// response, and learn each client's id so events can be routed to it.
pub trait ClientHandler {
    /// The queue is a `SyncSender`, not a `Sender`: it is bounded, and a
    /// broadcast that overflows it drops the client rather than the daemon's
    /// memory. See [`crate::clients::OUTBOUND_QUEUE_DEPTH`].
    fn client_connected(&self, out: std::sync::mpsc::SyncSender<String>) -> u64;
    fn client_disconnected(&self, client: u64);
    fn dispatch(&self, client: u64, request: &trix_proto::Request) -> trix_proto::Response;

    /// This client's eviction flag, if the handler keeps one. Set by the
    /// registry when a broadcast overflows the client's queue and its entry is
    /// dropped (`clients::Clients::send_to`), and polled by [`serve_one`] so
    /// the *connection* ends too.
    ///
    /// Without it, eviction stops the daemon's memory growing and nothing
    /// else: the socket stays open, the session thread keeps answering
    /// requests when it unparks, and the client is permanently event-deaf and
    /// never told why. A UI that stalled for a moment would come back
    /// answering `status` correctly while silently missing every `clip_saved`.
    /// A closed connection is the honest outcome — the client sees the
    /// disconnect and reconnects.
    ///
    /// Deliberately *not* defaulted, though it was when it was introduced. A
    /// `{ None }` default made this method optional to implement, and a handler
    /// that omitted it got half a feature with no compile error and no test
    /// failure: eviction would stop the daemon's memory growing and leave the
    /// socket open forever, event-deaf. That was not hypothetical — the
    /// `EventHandler` in `tests/events.rs` drives a *real* [`crate::clients::Clients`]
    /// registry and had inherited exactly that hole. Requiring the method makes
    /// the compiler catch the next one, which is precisely what the default was
    /// hiding. A handler that genuinely never evicts returns `None` explicitly,
    /// which costs it one line and says so out loud.
    fn eviction_flag(&self, client: u64) -> Option<Arc<AtomicBool>>;
}

/// One connected client: read a line, dispatch it, write the response —
/// sequentially, on a single thread.
///
/// The brief's design put the write on a *second* thread, fed by a channel,
/// reasoning that "an event broadcast to every client never blocks behind
/// one slow reader." That does not work on a synchronous named pipe: a pipe
/// instance created without `FILE_FLAG_OVERLAPPED` allows only one *pending*
/// I/O operation against its underlying file object at a time, even across
/// duplicated handles on different threads. Verified empirically — with a
/// read blocked on one thread, a write attempted from a second thread never
/// even starts completing (not "slow": the daemon's own `write_all` sat
/// pending for the client's full 30 s test timeout, every time, only
/// returning once the client gave up and the OS reported the pipe broken).
/// Moving the write onto the reading thread — same handles, same data, only
/// the thread changed — fixed it instantly and reproducibly. Real concurrent
/// read+write needs overlapped I/O, which the brief itself rules out as
/// unwarranted complexity for this transport ("a large amount of unsafe code
/// bought for nothing"); the same tradeoff applies here, more so, since a
/// hand-rolled `OVERLAPPED` read/write pair is a bigger undertaking than the
/// accept loop the brief was talking about. A single thread doing read then
/// write has no such contention and is what this function does instead.
///
/// `out_tx`/`out_rx` carry the event broadcast (`clients::Clients::broadcast`)
/// to this thread, and anything queued on them is drained and written out
/// before the loop waits for the next request.
///
/// Task 4 left one gap in that, which this task closes. Draining before a
/// *blocking* read is not enough once something actually broadcasts: a UI
/// sitting idle is parked inside `read_line_capped`, so an event queued on its
/// channel would not be delivered until that UI happened to send its next
/// command — and spec §4.4 requires unsolicited events. The loop therefore
/// polls (every [`EVENT_POLL_MS`]) instead of blocking indefinitely, and only
/// enters `read_line_capped` once there is a byte to read.
///
/// "Is there a byte to read" is deliberately two questions. [`request_pending`]
/// peeks the *OS* buffer, but the session reads through a `BufReader` that
/// empties that buffer on its first read — so a client that writes two complete
/// request lines in one `write_all` leaves the second one in the reader, where
/// the peek cannot see it. The reader's own buffer is therefore checked first
/// and the OS is consulted only when it is empty. Getting that backwards is a
/// hang, not a slowdown: the client waits forever for a reply to a request the
/// daemon is holding but never looks at.
///
/// What the poll does *not* bound is a half-written line. Once one byte has
/// arrived, `read_line_capped` blocks until the newline does, so a client that
/// sends `{` and then stops parks this thread — and this thread alone —
/// indefinitely; it also stops flushing that client's events, which are its
/// own. [`MAX_LINE_BYTES`] bounds the memory such a client can cost, nothing
/// bounds the time. Fixing that needs a read deadline, which on a synchronous
/// pipe means overlapped I/O — the unsafe complexity this transport is
/// explicitly not paying for. The failure is self-inflicted and confined to the
/// connection that caused it.
fn serve_one<H: ClientHandler>(instance: std::fs::File, handler: Arc<H>) -> Result<()> {
    let (out_tx, out_rx) =
        std::sync::mpsc::sync_channel::<String>(crate::clients::OUTBOUND_QUEUE_DEPTH);
    // Cloned before `client_connected` registers this connection: if either
    // `try_clone` fails, the `?` below must return without ever having
    // registered a client, or the registry would keep a live entry for a
    // connection that never actually started (nothing would call
    // `client_disconnected` to remove it).
    let mut writer = instance.try_clone().context("cloning the pipe handle")?;
    let mut reader = BufReader::new(instance.try_clone().context("cloning the pipe handle")?);
    let client = handler.client_connected(out_tx.clone());
    // Taken once, right after registration, while the entry certainly exists —
    // eviction drops that entry, so asking later could not find it.
    let evicted = handler.eviction_flag(client);
    let mut line = String::new();

    // Writes `text` to the pipe. `flush()` is unconditionally `Ok(())` for a
    // `File` on Windows (std never calls `FlushFileBuffers`), so it cannot by
    // itself report anything; it stays in the chain only so a failure from
    // either call is what makes `send` return `false`.
    let mut send = |text: &str| -> bool {
        writer.write_all(text.as_bytes()).is_ok() && writer.flush().is_ok()
    };

    'session: loop {
        // Evicted for not draining its events (see `ClientHandler::eviction_flag`).
        // Checked first, and before the drain: the registry entry is already
        // gone, so what is left in the queue is a backlog for a client that has
        // been given up on. Ending the connection here is what makes eviction
        // visible to that client — it unparks, sees the disconnect, reconnects.
        if evicted.as_ref().is_some_and(|flag| flag.load(Ordering::Acquire)) {
            tracing::debug!(client, "closing an evicted client's connection");
            break 'session;
        }

        // Anything a broadcast queued while this client was idle.
        while let Ok(text) = out_rx.try_recv() {
            if !send(&text) {
                break 'session;
            }
        }

        // The OS is asked only when the reader has nothing of its own. A
        // `BufReader` drains the whole pipe buffer on its first read, so after
        // a client writes two requests in one `write_all` the second one lives
        // in `reader` and `PeekNamedPipe` — correctly — reports zero bytes
        // available. Peeking unconditionally would park this loop in its 25 ms
        // sleep forever while holding a request it had already been given.
        if reader.buffer().is_empty() {
            match request_pending(&instance) {
                Ok(true) => {}
                Ok(false) => {
                    std::thread::sleep(std::time::Duration::from_millis(EVENT_POLL_MS));
                    continue 'session;
                }
                Err(e) => {
                    tracing::debug!(error = %e, "peek failed; client is gone");
                    break 'session;
                }
            }
        }

        match read_line_capped(&mut reader, &mut line) {
            Ok(false) => break, // clean EOF
            Ok(true) => {}
            Err(LineError::TooLong) => {
                // The one case where a bad line does close the connection:
                // past the cap the remaining bytes cannot be resynchronized to
                // a frame boundary, so there is nothing to recover to.
                let response =
                    Response::err(RESERVED_ID, format!("line exceeded {MAX_LINE_BYTES} bytes"));
                if let Ok(text) = encode_line(&response) {
                    send(&text);
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
                if !send(&text) {
                    break;
                }
            }
            Err(e) => tracing::error!(error = %e, "could not serialize a response"),
        }
    }

    handler.client_disconnected(client);
    drop(out_tx);
    // Deliberately no `DisconnectNamedPipe` call here. `write_all` only
    // copies bytes into the pipe's output buffer, and `File::flush` is a
    // no-op on Windows (std does not call `FlushFileBuffers`), so bytes
    // written by `send` just above — including the one response this design
    // most wants a client to receive before the connection ends, the
    // `LineError::TooLong` diagnostic — can still be sitting unread.
    // `DisconnectNamedPipe` is documented to discard exactly that: data the
    // client has not yet read. Letting `instance` (and its clones `writer`/
    // `reader`) close on drop instead leaves those bytes in the pipe for the
    // client to read; the client then sees a clean end-of-stream once it has
    // drained them, rather than a broken-pipe error in place of the message.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pipe name is part of the published protocol — third-party UIs hard
    /// code it. It does not get to drift.
    #[test]
    fn pipe_name_is_the_published_one() {
        assert_eq!(PIPE_NAME, r"\\.\pipe\trix-control");
    }

    /// The SDDL *string builder*, and only that. Kept because the literal is
    /// the published form, but note what it does not say: nothing here proves
    /// the string ever reaches a pipe. That claim belongs to
    /// `the_live_pipe_carries_the_user_only_dacl` below, which is the test that
    /// fails if the security attributes are dropped from `CreateNamedPipeW`.
    #[test]
    fn the_security_descriptor_grants_only_this_user() {
        let sid = current_user_sid_string().expect("current user must have a SID");
        assert!(sid.starts_with("S-1-"), "unexpected SID form: {sid}");
        assert_eq!(sddl_for(&sid), format!("D:P(A;;GA;;;{sid})"));
    }

    /// The DACL as the kernel actually holds it, read back off a live pipe.
    ///
    /// This exists because the string-builder test above is a tautology with
    /// respect to the thing that matters. Replace `Some(&attributes as *const
    /// _)` in `create_instance` with `None` and that test — and every other
    /// test in the workspace — stays green while the pipe silently inherits the
    /// permissive default DACL, which for a named pipe grants read access to
    /// Everyone and to the anonymous account. Spec §4.1 makes the user-only
    /// DACL a binding constraint; hand verification does not survive the next
    /// refactor, so the constraint needs a test that can fail.
    ///
    /// Binds a pid-private name rather than the live [`PIPE_NAME`], for the
    /// same reason [`serve_at`] exists: running the suite must not fight a
    /// daemon that is already serving the real socket.
    ///
    /// What is asserted is deliberately not the exact ACE text. `GA`
    /// (`GENERIC_ALL`) is mapped to the object type's specific rights when the
    /// descriptor is applied, so the rights field comes back as a hex mask
    /// rather than the `GA` that went in — asserting on it would pin an
    /// implementation detail of the kernel's generic mapping. The three things
    /// asserted are the three the security argument actually rests on: the DACL
    /// is *protected* (`D:P`, so it did not inherit anything), it names this
    /// user, and it contains exactly one allow ACE and no deny ACEs — so there
    /// is nobody else on it.
    #[test]
    fn the_live_pipe_carries_the_user_only_dacl() {
        use windows::Win32::Foundation::ERROR_SUCCESS;
        use windows::Win32::Security::Authorization::{
            ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo, SE_KERNEL_OBJECT,
        };
        use windows::Win32::Security::DACL_SECURITY_INFORMATION;

        let sid = current_user_sid_string().expect("current user must have a SID");
        let security = user_only_security_descriptor().expect("building the descriptor");
        let name = format!(r"\\.\pipe\trix-dacl-test-{}", std::process::id());
        let instance = create_instance(&name, &security, false).expect("binding the test pipe");

        // The descriptor the kernel hands back is its own allocation, freed
        // with `LocalFree` — hence the guard type, which also covers the early
        // return an assertion failure would take.
        let mut live = PSECURITY_DESCRIPTOR::default();
        let status = unsafe {
            GetSecurityInfo(
                HANDLE(handle_of(&instance)),
                SE_KERNEL_OBJECT,
                DACL_SECURITY_INFORMATION,
                None,
                None,
                None,
                None,
                Some(&mut live),
            )
        };
        assert_eq!(status, ERROR_SUCCESS, "GetSecurityInfo on the live pipe failed: {status:?}");
        let live = LocalSecurityDescriptor(live);

        let mut text = PWSTR::null();
        unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                live.0,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &mut text,
                None,
            )
            .expect("converting the live descriptor back to SDDL");
        }
        let sddl = unsafe { text.to_string() }.expect("the live SDDL is valid UTF-16");
        unsafe {
            let _ = LocalFree(Some(windows::Win32::Foundation::HLOCAL(text.0.cast())));
        }

        assert!(
            sddl.starts_with("D:P"),
            "the live DACL is not protected, so it inherited a default: {sddl}"
        );
        assert!(sddl.contains(&sid), "the live DACL does not name this user ({sid}): {sddl}");
        assert_eq!(
            sddl.matches("(A;").count(),
            1,
            "the live DACL grants access to somebody other than this user: {sddl}"
        );
        assert_eq!(
            sddl.matches("(D;").count(),
            0,
            "the live DACL carries a deny ACE, which this descriptor never builds: {sddl}"
        );
    }

    /// `PIPE_REJECT_REMOTE_CLIENTS` is the other half of spec §4.1's transport
    /// constraint, and unlike the DACL it cannot be read back off a handle —
    /// which is exactly why the mode word is a named constant. Asserting on
    /// [`PIPE_MODE`] is not a tautology the way asserting on `sddl_for` was:
    /// `create_instance` passes this same constant to `CreateNamedPipeW`, so
    /// there is one definition and no second copy to drift from.
    #[test]
    fn the_pipe_mode_rejects_remote_clients() {
        assert_ne!(
            PIPE_MODE.0 & PIPE_REJECT_REMOTE_CLIENTS.0,
            0,
            "a pipe that accepts remote clients is reachable from off the machine"
        );
        // The other three names in the word contribute no bits at all:
        // `PIPE_TYPE_BYTE`, `PIPE_READMODE_BYTE` and `PIPE_WAIT` are Win32's
        // defaults and are literally zero — it is their opposites
        // (`PIPE_TYPE_MESSAGE`, `PIPE_READMODE_MESSAGE`, `PIPE_NOWAIT`) that
        // carry bits. So `PIPE_MODE & PIPE_WAIT != 0` would be a *false*
        // assertion, and the true claim is that none of those opposites is
        // set. Pinning the whole word says exactly that, and fails either way:
        // drop `PIPE_REJECT_REMOTE_CLIENTS` and it fails, add message framing
        // or non-blocking I/O — both of which `serve_one` is not written for —
        // and it fails too.
        assert_eq!(
            PIPE_MODE.0, PIPE_REJECT_REMOTE_CLIENTS.0,
            "the mode word changed; the session loop assumes a blocking byte stream (spec §4.1)"
        );
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
