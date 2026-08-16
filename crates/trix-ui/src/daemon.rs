//! Finding the daemon, connecting to it, launching it, and reconnecting when
//! it goes away.
//!
//! The app is useless without the daemon and must never look broken because of
//! it: spec §4.5 says a missing daemon is an offer to start one, not an error
//! dialog. So this module always has an answer — connected, or connecting, or
//! "not running" with a button — and never a stack trace.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};
use tauri::{AppHandle, Emitter as _, Manager as _};

use crate::pipe::{CALL_TIMEOUT, Connection};
use crate::pipe_reader::open_halves;

/// The daemon's published socket. Byte-mode, newline-framed, ACL'd to the
/// current user (spec §4.1). Opening it takes more than a plain `File`, though
/// — see [`crate::pipe_reader::open_halves`], which this module's `connect`
/// uses, for why a bare open deadlocks the app on its first command.
const PIPE_PATH: &str = r"\\.\pipe\trix-control";

/// First reconnect delay. Short enough that the app is back before the user
/// has read the "not running" panel when the daemon merely restarted.
pub(crate) const FIRST_RETRY: Duration = Duration::from_millis(400);
/// Ceiling on the reconnect delay. A daemon the user has no intention of
/// starting must not cost a syscall every frame forever, and 5 s is still
/// quick enough that starting it from the tray feels instant here.
pub(crate) const MAX_RETRY: Duration = Duration::from_secs(5);

/// How long [`Supervisor::stop`] waits for the daemon to actually leave.
///
/// Disarming releases the capture stack, and that is not instant: the Intel
/// driver stack has been measured holding on for about ten seconds after a
/// disarm. Terminating inside that window would kill a daemon that was
/// shutting down correctly, mid-mux, and cost the user the clip they had just
/// saved. Fifteen seconds clears it with margin and still bounds a wedge.
pub(crate) const SHUTDOWN_WAIT: Duration = Duration::from_secs(15);

/// How often the pipe is probed while waiting. Fast enough that the common
/// case -- an idle daemon, gone in well under a second -- does not look slow.
pub(crate) const SHUTDOWN_POLL: Duration = Duration::from_millis(250);

/// How long a connection has to last before it counts as one that really
/// worked.
///
/// A daemon the user restarts holds a connection for minutes or hours. A
/// connection that opens and dies again in the same breath is not a restart, it
/// is a failure that happens to get past `CreateFile` — and every one of them
/// costs a `trix-disconnected`/`trix-connected` pair in the webview and a
/// flicker of the daemon-down panel. Two seconds is far below any real session
/// and far above any spin.
pub(crate) const HEALTHY_SESSION: Duration = Duration::from_secs(2);

pub(crate) fn next_retry(current: Duration) -> Duration {
    (current * 2).min(MAX_RETRY)
}

/// After a connection ends: how long to wait before trying again, and what the
/// delay after *that* should be.
///
/// Exists as a pure function so the floor below can be tested without a daemon,
/// a socket, or a two-second sleep in the suite.
///
/// The `Ok` arm of [`Supervisor::run`] used to reset the delay and loop with no
/// wait at all, which is right for the case it was written for — the daemon
/// restarted, and the app should be back before the user has finished reading
/// the panel — and has no floor under it whatsoever. A connect that succeeds
/// and drops immediately, forever, spun that loop at full speed: one
/// `CreateFile` and one pair of webview events per iteration, with nothing
/// slowing it down and nothing logged. Backing a short session off exactly as
/// if the connect had failed closes that, and leaves the restart case untouched
/// because a restart is on the other side of [`HEALTHY_SESSION`].
pub(crate) fn after_connection(delay: Duration, lasted: Duration) -> (Duration, Duration) {
    if lasted >= HEALTHY_SESSION {
        (Duration::ZERO, FIRST_RETRY)
    } else {
        (delay, next_retry(delay))
    }
}

/// Where `trix-daemon.exe` lives, given this executable's path.
///
/// Beside us, always: spec §8 ships all three binaries in one directory, and
/// cargo puts them in one profile directory too, so the installed and the
/// development layouts need the same single rule.
pub(crate) fn daemon_path_beside(ui_exe: &Path) -> Option<PathBuf> {
    // `Path::parent` returns `Some("")` for a single-component relative path
    // like `trix-ui.exe`, not `None` — an empty parent is exactly the "no
    // parent, no guess" case the bare-filename test below requires, so it is
    // treated the same as no parent at all.
    let parent = ui_exe.parent()?;
    if parent.as_os_str().is_empty() {
        return None;
    }
    Some(parent.join("trix-daemon.exe"))
}

/// Whether anything still owns the control pipe.
///
/// `WaitNamedPipe` rather than `CreateFile`: it asks whether the name exists
/// without opening an instance, so probing cannot itself take the slot a
/// reconnecting supervisor wants.
///
/// This, and not [`Supervisor::is_connected`], is the question "is a daemon
/// running". `is_connected` answers whether *this app* is holding a socket
/// right now, which goes false for seconds at a time whenever [`Supervisor::run`]
/// is between attempts — an ordinary transient state, not a stopped daemon.
/// The pipe is bound with `FILE_FLAG_FIRST_PIPE_INSTANCE`, so while the name
/// resolves some daemon owns it, no matter who started it or who is talking
/// to it. `pub(crate)` for the updater, which has to get this exactly right
/// before it renames `trix-daemon.exe`.
///
/// `WaitNamedPipeW` returns a raw `BOOL`, not a `windows::core::Result` --
/// unlike `OpenProcess`/`TerminateProcess`/`CloseHandle` below, which are
/// `Result`-returning wrappers. `BOOL::as_bool` is the direct read of it;
/// there is no error value here worth keeping, only "does the name resolve".
pub(crate) fn pipe_exists() -> bool {
    use windows::Win32::System::Pipes::WaitNamedPipeW;
    use windows::core::HSTRING;
    // 1 ms, not zero: zero means "use the server's default timeout", which is
    // whatever the daemon chose and not what is wanted here.
    unsafe { WaitNamedPipeW(&HSTRING::from(PIPE_PATH), 1).as_bool() }
}

/// Pulls the PID out of a `shutdown` reply, if it said one.
///
/// A pure function of the reply -- present, absent, present but not a number,
/// or present but too large for a `u32` -- so every one of those shapes can be
/// tested without a call, a connection, or a daemon to answer one. The
/// too-large case is not academic: `u32::try_from` failing folds a PID the
/// daemon *did* tell us into the same `None` as a reply that never had one,
/// and [`Supervisor::stop`] reports a different error for the two ("did not
/// stop, even after being closed" vs. "did not say which process it is"), so
/// getting this silently wrong changes which message the user sees.
fn pid_from_reply(reply: &Value) -> Option<u32> {
    reply.get("pid").and_then(Value::as_u64).and_then(|p| u32::try_from(p).ok())
}

/// Polls `gone` until it reports the thing waited for has gone, or `budget`
/// runs out; sleeps `poll` between checks. Returns whether it went away in
/// time.
///
/// The probe is checked *before* anything else, every time through the loop:
/// an already-gone daemon returns `true` on the first call and never sleeps
/// at all, which matters because the common case -- an idle daemon that was
/// already on its way out -- must not be made to look slow. The deadline is
/// only checked after that probe has come back negative, and the loop's last
/// probe happens right at the deadline rather than up to one `poll` short of
/// it: check, then check the clock, then sleep, in that order, never the
/// reverse. That ordering is not cosmetic -- [`Supervisor::stop`] calls this
/// with [`terminate`] on the other side of a `false`, and the one property
/// the whole waiting scheme exists to guarantee is that `TerminateProcess`
/// stays unreachable until the *full* budget has actually elapsed, not until
/// a poll interval's worth of slack has been given away.
///
/// Takes the probe as a closure rather than calling [`pipe_exists`] directly
/// so `stop`'s two waits -- the honest one before `terminate`, and the
/// shorter confirmation after it -- can share this one loop instead of each
/// hand-rolling it, and so a test can drive the loop from an in-process flag
/// instead of a real named pipe.
fn wait_until_gone(budget: Duration, poll: Duration, gone: &dyn Fn() -> bool) -> bool {
    let deadline = Instant::now() + budget;
    loop {
        if gone() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(poll);
    }
}

/// Last resort for a daemon that acknowledged `shutdown` and then wedged.
fn terminate(pid: u32) -> Result<(), String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, false, pid)
            .map_err(|e| format!("could not open the recorder process: {e}"))?;
        let result = TerminateProcess(handle, 1)
            .map_err(|e| format!("could not stop the recorder process: {e}"));
        // Closed on every path, success or failure: an error here is not worth
        // surfacing over whatever `result` already carries, and leaking the
        // handle would outlive the process it named.
        let _ = CloseHandle(handle);
        result
    }
}

/// Lets the webview load `<dir>\*.mp4` and `*.jpg` through `asset:`.
///
/// A free function, not a method, so the event closure in [`Supervisor::connect`]
/// can call it too: that closure must be `'static` (the reader thread that owns
/// it outlives the `connect` call), so it can only capture an owned clone of
/// `AppHandle`, never a borrow of `&Supervisor`. Sharing this one line is what
/// keeps the app-initiated grant (`Supervisor::allow_clip_dir`, called from
/// `on_connected`) and the event-driven one from becoming two copies of the
/// same rule that could drift apart.
fn grant_clip_dir(app: &AppHandle, dir: &str) {
    let _ = app.asset_protocol_scope().allow_directory(dir, false);
}

/// Owns the current connection and the thread that keeps trying to make one.
pub struct Supervisor {
    app: AppHandle,
    current: Mutex<Option<Arc<Connection>>>,
}

impl Supervisor {
    pub fn start(app: AppHandle) -> Arc<Self> {
        let supervisor = Arc::new(Self { app, current: Mutex::new(None) });
        let worker = Arc::clone(&supervisor);
        // If this thread cannot start there is nothing useful left to do, but
        // there is still a window: it stays on the "not running" panel, whose
        // Start button calls `launch` directly.
        let _ = std::thread::Builder::new()
            .name("trix-daemon-supervisor".into())
            .spawn(move || worker.run());
        supervisor
    }

    fn run(&self) {
        let mut delay = FIRST_RETRY;
        loop {
            match self.connect() {
                Ok(connection) => {
                    let opened = Instant::now();
                    self.on_connected(&connection);
                    // Park until the reader thread reports the socket gone.
                    while !connection.is_closed() {
                        std::thread::sleep(Duration::from_millis(120));
                    }
                    if let Ok(mut current) = self.current.lock() {
                        *current = None;
                    }
                    // No `trix-disconnected` here: the reader thread's
                    // `on_close` already emitted one for this same drop, up to
                    // a poll interval earlier. Emitting again would double
                    // every disconnect the frontend sees, which is fine for a
                    // flag and wrong for anything counted or shown once.
                    let (wait, next) = after_connection(delay, opened.elapsed());
                    delay = next;
                    // `Duration::ZERO` after a real session, so a daemon that
                    // restarted is retried as immediately as it always was.
                    std::thread::sleep(wait);
                }
                Err(()) => {
                    std::thread::sleep(delay);
                    delay = next_retry(delay);
                }
            }
        }
    }

    fn connect(&self) -> Result<Arc<Connection>, ()> {
        // Read+write on one handle, a duplicate for the reader, and
        // `PeekingPipeReader` between that duplicate and the `BufReader` — see
        // `pipe_reader::open_halves`, which is what makes using the two from
        // different threads work at all and is the same call
        // `tests/pipe_roundtrip.rs` makes against its stub.
        let (reader, write_half) = open_halves(PIPE_PATH).map_err(|_| ())?;

        let app = self.app.clone();
        let closed_app = self.app.clone();
        let connection = Connection::start(
            reader,
            write_half,
            move |event| {
                // `config_changed` is the one event this layer acts on itself
                // rather than only forwarding. It is broadcast to every
                // connected client, including this app's own, for every
                // accepted `config.set` regardless of who sent it — which is
                // what makes this the single place that keeps the asset scope
                // current. Before this, the grant only ran right after a
                // `config.set` this app itself issued (see `commands.rs`),
                // which missed the tray's "Change clips folder…": an
                // already-open app kept building `asset:` URLs against the old
                // directory, and the new one was never granted, so every
                // thumbnail broke and every clip stopped playing until the
                // daemon restarted.
                if event.event == "config_changed"
                    && let Some(dir) = event.data.get("clip_dir_resolved").and_then(Value::as_str)
                {
                    grant_clip_dir(&app, dir);
                }
                // One channel for every daemon event; the frontend switches on
                // `event`. A Tauri event per protocol event would mean a
                // listener to register for each, and a silent miss whenever the
                // daemon grows one.
                let _ = app.emit("trix-event", event);
            },
            move || {
                let _ = closed_app.emit("trix-disconnected", ());
            },
        )
        .map_err(|_| ())?;

        if let Ok(mut current) = self.current.lock() {
            *current = Some(Arc::clone(&connection));
        }
        Ok(connection)
    }

    /// Announces the connection, and opens the asset scope onto wherever clips
    /// currently live.
    ///
    /// The scope has to be opened here rather than in `tauri.conf.json`
    /// because `clip_dir` is a config key the user can change from the tray at
    /// any time; a static scope would be a guess that goes stale the first
    /// time they do.
    fn on_connected(&self, connection: &Arc<Connection>) {
        if let Ok(status) = connection.call("status", Map::new(), CALL_TIMEOUT) {
            if let Some(dir) = status.get("clip_dir").and_then(Value::as_str) {
                self.allow_clip_dir(dir);
            }
            let _ = self.app.emit("trix-connected", status);
        } else {
            let _ = self.app.emit("trix-connected", Value::Null);
        }
    }

    /// Lets the webview load `<clip_dir>\*.mp4` and `*.jpg` through `asset:`.
    ///
    /// Non-recursive: the library is flat by design (spec §5.1), so the
    /// directory's own children are exactly the grant needed and subdirectories
    /// are not this app's business.
    ///
    /// Only ever needed for the *initial* grant on connect now — every later
    /// change rides the `config_changed` event handled in `connect`'s own
    /// closure above, which calls [`grant_clip_dir`] directly because it
    /// cannot hold a borrow of `self` across the reader thread's lifetime.
    pub fn allow_clip_dir(&self, dir: &str) {
        grant_clip_dir(&self.app, dir);
    }

    pub fn connection(&self) -> Option<Arc<Connection>> {
        self.current.lock().ok().and_then(|c| c.clone())
    }

    pub fn is_connected(&self) -> bool {
        self.connection().is_some_and(|c| !c.is_closed())
    }

    pub fn call(&self, cmd: &str, args: Map<String, Value>) -> Result<Value, String> {
        let connection = self.connection().ok_or("not connected to the Trix daemon")?;
        connection.call(cmd, args, CALL_TIMEOUT)
    }

    /// Starts `trix-daemon.exe`. The supervisor's own retry loop picks the
    /// socket up; this does not wait for it.
    pub fn launch(&self) -> Result<(), String> {
        let exe =
            std::env::current_exe().map_err(|e| format!("could not locate trix-ui.exe: {e}"))?;
        let daemon =
            daemon_path_beside(&exe).ok_or("could not work out where trix-daemon.exe is")?;
        if !daemon.exists() {
            return Err(format!("trix-daemon.exe is not beside the app at {}", daemon.display()));
        }

        // CREATE_NO_WINDOW: the daemon is a console-subsystem binary, so
        // spawning it from a windowed app flashes a console on screen and then
        // hands it a window nobody asked for.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        use std::os::windows::process::CommandExt as _;
        std::process::Command::new(&daemon)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| format!("could not start the Trix daemon: {e}"))?;
        Ok(())
    }

    /// Asks the daemon to exit and returns only once it actually has.
    ///
    /// The daemon's disappearance is observed on the pipe rather than on a
    /// process handle: `\\.\pipe\trix-control` is bound with
    /// `FILE_FLAG_FIRST_PIPE_INSTANCE`, so while the name resolves *some*
    /// daemon owns it, and when it stops resolving the process is gone. That
    /// works whether this app started the daemon or Windows did at login,
    /// which a `Child` handle does not -- with "Start with Windows" on, the
    /// supervisor never spawned it and holds nothing to wait on.
    ///
    /// The PID from the reply is the fallback for a daemon that answered and
    /// then wedged. It is only ever used after [`SHUTDOWN_WAIT`] has elapsed,
    /// so a slow-but-honest shutdown is never the thing that gets terminated.
    pub fn stop(&self) -> Result<(), String> {
        let reply = self.call("shutdown", Map::new())?;
        let pid = pid_from_reply(&reply);

        // `pipe_exists` reports whether the pipe is still there, i.e. the
        // opposite of "gone" -- inverted here rather than changing what
        // `pipe_exists` means everywhere else it's used.
        let gone = || !pipe_exists();
        if wait_until_gone(SHUTDOWN_WAIT, SHUTDOWN_POLL, &gone) {
            return Ok(());
        }

        match pid {
            Some(pid) => {
                terminate(pid)?;
                // One more poll round: TerminateProcess is asynchronous.
                if wait_until_gone(Duration::from_secs(2), SHUTDOWN_POLL, &gone) {
                    Ok(())
                } else {
                    Err("the Trix recorder did not stop, even after being closed".into())
                }
            }
            None => {
                Err("the Trix recorder did not stop, and did not say which process it is".into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn the_daemon_is_looked_for_beside_this_exe() {
        let ui = Path::new(r"C:\Program Files\Trix\trix-ui.exe");
        assert_eq!(
            daemon_path_beside(ui),
            Some(PathBuf::from(r"C:\Program Files\Trix\trix-daemon.exe")),
            "installed layout: spec §8 puts all three binaries in one directory"
        );

        let dev = Path::new(r"C:\src\trix\target\debug\trix-ui.exe");
        assert_eq!(
            daemon_path_beside(dev),
            Some(PathBuf::from(r"C:\src\trix\target\debug\trix-daemon.exe")),
            "cargo puts both binaries in the same profile directory, so dev needs no special case"
        );

        assert_eq!(daemon_path_beside(Path::new("trix-ui.exe")), None, "no parent, no guess");
    }

    /// Backoff exists so a daemon the user never intends to start does not
    /// cost a reconnect attempt every frame, and so one that is restarting is
    /// picked up quickly rather than after a fixed long wait.
    #[test]
    fn backoff_grows_from_prompt_to_patient_and_stops_there() {
        let mut delay = FIRST_RETRY;
        let mut seen = vec![delay];
        for _ in 0..8 {
            delay = next_retry(delay);
            seen.push(delay);
        }
        assert_eq!(seen[0], Duration::from_millis(400), "the first retry is quick");
        assert!(seen[1] > seen[0] && seen[2] > seen[1], "it must actually back off");
        assert!(seen.iter().all(|d| *d <= MAX_RETRY), "and it must never exceed the cap: {seen:?}");
        assert_eq!(*seen.last().expect("non-empty"), MAX_RETRY, "it settles at the cap");
    }

    /// A daemon that was actually being used and then went away is the case the
    /// short first retry was written for: no wait at all before the next
    /// attempt, and the delay back down to its quickest.
    #[test]
    fn a_real_session_that_ends_is_retried_immediately() {
        let (wait, next) = after_connection(MAX_RETRY, HEALTHY_SESSION);
        assert_eq!(wait, Duration::ZERO, "a restart must not be made to wait");
        assert_eq!(next, FIRST_RETRY, "and the backoff earned before it is forgotten");
    }

    /// The floor. A socket that opens and dies again immediately is not a
    /// restart, and without this it cost a `CreateFile` plus a
    /// `trix-disconnected`/`trix-connected` pair per iteration for as long as
    /// it kept happening — a spinning core and a daemon-down panel strobing at
    /// whatever rate the loop managed.
    #[test]
    fn a_connection_that_dies_instantly_cannot_spin_the_loop() {
        let instant = Duration::from_millis(1);
        let mut delay = FIRST_RETRY;
        let mut total = Duration::ZERO;
        for _ in 0..20 {
            let (wait, next) = after_connection(delay, instant);
            assert!(wait > Duration::ZERO, "every failed-fast connection must cost a wait");
            total += wait;
            delay = next;
        }
        assert_eq!(delay, MAX_RETRY, "and it must back off to the cap like a failed connect");
        assert!(
            total > Duration::from_secs(20),
            "20 instant drops should take a minute or so, not a millisecond: {total:?}"
        );
    }

    /// The wait is bounded because a wedged daemon must not be able to hang the
    /// update forever, and generous because disarming releases the capture stack:
    /// the Intel driver stack has been measured taking about ten seconds to let
    /// go. A budget under that would terminate a daemon that was shutting down
    /// correctly, mid-mux, costing the user the clip they just saved.
    #[test]
    fn the_shutdown_budget_outlasts_a_slow_driver_release() {
        assert!(
            SHUTDOWN_WAIT >= Duration::from_secs(12),
            "the capture stack can take ~10 s to release; {SHUTDOWN_WAIT:?} would kill an honest shutdown"
        );
        assert!(
            SHUTDOWN_WAIT <= Duration::from_secs(30),
            "a user watching a progress bar will not wait {SHUTDOWN_WAIT:?} for a hung daemon"
        );
        assert!(
            SHUTDOWN_POLL < Duration::from_secs(1),
            "the poll decides how quickly a fast shutdown is noticed"
        );
    }

    /// Already gone before the first check: the common case, an idle daemon
    /// that had already let go of the pipe by the time `stop` got around to
    /// looking. Must come back on the very first probe and must not sleep at
    /// all — a real `SHUTDOWN_POLL` is 250 ms, and paying even one of those on
    /// a daemon that was never there to wait for is exactly the "does not
    /// look slow" property the module doc for `SHUTDOWN_POLL` promises.
    #[test]
    fn already_gone_returns_immediately_without_sleeping() {
        let calls = AtomicUsize::new(0);
        let probe = || {
            calls.fetch_add(1, Ordering::SeqCst);
            true
        };
        let started = Instant::now();
        let result = wait_until_gone(Duration::from_secs(10), Duration::from_millis(500), &probe);
        let elapsed = started.elapsed();
        assert!(result, "an already-gone probe must report gone");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "must return on the first probe, not loop");
        assert!(
            elapsed < Duration::from_millis(100),
            "must not have slept even once: {elapsed:?} elapsed against a 500 ms poll"
        );
    }

    /// Gone after a few polls: the probe starts negative and flips true partway
    /// through the budget. Must notice on the poll where it flips rather than
    /// riding out the whole budget regardless.
    #[test]
    fn gone_after_a_few_polls_returns_true_before_the_budget_elapses() {
        let calls = AtomicUsize::new(0);
        let probe = || calls.fetch_add(1, Ordering::SeqCst) + 1 >= 3;
        let budget = Duration::from_millis(500);
        let started = Instant::now();
        let result = wait_until_gone(budget, Duration::from_millis(10), &probe);
        let elapsed = started.elapsed();
        assert!(result, "a probe that eventually reports gone must return true");
        assert_eq!(calls.load(Ordering::SeqCst), 3, "must stop probing the moment it flips");
        assert!(
            elapsed < budget,
            "must return well before the full budget when the daemon left partway through: \
             {elapsed:?} against a {budget:?} budget"
        );
    }

    /// Never gone: the probe that models a wedged daemon. Must return `false`,
    /// and must not do so before the budget has actually elapsed — this is the
    /// property [`Supervisor::stop`] leans on to keep `terminate` unreachable
    /// until the honest wait has fully run out.
    #[test]
    fn never_gone_returns_false_only_after_the_budget_elapses() {
        let calls = AtomicUsize::new(0);
        let probe = || {
            calls.fetch_add(1, Ordering::SeqCst);
            false
        };
        let budget = Duration::from_millis(80);
        let started = Instant::now();
        let result = wait_until_gone(budget, Duration::from_millis(10), &probe);
        let elapsed = started.elapsed();
        assert!(!result, "a probe that never reports gone must return false");
        assert!(
            elapsed >= budget,
            "must not give up before the budget elapses: {elapsed:?} against a {budget:?} budget"
        );
        assert!(
            calls.load(Ordering::SeqCst) > 1,
            "must have actually polled more than once, not just checked once and slept out the clock"
        );
    }

    #[test]
    fn pid_from_reply_reads_a_well_formed_reply() {
        let reply = serde_json::json!({"pid": 4242});
        assert_eq!(pid_from_reply(&reply), Some(4242));
    }

    #[test]
    fn pid_from_reply_is_none_without_a_pid_key() {
        let reply = serde_json::json!({"status": "bye"});
        assert_eq!(pid_from_reply(&reply), None, "no pid key, no PID to fall back on");
    }

    #[test]
    fn pid_from_reply_is_none_when_pid_is_not_a_number() {
        let reply = serde_json::json!({"pid": "4242"});
        assert_eq!(pid_from_reply(&reply), None, "a string is not a PID, however numeric-looking");
    }

    /// A `pid` too large for `u32` must fold to `None`, the same as a reply
    /// that never had one -- silently losing this would leave `stop` treating
    /// "the daemon told us its PID" as "we can terminate it" when the value
    /// cannot actually be used with `OpenProcess`.
    #[test]
    fn pid_from_reply_is_none_when_pid_overflows_u32() {
        let reply = serde_json::json!({"pid": u64::from(u32::MAX) + 1});
        assert_eq!(pid_from_reply(&reply), None, "must not silently truncate an oversized PID");
    }
}
