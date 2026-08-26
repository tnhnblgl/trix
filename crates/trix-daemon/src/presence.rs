//! Discord Rich Presence: the card on the user's Discord profile that says
//! Trix is running, with a button their friends can click to get it.
//!
//! Local IPC, not a network API. Everything here talks to a named pipe that
//! Discord's own desktop client owns (`\\.\pipe\discord-ipc-0` through `-9`);
//! Trix opens no outbound connection and sends nothing about the user
//! anywhere. That matters for this project's posture — a feature that phoned a
//! server would be the first one in the tree, and this is not it.
//!
//! **Hand-rolled rather than `discord-rich-presence`.** The framing is eight
//! bytes — a little-endian opcode and a little-endian length — and the whole
//! conversation is a handshake plus one command. Taking a crate for that would
//! add a dependency tree to a daemon whose entire selling point is that it does
//! not have one.
//!
//! ## What the card says, and which half of it we control
//!
//! ```text
//! Playing                            <- Discord's own label
//! Clipping with Trix                 <- the *application's* name in Discord's portal
//! Without thinking FPS and Memory    <- `details`, sent from here
//! [logo] 3:48                        <- `assets.large_image` + `timestamps.start`
//! [ Get Trix ]                       <- `buttons[0]`
//! ```
//!
//! The bold line is not ours to send. Discord renders the registered
//! application's name there and ignores any `name` we put in the activity, so
//! "Clipping with Trix" is achieved by naming the application that — see
//! `docs/notes/2026-08-22-discord-rich-presence.md`. Sending a `name` field
//! anyway was considered and rejected: it would be dead weight in the payload
//! at best, and a rejected `SET_ACTIVITY` at worst.
//!
//! ## Why this is a poll and not an event hook
//!
//! Presence follows "is Trix running", not "is Trix armed", so there is no
//! transition to hang it off — the daemon starting *is* the transition, and it
//! has already happened by the time this thread exists. What is left to watch
//! for is a user flipping the setting and Discord starting or quitting
//! underneath us, neither of which announces itself. A quarter-second peek at
//! one bool covers both, and matches [`crate::stats`], which is the same shape
//! for the same reason.
//!
//! Discord throttles `SET_ACTIVITY` to roughly one update per fifteen seconds.
//! Nothing here comes near that: the activity is static, so it is sent once per
//! connection and never updated. The only traffic after that is a PING every
//! [`Cadence::heartbeat`], which is not rate limited and is the only way to
//! notice that Discord has quit — a write that is never attempted cannot fail,
//! and a presence nobody re-establishes is a feature that silently stops
//! working the first time the user restarts Discord.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json};

use crate::state::Daemon;

/// The Discord application this presence belongs to.
///
/// **Empty until the application is registered**, and empty is not a bug: an
/// application has to be created by hand in Discord's developer portal by
/// somebody holding the account, and no agent, script or build step can do it.
/// [`spawn_presence_thread`] refuses to start the loop while this is empty, so
/// an unconfigured build costs nothing at all rather than reconnecting to a
/// handshake Discord will always refuse.
///
/// Public by design, not a secret — every shipped copy of Trix carries the same
/// id, and Discord's own documentation treats it as public. The consequence to
/// accept is that the id is permanent: delete the application in the portal and
/// every installed Trix shows nothing, with no way to point it somewhere else
/// short of a new release.
pub const CLIENT_ID: &str = "";

/// The line under the application name. The user-facing promise of the product,
/// not a status — there is deliberately nothing dynamic in this card.
pub const DETAILS: &str = "Without thinking FPS and Memory";

/// Key of the art uploaded to the application's Rich Presence assets.
///
/// A key, not a path or a URL: Discord serves the image from its own CDN and
/// only ever sees the name. `assets/logo.png` in this repository is the file to
/// upload under this name.
pub const LARGE_IMAGE: &str = "logo";

/// Hover text on the art.
pub const LARGE_TEXT: &str = "Trix";

/// The button's label. Discord caps these at 32 characters.
pub const BUTTON_LABEL: &str = "Get Trix";

/// Where the button goes. `releases/latest` rather than a pinned tag so the
/// link in a shipped binary does not rot into pointing at an old release.
pub const BUTTON_URL: &str = "https://github.com/tnhnblgl/trix/releases/latest";

/// Discord's IPC opcodes. Only these five exist.
const OP_HANDSHAKE: u32 = 0;
const OP_FRAME: u32 = 1;
const OP_CLOSE: u32 = 2;
const OP_PING: u32 = 3;

/// Refuse a frame claiming to be larger than this.
///
/// Every frame in this conversation is a few hundred bytes. The cap exists
/// because the length is read off a pipe and used to size an allocation, and
/// "Discord would never send that" is an argument about a well-behaved peer,
/// not about what arrives.
const MAX_FRAME: usize = 64 * 1024;

/// How often the loop wakes, retries a dead connection, and checks the peer.
///
/// A parameter rather than three constants inlined into the loop so the tests
/// below can run the whole state machine in milliseconds; production passes
/// [`Cadence::default`] and nothing about it changes.
#[derive(Debug, Clone, Copy)]
pub struct Cadence {
    /// How often the loop wakes to ask whether the setting has changed.
    ///
    /// This is the toggle's latency, and the only reason it is short. Nothing
    /// else in this loop wants waking four times a second — but a settings
    /// switch that takes fifteen seconds to visibly do anything reads as
    /// broken, and the cost of the peek is one uncontended mutex.
    pub poll: Duration,
    /// How long to wait before trying Discord again after a failed connection.
    ///
    /// Discord not running is the *normal* case, not an error case, so this is
    /// a steady retry rather than a backoff: the user who launches Discord an
    /// hour after Trix should get their presence within fifteen seconds, not
    /// after an exponential delay has grown to something absurd.
    pub reconnect: Duration,
    /// How often to PING a live connection to find out it is still there.
    pub heartbeat: Duration,
}

impl Default for Cadence {
    fn default() -> Self {
        Self {
            poll: Duration::from_millis(250),
            reconnect: Duration::from_secs(15),
            heartbeat: Duration::from_secs(15),
        }
    }
}

/// One live conversation with Discord.
///
/// A trait because the half of this module worth testing is the state machine
/// — connect, publish, notice a drop, clear on the way out — and every one of
/// those transitions is reachable only through a running Discord. The
/// implementation below is the only production one; the tests supply a fake
/// that records what it was asked to do.
pub trait Session: Send {
    /// Publishes an activity, or clears the presence when given `None`.
    fn set_activity(&mut self, activity: Option<Value>) -> Result<()>;
    /// Round-trips a PING. An error means the connection is gone.
    fn ping(&mut self) -> Result<()>;
}

/// The activity payload: everything the card shows except the application name.
///
/// `start` is a Unix timestamp in **seconds**, which is what Discord's IPC
/// expects, and it is the only thing in here that is not a constant. Discord
/// renders it as a running clock, so this value is what decides whether the
/// timer means "since Trix started" or "since this connection did" — see
/// [`spawn_with`], which keeps it stable across reconnects on purpose.
pub fn activity(start: u64) -> Value {
    json!({
        "details": DETAILS,
        "timestamps": { "start": start },
        "assets": { "large_image": LARGE_IMAGE, "large_text": LARGE_TEXT },
        "buttons": [ { "label": BUTTON_LABEL, "url": BUTTON_URL } ],
    })
}

/// The `SET_ACTIVITY` command wrapping an [`activity`], or clearing it.
///
/// `pid` is Discord's own housekeeping: it drops the presence if the process
/// that claimed it dies, which is what stops a crashed daemon from leaving
/// "Clipping with Trix" on a profile forever.
fn set_activity_command(pid: u32, nonce: &str, activity: Option<Value>) -> Value {
    json!({
        "cmd": "SET_ACTIVITY",
        "nonce": nonce,
        "args": { "pid": pid, "activity": activity },
    })
}

/// Serializes one frame: little-endian opcode, little-endian length, JSON.
fn encode_frame(op: u32, payload: &[u8]) -> Result<Vec<u8>> {
    let len = u32::try_from(payload.len())
        .ok()
        .filter(|_| payload.len() <= MAX_FRAME)
        .with_context(|| format!("a {}-byte presence frame is too large to send", payload.len()))?;
    let mut frame = Vec::with_capacity(8 + payload.len());
    frame.extend_from_slice(&op.to_le_bytes());
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

/// Reads one frame, returning its opcode and raw body.
///
/// The body is handed back undecoded because nothing here reads it: the loop
/// cares only whether a reply arrived and whether it was a CLOSE. Parsing a
/// payload we do not use would be a second thing that can fail on a path whose
/// whole job is to notice failure.
fn read_frame(reader: &mut impl Read) -> Result<(u32, Vec<u8>)> {
    let mut header = [0u8; 8];
    reader.read_exact(&mut header).context("Discord closed the connection")?;
    let op = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
    let len = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
    if len > MAX_FRAME {
        bail!("Discord announced a {len}-byte frame, which is past the {MAX_FRAME}-byte cap");
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).context("Discord closed the connection mid-frame")?;
    Ok((op, body))
}

/// A connected Discord IPC pipe.
///
/// Strictly one thread, and strictly write-then-read. That is not a
/// simplification, it is the requirement: the pipe is opened without
/// `FILE_FLAG_OVERLAPPED`, so a blocking read parked on this handle would stall
/// a write issued from anywhere else — the same trap `trix-ui`'s
/// `pipe_reader.rs` exists to work around on the control socket. Here there is
/// nothing to work around, because there is never more than one operation in
/// flight.
///
/// The cost of that simplicity is the one caveat worth stating plainly: a
/// Discord that accepts a frame and then never answers parks this thread
/// forever. Nothing else in the daemon notices or cares — the thread holds no
/// lock and owns nothing else — but the presence would stay stale until Trix
/// restarts. Fixing it needs overlapped IO, which is real unsafe complexity
/// bought for a failure mode Discord has never been observed to produce.
struct IpcSession {
    pipe: File,
    pid: u32,
    /// Bumped per command so each carries a distinct nonce.
    sent: u64,
}

impl IpcSession {
    fn send(&mut self, op: u32, payload: &Value) -> Result<()> {
        let body = serde_json::to_vec(payload).context("could not serialize a presence frame")?;
        let frame = encode_frame(op, &body)?;
        self.pipe.write_all(&frame).context("could not write to the Discord pipe")?;
        self.pipe.flush().context("could not flush the Discord pipe")?;
        Ok(())
    }

    /// Sends a command and waits for the reply Discord owes it.
    ///
    /// Exactly one frame is read, and its nonce is deliberately not matched
    /// against the one just sent. Discord can interleave an unsolicited
    /// dispatch, which would put every subsequent reply one frame behind — and
    /// that is harmless here, because the only thing read off a reply is
    /// whether it was a CLOSE. Correlating them would need the reader thread
    /// this whole design exists to avoid.
    fn call(&mut self, op: u32, payload: &Value) -> Result<()> {
        self.send(op, payload)?;
        let (op, body) = read_frame(&mut self.pipe)?;
        if op == OP_CLOSE {
            bail!("Discord closed the connection: {}", String::from_utf8_lossy(&body));
        }
        Ok(())
    }

    fn handshake(&mut self, client_id: &str) -> Result<()> {
        self.call(OP_HANDSHAKE, &json!({ "v": 1, "client_id": client_id }))
            .context("Discord refused the handshake")
    }
}

impl Session for IpcSession {
    fn set_activity(&mut self, activity: Option<Value>) -> Result<()> {
        self.sent += 1;
        let nonce = format!("{}-{}", self.pid, self.sent);
        let command = set_activity_command(self.pid, &nonce, activity);
        self.call(OP_FRAME, &command)
    }

    fn ping(&mut self) -> Result<()> {
        self.call(OP_PING, &json!({}))
    }
}

/// Opens the first Discord IPC pipe that answers and completes the handshake.
///
/// Ten candidate names because Discord numbers its pipe by instance: a user
/// running stable and PTB at once puts them on `-0` and `-1`, and only trying
/// `-0` would silently miss the second. A pipe that does not exist fails
/// immediately, so the sweep costs nothing on a machine with no Discord at all
/// — which is the case this runs in most often.
fn connect_ipc() -> Result<Box<dyn Session>> {
    for instance in 0..10 {
        let path = format!(r"\\.\pipe\discord-ipc-{instance}");
        let Ok(pipe) = OpenOptions::new().read(true).write(true).open(&path) else { continue };
        let mut session = IpcSession { pipe, pid: std::process::id(), sent: 0 };
        // A refused handshake is not "try the next pipe": every instance would
        // refuse the same id for the same reason, and the error naming why is
        // worth more than nine more attempts at it.
        session.handshake(CLIENT_ID)?;
        return Ok(Box::new(session));
    }
    bail!("no Discord IPC pipe answered; Discord is probably not running")
}

/// Unix seconds, or 0 if the clock is set before 1970.
fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs())
}

/// Publishes Trix's Discord presence for as long as the daemon lives and the
/// setting is on.
///
/// Returns without spawning anything when no application id is compiled in —
/// see [`CLIENT_ID`]. That is logged at `info` rather than swallowed, because
/// the symptom otherwise ("the toggle is on and nothing happens") has no other
/// explanation the user could find.
pub fn spawn_presence_thread(daemon: Arc<Daemon>) -> Result<()> {
    if CLIENT_ID.is_empty() {
        tracing::info!(
            "no Discord application id is compiled in, so Discord presence stays off; \
             see docs/notes/2026-08-22-discord-rich-presence.md"
        );
        return Ok(());
    }
    let daemon: Weak<Daemon> = Arc::downgrade(&daemon);
    spawn_with(
        move || daemon.upgrade().map(|daemon| daemon.discord_presence_enabled()),
        Cadence::default(),
        connect_ipc,
    )
}

/// [`spawn_presence_thread`] with the daemon and the transport lifted out.
///
/// `enabled` answers "should there be a presence right now", and `None` means
/// the daemon is gone and the thread should end — the same `Weak` discipline
/// [`crate::stats`] uses, expressed as a closure so this loop needs no `Daemon`
/// at all and its tests need no capture hardware, no clip directory and no
/// config file.
fn spawn_with(
    enabled: impl Fn() -> Option<bool> + Send + 'static,
    cadence: Cadence,
    connect: impl Fn() -> Result<Box<dyn Session>> + Send + 'static,
) -> Result<()> {
    std::thread::Builder::new()
        .name("trix-discord-presence".into())
        .spawn(move || {
            let mut session: Option<Box<dyn Session>> = None;
            // `None` while the presence is off, so the clock restarts when the
            // user turns it back on. Held across a *reconnect*, though: Discord
            // restarting is Discord's business, and resetting the timer for it
            // would report an uptime the user did not have.
            let mut start: Option<u64> = None;
            let mut next_connect = Instant::now();
            let mut next_beat = Instant::now();

            loop {
                std::thread::sleep(cadence.poll);
                let Some(wanted) = enabled() else { return };
                let now = Instant::now();

                if !wanted {
                    // Clearing is best-effort and its failure is uninteresting:
                    // the only way it fails is a connection that is already
                    // gone, which is also the only way it was unnecessary.
                    if let Some(mut live) = session.take()
                        && let Err(e) = live.set_activity(None)
                    {
                        tracing::debug!(error = %format!("{e:#}"), "could not clear the presence");
                    }
                    start = None;
                    next_connect = now;
                    continue;
                }

                // Set on the first enabled tick rather than at thread start, so
                // a user who turns the setting on an hour in gets a clock that
                // starts at zero instead of one that claims an hour.
                let start = *start.get_or_insert_with(unix_now);

                match session.as_mut() {
                    None => {
                        if now < next_connect {
                            continue;
                        }
                        next_connect = deadline(now, cadence.reconnect);
                        match connect() {
                            Ok(mut live) => match live.set_activity(Some(activity(start))) {
                                Ok(()) => {
                                    next_beat = deadline(now, cadence.heartbeat);
                                    session = Some(live);
                                    tracing::info!("Discord presence is live");
                                }
                                // Connected but refused: dropped rather than
                                // kept, so the next attempt is a clean
                                // handshake instead of a session in an unknown
                                // state.
                                Err(e) => {
                                    tracing::debug!(
                                        error = %format!("{e:#}"),
                                        "Discord accepted the handshake but refused the activity"
                                    );
                                }
                            },
                            // At `trace`, not `warn`: "Discord is not running"
                            // is the expected answer on most machines most of
                            // the time, and logging it every fifteen seconds
                            // would bury the daemon's real output.
                            Err(e) => tracing::trace!(error = %format!("{e:#}"), "no Discord yet"),
                        }
                    }
                    Some(live) => {
                        if now < next_beat {
                            continue;
                        }
                        next_beat = deadline(now, cadence.heartbeat);
                        if let Err(e) = live.ping() {
                            tracing::debug!(
                                error = %format!("{e:#}"),
                                "the Discord connection dropped; reconnecting"
                            );
                            session = None;
                            // Now, not in fifteen seconds. A drop we just
                            // detected is the one moment reconnecting is most
                            // likely to work — Discord restarting is the usual
                            // cause, and it is already back up by the time the
                            // old pipe reports itself dead.
                            next_connect = now;
                        }
                    }
                }
            }
        })
        .context("failed to spawn the Discord presence thread")?;
    Ok(())
}

/// `now + interval`, saturating instead of panicking.
///
/// `Instant + Duration` panics on overflow, and this is a `panic = "abort"`
/// build. The intervals are constants today so it cannot happen — but that is
/// an argument about the current [`Cadence`], not about the addition.
fn deadline(now: Instant, interval: Duration) -> Instant {
    now.checked_add(interval).unwrap_or(now)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    /// What a [`Session`] was asked to do, in order.
    #[derive(Debug, PartialEq, Eq)]
    enum Call {
        Publish(u64),
        Clear,
        Ping,
    }

    /// A [`Session`] that records its calls and fails on command.
    struct Fake {
        log: Arc<Mutex<Vec<Call>>>,
        ping_fails: Arc<AtomicBool>,
    }

    impl Session for Fake {
        fn set_activity(&mut self, activity: Option<Value>) -> Result<()> {
            let call = match activity {
                Some(value) => {
                    let start = value["timestamps"]["start"].as_u64().expect("a start timestamp");
                    Call::Publish(start)
                }
                None => Call::Clear,
            };
            self.log.lock().expect("the call log").push(call);
            Ok(())
        }

        fn ping(&mut self) -> Result<()> {
            self.log.lock().expect("the call log").push(Call::Ping);
            if self.ping_fails.load(Ordering::SeqCst) {
                bail!("the pipe is gone");
            }
            Ok(())
        }
    }

    /// A cadence fast enough to run the whole state machine inside a test.
    fn brisk() -> Cadence {
        Cadence {
            poll: Duration::from_millis(5),
            reconnect: Duration::from_millis(20),
            heartbeat: Duration::from_millis(20),
        }
    }

    /// Waits for `want` to hold, or gives up after a generous budget.
    ///
    /// Polled rather than slept-then-asserted: a fixed sleep long enough to be
    /// reliable on a loaded CI box is a fixed sleep every run pays, and this
    /// returns the moment the condition holds.
    fn until(want: impl Fn() -> bool) -> bool {
        for _ in 0..400 {
            if want() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    fn calls(log: &Arc<Mutex<Vec<Call>>>) -> usize {
        log.lock().expect("the call log").len()
    }

    /// The happy path: the setting is on, Discord answers, one activity goes
    /// out carrying a real timestamp.
    #[test]
    fn an_enabled_presence_connects_once_and_publishes() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let connects = Arc::new(Mutex::new(0usize));
        let (log_for_connect, connects_for_connect) = (Arc::clone(&log), Arc::clone(&connects));
        let ping_fails = Arc::new(AtomicBool::new(false));

        spawn_with(
            || Some(true),
            Cadence { heartbeat: Duration::from_secs(3600), ..brisk() },
            move || {
                *connects_for_connect.lock().expect("the connect count") += 1;
                Ok(Box::new(Fake {
                    log: Arc::clone(&log_for_connect),
                    ping_fails: Arc::clone(&ping_fails),
                }) as Box<dyn Session>)
            },
        )
        .expect("the presence thread");

        assert!(until(|| calls(&log) >= 1), "an enabled presence must publish an activity");
        let recorded = log.lock().expect("the call log");
        match recorded[0] {
            Call::Publish(start) => assert!(start > 0, "the card's clock needs a real timestamp"),
            ref other => panic!("the first call must be a publish, not {other:?}"),
        }
        // The heartbeat is parked an hour out, so anything past the first call
        // would be a redial — which is the rate-limit bug this asserts against.
        assert_eq!(recorded.len(), 1, "a static activity is published once per connection");
        assert_eq!(*connects.lock().expect("the connect count"), 1);
    }

    /// Turning the setting off clears the card rather than just abandoning it.
    /// Abandoning it would leave "Clipping with Trix" on the profile until the
    /// daemon exited — the exact thing a user turning it off is asking not to
    /// have.
    #[test]
    fn disabling_clears_the_presence_and_drops_the_connection() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let on = Arc::new(AtomicBool::new(true));
        let (log_for_connect, on_for_enabled) = (Arc::clone(&log), Arc::clone(&on));
        let connects = Arc::new(Mutex::new(0usize));
        let connects_for_connect = Arc::clone(&connects);

        spawn_with(
            move || Some(on_for_enabled.load(Ordering::SeqCst)),
            Cadence { heartbeat: Duration::from_secs(3600), ..brisk() },
            move || {
                *connects_for_connect.lock().expect("the connect count") += 1;
                Ok(Box::new(Fake {
                    log: Arc::clone(&log_for_connect),
                    ping_fails: Arc::new(AtomicBool::new(false)),
                }) as Box<dyn Session>)
            },
        )
        .expect("the presence thread");

        assert!(until(|| calls(&log) >= 1), "the presence must go up before it can come down");
        on.store(false, Ordering::SeqCst);
        assert!(until(|| calls(&log) >= 2), "turning the setting off must clear the card");
        assert_eq!(log.lock().expect("the call log")[1], Call::Clear);

        // Off means off: no reconnect loop behind a disabled toggle.
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(*connects.lock().expect("the connect count"), 1);
        assert_eq!(calls(&log), 2, "a disabled presence must do nothing at all");
    }

    /// The clock restarts when the user turns the feature back on, because the
    /// span it is reporting genuinely did.
    #[test]
    fn re_enabling_publishes_a_fresh_clock() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let on = Arc::new(AtomicBool::new(true));
        let (log_for_connect, on_for_enabled) = (Arc::clone(&log), Arc::clone(&on));

        spawn_with(
            move || Some(on_for_enabled.load(Ordering::SeqCst)),
            Cadence { heartbeat: Duration::from_secs(3600), ..brisk() },
            move || {
                Ok(Box::new(Fake {
                    log: Arc::clone(&log_for_connect),
                    ping_fails: Arc::new(AtomicBool::new(false)),
                }) as Box<dyn Session>)
            },
        )
        .expect("the presence thread");

        assert!(until(|| calls(&log) >= 1));
        on.store(false, Ordering::SeqCst);
        assert!(until(|| calls(&log) >= 2));
        // A whole second, so the two timestamps cannot land in the same one.
        // The assertion is about the *reset*, and a sub-second gap would let a
        // held-over timestamp pass as a fresh one.
        std::thread::sleep(Duration::from_millis(1100));
        on.store(true, Ordering::SeqCst);
        assert!(until(|| calls(&log) >= 3), "turning it back on must publish again");

        let recorded = log.lock().expect("the call log");
        let (Call::Publish(first), Call::Publish(second)) = (&recorded[0], &recorded[2]) else {
            panic!("expected a publish, a clear, and a publish, got {recorded:?}");
        };
        assert!(second > first, "the clock must restart, not resume: {first} then {second}");
    }

    /// A dead pipe is noticed by the heartbeat and repaired, and the clock
    /// survives it — a Discord restart is not a new Trix session.
    #[test]
    fn a_failed_ping_reconnects_and_keeps_the_clock() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let ping_fails = Arc::new(AtomicBool::new(true));
        let connects = Arc::new(Mutex::new(0usize));
        let (log_for_connect, connects_for_connect) = (Arc::clone(&log), Arc::clone(&connects));
        let ping_for_connect = Arc::clone(&ping_fails);

        spawn_with(
            || Some(true),
            brisk(),
            move || {
                *connects_for_connect.lock().expect("the connect count") += 1;
                Ok(Box::new(Fake {
                    log: Arc::clone(&log_for_connect),
                    ping_fails: Arc::clone(&ping_for_connect),
                }) as Box<dyn Session>)
            },
        )
        .expect("the presence thread");

        assert!(
            until(|| *connects.lock().expect("the connect count") >= 2),
            "a failing ping must be followed by a reconnect"
        );
        ping_fails.store(false, Ordering::SeqCst);

        let recorded = log.lock().expect("the call log");
        let published: Vec<u64> = recorded
            .iter()
            .filter_map(|call| match call {
                Call::Publish(start) => Some(*start),
                _ => None,
            })
            .collect();
        assert!(published.len() >= 2, "each reconnect republishes the activity: {recorded:?}");
        assert_eq!(
            published[0], published[1],
            "a reconnect must not restart the clock the user is watching"
        );
    }

    /// A daemon that has been dropped ends the thread instead of leaving a live
    /// loop behind for the rest of the test run.
    #[test]
    fn a_dropped_daemon_ends_the_thread() {
        let connects = Arc::new(Mutex::new(0usize));
        let connects_for_connect = Arc::clone(&connects);

        spawn_with(
            || None,
            brisk(),
            move || {
                *connects_for_connect.lock().expect("the connect count") += 1;
                bail!("nothing should ever ask")
            },
        )
        .expect("the presence thread");

        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(*connects.lock().expect("the connect count"), 0);
    }

    #[test]
    fn a_frame_is_a_little_endian_opcode_length_and_body() {
        let frame = encode_frame(OP_FRAME, b"hi").expect("a two-byte frame");
        assert_eq!(frame, vec![1, 0, 0, 0, 2, 0, 0, 0, b'h', b'i']);
    }

    #[test]
    fn a_frame_round_trips() {
        let frame = encode_frame(OP_HANDSHAKE, br#"{"v":1}"#).expect("a handshake frame");
        let (op, body) = read_frame(&mut frame.as_slice()).expect("the frame reads back");
        assert_eq!(op, OP_HANDSHAKE);
        assert_eq!(body, br#"{"v":1}"#);
    }

    /// The length is attacker-shaped input in the sense that matters: it is a
    /// number off a pipe used to size an allocation.
    #[test]
    fn an_oversized_frame_is_refused_rather_than_allocated() {
        let mut header = Vec::new();
        header.extend_from_slice(&OP_FRAME.to_le_bytes());
        header.extend_from_slice(&u32::MAX.to_le_bytes());
        let error = read_frame(&mut header.as_slice()).expect_err("4 GB must be refused");
        assert!(format!("{error}").contains("past the"), "unexpected error: {error}");
    }

    /// The card's contents, asserted where a typo in a shipped string would be
    /// caught before a release rather than by a user reading their own profile.
    #[test]
    fn the_activity_carries_the_details_art_clock_and_button() {
        let card = activity(1_700_000_000);
        assert_eq!(card["details"], "Without thinking FPS and Memory");
        assert_eq!(card["timestamps"]["start"], 1_700_000_000u64);
        assert_eq!(card["assets"]["large_image"], "logo");
        assert_eq!(card["buttons"][0]["label"], "Get Trix");
        assert_eq!(card["buttons"][0]["url"], "https://github.com/tnhnblgl/trix/releases/latest");
        assert!(
            BUTTON_LABEL.len() <= 32,
            "Discord caps a button label at 32 characters, and a longer one is refused whole"
        );
    }

    /// `null` rather than an omitted key: an absent `activity` leaves the old
    /// card standing, and clearing is the entire point of the disabled path.
    #[test]
    fn clearing_sends_an_explicit_null_activity() {
        let command = set_activity_command(4321, "4321-2", None);
        assert_eq!(command["cmd"], "SET_ACTIVITY");
        assert_eq!(command["nonce"], "4321-2");
        assert_eq!(command["args"]["pid"], 4321);
        assert!(command["args"]["activity"].is_null(), "a cleared activity must be JSON null");
    }
}
