//! Reading the control pipe without holding it against the writer.
//!
//! One type, and the only Windows-specific code in this crate. It is a module
//! of its own rather than a private item in `daemon.rs` for one reason:
//! `trix-ui` is a `[[bin]]`-only crate, so an integration test cannot import
//! anything from it, and `tests/pipe_roundtrip.rs` — the test that proves this
//! against a real daemon — pulls this file in with `#[path]`. That makes the
//! test compile *this* source rather than a copy of it, so reverting the fix
//! breaks the test instead of quietly leaving it passing. `daemon.rs` cannot be
//! included that way: it needs `tauri` and `crate::pipe`.

use std::fs::File;
use std::io::Read;
use std::time::Duration;

use windows::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED, HANDLE, WIN32_ERROR,
};
use windows::Win32::System::Pipes::PeekNamedPipe;

/// How long the reader waits before asking the pipe again whether anything has
/// arrived.
///
/// The same 25 ms the daemon's own loop uses (`trix-daemon/src/pipe.rs`,
/// `EVENT_POLL_MS`), for the same reason and with the same tradeoff: it is
/// below the threshold at which a person perceives a UI as lagging, and it
/// costs one `PeekNamedPipe` per interval — a few dozen cheap syscalls a
/// second — against a daemon that may be hardware-encoding at 60 fps. It
/// bounds how long a reply can sit unnoticed, and it is paid only while the
/// socket is idle.
pub(crate) const PIPE_POLL: Duration = Duration::from_millis(25);

/// A `Read` over the control pipe that never parks inside a blocking read.
///
/// The problem it exists to solve is not this crate's invention — the daemon
/// hit the identical wall on the other end of the same pipe, and its `pipe.rs`
/// carries the long version of this comment. A named pipe opened without
/// `FILE_FLAG_OVERLAPPED` gets `FO_SYNCHRONOUS_IO` on its *file object*, and
/// the I/O manager serializes every operation on that object. `File::try_clone`
/// is `DuplicateHandle`, which produces a second handle onto the *same* file
/// object, so the duplicate inherits that serialization. A blocking read
/// pending on one handle therefore stalls a write issued on the other, however
/// many threads are involved.
///
/// `Supervisor::connect` has exactly that layout, and without this wrapper the
/// app deadlocks on its very first command: the reader thread parks in
/// `read_line`, the write of the request queues behind that read, the read
/// cannot finish until the daemon answers, and the daemon cannot answer a
/// request it never received. Every `trix_call` hangs — not slowly, but
/// forever, past its own 60 s timeout — while the daemon serves other clients
/// instantly.
///
/// The fix is to never issue a read that can block: `PeekNamedPipe` first, and
/// read only once the OS says bytes are actually there. While there is nothing
/// to read this sleeps, and a sleeping thread holds no I/O lock, so writes get
/// through. `BufReader::read_line` looping over this is safe for the same
/// reason: between two partial reads the thread is asleep, not pending.
///
/// This duplicates a few lines of the daemon's `request_pending` on purpose.
/// `trix-ui` must not depend on `trix-core` (spec §3.2, enforced by
/// `scripts/ui-isolation.ps1`) and has no business depending on `trix-daemon`
/// either — the app is a socket client and nothing else. Ten lines of
/// `PeekNamedPipe` in each crate is a far smaller price than a shared crate
/// that exists only to hold them.
pub(crate) struct PeekingPipeReader {
    pipe: File,
}

impl PeekingPipeReader {
    pub(crate) fn new(pipe: File) -> Self {
        Self { pipe }
    }

    /// How many bytes the OS is holding for us right now. `Err` means the pipe
    /// is no longer usable.
    fn available(&self) -> Result<u32, windows::core::Error> {
        use std::os::windows::io::AsRawHandle as _;
        let mut available = 0u32;
        // SAFETY: the handle is borrowed from `self.pipe`, which owns it and
        // keeps it open for the whole call. The only pointer argument that is
        // not `None` is `&mut available`, a live local; with every buffer
        // argument `None`, the call writes to nothing else.
        unsafe {
            PeekNamedPipe(
                HANDLE(self.pipe.as_raw_handle().cast()),
                None,
                0,
                None,
                Some(&mut available),
                None,
            )?;
        }
        Ok(available)
    }
}

impl Read for PeekingPipeReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        // A caller that asked for nothing gets nothing: polling until a
        // zero-length buffer can be filled would never terminate, and `Ok(0)`
        // here is not EOF to anyone who passed an empty slice.
        if out.is_empty() {
            return Ok(0);
        }
        loop {
            match self.available() {
                // Nothing yet. Sleeping *outside* every I/O call is the whole
                // point: a thread parked here is not holding the file object,
                // so a request written on the other handle completes.
                Ok(0) => std::thread::sleep(PIPE_POLL),
                // Bytes are queued, so this read returns them promptly rather
                // than blocking — a byte-mode pipe hands over whatever it has.
                Ok(_) => return self.pipe.read(out),
                Err(e) => return end_of_stream_or_error(e),
            }
        }
    }
}

/// The three `PeekNamedPipe` failures that mean "there is no daemon on the
/// other end any more", as opposed to "the call itself went wrong".
///
/// `ERROR_BROKEN_PIPE` once the far end has closed *and* its bytes have been
/// drained (see the daemon's `request_pending`, which measured exactly that),
/// `ERROR_PIPE_NOT_CONNECTED` if it was never there, and `ERROR_NO_DATA` while
/// its handle is on the way down.
const PIPE_IS_GONE: [WIN32_ERROR; 3] = [ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED];

/// Turns a failed peek into either a clean end of stream or a real error.
///
/// A gone pipe is reported as EOF because that is what it means to this reader,
/// and because it is what makes `Connection`'s reader thread end cleanly and
/// fire `on_close`, which is how the supervisor learns to start reconnecting.
///
/// Everything else is propagated. This used to be `Err(_) => Ok(0)`, which
/// forged *every* peek failure — a bad handle, a permissions problem, anything
/// the future adds — into a clean disconnect. That reads as a harmless
/// simplification, because `Connection`'s reader thread treats `Ok(0)` and
/// `Err(_)` identically, and it is not: a failure that persists rather than
/// resolves became an unbounded loop of connect, fake-EOF, reconnect, with a
/// `trix-disconnected`/`trix-connected` pair emitted into the webview on every
/// pass. `Supervisor::run` now puts a floor under that loop, and this stops
/// lying to it about why the socket ended. Nothing in this crate logs, so an
/// error that is invented here is an error nobody can ever recover.
fn end_of_stream_or_error(e: windows::core::Error) -> std::io::Result<usize> {
    if PIPE_IS_GONE.iter().any(|gone| gone.to_hresult() == e.code()) {
        return Ok(0);
    }
    // `io::Error::other` rather than the `From<windows::core::Error>` impl:
    // that impl feeds the raw `HRESULT` to `from_raw_os_error`, which expects a
    // Win32 code, so the resulting error renders as a nonsense message under a
    // number that is not the one Windows reported. Keeping the `windows::core::Error`
    // as the source keeps its own `Display` — the only description of this
    // failure anyone will ever see.
    Err(std::io::Error::other(e))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The distinction the whole function exists to make. A gone pipe is the
    /// normal way a session ends and must stay indistinguishable from EOF; a
    /// peek that fails for any other reason must not be dressed up as one,
    /// because `Supervisor::run` cannot tell "the daemon quit" from "this call
    /// will fail again in 400 ms" if both arrive as a clean disconnect.
    #[test]
    fn only_a_gone_pipe_is_reported_as_end_of_stream() {
        for gone in PIPE_IS_GONE {
            let result =
                end_of_stream_or_error(windows::core::Error::from_hresult(gone.to_hresult()));
            assert!(
                matches!(result, Ok(0)),
                "{gone:?} means the daemon is gone, which is this reader's EOF: {result:?}"
            );
        }

        // A handle this reader should never have had. Nothing about it says the
        // daemon exited, and answering "clean disconnect" would send the
        // supervisor round the reconnect loop forever against a bug it cannot
        // fix by reconnecting.
        let bogus = windows::core::Error::from_hresult(
            windows::Win32::Foundation::ERROR_INVALID_HANDLE.to_hresult(),
        );
        assert!(
            end_of_stream_or_error(bogus).is_err(),
            "a peek failure that is not a dead pipe must be propagated, not forged into EOF"
        );
    }
}
