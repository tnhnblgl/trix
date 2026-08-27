//! Driving the replay ring from something other than a terminal.
//!
//! [`crate::replay::run`] owns a loop that waits on the clip hotkey. The
//! daemon needs the same loop waiting on a control socket instead, so the
//! loop's input is a channel of [`EngineCommand`] and both callers feed it:
//! the CLI forwards hotkey presses, the daemon forwards `clip` requests.
//!
//! Everything downstream of that channel — the capture session, the fps
//! pacer, eviction, the rebuild policy — is the code that Phases 0–7
//! certified, unchanged.

use std::sync::{
    Arc, Mutex,
    mpsc::{Sender, channel},
};
use std::thread::JoinHandle;

use anyhow::{Context as _, Result, anyhow};
use trix_proto::{ClipMeta, ShotMeta};

use crate::{capture::audio::AudioGains, config::Config, replay};

/// What the control loop accepts. All three variants are terminal for the
/// caller: `Clip` and `Screenshot` always answer on their reply channel,
/// `Stop` always ends the loop.
pub enum EngineCommand {
    /// Save a clip now. `Ok(None)` means nothing is buffered yet.
    Clip { reply: Sender<Result<Option<ClipMeta>>> },
    /// Write a screenshot now.
    ///
    /// No `Option` in the reply, unlike `Clip`. A clip can legitimately have
    /// nothing to save because the ring is still filling; a screenshot needs
    /// one frame, so a session that cannot produce one within the wait is a
    /// frozen screen — an error, not an empty success.
    Screenshot { reply: Sender<Result<ShotMeta>> },
    /// Leave the rebuild loop and shut the capture session down.
    Stop,
}

/// Live view of the engine, refreshed on the control loop's 250 ms tick.
/// Read under a mutex that the capture callback never touches.
#[derive(Debug, Clone, Default)]
pub struct EngineStatus {
    pub encoder: String,
    pub monitor_index: u32,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Seconds of footage currently held in the ring.
    pub ring_seconds_used: f64,
    /// Configured `replay_seconds` — what the ring fills toward.
    pub ring_seconds_total: u32,
    pub frames: u64,
    pub dropped: u64,
    pub paced: u64,
}

/// An armed engine running on its own thread.
pub struct EngineHandle {
    tx: Sender<EngineCommand>,
    status: Arc<Mutex<EngineStatus>>,
    join: Option<JoinHandle<Result<()>>>,
}

impl EngineHandle {
    /// Starts capture and returns once the first session is live, so a caller
    /// answering an `arm` command can report "no hardware encoder available"
    /// synchronously instead of optimistically claiming success.
    pub fn spawn(config: Config, gains: Arc<AudioGains>) -> Result<Self> {
        let (tx, rx) = channel();
        // The readiness channel carries the first session's status, not just a
        // unit: it already fires at exactly the moment capture goes live, and
        // widening it is cheaper and less racy than a second handshake.
        let (ready_tx, ready_rx) = channel::<Result<EngineStatus>>();
        let status = Arc::new(Mutex::new(EngineStatus {
            monitor_index: config.monitor_index,
            fps: config.fps,
            ring_seconds_total: config.replay_seconds,
            ..EngineStatus::default()
        }));
        let thread_status = Arc::clone(&status);

        let join = std::thread::Builder::new()
            .name("trix-engine".into())
            .spawn(move || replay::run_driven(&config, gains, rx, thread_status, Some(ready_tx)))
            .context("failed to spawn the engine thread")?;

        // A failure before the first session starts arrives here; a failure
        // after it is handled by the rebuild loop and surfaces on stop().
        match ready_rx.recv() {
            Ok(Ok(first)) => {
                // Publish before returning. The control loop's own first tick
                // is up to 250 ms away and `arm` answers immediately, so
                // without this the caller reads the all-zero default: a null
                // encoder, a 0x0 geometry, and — during a rebuild — a stale
                // snapshot in which a dead ring looks fully buffered.
                *status.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = first;
                Ok(Self { tx, status, join: Some(join) })
            }
            Ok(Err(e)) => Err(e),
            // The channel closed without a message: the thread ended before it
            // could report. Its own Result is the better error.
            Err(_) => match join.join() {
                Ok(Err(e)) => Err(e),
                Ok(Ok(())) => Err(anyhow!("engine thread ended before capture started")),
                Err(_) => Err(anyhow!("engine thread panicked during startup")),
            },
        }
    }

    /// The latest snapshot the control loop published. A poisoned lock is
    /// recovered rather than propagated: a panic elsewhere must not take an
    /// armed ring down with it.
    pub fn status(&self) -> EngineStatus {
        self.status.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }

    /// Saves a clip, blocking until the mux completes. `Ok(None)` means
    /// nothing is buffered yet.
    pub fn clip(&self) -> Result<Option<ClipMeta>> {
        let (reply, answer) = channel();
        self.tx
            .send(EngineCommand::Clip { reply })
            .map_err(|_| anyhow!("engine thread is gone"))?;
        // The rebuild loop drains queued commands while it waits for the
        // display to settle, so an unanswered clip means "no ring right now",
        // not a dead engine.
        answer.recv().map_err(|_| anyhow!("capture is rebuilding — try again in a moment"))?
    }

    /// Writes a screenshot from the live session, answering with its metadata.
    pub fn screenshot(&self) -> Result<ShotMeta> {
        let (reply, answer) = channel();
        self.tx
            .send(EngineCommand::Screenshot { reply })
            .map_err(|_| anyhow!("engine thread is gone"))?;
        answer.recv().map_err(|_| anyhow!("capture is rebuilding — try again in a moment"))?
    }

    /// Stops capture and joins the thread, surfacing whatever the rebuild loop
    /// finally returned.
    pub fn stop(mut self) -> Result<()> {
        let _ = self.tx.send(EngineCommand::Stop);
        match self.join.take() {
            Some(join) => join.join().map_err(|_| anyhow!("engine thread panicked"))?,
            None => Ok(()),
        }
    }
}

impl Drop for EngineHandle {
    /// A handle dropped without `stop()` must still stop capture — otherwise
    /// the encoder stays held and the next `arm` fails.
    fn drop(&mut self) {
        if let Some(join) = self.join.take() {
            let _ = self.tx.send(EngineCommand::Stop);
            let _ = join.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The daemon's `status` response is built from these fields. Nothing else
    /// in the suite would notice one being dropped from the struct, and the
    /// daemon crate would only fail to compile — after this crate is green.
    #[test]
    fn status_defaults_are_the_disarmed_shape() {
        let status = EngineStatus::default();
        assert!(status.encoder.is_empty());
        assert_eq!(status.ring_seconds_used, 0.0);
        assert_eq!(status.ring_seconds_total, 0);
        assert_eq!((status.frames, status.dropped, status.paced), (0, 0, 0));
    }

    /// `arm` must answer with the encoder it got, not `null`.
    ///
    /// `spawn` used to return as soon as the first session was live but before
    /// the control loop's first 250 ms tick had published anything, so a UI had
    /// to poll `status` for up to 3 s to learn which encoder it was using — and
    /// during a display-change rebuild that poll returned the *stale* snapshot,
    /// making a dead ring read as fully buffered.
    ///
    /// Needs a real encoder, so it is ignored by default and run explicitly;
    /// the stage-3 gate runs it too.
    #[test]
    #[ignore = "needs a real encoder; run with --ignored"]
    fn spawn_returns_a_status_that_already_names_the_encoder() {
        let engine = EngineHandle::spawn(Config::default(), AudioGains::new(100, 100))
            .expect("arm on real hardware");
        let status = engine.status();
        assert!(!status.encoder.is_empty(), "arm must not answer with a null encoder");
        assert!(
            status.width > 0 && status.height > 0,
            "geometry must be live too, got {}x{}",
            status.width,
            status.height
        );
        assert_eq!(status.fps, Config::default().fps, "the configured fps must be published");
        engine.stop().expect("clean stop");
    }

    /// `arm` has to be able to answer "that monitor does not exist" instead of
    /// claiming success and failing invisibly on the engine thread — so a
    /// first session that cannot start must come back as an `Err` from
    /// `spawn` itself, not as a hang or an optimistic handle. Monitor 99 is
    /// the cheapest startup failure: it fails before any capture, audio, or
    /// encoder resource is touched, so this stays a hardware-free test.
    #[test]
    fn spawn_reports_a_first_session_that_cannot_start() {
        let config = Config {
            monitor_index: 99,
            // Skip the process-wide GPU scheduling call; irrelevant here.
            gpu_priority: "normal".into(),
            ..Config::default()
        };
        let error = match EngineHandle::spawn(config, AudioGains::new(100, 100)) {
            Ok(_) => panic!("monitor 99 must not produce a live engine"),
            Err(e) => format!("{e:#}"),
        };
        assert!(
            error.contains("monitor 99 not available"),
            "the caller needs the real reason, got: {error}"
        );
    }
}
