//! The `stats` event's timing loop.
//!
//! In the library half rather than in `main.rs`, where it started, for the
//! reason `main.rs`'s own doc comment already gives about the payload: a bin
//! target's code is not reachable by `cargo test`, and this is a timing loop —
//! two gates, a deadline, a reset branch and an overflow guard — which is
//! exactly the shape of code that needs exercising rather than reading. The
//! poll interval is a parameter so a test can run the whole loop in
//! milliseconds; `main.rs` passes [`STATS_POLL`] and nothing about production
//! changes.

use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use serde_json::Value;
use trix_proto::Event;

use crate::state::Daemon;

/// How often the stats thread wakes to ask whether an event is due.
///
/// Not the event interval — that is `Daemon::stats_interval`, which a client
/// can change at runtime through `config.set stats_seconds`. Sleeping for the
/// interval itself would mean a `stats.subscribe` sent just after a tick waited
/// a full period (a day, at `stats_seconds = 86400`) before anything arrived,
/// and a shortened interval would not take effect until the old one expired.
/// A quarter second is under the threshold at which a person notices a meter
/// starting late, and costs four mutex peeks a second on a daemon nobody is
/// watching.
pub const STATS_POLL: Duration = Duration::from_millis(250);

/// Broadcasts `stats` to subscribed clients while the daemon is armed.
///
/// Two gates and then a measurement, in that order, and the order is the point.
/// **Nobody subscribed** means nothing measures anything at all — that is what
/// makes the `stats_seconds = 0` default coherent (spec §4.4), and it is
/// checked first because it is the cheap check and the common case: a daemon
/// with no UI attached does no work here beyond waking up. **Nothing due yet**
/// is checked second, and everything expensive lives below it: `stats_json`
/// takes the `armed` lock, clones `EngineStatus`, issues a
/// `K32GetProcessMemoryInfo` syscall and builds an eleven-entry map, and at
/// `stats_seconds = 3600` all but one in 14,400 of those samples would be
/// thrown away — while contending four times a second for the same `armed`
/// lock `clip` needs. **Nothing armed** is last, and is `stats_json` answering
/// `None`: there are no counters to report, and the tick is skipped rather
/// than broadcasting a payload of zeroes that a live meter would render as a
/// stalled encoder. That last case deliberately leaves `next_at` in the past,
/// so arming mid-interval delivers within one poll instead of waiting out an
/// interval the client could not have been served during.
///
/// The thread runs for the life of the process and is never joined — like the
/// shutdown watcher in `main.rs`, `std::process::exit` takes it with us. It
/// holds only a `Weak<Daemon>`, takes the `clients` and `armed` locks briefly
/// and one at a time, and cannot deadlock against a broadcast: it never holds
/// one while taking the other.
pub fn spawn_stats_thread(daemon: Arc<Daemon>, poll: Duration) -> Result<()> {
    spawn_sampling(daemon, poll, Daemon::stats_json)
}

/// [`spawn_stats_thread`] with the measurement itself injectable.
///
/// The seam exists because the interesting half of this loop is only reachable
/// on an armed daemon, and arming needs a real capture session, a real encoder
/// and the process-wide single-instance slot — none of which a unit test may
/// take. `sample` stands in for "the engine has counters to report"; every
/// other input the loop reads (who is subscribed, what the interval is, what
/// time it is) stays real. The idle case does not need the seam and does not
/// use it: `a_subscribed_but_idle_daemon_is_sent_nothing` below drives
/// [`spawn_stats_thread`] itself — though that test asserts nothing *arrives*,
/// so what it establishes is that the real `Daemon::stats_json` stays silent on
/// an unarmed daemon, not that the delivering path is wired up. Nothing here
/// proves an armed daemon broadcasts; only hand verification against real
/// capture hardware does, which is what Step 5 of the task brief is for.
fn spawn_sampling(
    daemon: Arc<Daemon>,
    poll: Duration,
    sample: impl Fn(&Daemon) -> Option<Value> + Send + 'static,
) -> Result<()> {
    // Downgraded deliberately. In production this changes nothing — `main`'s
    // `Arc` and the shutdown watcher's slot both outlive the process — but it
    // means a test's thread ends within one poll of that test dropping its
    // daemon, instead of every test in the run leaving a live loop behind.
    let daemon: Weak<Daemon> = Arc::downgrade(&daemon);
    std::thread::Builder::new()
        .name("trix-stats".into())
        .spawn(move || {
            let mut next_at = Instant::now();
            loop {
                std::thread::sleep(poll);

                // The only owner is gone, so there is nothing left to report
                // on and nobody left to report to.
                let Some(daemon) = daemon.upgrade() else { return };

                if !daemon.clients.any_stats_subscribers() {
                    // Nothing is being measured, so nothing is owed. The
                    // deadline is pushed out rather than left in the past, so
                    // the first tick after someone subscribes is a fresh
                    // interval and not a burst of catch-up events.
                    next_at = Instant::now();
                    continue;
                }

                let now = Instant::now();
                if now < next_at {
                    continue;
                }

                // Below both gates: nothing above this line has measured
                // anything or taken the `armed` lock.
                let Some(data) = sample(&daemon) else { continue };

                // `checked_add` rather than `+`: the interval comes from
                // `stats_seconds`, which a client sets over the socket, and
                // `Instant + Duration` panics on overflow. That key is bounded
                // to a day by `state::NUMERIC_BOUNDS` now, so this cannot be
                // reached from the socket at all — but the guard stays,
                // because "cannot overflow" is an argument about the *current*
                // bound and this is a `panic = "abort"` build. Falling back to
                // `now` means an absurd interval degrades to every tick rather
                // than to a dead process.
                next_at = now.checked_add(daemon.stats_interval()).unwrap_or(now);
                daemon.clients.broadcast_stats(&Event::new("stats", data));
            }
        })
        .context("failed to spawn the stats thread")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{Receiver, sync_channel};

    use serde_json::Map;
    use trix_core::config::Config;

    use super::*;
    use crate::clients::{ClientId, OUTBOUND_QUEUE_DEPTH};

    /// Fast enough that a whole interval's worth of polls fits inside a test,
    /// slow enough that the loop is not a spin.
    const TEST_POLL: Duration = Duration::from_millis(10);

    /// How long "an event should have arrived by now" waits before failing.
    /// Deliberately generous: the longest real wait any test here has is the
    /// one-second floor `Daemon::stats_interval` puts under the event rate, and
    /// a timeout that is merely *close* to the expected latency is how a
    /// timing test becomes a flaky one. It costs nothing when the test passes.
    const EXPECT_WITHIN: Duration = Duration::from_secs(3);

    /// How long "nothing should arrive" watches for. Twenty polls, so a loop
    /// that was going to broadcast has had twenty chances to.
    const QUIET: Duration = Duration::from_millis(200);

    /// A daemon over an empty temp clip directory: `Daemon::new` scans
    /// `clip_dir` at construction, and pointing that at the developer's real
    /// `Videos\Trix` would make these timing tests wait on however much footage
    /// happens to be sitting there.
    fn fixture(name: &str) -> (Arc<Daemon>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("trix-stats-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp clip dir");
        let config = Config { clip_dir: dir.to_string_lossy().into_owned(), ..Config::default() };
        (Arc::new(Daemon::new_at(config, None)), dir)
    }

    /// A registered client that has *not* subscribed yet.
    fn connect(daemon: &Daemon) -> (ClientId, Receiver<String>) {
        let (tx, rx) = sync_channel(OUTBOUND_QUEUE_DEPTH);
        (daemon.clients.register(tx), rx)
    }

    /// What an armed daemon's `stats_json` stands in as. Shape does not matter
    /// to the loop — `state.rs` owns the payload's key set — only that there is
    /// something to report.
    fn counters(_: &Daemon) -> Option<Value> {
        let mut fields = Map::new();
        fields.insert("frames".to_string(), Value::from(1));
        Some(Value::Object(fields))
    }

    /// Bounded, never a bare sleep: a timing loop that stopped working must
    /// fail with a sentence rather than hang the suite.
    fn expect_stats(rx: &Receiver<String>, why: &str) {
        let line = match rx.recv_timeout(EXPECT_WITHIN) {
            Ok(line) => line,
            Err(e) => panic!("no stats event within {EXPECT_WITHIN:?} ({e}): {why}"),
        };
        let event: Event = serde_json::from_str(line.trim_end())
            .unwrap_or_else(|e| panic!("event line did not decode: {e}\nline: {line}"));
        assert_eq!(event.event, "stats", "the stats thread broadcasts `stats` and nothing else");
    }

    fn expect_quiet(rx: &Receiver<String>, why: &str) {
        expect_quiet_for(rx, QUIET, why);
    }

    /// [`expect_quiet`] with the window named explicitly, for the one test
    /// whose claim is about an *interval* rather than about the loop being
    /// idle. A quiet window has to outlast the wait it rules out: watching for
    /// 200 ms proves nothing about whether the next event is a second away or
    /// an hour away, because neither would have arrived yet.
    fn expect_quiet_for(rx: &Receiver<String>, window: Duration, why: &str) {
        if let Ok(line) = rx.recv_timeout(window) {
            panic!("an event arrived that should not have: {why}\nline: {line}");
        }
    }

    /// The delivering case: someone is listening and there are counters to
    /// report, so events flow.
    #[test]
    fn a_subscribed_daemon_with_counters_is_sent_stats_events() {
        let (daemon, dir) = fixture("subscribed");
        let (id, rx) = connect(&daemon);
        daemon.clients.set_stats(id, true);

        spawn_sampling(Arc::clone(&daemon), TEST_POLL, counters).expect("spawn the stats thread");

        expect_stats(&rx, "a subscriber on a daemon with counters must be told");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The armed gate, against the real `Daemon::stats_json` and the real
    /// entry point: nothing is armed, so there are no counters, so nothing is
    /// broadcast — not a payload of zeroes a meter would render as a stalled
    /// encoder.
    #[test]
    fn a_subscribed_but_idle_daemon_is_sent_nothing() {
        let (daemon, dir) = fixture("idle");
        let (id, rx) = connect(&daemon);
        daemon.clients.set_stats(id, true);

        spawn_stats_thread(Arc::clone(&daemon), TEST_POLL).expect("spawn the stats thread");

        expect_quiet(&rx, "an idle daemon has no counters, so a subscriber gets no events");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The subscriber gate and the deadline reset behind it.
    ///
    /// The interval is set to an hour on purpose: it makes the reset branch
    /// *falsifiable*. Without it, the deadline set by the first event would
    /// still be in force when the client resubscribes, and the last assertion
    /// would wait an hour for an event that never comes.
    #[test]
    fn nobody_subscribed_means_nothing_measured_and_no_deadline_to_wait_out() {
        let (daemon, dir) = fixture("unsubscribed");
        daemon.config.lock().expect("config lock").stats_seconds = 3600;
        let (id, rx) = connect(&daemon);

        spawn_sampling(Arc::clone(&daemon), TEST_POLL, counters).expect("spawn the stats thread");

        expect_quiet(&rx, "a connected client that never sent stats.subscribe is not measured for");

        daemon.clients.set_stats(id, true);
        expect_stats(&rx, "the first tick after subscribing is due immediately");
        expect_quiet(&rx, "the second is an hour out — the configured interval is honoured");

        // Long enough that the loop has certainly seen the unsubscribe, and
        // the assertion it carries is real: an unsubscribed client stays quiet.
        daemon.clients.set_stats(id, false);
        expect_quiet(&rx, "unsubscribing stops the events");

        daemon.clients.set_stats(id, true);
        expect_stats(
            &rx,
            "resubscribing must not wait out an hour-long deadline set while nobody was listening \
             — the no-subscribers branch resets it every poll",
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `stats_seconds` is read from the live config on every broadcast, not
    /// captured when the thread starts — a settings page changes it through
    /// `config.set` on a daemon that is already running.
    #[test]
    fn a_runtime_change_to_stats_seconds_is_picked_up() {
        let (daemon, dir) = fixture("runtime");
        let (id, rx) = connect(&daemon);
        daemon.clients.set_stats(id, true);

        spawn_sampling(Arc::clone(&daemon), TEST_POLL, counters).expect("spawn the stats thread");

        // Default `stats_seconds = 0`, which `stats_interval` reads as one
        // second, so this event is due immediately and the next one a second
        // later.
        expect_stats(&rx, "the first tick is due immediately");

        daemon.config.lock().expect("config lock").stats_seconds = 3600;

        // The deadline already standing was computed from the old interval, so
        // this one still arrives about a second in — the change takes effect
        // from the next event onward, not retroactively.
        expect_stats(&rx, "the deadline already set is honoured at its original length");
        // And *that* broadcast re-read the config, so the one after it is an
        // hour away rather than another second. The window has to outlast the
        // second it rules out — with the default 200 ms this assertion would
        // hold just as well if the interval had been read once at thread start,
        // which is exactly the bug it exists to catch.
        expect_quiet_for(
            &rx,
            Duration::from_millis(1_500),
            "the new interval was picked up without restarting the thread",
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
