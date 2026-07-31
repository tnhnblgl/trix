//! Who is connected, and where events go.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};

use trix_proto::{Event, encode_line};

/// Opaque per-connection id, assigned by [`Clients::register`]. A plain
/// `u64` alias rather than a newtype: the pipe transport (`pipe::ClientHandler`)
/// already speaks raw `u64` at its seam, and a newtype there would just push a
/// conversion onto every call site for no added safety in this crate.
pub type ClientId = u64;

/// How many event lines may be queued for one client before the daemon gives
/// up on it. The queue is *bounded* because an unbounded one is a memory leak
/// with a friendly name: a client that connects and then stops reading — pipe
/// buffer full, process wedged — would otherwise accumulate every event the
/// daemon ever emits, forever, in the one process that must not grow without
/// bound while it holds a multi-hundred-megabyte replay ring.
///
/// 256 is chosen against the fastest stream this protocol has: `stats` at
/// 1 Hz (spec §4.4) plus the occasional `clip_saved`/`armed`. A healthy client
/// drains every `EVENT_POLL_MS` (25 ms, `pipe.rs`), so 256 queued lines is
/// roughly four minutes of backlog — far past any transient scheduling hiccup,
/// GC pause, or debugger breakpoint, and comfortably short of the burst a
/// genuinely stuck client produces. The memory bound it buys is what matters:
/// event lines run a few hundred bytes, so a stuck client costs well under a
/// megabyte before it is evicted, not gigabytes.
pub const OUTBOUND_QUEUE_DEPTH: usize = 256;

struct Client {
    id: ClientId,
    out: SyncSender<String>,
    /// Set when this client is evicted for not draining its queue, and shared
    /// with its session thread (`pipe::serve_one` takes a clone via
    /// `ClientHandler::eviction_flag`). Dropping the registry entry stops the
    /// daemon's memory growing; setting this is what actually ends the
    /// connection, so the client learns it was dropped instead of running on
    /// as a healthy-looking socket that never delivers another event.
    evicted: Arc<AtomicBool>,
    /// Set by `stats.subscribe`. With nobody subscribed, nothing measures
    /// anything — which is what makes the `stats_seconds = 0` default
    /// coherent (spec §4.4).
    stats: bool,
}

#[derive(Default)]
pub struct Clients {
    inner: Mutex<Vec<Client>>,
    next_id: AtomicU64,
}

impl Clients {
    pub fn register(&self, out: SyncSender<String>) -> ClientId {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        self.lock().push(Client {
            id,
            out,
            evicted: Arc::new(AtomicBool::new(false)),
            stats: false,
        });
        id
    }

    /// A clone of `id`'s eviction flag, for that client's session thread to
    /// poll. `None` once the client is gone from the registry — callers ask
    /// immediately after [`Self::register`], while the entry certainly exists.
    pub fn eviction_flag(&self, id: ClientId) -> Option<Arc<AtomicBool>> {
        self.lock().iter().find(|c| c.id == id).map(|c| Arc::clone(&c.evicted))
    }

    pub fn unregister(&self, id: ClientId) {
        self.lock().retain(|client| client.id != id);
    }

    pub fn set_stats(&self, id: ClientId, enabled: bool) {
        if let Some(client) = self.lock().iter_mut().find(|c| c.id == id) {
            client.stats = enabled;
        }
    }

    /// True while at least one client wants `stats`. The stats thread checks
    /// this before doing any work.
    pub fn any_stats_subscribers(&self) -> bool {
        self.lock().iter().any(|client| client.stats)
    }

    pub fn broadcast(&self, event: &Event) {
        self.send_to(event, |_| true);
    }

    pub fn broadcast_stats(&self, event: &Event) {
        self.send_to(event, |client| client.stats);
    }

    fn send_to(&self, event: &Event, want: impl Fn(&Client) -> bool) {
        let Ok(line) = encode_line(event) else {
            tracing::error!(event = %event.event, "could not serialize an event");
            return;
        };
        self.lock().retain(|client| {
            if !want(client) {
                return true;
            }
            match client.out.try_send(line.clone()) {
                Ok(()) => true,
                // The client has stopped draining its socket: its pipe buffer
                // is full and its session thread is parked in `write_all`.
                // Every further event would be daemon memory held on behalf of
                // a process that has already failed, so the registry entry goes
                // instead. This is the mirror image of what `MAX_LINE_BYTES`
                // prevents on the read side.
                //
                // Dropping the entry is only half of it. The *connection* is
                // still open, and a wedged client that later resumes reading
                // would unpark, flush its backlog, and go on answering requests
                // normally — while never receiving another event, forever, and
                // never being told why. The flag closes that: the session
                // thread checks it at the top of its loop and ends the
                // connection as soon as it unparks, so the client sees a
                // disconnect and a UI worth the name reconnects.
                Err(TrySendError::Full(_)) => {
                    client.evicted.store(true, Ordering::Release);
                    tracing::warn!(
                        client = client.id,
                        depth = OUTBOUND_QUEUE_DEPTH,
                        "client stopped reading its events; dropping it rather than the daemon's memory"
                    );
                    false
                }
                // That client's session thread has returned and dropped its
                // receiver; drop the registry entry rather than letting the
                // registry accumulate dead entries.
                Err(TrySendError::Disconnected(_)) => false,
            }
        });
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Client>> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

// The mutex is recovered from poisoning rather than unwrapping. Under
// `panic = "abort"` a poisoned lock cannot occur at all, so this costs
// nothing today — but `inner.lock().unwrap()` is a panicking call on a
// socket-reachable path, and the Global Constraints forbid those outright.
// Written this way, the code stays correct if the profile ever changes.

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

    /// One client's outbound queue at production depth.
    fn channel() -> (SyncSender<String>, Receiver<String>) {
        sync_channel(OUTBOUND_QUEUE_DEPTH)
    }

    #[test]
    fn a_registered_client_receives_a_broadcast() {
        let clients = Clients::default();
        let (tx, rx) = channel();
        clients.register(tx);

        let event = Event::new("ping", json!(null));
        clients.broadcast(&event);

        let line = rx.try_recv().expect("a registered client should receive the broadcast");
        assert_eq!(line, encode_line(&event).unwrap());
    }

    #[test]
    fn an_unregistered_client_does_not_receive_a_broadcast() {
        let clients = Clients::default();
        let (tx, rx) = channel();
        let id = clients.register(tx);
        clients.unregister(id);

        clients.broadcast(&Event::new("ping", json!(null)));

        assert!(rx.try_recv().is_err(), "an unregistered client must not receive events");
    }

    #[test]
    fn broadcast_stats_reaches_only_subscribers() {
        let clients = Clients::default();

        let (sub_tx, sub_rx) = channel();
        let sub_id = clients.register(sub_tx);
        clients.set_stats(sub_id, true);

        let (plain_tx, plain_rx) = channel();
        clients.register(plain_tx);

        clients.broadcast_stats(&Event::new("stats", json!({"fps": 60})));

        assert!(sub_rx.try_recv().is_ok(), "the subscriber should receive the stats event");
        assert!(plain_rx.try_recv().is_err(), "a non-subscriber must not receive stats events");
    }

    /// A dropped receiver makes the paired `Sender::send` fail; that failure
    /// is how a dead client gets pruned without any explicit disconnect
    /// notice ever arriving (the pipe read loop may still be blocked).
    #[test]
    fn a_client_whose_receiver_has_been_dropped_is_evicted_after_one_failed_broadcast() {
        let clients = Clients::default();

        let (dead_tx, dead_rx) = channel();
        clients.register(dead_tx);
        drop(dead_rx);

        let (live_tx, live_rx) = channel();
        let live_id = clients.register(live_tx);

        assert_eq!(clients.lock().len(), 2, "both clients start out registered");

        clients.broadcast(&Event::new("ping", json!(null)));

        {
            let remaining = clients.lock();
            assert_eq!(remaining.len(), 1, "the client with a dropped receiver should be evicted");
            assert_eq!(remaining[0].id, live_id, "the live client should be the one left behind");
        }
        assert!(
            live_rx.try_recv().is_ok(),
            "the live client should still have received the broadcast"
        );
    }

    /// The bound is the point: a client that stops reading must cost the daemon
    /// a fixed amount of memory and then get dropped, not grow the queue until
    /// the process dies holding a replay ring.
    #[test]
    fn a_client_that_stops_reading_is_evicted_instead_of_buffered_without_bound() {
        let clients = Clients::default();

        // `_stalled_rx` is deliberately kept alive and never read from: this is
        // a client that is connected and simply not draining, which is a
        // different failure from the dropped-receiver case above.
        let (stalled_tx, _stalled_rx) = channel();
        clients.register(stalled_tx);

        let (live_tx, live_rx) = channel();
        let live_id = clients.register(live_tx);

        for n in 0..OUTBOUND_QUEUE_DEPTH {
            clients.broadcast(&Event::new("ping", json!(null)));
            // The healthy client drains as its session thread would, so its own
            // queue never fills — otherwise this test would evict both and
            // prove nothing about *which* client the policy drops.
            assert!(live_rx.try_recv().is_ok(), "the live client should receive event {n}");
        }
        assert_eq!(
            clients.lock().len(),
            2,
            "a queue that is full but has not overflowed keeps the client"
        );

        clients.broadcast(&Event::new("ping", json!(null)));

        {
            let remaining = clients.lock();
            assert_eq!(remaining.len(), 1, "one event past the cap evicts the stalled client");
            assert_eq!(remaining[0].id, live_id, "the client that kept reading stays");
        }
        assert!(
            live_rx.try_recv().is_ok(),
            "evicting one client must not cost the others the event that tripped it"
        );
    }

    /// Eviction has to reach the *connection*, not just the registry. The flag
    /// is the only channel it has: the session thread is parked in `write_all`
    /// on a full pipe and cannot be told anything else. A client whose entry is
    /// dropped while its socket stays open is worse than a disconnected one —
    /// it looks healthy, answers `status`, and silently never sees another
    /// `clip_saved`.
    #[test]
    fn an_evicted_client_has_its_session_told_to_close() {
        let clients = Clients::default();
        let (stalled_tx, _stalled_rx) = channel();
        let id = clients.register(stalled_tx);
        // Taken at registration, exactly as `pipe::serve_one` takes it.
        let flag = clients.eviction_flag(id).expect("a registered client has an eviction flag");
        assert!(!flag.load(Ordering::Acquire), "a fresh client is not evicted");

        for _ in 0..=OUTBOUND_QUEUE_DEPTH {
            clients.broadcast(&Event::new("ping", json!(null)));
        }

        assert!(clients.lock().is_empty(), "the stalled client should have been evicted");
        assert!(
            flag.load(Ordering::Acquire),
            "the session thread must be able to see that it was evicted, or the connection stays \
             open and permanently event-deaf"
        );
    }

    /// The flag outlives the registry entry — the session thread holds an
    /// `Arc`, so reading it after eviction is well defined rather than a
    /// lookup that now returns `None`.
    #[test]
    fn the_eviction_flag_is_gone_once_the_client_is_unregistered() {
        let clients = Clients::default();
        let (tx, _rx) = channel();
        let id = clients.register(tx);
        clients.unregister(id);
        assert!(clients.eviction_flag(id).is_none());
    }
}
