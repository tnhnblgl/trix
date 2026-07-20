//! `trix record` (Phase 2 + 4, rebuilt on `encode::mf` in Phase 5): capture →
//! hardware encode → MP4 with audio, via our own `IMFSinkWriter` — capture
//! textures never leave VRAM, and the working set stays a fraction of the
//! crate encoder's ~199 MB MediaTranscoder pipeline.
//!
//! Audio sync contract: both streams share one timeline whose zero is the
//! first video frame's QPC instant. The audio side is fed as one *continuous*
//! PCM stream: real loopback packets are placed by their QPC stamps,
//! head/overlap excess is trimmed, and idle gaps (WASAPI loopback goes quiet
//! when nothing renders) are filled with synthesized silence. Audio
//! timestamps are derived from samples-emitted, so the stream is gapless by
//! construction.

use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use windows_capture::{
    capture::{CaptureControl, Context, GraphicsCaptureApiHandler},
    frame::Frame,
    graphics_capture_api::InternalCaptureControl,
    monitor::Monitor,
    settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    },
};

use crate::{
    capture::audio::{
        AudioPacket, AudioTimeline, LoopbackCapture, SAMPLE_RATE, SILENCE_GRACE_100NS,
        frames_to_100ns,
    },
    config::Config,
    encode::mf::{MfRecorder, RecorderSettings},
    stats::{LatencyHistogram, StatsReporter, mb},
};

pub struct RecordOptions {
    pub duration_secs: u64,
    pub output: PathBuf,
    pub no_audio: bool,
}

struct RecordFlags {
    output: PathBuf,
    width: u32,
    height: u32,
    fps: u32,
    bitrate_bps: u32,
    deadline: Duration,
    done: Sender<Result<Summary>>,
    audio_rx: Option<Receiver<AudioPacket>>,
    stats_seconds: u32,
}

struct Summary {
    frames: u64,
    frames_dropped: u64,
    audio_frames: u64,
    silence_frames: u64,
    elapsed: Duration,
}

struct RecordSession {
    recorder: Option<MfRecorder>,
    deadline: Duration,
    done: Sender<Result<Summary>>,
    started: Instant,
    frames: u64,

    audio_rx: Option<Receiver<AudioPacket>>,
    timeline: AudioTimeline,
    t0_qpc: Option<i64>,
    last_frame_qpc: i64,
    /// Last seen capture frame size — display mode changes mid-recording are
    /// scaled into the negotiated resolution by the blit; logged once.
    input_size: (u32, u32),

    /// Wall time spent inside the capture callback per frame (convert +
    /// WriteSample submit + audio drain) — the gameplay-impact number.
    frame_latency: LatencyHistogram,
    stats: StatsReporter,
}

impl RecordSession {
    /// Drains the audio timeline into the AAC stream up to `target_qpc`.
    fn pump_audio(&mut self, target_qpc: i64) {
        let (Some(rx), Some(recorder)) = (&self.audio_rx, &mut self.recorder) else { return };
        self.timeline.pump(rx, target_qpc, &mut |start_frame, pcm| {
            if let Err(e) = recorder.write_audio(frames_to_100ns(start_frame), pcm) {
                tracing::warn!("audio buffer rejected: {e}");
            }
        });
    }

    /// Control-thread tick while the screen is static: WGC stops calling
    /// `on_frame_arrived`, but loopback keeps producing — the channel must
    /// not buffer PCM unboundedly (same starvation the replay soak found).
    fn idle_pump(&mut self) {
        if self.t0_qpc.is_none() {
            return; // timeline anchors to the first video frame
        }
        let now = unsafe { windows::Win32::Media::MediaFoundation::MFGetSystemTime() };
        self.pump_audio(now - SILENCE_GRACE_100NS);
    }

    /// Control-thread tick: emit a periodic performance line when the stats
    /// interval elapses.
    fn report_if_due(&mut self) {
        if self.stats.due() {
            self.log_perf("perf");
        }
    }

    fn log_perf(&self, tag: &str) {
        let dropped = self.recorder.as_ref().map_or(0, |r| r.frames_dropped);
        let m = self.stats.memory();
        tracing::info!(
            ws_mb = mb(m.working_set),
            gpu_dedicated_mb = mb(m.gpu_local),
            gpu_shared_mb = mb(m.gpu_shared),
            ws_minus_gpu_mb = mb(m.ws_minus_gpu()),
            frames = self.frames,
            dropped,
            latency = %self.frame_latency.summary(),
            "{tag}"
        );
    }

    /// Closes the audio timeline at the last video frame, finalizes the MP4
    /// (flushes the moov atom), and reports the outcome.
    fn finish(&mut self) {
        if self.recorder.is_some() && self.timeline.started() {
            self.pump_audio(self.last_frame_qpc);
        }
        self.timeline.log_diagnostics();
        self.log_perf("recording finalizing");
        if let Some(recorder) = self.recorder.take() {
            let frames_dropped = recorder.frames_dropped;
            let result = recorder
                .finish()
                .map(|()| Summary {
                    frames: self.frames,
                    frames_dropped,
                    audio_frames: self.timeline.frames_emitted(),
                    silence_frames: self.timeline.silence_frames_emitted(),
                    elapsed: self.started.elapsed(),
                })
                .map_err(|e| anyhow!("failed to finalize MP4: {e}"));
            let _ = self.done.send(result);
        }
    }
}

impl GraphicsCaptureApiHandler for RecordSession {
    type Flags = RecordFlags;
    type Error = anyhow::Error;

    fn new(ctx: Context<Self::Flags>) -> Result<Self> {
        let flags = ctx.flags;
        let recorder = MfRecorder::new(
            &flags.output,
            &ctx.device,
            &RecorderSettings {
                width: flags.width,
                height: flags.height,
                fps: flags.fps,
                bitrate_bps: flags.bitrate_bps,
                with_audio: flags.audio_rx.is_some(),
            },
        )
        .map_err(|e| anyhow!("failed to create encoder: {e}"))?;
        let stats = StatsReporter::new(&ctx.device, flags.stats_seconds);

        tracing::info!(
            width = flags.width,
            height = flags.height,
            fps = flags.fps,
            bitrate_bps = flags.bitrate_bps,
            audio = flags.audio_rx.is_some(),
            output = %flags.output.display(),
            "sink writer ready"
        );

        Ok(Self {
            recorder: Some(recorder),
            deadline: flags.deadline,
            done: flags.done,
            started: Instant::now(),
            frames: 0,
            audio_rx: flags.audio_rx,
            timeline: AudioTimeline::new(),
            t0_qpc: None,
            last_frame_qpc: 0,
            input_size: (flags.width, flags.height),
            frame_latency: LatencyHistogram::new(),
            stats,
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        capture_control: InternalCaptureControl,
    ) -> Result<()> {
        if self.started.elapsed() >= self.deadline {
            self.finish();
            capture_control.stop();
            return Ok(());
        }

        let frame_qpc = match frame.timestamp() {
            Ok(t) => t.Duration,
            Err(e) => {
                tracing::warn!("frame without timestamp, skipped: {e}");
                return Ok(());
            }
        };

        if (frame.width(), frame.height()) != self.input_size {
            tracing::info!(
                from = ?self.input_size,
                to = ?(frame.width(), frame.height()),
                "capture input resized — scaling into fixed encoder resolution"
            );
            self.input_size = (frame.width(), frame.height());
        }

        let callback_start = Instant::now();
        if let Some(recorder) = &mut self.recorder {
            // Timeline zero = first frame's QPC (so the first sample lands at 0).
            let t0 = *self.t0_qpc.get_or_insert(frame_qpc);
            self.timeline.start(t0);
            match recorder.write_frame(frame.as_raw_texture(), frame_qpc - t0) {
                Ok(true) => self.frames += 1,
                Ok(false) => {} // pool exhausted — dropped, counted by the recorder
                Err(e) => {
                    let _ = self.done.send(Err(anyhow!("encoder rejected frame: {e}")));
                    capture_control.stop();
                    return Ok(());
                }
            }
            self.last_frame_qpc = frame_qpc;
            self.pump_audio(frame_qpc - SILENCE_GRACE_100NS);
            self.frame_latency.record(callback_start.elapsed());
        }
        Ok(())
    }

    fn on_closed(&mut self) -> Result<()> {
        self.finish();
        Ok(())
    }
}

pub fn run(config: &Config, options: RecordOptions) -> Result<()> {
    let monitor = Monitor::from_index(config.monitor_index as usize + 1)
        .map_err(|e| anyhow!("monitor {} not available: {e}", config.monitor_index))?;
    // Physical pixels — WGC frames come in native resolution, not DPI-scaled.
    let width = monitor.width().map_err(|e| anyhow!("monitor width: {e}"))?;
    let height = monitor.height().map_err(|e| anyhow!("monitor height: {e}"))?;

    let audio = if options.no_audio {
        None
    } else {
        Some(LoopbackCapture::start().map_err(|e| anyhow!("audio capture failed to start: {e}"))?)
    };
    let (audio_handle, audio_rx) = match audio {
        Some((handle, rx)) => (Some(handle), Some(rx)),
        None => (None, None),
    };

    println!(
        "recording {}x{} at {} fps, {} kbps, audio {} → {} ({} s)",
        width,
        height,
        config.fps,
        config.bitrate_kbps,
        if audio_rx.is_some() { "on" } else { "off" },
        options.output.display(),
        options.duration_secs,
    );

    let (done, outcome) = mpsc::channel();
    let flags = RecordFlags {
        output: options.output.clone(),
        width,
        height,
        fps: config.fps,
        bitrate_bps: config.bitrate_kbps.saturating_mul(1000),
        deadline: Duration::from_secs(options.duration_secs),
        done,
        audio_rx,
        stats_seconds: config.stats_seconds,
    };

    let settings = Settings::new(
        monitor,
        CursorCaptureSettings::WithCursor,
        DrawBorderSettings::WithoutBorder,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        flags,
    );

    let control = RecordSession::start_free_threaded(settings)
        .map_err(|e| anyhow!("failed to start capture: {e}"))?;

    // Normal path: the handler finishes itself when the deadline passes;
    // Ctrl+C finalizes early with what was captured so the MP4 stays valid
    // (an unflushed moov atom means an unplayable file). The grace timeout
    // only fires if the screen goes fully static (WGC stops delivering
    // frames), in which case we stop the session and finalize from here.
    let grace = Duration::from_secs(options.duration_secs) + Duration::from_secs(5);
    let wait_started = Instant::now();
    let stop_and_finish = |control: CaptureControl<RecordSession, anyhow::Error>| -> Result<()> {
        let handler = control.callback();
        control.stop().map_err(|e| anyhow!("failed to stop capture: {e}"))?;
        handler.lock().finish();
        Ok(())
    };
    let outcome = loop {
        match outcome.recv_timeout(Duration::from_millis(200)) {
            Ok(result) => {
                control.stop().map_err(|e| anyhow!("failed to stop capture: {e}"))?;
                break result;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                {
                    let handler = control.callback();
                    let mut session = handler.lock();
                    session.idle_pump();
                    session.report_if_due();
                }
                if crate::control::shutdown_requested() {
                    println!("stop requested — finalizing recording");
                    stop_and_finish(control)?;
                    break outcome
                        .try_recv()
                        .unwrap_or_else(|_| Err(anyhow!("finalize produced no summary")));
                }
                if wait_started.elapsed() >= grace {
                    stop_and_finish(control)?;
                    break Err(anyhow!(
                        "no frames arrived near the deadline (static screen?) — \
                         recording finalized with what was captured"
                    ));
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                break Err(anyhow!("capture session ended unexpectedly"));
            }
        }
    };
    crate::control::mark_finalized();

    if let Some(handle) = audio_handle {
        if let Err(e) = handle.stop() {
            tracing::warn!("audio capture thread: {e}");
        }
    }

    let summary = outcome?;
    let bytes = std::fs::metadata(&options.output).map(|m| m.len()).unwrap_or(0);
    let audio_secs = summary.audio_frames as f64 / SAMPLE_RATE as f64;
    let silence_secs = summary.silence_frames as f64 / SAMPLE_RATE as f64;
    println!(
        "done: {} frames ({} dropped) in {:.1} s ({:.1} fps effective), audio {:.1} s ({:.1} s synthesized silence), {:.1} MB → {}",
        summary.frames,
        summary.frames_dropped,
        summary.elapsed.as_secs_f64(),
        summary.frames as f64 / summary.elapsed.as_secs_f64().max(0.001),
        audio_secs,
        silence_secs,
        bytes as f64 / (1024.0 * 1024.0),
        options.output.display(),
    );
    Ok(())
}
