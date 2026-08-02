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

use windows::Win32::Foundation::HANDLE;
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
                // The daemon exited, or the pipe is otherwise unusable.
                // Reported as EOF rather than an error because that is what it
                // means to this reader, and because it is what makes
                // `Connection`'s reader thread end cleanly and fire `on_close`,
                // which is how the supervisor learns to start reconnecting.
                Err(_) => return Ok(0),
            }
        }
    }
}
