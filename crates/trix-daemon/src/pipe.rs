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
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{HSTRING, PWSTR};

/// The control socket's well-known name. Part of the published protocol.
pub const PIPE_NAME: &str = r"\\.\pipe\trix-control";

const PIPE_BUFFER_BYTES: u32 = 64 * 1024;

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
        let owned = sid_string.to_string().context("SID string was not valid UTF-16")?;
        let _ = LocalFree(Some(windows::Win32::Foundation::HLOCAL(sid_string.0.cast())));
        Ok(owned)
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

/// A `HANDLE` closed on drop. Used for the token and for pipe instances that
/// fail before ownership moves into a `File`.
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
fn create_instance(security: &LocalSecurityDescriptor, first: bool) -> Result<std::fs::File> {
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
            &HSTRING::from(PIPE_NAME),
            open_mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
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
                "could not create {PIPE_NAME} ({error}) — another trix daemon is \
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
/// for Win32 calls (`ConnectNamedPipe`, `DisconnectNamedPipe`) that take a
/// bare `HANDLE` rather than a `File`.
fn handle_of(file: &std::fs::File) -> *mut std::ffi::c_void {
    use std::os::windows::io::AsRawHandle;
    file.as_raw_handle().cast()
}

/// Accepts clients forever, handing each to `handle` on its own thread.
///
/// One thread per client, blocking IO: a control socket sees a handful of
/// connections, and overlapped IO would be a large amount of unsafe code
/// bought for nothing.
pub fn serve<H>(handler: Arc<H>) -> Result<()>
where
    H: ClientHandler + Send + Sync + 'static,
{
    let security = user_only_security_descriptor()?;
    let mut first = true;
    loop {
        let instance = create_instance(&security, first)?;
        // Logged here, after the first instance is actually bound, rather
        // than by the caller before calling `serve` — `FILE_FLAG_FIRST_PIPE_INSTANCE`
        // makes that first `create_instance` the single-instance check, and a
        // second daemon must not claim to be listening right before failing it.
        if first {
            tracing::info!("listening on {PIPE_NAME}");
        }
        first = false;

        // ERROR_PIPE_CONNECTED means the client connected between creation and
        // this call — already connected, not an error.
        let connected = unsafe { ConnectNamedPipe(HANDLE(handle_of(&instance)), None) };
        if let Err(e) = connected
            && e.code() != ERROR_PIPE_CONNECTED.to_hresult()
        {
            tracing::warn!(error = %e, "ConnectNamedPipe failed, retrying");
            continue;
        }

        let handler = Arc::clone(&handler);
        std::thread::Builder::new()
            .name("trix-client".into())
            .spawn(move || {
                if let Err(e) = serve_one(instance, handler) {
                    tracing::debug!(error = %format!("{e:#}"), "client session ended");
                }
            })
            .context("failed to spawn a client thread")?;
    }
}

/// What the transport needs from the daemon: turn one request into one
/// response, and learn each client's id so events can be routed to it.
pub trait ClientHandler {
    fn client_connected(&self, out: std::sync::mpsc::Sender<String>) -> u64;
    fn client_disconnected(&self, client: u64);
    fn dispatch(&self, client: u64, request: &trix_proto::Request) -> trix_proto::Response;
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
/// `out_tx`/`out_rx` still exist because `ClientHandler::client_connected`
/// needs a `Sender<String>` — that channel is the seam Task 5's event
/// broadcast (`clients::Clients::broadcast`) hangs off. Anything queued on it
/// is drained and written out before each blocking read. Nothing calls
/// `Clients::broadcast` in this task, so that drain never has anything to
/// do — but when Task 5 wires it up, a broadcast that lands *while* this
/// thread is already blocked waiting for the client's next line will still
/// have to wait for that line (or a disconnect) before it can go out; truly
/// concurrent delivery needs the same overlapped-IO investment noted above.
/// Documented here so Task 5 does not have to rediscover it.
fn serve_one<H: ClientHandler>(instance: std::fs::File, handler: Arc<H>) -> Result<()> {
    let (out_tx, out_rx) = std::sync::mpsc::channel::<String>();
    let client = handler.client_connected(out_tx.clone());
    let mut writer = instance.try_clone().context("cloning the pipe handle")?;
    let mut reader = BufReader::new(instance.try_clone().context("cloning the pipe handle")?);
    let mut line = String::new();

    // Writes `text` and flushes; `false` means the client hung up.
    let mut send = |text: &str| -> bool { writer.write_all(text.as_bytes()).is_ok() && writer.flush().is_ok() };

    'session: loop {
        // Deliver anything already queued (Task 5's broadcast path) before
        // blocking on the next read — see the doc comment above.
        while let Ok(text) = out_rx.try_recv() {
            if !send(&text) {
                break 'session;
            }
        }

        match read_line_capped(&mut reader, &mut line) {
            Ok(false) => break, // clean EOF
            Ok(true) => {}
            Err(LineError::TooLong) => {
                // The one case where a bad line does close the connection:
                // past the cap the remaining bytes cannot be resynchronized to
                // a frame boundary, so there is nothing to recover to.
                let response = Response::err(RESERVED_ID, format!("line exceeded {MAX_LINE_BYTES} bytes"));
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
    unsafe {
        let _ = DisconnectNamedPipe(HANDLE(handle_of(&instance)));
    }
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

    /// The DACL is the whole security story: `D:P` protects it from inheriting
    /// a permissive default, and the single ACE grants the current user alone.
    #[test]
    fn the_security_descriptor_grants_only_this_user() {
        let sid = current_user_sid_string().expect("current user must have a SID");
        assert!(sid.starts_with("S-1-"), "unexpected SID form: {sid}");
        assert_eq!(sddl_for(&sid), format!("D:P(A;;GA;;;{sid})"));
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
