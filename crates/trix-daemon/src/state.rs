//! Everything the daemon owns, and the rules for changing it.
//!
//! Deliberately transport-free: nothing here knows about pipes, requests, or
//! events. [`crate::dispatch`] turns these methods into responses and decides
//! what to broadcast — which is also what keeps the lock ordering below
//! trivially true.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use anyhow::{Context as _, Result, bail};
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

impl Daemon {
    pub fn new(config: Config) -> Self {
        // One scan, at startup. A failure here is not fatal: an unreadable clip
        // directory must not stop the daemon from arming and capturing.
        let library = match scan_and_log(&config.clip_dir_path()) {
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

    // --- The clip library ---------------------------------------------------
    //
    // The cache in `self.library` *is* the library as far as the socket is
    // concerned (spec §5.2): `list` never touches the disk, and every mutation
    // writes the disk first and then updates the cache, so a failed write never
    // leaves a client believing something happened that did not.

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

    fn lock_config(&self) -> MutexGuard<'_, Config> {
        self.config.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_library(&self) -> MutexGuard<'_, Vec<ClipMeta>> {
        self.library.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

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
        let config = Config {
            clip_dir: dir.to_string_lossy().into_owned(),
            ..Config::default()
        };
        let daemon = Daemon::new(config);
        daemon.rescan_library().unwrap();
        (daemon, dir)
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

        let on_disk = trix_core::library::read_sidecar(
            &trix_core::library::sidecar_path(&dir, id),
        )
        .unwrap();
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
