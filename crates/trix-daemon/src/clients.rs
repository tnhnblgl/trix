//! Who is connected, and where events go.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;

use trix_proto::{Event, encode_line};

/// Opaque per-connection id, assigned by [`Clients::register`]. A plain
/// `u64` alias rather than a newtype: the pipe transport (`pipe::ClientHandler`)
/// already speaks raw `u64` at its seam, and a newtype there would just push a
/// conversion onto every call site for no added safety in this crate.
pub type ClientId = u64;

struct Client {
    id: ClientId,
    out: Sender<String>,
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
    pub fn register(&self, out: Sender<String>) -> ClientId {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        self.lock().push(Client { id, out, stats: false });
        id
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
        // A failed send means that client's session thread has returned and
        // dropped its receiver; drop the registry entry here rather than
        // letting the registry accumulate dead entries.
        self.lock()
            .retain(|client| !want(client) || client.out.send(line.clone()).is_ok());
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
    use std::sync::mpsc::channel;

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
        assert!(live_rx.try_recv().is_ok(), "the live client should still have received the broadcast");
    }
}
