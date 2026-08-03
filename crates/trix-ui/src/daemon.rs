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
}

#[cfg(test)]
mod tests {
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
}
