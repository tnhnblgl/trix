//! Everything the daemon owns, and the rules for changing it.
//!
//! Deliberately transport-free: nothing here knows about pipes, requests, or
//! events. [`crate::dispatch`] turns these methods into responses and decides
//! what to broadcast — which is also what keeps the lock ordering below
//! trivially true.

use std::sync::{Mutex, MutexGuard};

use anyhow::{Result, bail};
use trix_core::{config::Config, control, control::SingleInstance, engine::EngineHandle, library};
use trix_proto::ClipMeta;

use crate::clients::Clients;

/// The engine plus the single-instance slot it holds. Armed as a unit,
/// disarmed as a unit — dropping this releases the encoder *and* the slot, so
/// `trix replay` from a terminal works again the moment the daemon disarms.
struct Armed {
    engine: EngineHandle,
    _slot: SingleInstance,
}

/// What `status` answers with, and the payload of the `armed` event.
///
/// A struct rather than a bare `serde_json::Value` so this module's tests can
/// assert on fields, but [`Self::to_json`] is the only thing that reaches the
/// wire and its key set is frozen by spec §4.2: `armed`, `encoder`,
/// `monitor_index`, `ring_seconds_used`, `ring_seconds_total`, `version`,
/// `clip_dir`. A UI binds to those names, so they do not get to drift — see
/// `the_status_wire_shape_is_the_published_one` below.
pub struct DaemonStatus {
    pub armed: bool,
    /// `None` when disarmed — and also, for up to one 250 ms engine tick right
    /// after `arm` returns, when armed. [`EngineHandle::spawn`] returns as soon
    /// as the first capture session is live, but `EngineStatus.encoder` is only
    /// published on the engine's control tick, so a `status` (or an `armed`
    /// event) issued immediately after arming can legitimately carry `null`
    /// here. The next `status` has the real name. Closing that window wants a
    /// liveness field on the engine itself, which is Task 7's job — guessing a
    /// name here would be worse than reporting "not known yet".
    pub encoder: Option<String>,
    pub monitor_index: u32,
    pub ring_seconds_used: f64,
    pub ring_seconds_total: u32,
    pub clip_dir: String,
}

impl DaemonStatus {
    /// The wire object, built with explicit `Value::from` conversions rather
    /// than `serde_json::json!` or a `Serialize` derive: every arm of that
    /// macro other than its `true`/`false`/`null`/array/object literals expands
    /// to `serde_json::to_value(&x).unwrap()`, and this runs on a path a socket
    /// message reaches directly in a `panic = "abort"` build. The `From` impls
    /// used here (integers, `f64`, `&str`) cannot fail, so there is nothing to
    /// unwrap.
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::{Map, Value};
        let mut fields = Map::new();
        fields.insert("armed".to_string(), Value::Bool(self.armed));
        fields.insert(
            "encoder".to_string(),
            match &self.encoder {
                Some(name) => Value::from(name.as_str()),
                None => Value::Null,
            },
        );
        fields.insert("monitor_index".to_string(), Value::from(self.monitor_index));
        fields.insert("ring_seconds_used".to_string(), Value::from(self.ring_seconds_used));
        fields.insert("ring_seconds_total".to_string(), Value::from(self.ring_seconds_total));
        fields.insert("version".to_string(), Value::from(env!("CARGO_PKG_VERSION")));
        fields.insert("clip_dir".to_string(), Value::from(self.clip_dir.as_str()));
        Value::Object(fields)
    }
}

/// What [`Daemon::arm`] did.
pub struct ArmOutcome {
    /// False when the daemon was already armed. `arm` is idempotent, but the
    /// dispatcher only broadcasts `armed` on a real transition — a UI that
    /// reconnects and re-arms to sync its own toggle must not make every other
    /// client redraw.
    pub newly_armed: bool,
    pub status: DaemonStatus,
}

pub struct Daemon {
    pub config: Mutex<Config>,
    pub clients: Clients,
    armed: Mutex<Option<Armed>>,
    /// The library, scanned once at startup and updated incrementally.
    /// `library.list` never touches the disk after that (spec §5.2).
    pub library: Mutex<Vec<ClipMeta>>,
}

// Lock ordering, and the one rule that matters: `armed` is never held while
// `clients` is locked. Arming, disarming, and clipping all take `armed`; every
// event that results from them is broadcast by `dispatch` *after* those methods
// have returned and released it. So a broadcast — which locks `clients` and
// nothing else — can never deadlock against an arm, however many clients are
// connected. `armed` may be held while `config` or `library` is taken (that is
// the order `status` and `clip` use); the reverse never happens.
//
// `armed` is deliberately held across the slow operations: `EngineHandle::spawn`
// can take seconds, `EngineHandle::clip` blocks until the mux completes, and
// `disarm` holds it until `EngineHandle::stop` has joined *and* the
// single-instance slot has been released. A concurrent `status` waits that out,
// which is the right trade — the alternative is two threads racing to spawn two
// engines, a `disarm` tearing the ring down in the middle of a clip's write, or
// a daemon that answers `armed: false` while it is still holding the slot that
// makes the next `arm` fail.

impl Daemon {
    pub fn new(config: Config) -> Self {
        // One scan, at startup. A failure here is not fatal: an unreadable clip
        // directory must not stop the daemon from arming and capturing.
        let library = match library::scan(&config.clip_dir_path()) {
            Ok(clips) => clips,
            Err(e) => {
                tracing::warn!(
                    error = %format!("{e:#}"),
                    "could not scan the clip library at startup; starting empty"
                );
                Vec::new()
            }
        };
        Self {
            config: Mutex::new(config),
            clients: Clients::default(),
            armed: Mutex::new(None),
            library: Mutex::new(library),
        }
    }

    /// Starts capture. Idempotent: arming an already-armed daemon reports the
    /// current state rather than erroring, because a UI reconnecting after its
    /// own restart has no way to know whether the daemon it just found is
    /// already armed.
    pub fn arm(&self) -> Result<ArmOutcome> {
        let mut armed = self.lock_armed();
        if armed.is_some() {
            return Ok(ArmOutcome { newly_armed: false, status: self.status_of(armed.as_ref()) });
        }

        // The slot first, then the engine. With a `trix replay` session holding
        // the slot, "another trix capture session … is already running" is the
        // message the user can act on; taking the slot first is what makes it
        // win over the confusing encoder-in-use error the engine would
        // otherwise produce.
        let slot = control::acquire_single_instance()?;
        let config = self.lock_config().clone();
        // `spawn` returns only once the first capture session is live, so a
        // hardware failure ("no hardware encoder on the capture adapter")
        // arrives here verbatim instead of being lost on the engine thread.
        // Deliberately unbounded by a timeout: an honest arm takes seconds, and
        // interrupting one needs a thread-teardown design this task does not
        // have. On the `?`, `slot` drops and the single-instance mutex is
        // released before the error goes back — a failed arm must not leave the
        // CLI locked out.
        let engine = EngineHandle::spawn(config)?;
        *armed = Some(Armed { engine, _slot: slot });
        Ok(ArmOutcome { newly_armed: true, status: self.status_of(armed.as_ref()) })
    }

    /// Stops capture and releases the single-instance slot. `Ok(false)` means
    /// the daemon was already idle — a no-op rather than an error, so a UI
    /// syncing its toggle after a reconnect never sees a spurious failure.
    pub fn disarm(&self) -> Result<bool> {
        // The guard is bound, not left as a temporary of a `let-else`: a
        // temporary is released at the end of its statement, which would leave
        // `armed` reading `None` for the whole of the teardown below — while
        // the engine is still joining and the single-instance slot is still
        // held. In that window `status` reports `armed: false` about a daemon
        // that is still finalizing, and an `arm` racing it fails with "another
        // trix capture session (record or replay) is already running", pointing
        // the user at a CLI that is not running. Holding it means a concurrent
        // caller waits out the teardown and then gets a true answer.
        //
        // This does not touch the lock ordering: `clients` is still never taken
        // while `armed` is held. Nothing in here locks `clients` — the
        // `disarmed` event is broadcast by `dispatch` after this returns — and
        // `engine.stop()` is `trix-core`, which has no idea the registry exists.
        let mut armed = self.lock_armed();
        let Some(state) = armed.take() else { return Ok(false) };
        let Armed { engine, _slot } = state;
        // Whatever the rebuild loop finally returned is logged, not propagated.
        // By the time it is known, capture *is* stopped and the daemon *is*
        // idle; reporting that as a failed disarm would leave a UI's toggle
        // disagreeing with reality, which is the one outcome worth avoiding
        // here. The daemon log keeps the detail.
        if let Err(e) = engine.stop() {
            tracing::warn!(error = %format!("{e:#}"), "the engine reported an error on the way out");
        }
        // Only now, with the encoder actually released, is the slot handed
        // back — otherwise a `trix replay` racing the disarm could win the slot
        // and then fail on a still-busy encoder. The `armed` guard goes last,
        // so no other caller can observe the daemon as idle until the slot it
        // was holding is genuinely free.
        drop(_slot);
        drop(armed);
        Ok(true)
    }

    /// Saves a clip from the live ring. `Ok(None)` means there is no footage
    /// buffered yet; the caller turns that into the CLI's wording.
    pub fn clip(&self) -> Result<Option<ClipMeta>> {
        let saved = {
            let armed = self.lock_armed();
            let Some(state) = armed.as_ref() else {
                bail!("not armed — send arm first");
            };
            state.engine.clip()?
        };
        if let Some(meta) = &saved {
            // Prepended, matching `library::scan`'s newest-first order. This is
            // the incremental update that keeps `library.list` off the disk
            // after startup (spec §5.2).
            self.lock_library().insert(0, meta.clone());
        }
        Ok(saved)
    }

    pub fn status(&self) -> DaemonStatus {
        let armed = self.lock_armed();
        self.status_of(armed.as_ref())
    }

    /// Takes the `armed` slot as an argument rather than locking it, so `arm`
    /// can build the outcome's status without dropping and retaking the lock —
    /// which would leave a window where the answer describes a different state
    /// than the one just entered.
    fn status_of(&self, armed: Option<&Armed>) -> DaemonStatus {
        let config = self.lock_config();
        let clip_dir = config.clip_dir_path().to_string_lossy().into_owned();
        match armed {
            // Armed: every field comes from the engine, so a `monitor_index`
            // changed in config since arming still reports what is *actually*
            // being captured.
            Some(state) => {
                let engine = state.engine.status();
                DaemonStatus {
                    armed: true,
                    encoder: match engine.encoder.is_empty() {
                        true => None,
                        false => Some(engine.encoder),
                    },
                    monitor_index: engine.monitor_index,
                    ring_seconds_used: engine.ring_seconds_used,
                    ring_seconds_total: engine.ring_seconds_total,
                    clip_dir,
                }
            }
            // Idle: the config is the only truth there is.
            None => DaemonStatus {
                armed: false,
                encoder: None,
                monitor_index: config.monitor_index,
                ring_seconds_used: 0.0,
                ring_seconds_total: config.replay_seconds,
                clip_dir,
            },
        }
    }

    // Poisoning is recovered from rather than unwrapped: `lock().unwrap()` is a
    // panicking call on a path reachable straight from a socket message, and
    // the Global Constraints forbid those outright. See the same note in
    // `clients.rs`.
    fn lock_armed(&self) -> MutexGuard<'_, Option<Armed>> {
        self.armed.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_config(&self) -> MutexGuard<'_, Config> {
        self.config.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_library(&self) -> MutexGuard<'_, Vec<ClipMeta>> {
        self.library.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Disarming a daemon that was never armed is a no-op, not an error — a
    /// UI that reconnects and syncs its toggle must not see a spurious failure.
    #[test]
    fn disarm_when_idle_is_not_an_error() {
        let daemon = Daemon::new(Config::default());
        assert!(daemon.disarm().is_ok());
        assert!(!daemon.status().armed);
    }

    /// The status shape the UI binds to. Arming needs real hardware, so this
    /// covers only the disarmed branch — the armed branch is covered by the
    /// end-to-end gate in Task 8.
    #[test]
    fn idle_status_reports_config_not_engine_state() {
        let config = Config { replay_seconds: 30, monitor_index: 1, ..Config::default() };
        let daemon = Daemon::new(config);
        let status = daemon.status();
        assert!(!status.armed);
        assert_eq!(status.ring_seconds_total, 30);
        assert_eq!(status.monitor_index, 1);
        assert_eq!(status.ring_seconds_used, 0.0);
        assert!(status.encoder.is_none(), "a disarmed daemon has not chosen an encoder yet");
    }

    /// The exact key set from spec §4.2 — a UI is being built against it. This
    /// is the guard that a refactor of `DaemonStatus` cannot silently rename or
    /// drop one of them, and that `encoder` stays JSON `null` rather than
    /// becoming `""` when nothing is armed.
    #[test]
    fn the_status_wire_shape_is_the_published_one() {
        let daemon = Daemon::new(Config { clip_dir: r"D:\Clips".into(), ..Config::default() });
        let json = daemon.status().to_json();
        let object = json.as_object().expect("status must serialize as a JSON object");

        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "armed",
                "clip_dir",
                "encoder",
                "monitor_index",
                "ring_seconds_total",
                "ring_seconds_used",
                "version",
            ],
            "the status key set is published; a UI binds to exactly these"
        );

        assert_eq!(object.get("armed"), Some(&serde_json::Value::Bool(false)));
        assert_eq!(object.get("encoder"), Some(&serde_json::Value::Null));
        assert_eq!(object.get("clip_dir").and_then(|v| v.as_str()), Some(r"D:\Clips"));
        assert_eq!(
            object.get("version").and_then(|v| v.as_str()),
            Some(env!("CARGO_PKG_VERSION"))
        );
    }

    /// `clip` has to name the missing step rather than reporting some internal
    /// "no engine" condition — this string is what a user sees.
    #[test]
    fn clipping_while_idle_names_the_missing_step() {
        let daemon = Daemon::new(Config::default());
        let error = match daemon.clip() {
            Ok(_) => panic!("an idle daemon has no ring to clip from"),
            Err(e) => format!("{e:#}"),
        };
        assert_eq!(error, "not armed — send arm first");
    }
}
