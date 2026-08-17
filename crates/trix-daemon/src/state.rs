//! Everything the daemon owns, and the rules for changing it.
//!
//! Deliberately transport-free: nothing here knows about pipes, requests, or
//! events. [`crate::dispatch`] turns these methods into responses and decides
//! what to broadcast — which is also what keeps the lock ordering below
//! trivially true.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use serde_json::{Map, Value};
use trix_core::{
    capture::audio::AudioGains, config::Config, control, control::SingleInstance,
    engine::EngineHandle, engine::EngineStatus, library, stats,
};
use trix_proto::{ClipMeta, Event};

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

/// One page of the clip library, plus what a UI needs to render "3 of 47".
///
/// `total` is the *unpaged* count, not `clips.len()`: without it a client that
/// asked for 50 and got 50 cannot tell a full library from a full page, and
/// cannot size a scrollbar or a page count. `offset` is echoed back because
/// responses are matched by request id, not by argument — a client that has
/// several pages in flight would otherwise have to remember which id asked for
/// which page.
pub struct LibraryPage {
    pub clips: Vec<ClipMeta>,
    pub total: usize,
    pub offset: usize,
}

/// A sealed home for [`ClipPaths`], so the guarantee below is enforced by the
/// compiler against *this whole file*, not just against callers outside it.
///
/// A plain `struct ClipPaths { .. }` sitting directly in `state.rs` would not
/// be enough: private fields are visible to every function in the module that
/// declares them, so a fifth `impl Daemon` method added later — in this same
/// file — could still write a `ClipPaths { mp4: .., .. }` struct literal by
/// hand and skip validation entirely, and the compiler would not object. Only
/// a *child* module's private items are hidden from its parent, so nesting
/// the struct one level down and keeping its fields private to that nested
/// module is what actually closes the loophole: `state.rs` can call
/// [`ClipPaths::for_id`] and read the accessors, but it cannot see the fields
/// to construct one itself.
mod clip_paths {
    use std::path::{Path, PathBuf};

    use anyhow::{Result, bail};
    use trix_core::library;

    /// Already-built, already-validated paths for one clip id.
    ///
    /// The only way anywhere in this crate to end up holding one of these is
    /// [`ClipPaths::for_id`], which calls `library::is_valid_id` before it
    /// builds a single path. So holding a `ClipPaths` *is* the proof that the
    /// id it came from passed validation — a method that wants a clip's
    /// `.mp4`, sidecar, or thumbnail path has to call `for_id` and use the
    /// accessors below, and cannot get there by calling `library::mp4_path`
    /// (or its siblings) directly, because there is no other route to a path
    /// this type will hand out. Those `library` functions are still `pub` and
    /// still validate nothing on their own — that has not changed, and could
    /// not without touching `trix-core`, which is out of scope here — but a
    /// caller now has to go out of its way to reach them instead of through
    /// this door.
    pub struct ClipPaths {
        dir: PathBuf,
        mp4: PathBuf,
        sidecar: PathBuf,
        thumb: PathBuf,
    }

    impl ClipPaths {
        /// The one and only constructor. Validates `id` first; builds nothing
        /// if it is not a valid clip id, so a rejected id has caused no I/O
        /// and produced no path at all.
        pub(super) fn for_id(dir: PathBuf, id: &str) -> Result<Self> {
            if !library::is_valid_id(id) {
                bail!("{id:?} is not a valid clip id");
            }
            Ok(Self {
                mp4: library::mp4_path(&dir, id),
                sidecar: library::sidecar_path(&dir, id),
                thumb: library::thumb_path(&dir, id),
                dir,
            })
        }

        /// The clip directory itself — needed by `edit_meta`, which hands it
        /// to `library::write_sidecar` (that function derives the sidecar
        /// path internally, so it wants the directory, not a pre-built path).
        pub(super) fn dir(&self) -> &Path {
            &self.dir
        }

        pub(super) fn mp4(&self) -> &Path {
            &self.mp4
        }

        pub(super) fn sidecar(&self) -> &Path {
            &self.sidecar
        }

        pub(super) fn thumb(&self) -> &Path {
            &self.thumb
        }
    }
}
use clip_paths::ClipPaths;

/// What [`Daemon::arm`] did.
pub struct ArmOutcome {
    /// False when the daemon was already armed. `arm` is idempotent, but the
    /// dispatcher only broadcasts `armed` on a real transition — a UI that
    /// reconnects and re-arms to sync its own toggle must not make every other
    /// client redraw.
    pub newly_armed: bool,
    pub status: DaemonStatus,
}

/// The config keys that only take effect at the next `arm`.
///
/// Every one is baked into `RecorderSettings` or the capture session when
/// `EngineHandle::spawn` builds it, so setting one while armed changes the file
/// and nothing else. `config.set` *reports* them rather than acting on them:
/// silently re-arming would tear the live replay ring down — and with it the
/// last fifteen seconds the user may be about to clip — because someone nudged
/// a bitrate slider. The UI decides whether to prompt.
///
/// The three config keys deliberately absent take effect immediately:
/// `clip_dir` is read per clip, `stats_seconds` per stats tick, and
/// `clip_hotkey` belongs to `trix replay`'s own loop rather than to anything the
/// daemon arms.
pub(crate) const REQUIRES_REARM: [&str; 7] = [
    "fps",
    "bitrate_kbps",
    "max_bitrate_kbps",
    "rate_control",
    "replay_seconds",
    "monitor_index",
    "gpu_priority",
];

/// The two capture levels, which follow a different re-arm rule from every
/// other key.
const VOLUME_KEYS: [&str; 2] = ["system_volume", "mic_volume"];

/// True when a level moved onto or off zero.
///
/// Levels apply live: the capture session reads them from a shared cell every
/// pump, so `40 → 70` is audible in the next clip with no re-arm. The
/// exception is zero, which is not a quiet level but a closed capture stream —
/// and streams only open at arm time. Reporting those two transitions, and
/// only those, is what lets the settings page prompt exactly when a prompt
/// changes the outcome.
///
/// A non-numeric value on either side is not a crossing: it is a type error,
/// and `set_config`'s per-key retry produces a far better message for it.
fn crosses_zero(key: &str, before: Option<&Value>, after: Option<&Value>) -> bool {
    if !VOLUME_KEYS.contains(&key) {
        return false;
    }
    let (Some(before), Some(after)) =
        (before.and_then(Value::as_u64), after.and_then(Value::as_u64))
    else {
        return false;
    };
    (before == 0) != (after == 0)
}

/// The accepted range of every numeric config key, as `(key, min, max)`.
///
/// Sanity bounds, not a product policy. Their job is to reject the absurd —
/// the values that turn one socket line into a machine-wide failure — and to
/// leave every value a person might plausibly want alone. Serde's
/// `from_value` is airtight about *type* and says nothing about *magnitude*,
/// and `replay_seconds` proves why that gap matters: `{"replay_seconds":
/// 4294967295}` deserializes cleanly, persists, and at the next `arm` becomes a
/// 136-year retention window, so `replay.rs`'s eviction never evicts and the
/// ring grows until the process is OOM-killed. `bitrate_kbps` blows the same
/// RAM bound from the other side, since the ring's size is seconds × bitrate.
/// The precedent is `MAX_LIST_LIMIT` in `trix-proto`, which clamps
/// `library.list` for the same reason: a number off the wire that nothing else
/// bounds.
///
/// Why each number:
/// - `fps` 1..=480 — 0 persists a config nothing can arm from (`replay.rs` and
///   `capture/mod.rs` both `.max(1)`, so it survives, but only by ignoring
///   what the user asked for); 480 clears the fastest display anyone captures.
/// - `bitrate_kbps` 1..=200_000 — 200 Mbit/s is far above any real capture,
///   and 0 would ask the encoder for no bits at all.
/// - `max_bitrate_kbps` 0..=200_000 — 0 stays legal because it is the
///   documented "auto" (1.5× target), and is the default.
/// - `replay_seconds` 1..=600 — the ring is RAM, so ten minutes is the outer
///   bound; 0 would be a ring with nothing in it to clip.
/// - `monitor_index` 0..=63 — more attached displays than any consumer GPU
///   drives, and an out-of-range index only fails at `arm` anyway.
/// - `stats_seconds` 0..=86_400 — 0 stays legal because it is the documented
///   "no periodic log line", and is the default; a day is the outer bound.
/// - `max_library_gb` 0..=10_000 — 0 stays legal because it is the documented
///   "no ceiling"; 10 TB is past any drive this is likely to run on, and the
///   ceiling only ever *deletes*, so an absurdly large value is inert while an
///   absurdly small one is already handled (favorites are never pruned).
/// - `system_volume` 0..=100 — a percentage, where 100 is unity and the
///   maximum: Trix attenuates but never amplifies, so there is no legal value
///   above it. 0 stays legal and means the loopback stream is never opened.
/// - `mic_volume` 0..=100 — the same taper, and 0 carries more weight here than
///   anywhere else in this table: it is the *only* way to turn the microphone
///   off, because there is no separate toggle. Windows shows a microphone
///   indicator whenever a process holds an input stream, so 0 must leave the
///   stream unopened rather than open it and scale to silence.
///
/// The keys deliberately absent are the non-numeric ones. `rate_control` and
/// `gpu_priority` both document falling back on an unknown value
/// (`Config::rate_control`, `Config::gpu_priority_low`) and that behaviour is
/// not this constant's to change; `clip_hotkey` is parsed by `trix replay`'s
/// own loop; `clip_dir` is any path the user likes. What the table *must* not
/// do is fall silently behind `Config`, so
/// `every_numeric_config_key_is_range_checked` fails the build the day a
/// numeric key is added without a bound here.
const NUMERIC_BOUNDS: [(&str, u64, u64); 9] = [
    ("fps", 1, 480),
    ("bitrate_kbps", 1, 200_000),
    ("max_bitrate_kbps", 0, 200_000),
    ("replay_seconds", 1, 600),
    ("monitor_index", 0, 63),
    ("stats_seconds", 0, 86_400),
    ("max_library_gb", 0, 10_000),
    ("system_volume", 0, 100),
    ("mic_volume", 0, 100),
];

/// Refuses the whole request if any bounded key is out of range.
///
/// Runs before anything is merged, deserialized, or written, so a rejection has
/// touched neither the daemon's in-memory config nor the file — `config.set` is
/// all-or-nothing across the whole `values` map, and a settings page that sent
/// five keys and got an error must not have to guess which two landed.
///
/// Only values that are already non-negative whole numbers are checked here.
/// A `-1`, a `1.5`, or a `"sixty"` is not a range problem but a *type* problem,
/// and [`Daemon::set_config`]'s existing per-key retry produces a far better
/// message for it ("does not accept …") than a range would. Skipping those here
/// lets each error say the true thing.
fn check_ranges(values: &Map<String, Value>) -> Result<()> {
    for (key, value) in values {
        let Some(&(_, min, max)) = NUMERIC_BOUNDS.iter().find(|(name, _, _)| name == key) else {
            continue;
        };
        let Some(number) = value.as_u64() else { continue };
        if number < min || number > max {
            bail!("config.set: {key:?} accepts {min} to {max}, not {number}");
        }
    }
    Ok(())
}

/// What `stats_seconds = 0` means to the control socket: one event a second.
///
/// 0 disables the periodic *log line* (`trix replay`'s self-report) and that
/// stays true — but a client that sent `stats.subscribe {enabled:true}` has
/// asked to be told, and "off" is not an answer to that. 1 Hz is the rate spec
/// §4.4 describes and the one [`crate::clients::OUTBOUND_QUEUE_DEPTH`] is sized
/// against: fast enough for a live meter, slow enough to cost nothing
/// measurable next to a 60 fps hardware encode.
const DEFAULT_STATS_SECONDS: u32 = 1;

/// What `config.set` did (spec §4.3: "accepted values + which keys require a
/// re-arm to take effect").
///
/// `Debug`-derived so the brief's tests can call `expect_err` on a
/// `Result<ConfigUpdate>` — `expect_err` needs the `Ok` side printable for the
/// panic message it never actually reaches on a passing test.
#[derive(Debug)]
pub(crate) struct ConfigUpdate {
    /// The keys that were applied, with the values as they now stand — read
    /// back out of the saved config rather than echoed from the request, so a
    /// UI repaints from what the daemon holds and not from what it asked for.
    pub accepted: Map<String, Value>,
    /// Which of those keys need an `arm` before they change what is captured.
    pub requires_rearm: Vec<String>,
    /// `clip_dir` resolved the same way `config_json`'s `clip_dir_resolved`
    /// is. Carried out here, rather than left for `dispatch.rs` to ask
    /// `Daemon` for separately, because this module is deliberately
    /// transport-free (see the module doc) — `dispatch.rs` uses this to
    /// broadcast `config_changed` without a second round trip into `Daemon`.
    pub clip_dir_resolved: String,
}

pub struct Daemon {
    pub config: Mutex<Config>,
    /// Where `config.set` persists. `None` means there is nowhere to save —
    /// `%APPDATA%` unset — which is an error on that command and irrelevant to
    /// every other one, so it is not a startup failure.
    ///
    /// `pub(crate)`: `dispatch.rs`'s `sound.test` reads it directly to find the
    /// converted sound cache beside the config file, the same way
    /// `record_saved_clip` below does.
    pub(crate) config_path: Option<PathBuf>,
    /// `Arc`, not a bare `Clients`, so `window::spawn` can hand a clone to the
    /// pump thread — see `window.rs`'s `CLIENTS` thread-local and its doc for
    /// why: `hotkey_pressed` has to reach `Clients::broadcast` from `wnd_proc`
    /// itself, unconditionally, or a press the daemon received while the
    /// action queue was full never gets reported at all.
    pub clients: Arc<Clients>,
    armed: Mutex<Option<Armed>>,
    /// The library, scanned once at startup and updated incrementally.
    /// `library.list` never touches the disk after that (spec §5.2).
    pub library: Mutex<Vec<ClipMeta>>,
    /// The capture levels, owned for the daemon's whole lifetime and cloned
    /// into each armed session.
    ///
    /// One instance, not one per arm: `config.set` has to be able to apply a
    /// level whether or not anything is armed, and building a fresh one per
    /// arm would mean it had to find whichever instance the live session
    /// happened to hold — the arrangement where a level change silently lands
    /// on an object nobody is reading.
    pub(crate) gains: Arc<AudioGains>,
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

/// [`library::scan`] with the measurement around it.
///
/// The count and the elapsed time are logged at `info` deliberately rather than
/// `debug`: plan 3 owes a number for how a 5,000-clip library behaves, and this
/// is the instrument that produces it — from a real user's directory, on the
/// path that actually runs, without a benchmark harness that would measure
/// something else. `elapsed_ms` is an integer field rather than a formatted
/// `Duration` so the answer can be grepped straight out of the daemon log.
fn scan_and_log(dir: &Path) -> Result<Vec<ClipMeta>> {
    let started = std::time::Instant::now();
    let clips = library::scan(dir)?;
    tracing::info!(
        clips = clips.len(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        dir = %dir.display(),
        "scanned the clip library"
    );
    Ok(clips)
}

/// A config as a JSON object.
///
/// Every field, straight off the `Serialize` derive, so a config key added
/// later reaches `config.get` and becomes settable through `config.set` without
/// anyone remembering to touch this file. The `Result` is honoured rather than
/// unwrapped even though a struct of `u32`s and `String`s cannot fail to
/// serialize: this runs on a path a socket message reaches directly, in a
/// `panic = "abort"` build.
fn config_object(config: &Config) -> Result<Map<String, Value>> {
    match serde_json::to_value(config) {
        Ok(Value::Object(object)) => Ok(object),
        Ok(other) => bail!("the config did not serialize as an object: {other}"),
        Err(e) => Err(e).context("serializing the config"),
    }
}

impl Daemon {
    /// The daemon over the real `%APPDATA%\trix\config.toml`. Production
    /// callers want this one.
    pub fn new(config: Config) -> Self {
        Self::new_at(config, Config::path())
    }

    /// The daemon with `config.set` persisting to `config_path` — the same seam
    /// [`crate::pipe::serve_at`] is to `pipe::serve`, and for the same reason:
    /// without it, a test that exercises `config.set` at all would rewrite the
    /// settings of whoever ran `cargo test`.
    pub(crate) fn new_at(config: Config, config_path: Option<PathBuf>) -> Self {
        let clip_dir = config.clip_dir_path();
        // Best-effort, deliberately not fatal. On a fresh install
        // `%USERPROFILE%\Videos\Trix` does not exist yet, and creating it here
        // is what stops the tray's "Open clips folder" launching Explorer at a
        // path that is not there. On a machine whose clips live on an external
        // drive the directory may be genuinely absent right now and back in a
        // minute — refusing to start over that would be absurd. [`Self::arm`]
        // is where an unusable clip directory becomes an error, because that is
        // where the user is present and can act on it.
        if let Err(e) = library::ensure_writable(&clip_dir) {
            tracing::warn!(
                dir = %clip_dir.display(),
                error = %format!("{e:#}"),
                "the clip directory is not usable yet; arming will refuse until it is"
            );
        }
        // One scan, at startup. A failure here is not fatal: an unreadable clip
        // directory must not stop the daemon from arming and capturing.
        let library = match scan_and_log(&clip_dir) {
            Ok(clips) => clips,
            Err(e) => {
                tracing::warn!(
                    error = %format!("{e:#}"),
                    "could not scan the clip library at startup; starting empty"
                );
                Vec::new()
            }
        };
        let gains = AudioGains::new(config.system_volume, config.mic_volume);
        Self {
            config: Mutex::new(config),
            config_path,
            clients: Arc::new(Clients::default()),
            armed: Mutex::new(None),
            library: Mutex::new(library),
            gains,
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

        // Before the slot and before the engine. An unusable clip directory is
        // the one arm failure a user fixes in ten seconds — plug the drive back
        // in, or pick another folder — and arming is the moment they are
        // present and paying attention. The alternative is a green tray icon
        // over a replay ring whose clips can never be saved, discovered an hour
        // later at the exact moment they wanted one. `clip_dir` is read per
        // clip rather than baked into the engine, so this is the last check
        // that happens before the ring starts filling.
        //
        // Cheap enough to be unconditional: one directory create and one probe
        // file against an `arm` that takes seconds to bring a hardware encoder
        // up. Taking `config` under `armed` keeps the documented lock order.
        // Bare "cannot arm", not a second sentence naming the directory:
        // `ensure_writable` already names it and gives the OS's reason, and
        // repeating it produced "cannot arm: the clip directory D:\x is not
        // usable: could not create clip directory D:\x: …".
        library::ensure_writable(&self.lock_config().clip_dir_path()).context("cannot arm")?;

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
        let engine = EngineHandle::spawn(config, Arc::clone(&self.gains))?;
        *armed = Some(Armed { engine, _slot: slot });
        // Here rather than in the tray's own handler: `arm` is reached from the
        // tray menu, the control socket, and the desktop UI in stage 4, and an
        // icon that only tracked its own menu would show "idle" through a
        // UI-initiated recording. A no-op when there is no window.
        crate::window::publish_armed(true);
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
        // After the slot is back, so the icon never says "idle" about a daemon
        // that is still holding the encoder. See the note in `arm`.
        crate::window::publish_armed(false);
        Ok(true)
    }

    /// Saves a clip from the live ring. `Ok(None)` means there is no footage
    /// buffered yet; the caller turns that into the CLI's wording.
    ///
    /// Broadcasts `clip_saved` here rather than at the call sites, because
    /// there are two of them and only one ever did it. `dispatch::clip` (the
    /// socket command) broadcast; `window::handle_action`'s `Action::Clip`
    /// (the hotkey and the tray) did not, so a clip taken with the hotkey was
    /// written to disk and no client was ever told. The desktop app's grid
    /// only learned about it on a reload — which is precisely the failure
    /// spec §10's stage-4 gate opens by forbidding: "clip appears in the grid
    /// within a second of the hotkey".
    ///
    /// `dispatch::clip`'s comment had predicted the hotkey path would need
    /// this and asked for it to live in one place. It stayed at that one call
    /// site instead, and the second caller arrived a plan later without it.
    /// Emitting from inside the function every caller must go through is what
    /// makes that structural rather than a rule to remember.
    pub fn clip(&self) -> Result<Option<ClipMeta>> {
        let saved = {
            let armed = self.lock_armed();
            let Some(state) = armed.as_ref() else {
                bail!("not armed — send arm first");
            };
            state.engine.clip()?
        };
        if let Some(meta) = &saved {
            self.record_saved_clip(meta);
        }
        Ok(saved)
    }

    /// Takes a clip that has just been written to disk into the live library
    /// and tells every connected client about it.
    ///
    /// Split out of [`Self::clip`] so it can be tested. `clip` itself needs an
    /// armed engine with a real capture ring, so nothing in the suite can
    /// reach the lines below through it -- which is exactly how a saved clip
    /// went three plans without an event for the hotkey path. This half needs
    /// only a `ClipMeta`, so the guarantee that a recorded clip is an
    /// announced clip is now something a test can hold.
    fn record_saved_clip(&self, meta: &ClipMeta) {
        // Read before the library lock is taken, not after. `config` is only
        // ever held alone or under `armed` (see the lock ordering above), and
        // taking it after `library` here would introduce the first place in
        // the daemon where those two are held the other way round.
        let sound = {
            let config = self.lock_config();
            if !config.clip_sound {
                None
            } else if config.clip_sound_path.trim().is_empty() {
                // The built-in chime. `play` takes `None` to mean that.
                Some(None)
            } else {
                Some(self.config_path.as_deref().map(Config::sound_cache_path))
            }
        };

        // Prepended, matching `library::scan`'s newest-first order. This is
        // the incremental update that keeps `library.list` off the disk
        // after startup (spec §5.2).
        self.lock_library().insert(0, meta.clone());
        self.enforce_library_ceiling();
        // After the insert, so a client that answers the event by calling
        // `library.list` cannot race ahead of the clip it was told about.
        match serde_json::to_value(meta) {
            Ok(data) => self.clients.broadcast(&Event::new("clip_saved", data)),
            // `ClipMeta` is strings, integers and bools, so this cannot
            // actually happen -- but `panic = "abort"` leaves no room for
            // an `unwrap` on a path the hotkey reaches, and a clip that
            // saved is still saved even if nobody can be told.
            Err(e) => {
                tracing::error!(error = %e, "could not serialize a saved clip for clip_saved");
            }
        }

        // Last, after the clip is on disk and every client has been told. The
        // sound is feedback about a completed clip, so it goes behind
        // everything that makes the clip real.
        if let Some(custom) = sound {
            crate::sound::play(custom.as_deref());
        }
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

    // --- Configuration ------------------------------------------------------

    /// `config.get`: the effective config as a JSON object, plus
    /// `clip_dir_resolved`.
    ///
    /// The config's own keys come from its `Serialize` derive rather than a
    /// hand-written list, so a key added to `Config` later appears here without
    /// this file changing — which is what the key-set test in `dispatch.rs`
    /// pins down.
    ///
    /// `clip_dir_resolved` is not a config key and is not settable: `clip_dir`
    /// is empty by default and means "wherever the default is", and a settings
    /// page cannot show a user where their clips actually land without the
    /// daemon resolving it for them.
    /// Where clips are written, resolved. For the tray's "Open clips folder".
    pub fn clip_dir(&self) -> std::path::PathBuf {
        self.lock_config().clip_dir_path()
    }

    /// The configured clip hotkey, for `window::spawn`.
    ///
    /// Deliberately this one key rather than a whole-`Config` snapshot: a
    /// snapshot invites callers to read the other keys long after they have
    /// gone stale, and `config.set` can change any of them at any time. A
    /// changed hotkey is rebound live by `set_config` calling
    /// `window::rebind_hotkey` — but only when a pump is running and posting
    /// to it succeeds. `rebind_hotkey`'s own doc comment is the honest
    /// version of this: with no pump (unit tests, and the tail of shutdown)
    /// or a failed `PostMessageW`, the rebind is skipped entirely and the
    /// running registration — like this copy — stays stale until the next
    /// restart, exactly the behavior this feature exists to remove in the
    /// case where it does reach a live pump.
    pub fn clip_hotkey(&self) -> String {
        self.lock_config().clip_hotkey.clone()
    }

    pub fn config_json(&self) -> Result<Value> {
        let config = self.lock_config();
        let mut object = config_object(&config)?;
        object.insert(
            "clip_dir_resolved".to_string(),
            Value::from(config.clip_dir_path().to_string_lossy().as_ref()),
        );
        // Spec §7.4's first run is "no config file", and `Config::load` never
        // writes one, so this is an observation rather than a flag somebody
        // has to remember to clear.
        //
        // Read from this daemon's own `config_path` rather than from
        // `Config::path()`. They are the same thing in production, and very
        // much not in a test: a static lookup would consult the developer's
        // real `%APPDATA%\trix\config.toml` and answer `true` no matter what
        // this daemon was pointed at, which would make the test below pass
        // for the wrong reason and hide the case it exists to cover.
        object.insert(
            "config_file_exists".to_string(),
            Value::Bool(self.config_path.as_ref().is_some_and(|path| path.is_file())),
        );
        // Overridden, not read from the file: the registry is the source of
        // truth for autostart (spec §7.3). A user who removed the Run entry in
        // regedit or a startup manager has disabled autostart, and a settings
        // page still showing the toggle on would be telling them otherwise.
        // The key exists in `Config` so it round-trips through `config.set`
        // and survives in the file; what it holds is advisory.
        object.insert("autostart".to_string(), Value::Bool(crate::autostart::is_enabled()));
        Ok(Value::Object(object))
    }

    /// `config.set`: applies known keys, refuses unknown ones by name, writes
    /// the file, and reports which of the keys need a re-arm.
    ///
    /// Nothing is applied and nothing is written unless *every* key is
    /// acceptable — a settings page that sent five keys and got an error must
    /// not have to guess which two landed. "Acceptable" is three separate
    /// gates: the key is one `Config` has, its value is in range
    /// ([`NUMERIC_BOUNDS`]), and it deserializes at the right type. All three
    /// run before the file is opened.
    pub(crate) fn set_config(&self, values: &Map<String, Value>) -> Result<ConfigUpdate> {
        // The known-key set is read off a serialized `Config::default()`, not
        // listed here, for the same reason `config_json` serializes: a config
        // key added later is settable without anyone remembering this function
        // exists.
        let defaults = config_object(&Config::default())?;
        for key in values.keys() {
            if !defaults.contains_key(key) {
                let known: Vec<&str> = defaults.keys().map(String::as_str).collect();
                bail!("config.set: unknown key {key:?} (known keys: {})", known.join(", "));
            }
        }
        // Before the lock, before the merge, before the file: an out-of-range
        // value has cost nothing but the check by the time it is refused.
        check_ranges(values)?;

        // Held across the write, deliberately. `config.set` is a
        // read-modify-write of the whole file: two clients setting different
        // keys at once would otherwise each save a file built from what they
        // read, and the loser's key would silently revert. The cost is that a
        // concurrent `status` waits out a small write to local disk; the
        // alternative is a settings page that sometimes does not stick. This
        // does not touch the lock ordering documented above — `config` is only
        // ever taken alone or under `armed`, never the other way around, and
        // nothing here locks `armed` or `clients`.
        let mut config = self.lock_config();
        let base = config_object(&config)?;
        let mut merged = base.clone();
        for (key, value) in values {
            merged.insert(key.clone(), value.clone());
        }

        let updated: Config = match serde_json::from_value(Value::Object(merged)) {
            Ok(updated) => updated,
            // `from_value`'s message says what was wrong ("invalid type:
            // string, expected u32") but not *which* key, and a settings page
            // has to be able to point at the offending field. Re-applying one
            // key at a time finds it. This costs an extra deserialize per key
            // on a path that has already failed, and nothing at all otherwise.
            Err(e) => {
                for (key, value) in values {
                    let mut one = base.clone();
                    one.insert(key.clone(), value.clone());
                    if serde_json::from_value::<Config>(Value::Object(one)).is_err() {
                        bail!("config.set: {key:?} does not accept {value}: {e}");
                    }
                }
                bail!("config.set: {e}");
            }
        };

        // The last gate, and the only one that touches the filesystem. A
        // `clip_dir` naming somewhere that cannot take clips is refused here,
        // before the registry and before the file, so the all-or-nothing
        // contract holds: the caller is told no, the old directory is still in
        // force, and nothing was written. A settings page or the tray's folder
        // picker therefore cannot leave the user pointed at a folder that
        // silently swallows every clip.
        //
        // Guarded on the key being present, not run every time: this writes a
        // probe file, and five unrelated keys should not wake a possibly-absent
        // drive to save a bitrate. `updated` rather than the raw value, so an
        // empty string is checked as the default it resolves to.
        //
        // Not wrapped in a `config.set:` context like the errors above.
        // `ensure_writable` already names the directory and the OS's reason,
        // and this message is shown verbatim in the tray's folder-picker
        // message box, where "config.set" is jargon about a protocol the user
        // has never heard of.
        if values.contains_key("clip_dir") {
            library::ensure_writable(&updated.clip_dir_path())?;
        }

        // Decoded here, before anything is written, for the same all-or-nothing
        // reason as the gate above: a file that is not audio must leave the
        // user with the sound they already had. The bytes are held rather than
        // written because the config file has not been saved yet -- a new cache
        // beside an unchanged config is exactly the mismatch this ordering
        // exists to prevent.
        //
        // Guarded on the key being present, so five unrelated keys do not start
        // Media Foundation to save a bitrate.
        let converted = if values.contains_key("clip_sound_path") {
            let chosen = updated.clip_sound_path.trim();
            if chosen.is_empty() {
                // Reset: nothing to convert, and the cache must go.
                Some(None)
            } else {
                Some(Some(
                    trix_core::sound::decode::to_wav(std::path::Path::new(chosen))
                        .context("that file cannot be used as a sound")?,
                ))
            }
        } else {
            None
        };

        // Disk first, then memory — the same order the clip library uses. A
        // failed write leaves the daemon agreeing with the file rather than
        // capturing at a setting no restart would reproduce.
        let path = self
            .config_path
            .as_deref()
            .context("there is nowhere to save the config — APPDATA is not set")?;

        // Before the file, not after. `autostart` lives in the registry
        // (spec §7.3), so this is the one key whose real write is not
        // `save_to`, and it can fail on its own. Doing it first keeps this
        // function's all-or-nothing contract: a registry failure here means
        // nothing was written and nothing was applied, exactly like a value
        // that failed validation. Writing the file first would instead leave
        // every *other* key in this call persisted to disk while the caller
        // was told the call failed — and live after the next restart.
        //
        // The residue if the file write below fails is the registry entry
        // alone, which is harmless: the registry is the source of truth, so
        // `config.get` reports what actually happened rather than a stale
        // file value.
        if values.contains_key("autostart") {
            crate::autostart::set_enabled(updated.autostart)
                .with_context(|| format!("could not set autostart to {}", updated.autostart))?;
        }

        updated.save_to(path).context("config.set could not save the config")?;

        // After the config is on disk, not before. A failed write here is
        // repaired at the next startup only *because* the arm below deletes
        // whatever cache is left -- it is not true on its own.
        // `repair_sound_cache`'s only guard is "a cache file exists," with no
        // record of which sound produced it, so leaving a stale cache in
        // place (the rename in `write_sound_cache` is atomic, so a failure
        // here can only leave the *previous* sound's cache standing, never a
        // half-written one) would have every future startup see a cache
        // present, assume it matches the config it sits beside, and never
        // rebuild it. The daemon would keep playing the old sound forever
        // while `config.get` reports the new one, with no automatic recovery.
        // Deleting it here is what keeps "cache exists" meaning "cache
        // matches the configured sound."
        match converted {
            Some(Some(wav)) => {
                if let Err(e) = write_sound_cache(path, &wav) {
                    tracing::warn!(error = %format!("{e:#}"), "could not save the converted sound");
                    // Best-effort, for the reason above: a cache that still
                    // names the old sound is worse than no cache at all.
                    // Playback already falls back to the built-in chime when
                    // the cache is absent, so this is an honest intermediate
                    // state rather than a wrong one.
                    let _ = std::fs::remove_file(Config::sound_cache_path(path));
                }
            }
            Some(None) => {
                // Best-effort: there is usually no cache to remove, because the
                // user was on the built-in sound already.
                let _ = std::fs::remove_file(Config::sound_cache_path(path));
            }
            None => {}
        }

        let after = config_object(&updated)?;
        let mut accepted = Map::new();
        let mut requires_rearm = Vec::new();
        for key in values.keys() {
            if let Some(value) = after.get(key) {
                accepted.insert(key.clone(), value.clone());
            }
            if REQUIRES_REARM.contains(&key.as_str())
                || crosses_zero(key, base.get(key), after.get(key))
            {
                requires_rearm.push(key.clone());
            }
        }
        *config = updated;

        // After the write, like the hotkey rebind below and for the same
        // reason: a live session capturing at a level the user was told had
        // failed to save is exactly what this ordering prevents.
        self.gains.set(config.system_volume, config.mic_volume);

        // After the write, not before: a rebind that beat a failed write would
        // leave the running hotkey and the saved hotkey disagreeing, and
        // `config.set` is all-or-nothing everywhere else.
        if values.contains_key("clip_hotkey") {
            crate::window::rebind_hotkey(&config.clip_hotkey);
        }

        // Read under the same lock the write above just landed into, the same
        // way `config_json` computes it fresh rather than trusting a value
        // carried in from before the merge — `dispatch.rs` broadcasts this in
        // `config_changed` unconditionally, not only when `clip_dir` was one
        // of `values`, so a client always learns where clips land now.
        let clip_dir_resolved = config.clip_dir_path().to_string_lossy().into_owned();

        Ok(ConfigUpdate { accepted, requires_rearm, clip_dir_resolved })
    }

    /// Rebuilds the converted sound if it is missing.
    ///
    /// Two things reach this state and both are ordinary: a `config.toml`
    /// edited by hand, which names a sound no conversion ever ran for, and a
    /// cache someone deleted while tidying `%APPDATA%`.
    ///
    /// Called at startup rather than from the constructor so no test builds a
    /// `Daemon` that starts Media Foundation.
    pub fn repair_sound_cache(&self) {
        let Some(config_path) = self.config_path.as_deref() else {
            return;
        };
        let chosen = self.lock_config().clip_sound_path.trim().to_string();
        if chosen.is_empty() || Config::sound_cache_path(config_path).exists() {
            return;
        }

        match trix_core::sound::decode::to_wav(std::path::Path::new(&chosen)) {
            Ok(wav) => {
                if let Err(e) = write_sound_cache(config_path, &wav) {
                    tracing::warn!(error = %format!("{e:#}"), "could not rebuild the clip sound");
                }
            }
            // The setting is deliberately left alone. Putting the file back and
            // restarting is then all it takes; clearing it here would quietly
            // discard a choice the user still wants.
            Err(e) => tracing::warn!(
                error = %format!("{e:#}"),
                sound = %chosen,
                "the chosen clip sound could not be read; using the built-in one"
            ),
        }
    }

    // --- Statistics ---------------------------------------------------------

    /// The live engine's counters, or `None` when nothing is armed.
    ///
    /// Private: [`Self::stats_json`] two methods below is the only caller, and
    /// a second one would be a second place that has to know `EngineStatus`
    /// only exists while armed.
    fn engine_status(&self) -> Option<EngineStatus> {
        self.lock_armed().as_ref().map(|state| state.engine.status())
    }

    /// How long the stats thread waits between `stats` events.
    pub fn stats_interval(&self) -> Duration {
        let seconds = self.lock_config().stats_seconds.max(DEFAULT_STATS_SECONDS);
        Duration::from_secs(u64::from(seconds))
    }

    /// The `stats` event's payload (spec §4.4), or `None` when nothing is
    /// armed — every counter in it comes from the live engine, and there is no
    /// honest value for "frames captured" without one.
    ///
    /// Built key by key with `Value::from` for the same reason
    /// [`DaemonStatus::to_json`] is: `serde_json::json!` hides an `unwrap` and
    /// this reaches the socket in a `panic = "abort"` build.
    ///
    /// No GPU memory numbers, deliberately. `StatsReporter` reads those off the
    /// capture session's D3D device; the daemon has no device even while armed,
    /// because the reporter lives *inside* the session. Standing a second DXGI
    /// device up in the daemon purely so it could report on itself would cost
    /// the memory this project exists to save. `trix_core::stats::working_set`
    /// needs no device and is what ships.
    pub fn stats_json(&self) -> Option<Value> {
        let engine = self.engine_status()?;
        let mut fields = Map::new();
        fields.insert(
            "encoder".to_string(),
            match engine.encoder.is_empty() {
                true => Value::Null,
                false => Value::from(engine.encoder.as_str()),
            },
        );
        fields.insert("monitor_index".to_string(), Value::from(engine.monitor_index));
        fields.insert("width".to_string(), Value::from(engine.width));
        fields.insert("height".to_string(), Value::from(engine.height));
        fields.insert("fps".to_string(), Value::from(engine.fps));
        fields.insert("ring_seconds_used".to_string(), Value::from(engine.ring_seconds_used));
        fields.insert("ring_seconds_total".to_string(), Value::from(engine.ring_seconds_total));
        fields.insert("frames".to_string(), Value::from(engine.frames));
        fields.insert("dropped".to_string(), Value::from(engine.dropped));
        fields.insert("paced".to_string(), Value::from(engine.paced));
        fields.insert("working_set".to_string(), Value::from(stats::working_set()));
        Some(Value::Object(fields))
    }

    // --- The clip library ---------------------------------------------------
    //
    // The cache in `self.library` *is* the library as far as the socket is
    // concerned (spec §5.2): `list` never touches the disk, and every mutation
    // writes the disk first and then updates the cache, so a failed write never
    // leaves a client believing something happened that did not.

    /// Deletes the oldest non-favorite clips until the library fits under
    /// `max_library_gb`, and evicts whatever went from the cache (spec §5.4).
    ///
    /// Runs *after* a save, never before: the clip the user just asked for is
    /// never the one deleted to make room for itself. That means the library
    /// can sit one clip over the ceiling until the next save, which is the
    /// right trade — a ceiling is a housekeeping convenience, and losing the
    /// clip you just pressed the hotkey for is the product failing at its job.
    ///
    /// A failed prune never fails the clip that triggered it. The clip is on
    /// disk and in the cache by the time this runs; reporting an error now
    /// would tell the user their clip did not save when it did.
    ///
    /// The cache eviction is not optional bookkeeping. `library.list` reads the
    /// cache and never the disk, so a pruned clip left in it would be a row
    /// whose file cannot be opened, that `total` still counts, and that
    /// `rename` would happily write an orphan sidecar for — the same ghost-row
    /// failure [`Daemon::delete`] evicts to prevent.
    fn enforce_library_ceiling(&self) {
        let (dir, max_gb) = {
            let config = self.lock_config();
            (config.clip_dir_path(), config.max_library_gb)
        };
        match library::prune_to_ceiling(&dir, max_gb) {
            Ok(removed) if removed.is_empty() => {}
            Ok(removed) => {
                self.lock_library().retain(|clip| !removed.contains(&clip.id));
                tracing::info!(count = removed.len(), "the clip library ceiling pruned old clips");
            }
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "the clip library ceiling failed")
            }
        }
    }

    /// Re-reads the clip directory and replaces the cache.
    ///
    /// Deliberately not wired to a command in this build: [`Daemon::new`] scans
    /// at startup and every mutation below keeps the cache current, which is
    /// the whole point of caching it. It exists as the seam for the two things
    /// that need one — a test that wants a known library without depending on
    /// what the startup scan happened to see, and the `library.refresh` a later
    /// plan will want for "I deleted clips in Explorer behind your back", which
    /// is then a one-line dispatch arm rather than a new code path.
    pub fn rescan_library(&self) -> Result<()> {
        let dir = self.lock_config().clip_dir_path();
        let clips = scan_and_log(&dir)?;
        *self.lock_library() = clips;
        Ok(())
    }

    /// One page of the library, newest first — straight out of the cache, no
    /// disk access (spec §5.2).
    ///
    /// `limit` is already clamped to `MAX_LIST_LIMIT` by `Command::parse`, so
    /// it is not re-clamped here. `offset` is not, and does not need to be:
    /// `skip` past the end yields an empty iterator rather than the panic a
    /// slice range would give. An offset past the end is a short page, not an
    /// error — a UI paging a library that shrank under it should see the end of
    /// the list, not a failure.
    pub fn list(&self, offset: usize, limit: usize) -> LibraryPage {
        let library = self.lock_library();
        LibraryPage {
            total: library.len(),
            clips: library.iter().skip(offset).take(limit).cloned().collect(),
            offset,
        }
    }

    /// Removes a clip's `.mp4`, `.json`, and `.jpg`, then drops it from the
    /// cache.
    ///
    /// The `.mp4` is the clip: if it is not there, there is nothing to delete
    /// and that is an error. The sidecar and the thumbnail are derived files —
    /// nothing writes a `.jpg` yet, and a sidecar can legitimately be missing
    /// on an adopted clip — so their absence is not.
    pub fn delete(&self, id: &str) -> Result<()> {
        let paths = self.paths_for(id)?;

        // A missing `.mp4` is still an error, but it is not a reason to stop:
        // the companion cleanup and the cache eviction below both have to run
        // either way. See the two comments on those steps.
        let mp4_was_missing = match std::fs::remove_file(paths.mp4()) {
            Ok(()) => false,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
            Err(e) => return Err(e).with_context(|| format!("could not delete clip {id}")),
        };

        // The clip is gone whatever happens next, so a stuck sidecar or
        // thumbnail is logged rather than propagated: reporting a failed delete
        // for a clip that no longer exists would leave a UI showing a row whose
        // file it cannot open.
        //
        // This runs on the missing-`.mp4` path too, and that is the point. Once
        // the eviction below drops the row, no client can ever name this id
        // again — `library.list` reads the cache, and `library::scan` skips a
        // `.json` with no `.mp4` forever — so anything left here would be
        // unreachable litter the daemon could never be asked to remove. The
        // user deleting the `.mp4` in Explorer is exactly how that arises.
        for path in [paths.sidecar(), paths.thumb()] {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "the clip was deleted but one of its companion files was not"
                ),
            }
        }

        // The cache can be stale — the user may have deleted this clip in
        // Explorer while the daemon kept running. `library.list` still shows it
        // (cache-only reads are by design, spec §5.2), so without evicting here
        // the row would be stuck forever: there is no `library.refresh` in this
        // build, `total` would stay wrong until the daemon restarts, and a
        // `rename` on the ghost row would happily write an orphan `.json` next
        // to an `.mp4` that no longer exists. The brief only requires that a
        // missing `.mp4` be an error, not that the cache entry survive it, so
        // evicting does not contradict the plan — and
        // `an_unknown_but_wellformed_id_is_a_clean_error` still holds: that id
        // was never in the cache, so this is a no-op for it.
        self.lock_library().retain(|clip| clip.id != id);

        if mp4_was_missing {
            bail!("no clip {id} in the library")
        }
        Ok(())
    }

    /// Retitles a clip. The file never moves — the id is the filename stem and
    /// the title is metadata, so renaming cannot collide, cannot break a handle
    /// an open player is holding, and cannot invalidate the id a client is
    /// still using in other requests.
    pub fn rename(&self, id: &str, title: &str) -> Result<ClipMeta> {
        let title = title.trim();
        if title.is_empty() {
            bail!("a clip title cannot be blank");
        }
        self.edit_meta(id, |meta| meta.title = title.to_string())
    }

    /// The same write path as [`Daemon::rename`], with the star instead of the
    /// title.
    pub fn set_favorite(&self, id: &str, favorite: bool) -> Result<ClipMeta> {
        self.edit_meta(id, |meta| meta.favorite = favorite)
    }

    /// Opens Explorer with the clip selected.
    ///
    /// Built argument by argument, never as a formatted command line: the id is
    /// whitelisted by [`Self::paths_for`] but the clip *directory* comes
    /// from user config and can hold spaces, quotes, or an `&`, and handing
    /// that to a shell would be an injection with the user's own token.
    /// `std::process::Command` passes the path as one argument.
    ///
    /// A missing `.mp4` is an error here and, unlike [`Daemon::delete`], does
    /// *not* evict the stale cache row. The asymmetry is deliberate: `delete`
    /// is a mutation the user asked for and its whole job is to make the row go
    /// away, so discovering the file was already gone is just an earlier route
    /// to the same outcome. `reveal` is read-shaped — a UI sends it on a click,
    /// sometimes on hover — and a read that silently deletes the row the user
    /// was pointing at would surprise far more than the stale row does. The
    /// stale row still has a route out: the `delete` the user reaches for next,
    /// or the `library.refresh` a later plan owns.
    ///
    /// Explorer's exit code is not checked, and the child is not waited on:
    /// `explorer.exe /select,` routinely returns non-zero after opening the
    /// window correctly (it hands the request to the already-running shell
    /// process and exits). Whether it *spawned* is the only thing that
    /// distinguishes "the user is looking at their clip" from "nothing
    /// happened", so that is what is reported.
    pub fn reveal(&self, id: &str) -> Result<()> {
        let paths = self.paths_for(id)?;
        if !paths.mp4().exists() {
            bail!("no clip {id} in the library");
        }
        std::process::Command::new("explorer.exe")
            .arg("/select,")
            .arg(paths.mp4())
            .spawn()
            .with_context(|| format!("could not open Explorer for clip {id}"))?;
        Ok(())
    }

    /// The choke point every id-taking command goes through, and — because
    /// [`ClipPaths`] is sealed in its own submodule and [`ClipPaths::for_id`]
    /// is the only function anywhere that can build one — genuinely the only
    /// way to get a clip's paths from an id.
    ///
    /// Ids arrive from the socket and are concatenated into filesystem paths,
    /// so `for_id` validates *before* building any of them: a caller cannot
    /// get a `ClipPaths` without having passed the check, and a command added
    /// later cannot forget it without also having nowhere to get one from.
    /// That is a real, compiler-checked guarantee now, not just a hopeful
    /// comment — see the `mod clip_paths` doc comment above for why the
    /// struct had to move into its own module to make it one. It is also not
    /// a claim that the only way to *touch a clip file* changed:
    /// `Config::clip_dir_path` and `library::{mp4_path, sidecar_path,
    /// thumb_path}` are all still `pub` and still validate nothing on their
    /// own — `status_of`, `new`, and `rescan_library` call `clip_dir_path`
    /// directly, deliberately, because none of them take an id to validate in
    /// the first place. What changed is narrower, and stating it precisely
    /// matters more than stating it strongly: **no path a `ClipPaths` hands
    /// out can have come from an unvalidated id**, and no code in this crate
    /// currently reaches a clip file by any other route. Someone determined
    /// could still write `library::mp4_path(&self.lock_config().clip_dir_path(),
    /// id)` in two lines and it would compile — the seal makes the safe road
    /// the obvious one and the unsafe road a visible detour, which is what a
    /// compiler can buy here. It does not make the unsafe road impossible. The
    /// check itself is `trix_core::library::is_valid_id` — a whitelist of
    /// digits and underscores, which cannot express `..`, a separator, a
    /// drive letter, or a UNC prefix — and nothing here touches the disk, so
    /// a rejected id has caused no I/O at all.
    fn paths_for(&self, id: &str) -> Result<ClipPaths> {
        let dir = self.lock_config().clip_dir_path();
        ClipPaths::for_id(dir, id)
    }

    /// Disk first, then cache. Shared by `rename` and `set_favorite` so the two
    /// cannot drift on validation, on ordering, or on what a failed write
    /// leaves behind.
    ///
    /// The lock is taken twice — once to copy the entry out, once to put the
    /// edited one back — rather than held across `write_sidecar`, so a slow or
    /// hung disk cannot block every other client's `list`. The window that
    /// opens is a concurrent edit of the *same clip*, whose loser is simply the
    /// earlier write; there is one desktop user behind this socket, and the
    /// alternative costs every reader.
    ///
    /// The same window has a second shape: `edit_meta` racing `delete` on the
    /// same id. If `delete` wins first, this function's cache lookup above
    /// already fails it cleanly. If `edit_meta` wins the read and `delete`
    /// removes the `.mp4` before `write_sidecar` below runs, this call still
    /// returns `Ok` — it writes a fresh orphan `.json` for a clip whose `.mp4`
    /// is already gone, and the final `iter_mut().find` silently no-ops
    /// because `delete` has already evicted the cache entry. No behaviour
    /// change here either — one desktop user, acceptable — but worth knowing
    /// if `delete` is ever made to hold this lock across its own write.
    fn edit_meta(&self, id: &str, edit: impl FnOnce(&mut ClipMeta)) -> Result<ClipMeta> {
        let paths = self.paths_for(id)?;

        let mut meta = {
            let library = self.lock_library();
            let Some(found) = library.iter().find(|clip| clip.id == id) else {
                bail!("no clip {id} in the library");
            };
            found.clone()
        };
        edit(&mut meta);

        // The sidecar is rewritten before the cache is touched: if this fails,
        // the client gets an error and the cache still matches the disk. The
        // other order would answer "renamed" about a title that vanishes at the
        // next scan.
        library::write_sidecar(paths.dir(), &meta)
            .with_context(|| format!("could not update clip {id}"))?;

        if let Some(entry) = self.lock_library().iter_mut().find(|clip| clip.id == id) {
            *entry = meta.clone();
        }
        Ok(meta)
    }

    // Poisoning is recovered from rather than unwrapped: `lock().unwrap()` is a
    // panicking call on a path reachable straight from a socket message, and
    // the Global Constraints forbid those outright. See the same note in
    // `clients.rs`.
    fn lock_armed(&self) -> MutexGuard<'_, Option<Armed>> {
        self.armed.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// `pub(crate)`: `dispatch.rs`'s `sound.test` locks this directly to read
    /// `clip_sound_path`, the same way `record_saved_clip` above does.
    pub(crate) fn lock_config(&self) -> MutexGuard<'_, Config> {
        self.config.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_library(&self) -> MutexGuard<'_, Vec<ClipMeta>> {
        self.library.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Writes the converted sound beside the config file, through a temporary name.
///
/// Temp-then-rename rather than a direct write: a half-written cache is a valid
/// path with an invalid file at it, and `PlaySound` answers that with silence.
/// A rename on the same volume is atomic, so the cache is either the old sound
/// or the new one and never a prefix of either.
fn write_sound_cache(config_path: &std::path::Path, wav: &[u8]) -> Result<()> {
    let final_path = Config::sound_cache_path(config_path);
    let temp_path = final_path.with_extension("wav.tmp");
    if let Some(parent) = final_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    std::fs::write(&temp_path, wav)
        .with_context(|| format!("could not write {}", temp_path.display()))?;
    if let Err(e) = std::fs::rename(&temp_path, &final_path) {
        // Best-effort: a failed rename must not also leave the temp file
        // behind. Nothing else ever revisits `clip-sound.wav.tmp`, so
        // without this an error return here would litter it permanently.
        let _ = std::fs::remove_file(&temp_path);
        return Err(e).with_context(|| format!("could not replace {}", final_path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    fn meta(id: &str) -> ClipMeta {
        ClipMeta {
            id: id.to_string(),
            title: format!("clip_{id}"),
            created: "2026-07-26T10:00:00+03:00".into(),
            duration_ms: 15_000,
            bytes: 5,
            width: 1920,
            height: 1080,
            fps: 60,
            encoder: "test".into(),
            has_audio: true,
            favorite: false,
        }
    }

    fn fixture(name: &str) -> (Daemon, PathBuf) {
        let dir = std::env::temp_dir().join(format!("trix-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for id in ["20260726_100000", "20260726_110000", "20260726_120000"] {
            std::fs::write(trix_core::library::mp4_path(&dir, id), b"video").unwrap();
            trix_core::library::write_sidecar(&dir, &meta(id)).unwrap();
        }
        let config = Config { clip_dir: dir.to_string_lossy().into_owned(), ..Config::default() };
        let daemon = Daemon::new_at(config, None);
        daemon.rescan_library().unwrap();
        (daemon, dir)
    }

    /// A daemon that touches neither of the developer's real files, for the
    /// tests that need no clips at all.
    ///
    /// These were written as `Daemon::new(Config::default())`, which is the
    /// production constructor: it resolves `config_path` from `Config::path()`
    /// — the live `%APPDATA%\trix\config.toml` — and runs the startup library
    /// scan over the real `Videos\Trix`. So `disarm_when_idle_is_not_an_error`
    /// and its neighbours walked the user's actual footage on every
    /// `cargo test --workspace`, and the `config.set` tests were aimed at their
    /// actual settings. Nothing was ever written, because those tests reject
    /// before the write — but that is a property of the assertions, not of the
    /// fixture, and this crate already had the seam (`Daemon::new_at`) in three
    /// other places with a doc comment explaining exactly why it matters.
    ///
    /// `config_path` is `None`, so a `config.set` has nowhere to go. `clip_dir`
    /// is a scratch path that is deliberately never created — `library::scan`
    /// reports a missing directory as an empty library — so this does no
    /// filesystem I/O and leaves nothing to clean up.
    fn idle(name: &str, config: Config) -> Daemon {
        let dir = std::env::temp_dir().join(format!("trix-idle-{name}-{}", std::process::id()));
        Daemon::new_at(Config { clip_dir: dir.to_string_lossy().into_owned(), ..config }, None)
    }

    /// A daemon whose `config.set` really writes, to a scratch file the test
    /// owns.
    ///
    /// Both halves of the isolation are required, and a scratch `%APPDATA%`
    /// would supply neither: `Daemon::new_at` takes the config path directly,
    /// and `clip_dir` is a key *inside* that config whose empty default
    /// resolves to the developer's real `%USERPROFILE%\Videos\Trix` no matter
    /// what `%APPDATA%` says. A test that writes a config must own both or it
    /// is editing the settings of whoever ran `cargo test`.
    fn writable(name: &str) -> (Daemon, PathBuf) {
        let dir = std::env::temp_dir().join(format!("trix-vol-{name}-{}", std::process::id()));
        // The name is only unique per process id, and a test that fails an
        // assertion never reaches its `cleanup`. Without this, a reused pid
        // inherits the previous run's `config.toml` and clips.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch directory");
        let config = Config {
            clip_dir: dir.join("clips").to_string_lossy().into_owned(),
            ..Config::default()
        };
        (Daemon::new_at(config, Some(dir.join("config.toml"))), dir)
    }

    /// Removes a `writable` scratch directory. Best-effort: a leaked temp
    /// directory is not worth failing a passing test over.
    fn cleanup(dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A daemon whose `config.set` writes to a scratch file rather than the
    /// developer's real `%APPDATA%\trix\config.toml`, returning both halves of
    /// the isolation the sound tests below need: the scratch config path
    /// (`Config::sound_cache_path` derives the cache from it) and the scratch
    /// `clip_dir`.
    ///
    /// `dispatch.rs` has a helper of the same name and shape, but it is
    /// private to that module's own test module and out of scope here, so
    /// this is state.rs's own copy rather than a shared import.
    fn with_scratch_config(name: &str) -> (Daemon, PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("trix-config-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let config = Config { clip_dir: dir.to_string_lossy().into_owned(), ..Config::default() };
        (Daemon::new_at(config, Some(path.clone())), path, dir)
    }

    /// `config.get` must report the registry, not the file (spec §7.3).
    ///
    /// Loads the daemon with the *opposite* of whatever the registry actually
    /// says, so the two disagree and only one answer can be right. Asserting
    /// against `is_enabled()` alone would re-implement the override in its own
    /// assertion and pass no matter what `config_json` did; this fails the
    /// moment the override line is dropped, on a machine in either state.
    ///
    /// Read-only — it never writes a Run entry.
    #[test]
    fn config_get_reports_the_registry_and_not_the_config_file() {
        let truth = crate::autostart::is_enabled();
        let daemon = idle("autostart", Config { autostart: !truth, ..Config::default() });

        let json = daemon.config_json().unwrap();
        assert_eq!(
            json.get("autostart"),
            Some(&Value::Bool(truth)),
            "the Run entry decides, whatever the config file holds"
        );
    }

    #[test]
    fn list_pages_newest_first_and_clamps_past_the_end() {
        let (daemon, dir) = fixture("list");

        let page = daemon.list(0, 2);
        assert_eq!(page.total, 3, "total is the unpaged count — the UI needs it for \"3 of 47\"");
        let ids: Vec<&str> = page.clips.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["20260726_120000", "20260726_110000"]);

        let page = daemon.list(2, 50);
        assert_eq!(page.clips.len(), 1, "a short final page, not an error");

        let page = daemon.list(99, 50);
        assert!(page.clips.is_empty(), "an offset past the end is empty, not a panic");
        assert_eq!(page.total, 3);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The security boundary. Every id-taking command runs through the same
    /// check; if one forgets, this catches it.
    ///
    /// `delete` and `reveal` are proven by the `canary` id, which really would
    /// hit `canary.mp4` if validation were skipped — that is the load-bearing
    /// case. `rename` and `favorite` cannot be proven the same way: both go
    /// through `edit_meta`, whose cache lookup fails on all five of these ids
    /// regardless of whether `is_valid_id` ran (none of them name a clip
    /// `library::scan` would ever have adopted, since `scan` itself filters
    /// through `is_valid_id`). So for those two, `is_err()` alone would pass
    /// even with the check ripped out — what has to be asserted is the error
    /// *text*, which distinguishes "refused at the boundary" from "fell
    /// through to a `NotFound` lookup".
    #[test]
    fn every_id_command_rejects_a_traversal_attempt() {
        let (daemon, dir) = fixture("traversal");
        let canary = dir.join("canary.mp4");
        std::fs::write(&canary, b"must survive").unwrap();

        for evil in [
            "../../../Windows/System32/config/SAM",
            r"..\..\secrets",
            "x/../y",
            "canary",
            r"C:\Windows\System32\drivers\etc\hosts",
        ] {
            assert!(daemon.delete(evil).is_err(), "delete accepted {evil:?}");

            let rename_err = daemon.rename(evil, "x").unwrap_err().to_string();
            assert!(
                rename_err.contains("is not a valid clip id"),
                "rename on {evil:?} must be refused at validation, not fall through to a lookup: {rename_err}"
            );

            let favorite_err = daemon.set_favorite(evil, true).unwrap_err().to_string();
            assert!(
                favorite_err.contains("is not a valid clip id"),
                "favorite on {evil:?} must be refused at validation, not fall through to a lookup: {favorite_err}"
            );

            assert!(daemon.reveal(evil).is_err(), "reveal accepted {evil:?}");
        }
        assert!(canary.exists(), "a rejected id must not have touched the filesystem");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rename_edits_the_title_and_never_moves_the_file() {
        let (daemon, dir) = fixture("rename");
        let id = "20260726_110000";

        daemon.rename(id, "Ace on Ascent").unwrap();

        let on_disk =
            trix_core::library::read_sidecar(&trix_core::library::sidecar_path(&dir, id)).unwrap();
        assert_eq!(on_disk.title, "Ace on Ascent");
        assert_eq!(on_disk.id, id, "the id is the file stem and never changes");
        assert!(
            trix_core::library::mp4_path(&dir, id).exists(),
            "renaming edits metadata only — no collision logic, no broken handles"
        );

        let cached = daemon.list(0, 50);
        let entry = cached.clips.iter().find(|c| c.id == id).unwrap();
        assert_eq!(entry.title, "Ace on Ascent", "the cache must not go stale");

        assert!(daemon.rename(id, "   ").is_err(), "a blank title is not a rename");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn delete_removes_all_three_sidecar_files_and_the_cache_entry() {
        let (daemon, dir) = fixture("delete");
        let id = "20260726_100000";
        // Nothing writes thumbnails yet; deletion must already handle one.
        std::fs::write(trix_core::library::thumb_path(&dir, id), b"jpeg").unwrap();

        daemon.delete(id).unwrap();

        assert!(!trix_core::library::mp4_path(&dir, id).exists());
        assert!(!trix_core::library::sidecar_path(&dir, id).exists());
        assert!(!trix_core::library::thumb_path(&dir, id).exists());
        assert_eq!(daemon.list(0, 50).total, 2);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An id that is valid syntax but names no clip is a clean error, not a
    /// panic and not a silent success.
    #[test]
    fn an_unknown_but_wellformed_id_is_a_clean_error() {
        let (daemon, dir) = fixture("unknown");

        let err = daemon.delete("20991231_235959").unwrap_err().to_string();
        assert!(err.contains("20991231_235959"), "the error should name the clip: {err}");
        assert!(daemon.rename("20991231_235959", "x").is_err());
        assert_eq!(daemon.list(0, 50).total, 3, "a failed delete changed nothing");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A clip deleted in Explorer while the daemon keeps running: `library.list`
    /// still shows it, because cache-only reads are by design (spec §5.2). But
    /// there is no `library.refresh` in this build, so a `delete` that fails
    /// with `NotFound` has to evict the cache entry itself — otherwise the row
    /// is stuck forever and `total` stays wrong until the daemon restarts, and
    /// a `rename` on the ghost would write an orphan `.json` for an `.mp4` that
    /// no longer exists.
    #[test]
    fn a_clip_missing_behind_the_daemons_back_is_evicted_by_a_failed_delete() {
        let (daemon, dir) = fixture("ghost");
        let id = "20260726_100000";
        std::fs::write(trix_core::library::thumb_path(&dir, id), b"jpeg").unwrap();
        std::fs::remove_file(trix_core::library::mp4_path(&dir, id)).unwrap();

        assert_eq!(daemon.list(0, 50).total, 3, "the cache does not know yet — by design");

        let err = daemon.delete(id).unwrap_err().to_string();
        assert!(err.contains(id), "the error should name the clip: {err}");
        assert_eq!(
            daemon.list(0, 50).total,
            2,
            "a failed delete must still evict the ghost row from the cache"
        );

        // The companions must go with it. Once the row is evicted no client can
        // name this id again — `list` reads the cache and `scan` skips a
        // sidecar with no `.mp4` — so anything left here is litter the daemon
        // could never be asked to clean up.
        assert!(
            !trix_core::library::sidecar_path(&dir, id).exists(),
            "the orphaned sidecar must not be left behind unreachable"
        );
        assert!(
            !trix_core::library::thumb_path(&dir, id).exists(),
            "the orphaned thumbnail must not be left behind unreachable"
        );

        // A second delete of the same id is still a clean error, not a panic
        // and not a different message because the entry is now gone from the
        // cache too.
        let err_again = daemon.delete(id).unwrap_err().to_string();
        assert!(err_again.contains(id), "a repeat delete is still a clean error: {err_again}");
        assert_eq!(daemon.list(0, 50).total, 2, "nothing further changes");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Favoriting is the same write path as renaming, so what it needs its own
    /// coverage for is that it edits *only* the flag — a shared helper that
    /// clobbered the title would otherwise pass every test above.
    #[test]
    fn favoriting_sets_the_flag_on_disk_without_disturbing_the_title() {
        let (daemon, dir) = fixture("favorite");
        let id = "20260726_120000";

        daemon.rename(id, "Clutch").unwrap();
        daemon.set_favorite(id, true).unwrap();

        let on_disk =
            trix_core::library::read_sidecar(&trix_core::library::sidecar_path(&dir, id)).unwrap();
        assert!(on_disk.favorite);
        assert_eq!(on_disk.title, "Clutch", "favoriting must not undo a rename");

        daemon.set_favorite(id, false).unwrap();
        let page = daemon.list(0, 50);
        let entry = page.clips.iter().find(|c| c.id == id).unwrap();
        assert!(!entry.favorite, "un-favoriting is the same path in reverse");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Disarming a daemon that was never armed is a no-op, not an error — a
    /// UI that reconnects and syncs its toggle must not see a spurious failure.
    #[test]
    fn disarm_when_idle_is_not_an_error() {
        let daemon = idle("disarm", Config::default());
        assert!(daemon.disarm().is_ok());
        assert!(!daemon.status().armed);
    }

    /// The status shape the UI binds to. Arming needs real hardware, so this
    /// covers only the disarmed branch — the armed branch is covered by the
    /// end-to-end gate in Task 8.
    #[test]
    fn idle_status_reports_config_not_engine_state() {
        let config = Config { replay_seconds: 30, monitor_index: 1, ..Config::default() };
        let daemon = idle("status", config);
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
        // Not routed through `idle`: this test asserts on `clip_dir` reaching the
        // wire verbatim, so it has to choose the literal itself. `new_at(.., None)`
        // still keeps it off the real config file, and `D:\Clips` is nobody's real
        // clip directory -- a missing one scans as an empty library.
        let daemon =
            Daemon::new_at(Config { clip_dir: r"D:\Clips".into(), ..Config::default() }, None);
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
        assert_eq!(object.get("version").and_then(|v| v.as_str()), Some(env!("CARGO_PKG_VERSION")));
    }

    /// Nothing is armed, so there are no counters to report and the stats
    /// thread has nothing to broadcast. `None` rather than a payload of zeroes:
    /// a live meter reading "0 frames, 0 dropped" about a daemon that is not
    /// capturing is a lie a UI would render as a stalled encoder.
    #[test]
    fn an_idle_daemon_has_no_stats_payload() {
        assert!(idle("stats-idle", Config::default()).stats_json().is_none());
    }

    /// `stats_seconds = 0` disables the periodic log line, not a subscriber's
    /// events — a client that asked to be told is told once a second.
    #[test]
    fn a_zero_stats_interval_still_ticks_for_a_subscriber() {
        let daemon = idle("stats-zero", Config { stats_seconds: 0, ..Config::default() });
        assert_eq!(daemon.stats_interval(), Duration::from_secs(1));

        let daemon = idle("stats-five", Config { stats_seconds: 5, ..Config::default() });
        assert_eq!(daemon.stats_interval(), Duration::from_secs(5), "an explicit interval wins");
    }

    /// The guard that makes [`NUMERIC_BOUNDS`] a *rule* rather than a list
    /// somebody once wrote. Every numeric key `Config` serializes has to have a
    /// bound, so a config key added later cannot reach `config.set` unchecked —
    /// the same way `config.get`'s key set is derived from `Config` rather than
    /// hand-written. It fails on the day the key is added, not on the day a
    /// user sets it to four billion.
    ///
    /// Its reach is exactly "numeric in a serialized `Config::default()`",
    /// which is every field `Config` has today (six `u32`s) but is not the
    /// same claim as "every future field". An `Option<u32>` defaulting to
    /// `None` serializes as `null`, so this loop would skip it and the key
    /// would reach `config.set` unbounded; a signed or floating key would be
    /// forced into the table here, but `check_ranges` reads values through
    /// `as_u64`, which is `None` for a negative, so the bound would never
    /// fire. Both want a change to `check_ranges`, not just a row in the
    /// table — noted here so a later reader does not take this test for more
    /// cover than it gives.
    #[test]
    fn every_numeric_config_key_is_range_checked() {
        let defaults = config_object(&Config::default()).expect("the config serializes");
        for (key, value) in &defaults {
            if !value.is_number() {
                continue;
            }
            assert!(
                NUMERIC_BOUNDS.iter().any(|(name, _, _)| name == key),
                "{key:?} is a numeric config key with no entry in NUMERIC_BOUNDS — config.set \
                 would take any value serde can coerce, including one that costs the machine its \
                 memory"
            );
        }

        // And nothing in the table names a key that no longer exists, which
        // would be a bound quietly guarding nothing.
        for (key, _, _) in NUMERIC_BOUNDS {
            assert!(
                defaults.contains_key(key),
                "NUMERIC_BOUNDS names {key:?}, which Config has not"
            );
        }
    }

    /// The refusal itself, at the state layer: it names the key, the value and
    /// the range, and it is an error rather than a panic. `dispatch.rs` covers
    /// the wire behaviour and the "wrote nothing" half.
    #[test]
    fn an_out_of_range_value_is_refused_with_the_range_in_the_message() {
        let daemon = idle("range", Config::default());
        let mut values = Map::new();
        values.insert("replay_seconds".to_string(), Value::from(u32::MAX));

        let error = match daemon.set_config(&values) {
            Ok(_) => panic!("a 136-year replay ring is not a setting"),
            Err(e) => format!("{e:#}"),
        };
        assert!(error.contains("replay_seconds"), "the error names the key: {error}");
        assert!(error.contains("4294967295"), "the error quotes the value: {error}");
        assert!(error.contains("600"), "the error states the accepted range: {error}");
    }

    /// Out-of-range is checked; wrong-*type* is still reported as the type
    /// error it is, because "must be 1 to 480" is a poor description of
    /// `"sixty"`. The two messages must not swap places.
    #[test]
    fn a_wrong_type_value_still_gets_the_type_error_not_a_range_error() {
        let daemon = idle("wrong-type", Config::default());
        let mut values = Map::new();
        values.insert("fps".to_string(), Value::from("sixty"));

        let error = match daemon.set_config(&values) {
            Ok(_) => panic!("fps is a number"),
            Err(e) => format!("{e:#}"),
        };
        assert!(error.contains("fps"), "the error names the key: {error}");
        assert!(
            error.contains("does not accept"),
            "a string fps is a type problem, not a range problem: {error}"
        );
    }

    /// `clip` has to name the missing step rather than reporting some internal
    /// "no engine" condition — this string is what a user sees.
    #[test]
    fn clipping_while_idle_names_the_missing_step() {
        let daemon = idle("clip-idle", Config::default());
        let error = match daemon.clip() {
            Ok(_) => panic!("an idle daemon has no ring to clip from"),
            Err(e) => format!("{e:#}"),
        };
        assert_eq!(error, "not armed — send arm first");
    }

    /// A clip that reaches the library must reach the clients too.
    ///
    /// This is the regression that shipped: `clip_saved` was broadcast by
    /// `dispatch::clip`, the socket command's handler, so a clip taken with
    /// the **hotkey** — which calls `Daemon::clip` directly from
    /// `window::handle_action` and never touches `dispatch` — was written to
    /// disk and announced to nobody. The desktop app's grid only showed it
    /// after a reload, while spec §10's stage-4 gate opens by requiring the
    /// opposite: "clip appears in the grid within a second of the hotkey".
    ///
    /// It survived three plans because the only way in was `Daemon::clip`,
    /// which needs an armed engine and a real capture ring and so is
    /// unreachable from any test. Asserting on `record_saved_clip` is what
    /// makes the guarantee testable at all.
    #[test]
    fn a_recorded_clip_is_announced_to_every_client() {
        use std::sync::mpsc::sync_channel;

        let daemon = idle("clip-saved-broadcast", Config::default());
        let (tx, rx) = sync_channel(crate::clients::OUTBOUND_QUEUE_DEPTH);
        daemon.clients.register(tx);

        daemon.record_saved_clip(&meta("20260803_120000"));

        // The library half — the part that already worked.
        assert_eq!(
            daemon.lock_library().first().map(|c| c.id.as_str()),
            Some("20260803_120000"),
            "a saved clip is prepended to the live library"
        );

        // The event half — the part that did not.
        let line = rx.try_recv().expect("a saved clip must broadcast clip_saved");
        let event: Event = serde_json::from_str(line.trim_end())
            .unwrap_or_else(|e| panic!("event line did not decode: {e}\nline: {line}"));
        assert_eq!(event.event, "clip_saved");
        assert_eq!(
            event.data.get("id").and_then(Value::as_str),
            Some("20260803_120000"),
            "the event carries the ClipMeta the grid prepends"
        );
    }

    /// The sound must not be able to break the announcement. This drives the same
    /// function the hotkey reaches, with the sound turned on, and asserts the
    /// event still arrives -- the ordering the comment in `record_saved_clip`
    /// promises is otherwise only a comment.
    ///
    /// `idle` supplies the scratch `clip_dir`, and `config_path` is `None`, so
    /// nothing here can reach the developer's real config or clip folder.
    #[test]
    fn a_clip_is_still_announced_when_the_sound_is_on() {
        use std::sync::mpsc::sync_channel;

        let daemon = idle("clip-sound-on", Config { clip_sound: true, ..Config::default() });
        let (tx, rx) = sync_channel(crate::clients::OUTBOUND_QUEUE_DEPTH);
        daemon.clients.register(tx);

        daemon.record_saved_clip(&meta("20260817_120000"));

        let line = rx.try_recv().expect("a saved clip must still broadcast clip_saved");
        assert!(line.contains("clip_saved"), "the sound must not displace the event: {line}");
    }

    /// One `config.set` call carrying a single key.
    fn one(key: &str, value: u64) -> Map<String, Value> {
        let mut values = Map::new();
        values.insert(key.into(), Value::from(value));
        values
    }

    #[test]
    fn a_level_within_the_range_is_accepted() {
        let (daemon, dir) = writable("accepted");
        let update = daemon.set_config(&one("mic_volume", 40)).expect("40 is in range");
        assert_eq!(update.accepted.get("mic_volume"), Some(&Value::from(40)));
        cleanup(&dir);
    }

    #[test]
    fn a_level_above_the_range_is_refused() {
        let (daemon, dir) = writable("refused");
        let error = daemon.set_config(&one("mic_volume", 101)).expect_err("101 is out of range");
        assert!(format!("{error}").contains("0 to 100"), "unexpected message: {error}");
        cleanup(&dir);
    }

    #[test]
    fn changing_a_level_within_the_range_needs_no_rearm() {
        // Re-arming destroys the replay ring. A volume slider that costs the
        // user their last fifteen seconds on every nudge is a broken slider.
        let (daemon, dir) = writable("no-rearm");
        let update = daemon.set_config(&one("mic_volume", 60)).expect("60 is in range");
        assert!(update.requires_rearm.is_empty(), "a mid-range nudge asked for a re-arm");
        cleanup(&dir);
    }

    #[test]
    fn muting_a_source_needs_a_rearm() {
        // 0 closes a real capture stream, which only happens at arm time.
        let (daemon, dir) = writable("mute");
        let update = daemon.set_config(&one("mic_volume", 0)).expect("0 is in range");
        assert_eq!(update.requires_rearm, vec!["mic_volume".to_string()]);
        cleanup(&dir);
    }

    #[test]
    fn unmuting_a_source_needs_a_rearm() {
        let (daemon, dir) = writable("unmute");
        daemon.set_config(&one("system_volume", 0)).expect("0 is in range");
        let update = daemon.set_config(&one("system_volume", 80)).expect("80 is in range");
        assert_eq!(update.requires_rearm, vec!["system_volume".to_string()]);
        cleanup(&dir);
    }

    #[test]
    fn an_accepted_level_reaches_the_shared_gains() {
        // Without this the slider would move, the file would save, and
        // nothing the capture session reads would change.
        let (daemon, dir) = writable("applied");
        daemon.set_config(&one("mic_volume", 25)).expect("25 is in range");
        assert_eq!(daemon.gains.mic_percent(), 25);
        cleanup(&dir);
    }

    #[test]
    fn a_refused_change_leaves_the_shared_gains_alone() {
        // config.set is all-or-nothing: a rejected request must not have
        // applied a level the caller was told did not land.
        let (daemon, dir) = writable("refused-gains");
        daemon.set_config(&one("mic_volume", 999)).expect_err("999 is out of range");
        assert_eq!(daemon.gains.mic_percent(), 100);
        cleanup(&dir);
    }

    #[test]
    fn setting_one_level_leaves_the_other_alone() {
        // `gains.set` takes both percentages at once, so it reads them out of
        // the committed config rather than the request. Reading them from the
        // request instead would look correct in every single-key test and
        // silently zero the untouched source the moment a user moved one
        // slider -- the failure would be a mic that went dead when you turned
        // the game down.
        let (daemon, dir) = writable("one-at-a-time");
        daemon.set_config(&one("mic_volume", 30)).expect("30 is in range");
        assert_eq!(daemon.gains.mic_percent(), 30);
        assert_eq!(daemon.gains.system_percent(), 100, "the untouched level keeps its value");

        daemon.set_config(&one("system_volume", 40)).expect("40 is in range");
        assert_eq!(daemon.gains.system_percent(), 40);
        assert_eq!(daemon.gains.mic_percent(), 30, "the level set a moment ago survives");
        cleanup(&dir);
    }

    /// The gate must refuse before anything is written, like every other gate in
    /// `set_config`. A user who picks a text file must end up with the sound they
    /// had, not with a config pointing at something that will never play.
    #[test]
    fn a_file_that_is_not_audio_is_refused_and_changes_nothing() {
        let (daemon, config_path, dir) = with_scratch_config("sound-refused");
        let bogus = dir.join("not-audio.mp3");
        std::fs::write(&bogus, b"this is text, not audio").unwrap();

        // A real write first, so the "unchanged" assertion below compares a
        // file that exists against itself rather than against absence.
        // Without this, the decode gate moving to *after* `updated.save_to`
        // would still leave the file simply absent both before and after --
        // the in-memory write happens later in the function regardless of
        // where the gate sits, and the cache write is never reached on this
        // refusal path either way, so only a real on-disk comparison catches
        // the gate moving. Mirrors
        // `config_set_refuses_an_out_of_range_value_for_every_bounded_key` in
        // dispatch.rs.
        daemon.set_config(&one("fps", 30)).expect("the seed write must land");
        let before = std::fs::read(&config_path).unwrap();

        let mut values = Map::new();
        values.insert("clip_sound_path".into(), Value::from(bogus.to_string_lossy().as_ref()));
        let refused = daemon.set_config(&values);

        assert!(refused.is_err(), "a text file must not be accepted as a sound");
        let message = format!("{:#}", refused.unwrap_err());
        assert!(message.contains("not-audio.mp3"), "the error must name the file: {message}");
        assert_eq!(daemon.lock_config().clip_sound_path, "", "the old value must still stand");
        assert!(
            !Config::sound_cache_path(&config_path).exists(),
            "a refused sound must not leave a cache behind"
        );
        assert_eq!(
            std::fs::read(&config_path).unwrap(),
            before,
            "a refused clip_sound_path must leave the file on disk byte-unchanged -- this is \
             what catches the decode gate moving to after the save"
        );

        cleanup(&dir);
    }

    /// Clearing the key is "Reset to default", and the cache has to go with it --
    /// otherwise the next play finds a stale file beside a config that says
    /// "built-in" and plays the sound the user just removed.
    #[test]
    fn clearing_the_path_deletes_the_cache() {
        let (daemon, config_path, dir) = with_scratch_config("sound-reset");
        let cache = Config::sound_cache_path(&config_path);
        std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
        std::fs::write(&cache, b"RIFF____WAVEstale").unwrap();

        let mut values = Map::new();
        values.insert("clip_sound_path".into(), Value::from(""));
        daemon.set_config(&values).expect("clearing the sound must be accepted");

        assert!(!cache.exists(), "Reset must remove the converted copy");

        cleanup(&dir);
    }

    /// A missing path is refused by name rather than by codec error, because that
    /// is the mistake people actually make.
    #[test]
    fn a_path_that_does_not_exist_is_refused() {
        let (daemon, _config_path, dir) = with_scratch_config("sound-missing");
        let mut values = Map::new();
        values.insert("clip_sound_path".into(), Value::from(r"Z:\nope\ghost.mp3"));

        let message = format!("{:#}", daemon.set_config(&values).unwrap_err());
        assert!(message.contains("ghost.mp3"), "the error must name the file: {message}");

        cleanup(&dir);
    }

    /// Finding 1's fix, proven against a real `write_sound_cache` failure
    /// rather than only reasoned about: even though the write fails, a stale
    /// cache from a *previous* sound must not survive it, because
    /// `repair_sound_cache`'s only guard is "does a cache file exist" -- one
    /// left behind here would be trusted as current forever, and the daemon
    /// would keep playing sound A while `config.get` reports sound B.
    ///
    /// The failure is injected by pre-creating `clip-sound.wav.tmp` as a
    /// directory, so `write_sound_cache`'s own write to that path fails
    /// without ever touching the final cache path -- the closest a test gets
    /// to "disk full" or "antivirus lock" without either, and it leaves the
    /// stale cache itself unlocked, so a passing test proves the delete
    /// actually ran rather than merely that both sides failed together.
    ///
    /// Needs Media Foundation to decode the fixture sound, so this is
    /// `#[ignore]`d like every other test in the codebase that exercises a
    /// real decode -- see `trix_core::sound::decode`'s
    /// `a_real_file_decodes_to_a_capped_wav`.
    #[test]
    #[ignore = "needs Media Foundation; run by hand with --ignored"]
    fn a_failed_cache_write_does_not_strand_a_stale_cache() {
        let (daemon, config_path, dir) = with_scratch_config("sound-write-fail");

        let sound = dir.join("chosen.wav");
        let pcm = vec![0u8; 4 * 44_100]; // one second of silence, real WAV bytes
        std::fs::write(&sound, trix_core::sound::wav_from_pcm(&pcm, 2, 44_100, 16)).unwrap();

        let cache = Config::sound_cache_path(&config_path);
        std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
        std::fs::write(&cache, b"RIFF____WAVEstale-audio-from-a-previous-sound").unwrap();
        // Forces the write inside `write_sound_cache` to fail without
        // touching `cache` at all, so the fix's best-effort delete of
        // `cache` is not fighting the same failure that broke the write.
        let temp = cache.with_extension("wav.tmp");
        std::fs::create_dir_all(&temp).unwrap();

        let mut values = Map::new();
        values.insert("clip_sound_path".into(), Value::from(sound.to_string_lossy().as_ref()));
        daemon.set_config(&values).expect("decode succeeds even though the cache write fails");

        assert!(
            !cache.exists(),
            "a failed cache write must not strand the previous sound's cache -- the next \
             startup has to see it missing and rebuild, not trust a file that names the \
             wrong sound"
        );

        cleanup(&dir);
    }
}
