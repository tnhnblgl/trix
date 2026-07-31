//! `trix replay` (Phase 5b) — the Medal-style replay buffer.
//!
//! Continuously encodes the screen into an in-RAM ring of *compressed* H.264
//! packets (GOP-aligned) plus a PCM audio ring on the same timeline. On the
//! clip hotkey (`clip_hotkey` in config.toml, default Alt+F10) the last
//! `replay_seconds` are muxed to `clip_<timestamp>.mp4` —
//! video in passthrough (no re-encode), audio AAC-encoded at flush time.
//! Only compressed video and a few seconds of PCM ever touch system RAM.

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, RecvTimeoutError, Sender, channel},
    },
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow};
use trix_proto::ClipMeta;
use windows_capture::{
    capture::{CaptureControl, Context, GraphicsCaptureApiHandler},
    frame::Frame,
    graphics_capture_api::InternalCaptureControl,
    monitor::Monitor,
    settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, SecondaryWindowSettings, Settings,
    },
};

use crate::{
    capture::audio::{
        AudioPacket, AudioTimeline, ENCODER_BLOCK_ALIGN, LoopbackCapture, SAMPLE_RATE,
        SILENCE_GRACE_100NS, dur_100ns_to_frames, frames_to_100ns,
    },
    config::Config,
    control,
    encode::{
        convert::VideoConverter,
        h264::{EncodedPacket, H264Encoder},
        mf::{ClipMuxer, RecorderSettings, create_device_manager},
    },
    engine::{EngineCommand, EngineStatus},
    library,
    stats::{LatencyHistogram, StatsReporter, mb},
};

/// Extra ring depth beyond the clip length so a clip can always start on the
/// keyframe *before* its nominal start (hardware GOPs are ~2.3 s here).
const KEYFRAME_MARGIN_100NS: i64 = 30_000_000; // 3 s

pub struct ReplayOptions {
    /// Testing hook: save a clip automatically N seconds after start.
    pub auto_clip_secs: Option<u64>,
    /// Testing hook: exit after N seconds instead of running forever.
    pub exit_after_secs: Option<u64>,
    /// Print the `clip saved:` console line. True for the CLI; false for the
    /// daemon, which reports clips as `clip_saved` events instead.
    pub print_clips: bool,
}

struct ReplayFlags {
    settings: RecorderSettings,
    replay_100ns: i64,
    audio_rx: Option<Receiver<AudioPacket>>,
    stats_seconds: u32,
}

struct AudioChunk {
    start_frame: u64,
    data: Vec<u8>,
}

/// Everything needed to write one clip, cloned out of the ring under the
/// handler lock so muxing never stalls the capture callback.
struct ClipSnapshot {
    settings: RecorderSettings,
    /// The encoder's negotiated H.264 output type (used, not sent, across
    /// the lock — snapshot and mux both happen on the control thread).
    video_type: windows::Win32::Media::MediaFoundation::IMFMediaType,
    video: Vec<EncodedPacket>,
    base_pts: i64,
    /// Continuous PCM starting at `base_pts + audio_offset_100ns`.
    audio_pcm: Vec<u8>,
    /// Non-zero when audio older than the ring's span bound was trimmed
    /// (long static stretch): the track starts late instead of carrying an
    /// unbounded run of spliced silence.
    audio_offset_100ns: i64,
}

struct ReplaySession {
    settings: RecorderSettings,
    converter: VideoConverter,
    encoder: H264Encoder,
    replay_100ns: i64,
    frame_duration_100ns: i64,

    video_ring: VecDeque<EncodedPacket>,
    ring_bytes: usize,
    audio_ring: VecDeque<AudioChunk>,

    audio_rx: Option<Receiver<AudioPacket>>,
    timeline: AudioTimeline,
    t0_qpc: Option<i64>,
    last_frame_qpc: i64,
    /// QPC time the next frame is due; arrivals more than half a frame early
    /// are skipped untouched, pacing convert+encode to the configured fps
    /// even when WGC delivers at a 144/165 Hz monitor's full refresh rate.
    next_encode_qpc: i64,

    frames: u64,
    frames_dropped: u64,
    /// Frames intentionally skipped by the fps pacer (not a problem signal —
    /// it just means the monitor refreshes faster than the target fps).
    frames_paced: u64,
    /// Last seen capture frame size — WGC keeps delivering after a display
    /// mode change at the new size; the blit scales it into the encoder's
    /// fixed resolution, and this only exists to log the transition once.
    input_size: (u32, u32),

    /// Wall time spent inside the capture callback per frame (convert +
    /// encode submit + drain) — the gameplay-impact number.
    frame_latency: LatencyHistogram,
    stats: StatsReporter,

    // Keeps GPU access for the encoder MFT alive.
    _manager: windows::Win32::Media::MediaFoundation::IMFDXGIDeviceManager,
}

// SAFETY: same contract as the encode-side types — the session is moved into
// the capture thread and used from one thread at a time (snapshots go through
// the crate's handler mutex); the D3D device is multithread-protected.
unsafe impl Send for ReplaySession {}

impl ReplaySession {
    fn pump_encoder(&mut self) -> Result<()> {
        let ring = &mut self.video_ring;
        let bytes = &mut self.ring_bytes;
        self.encoder.pump(&mut |packet| {
            *bytes += packet.data.len();
            ring.push_back(packet);
        })
    }

    /// Drains loopback PCM into the audio ring up to `target_100ns` and
    /// bounds the ring's span. Must also run when no frames arrive: WGC goes
    /// quiet on a static screen while loopback keeps producing (~190 KB/s),
    /// and an undrained channel grows without limit (found by the Phase 6
    /// soak: +110 MB working set over 12 idle minutes).
    fn pump_audio_to(&mut self, target_100ns: i64) {
        if let Some(rx) = &self.audio_rx {
            let ring = &mut self.audio_ring;
            self.timeline.pump(rx, target_100ns, &mut |start_frame, pcm| {
                ring.push_back(AudioChunk { start_frame, data: pcm.to_vec() });
            });
        }
        // evict() trims audio against the *video* front, which stops moving
        // the moment the screen goes static — bound the span directly too.
        if let Some(back) = self.audio_ring.back() {
            let newest_end = back.start_frame + (back.data.len() / ENCODER_BLOCK_ALIGN) as u64;
            let budget = dur_100ns_to_frames(self.replay_100ns + KEYFRAME_MARGIN_100NS);
            let horizon = newest_end.saturating_sub(budget);
            while let Some(front) = self.audio_ring.front() {
                let front_end = front.start_frame + (front.data.len() / ENCODER_BLOCK_ALIGN) as u64;
                if front_end <= horizon {
                    self.audio_ring.pop_front();
                } else {
                    break;
                }
            }
        }
    }

    /// Control-thread tick while the screen is static (no frame callbacks).
    fn idle_pump(&mut self) {
        if self.t0_qpc.is_none() {
            return; // timeline anchors to the first video frame
        }
        let now = unsafe { windows::Win32::Media::MediaFoundation::MFGetSystemTime() };
        self.pump_audio_to(now - SILENCE_GRACE_100NS);
    }

    /// Seconds of footage the ring currently holds.
    fn ring_span_secs(&self) -> f64 {
        match (self.video_ring.front(), self.video_ring.back()) {
            (Some(front), Some(back)) => (back.pts_100ns - front.pts_100ns) as f64 / 10_000_000.0,
            _ => 0.0,
        }
    }

    /// Control-thread tick: emit a periodic performance line when the stats
    /// interval elapses. Cheap enough to poll every loop iteration.
    fn report_if_due(&mut self) {
        if self.stats.due() {
            self.log_perf("perf");
        }
    }

    fn log_perf(&self, tag: &str) {
        let m = self.stats.memory();
        let audio_ring_bytes: usize = self.audio_ring.iter().map(|c| c.data.len()).sum();
        // The exact CPU-side allocation we own and that scales with config —
        // the number the <30 MB budget is really about (working set is
        // UMA-inflated by GPU surfaces).
        let cpu_ring_mb = mb((self.ring_bytes + audio_ring_bytes) as u64);
        tracing::info!(
            ws_mb = mb(m.working_set),
            gpu_dedicated_mb = mb(m.gpu_local),
            gpu_shared_mb = mb(m.gpu_shared),
            ws_minus_gpu_mb = mb(m.ws_minus_gpu()),
            cpu_ring_mb,
            frames = self.frames,
            dropped = self.frames_dropped,
            paced = self.frames_paced,
            ring_packets = self.video_ring.len(),
            latency = %self.frame_latency.summary(),
            "{tag}"
        );
    }

    fn evict(&mut self) {
        let budget = self.replay_100ns + KEYFRAME_MARGIN_100NS;
        loop {
            let (Some(front), Some(back)) = (self.video_ring.front(), self.video_ring.back())
            else {
                break;
            };
            if back.pts_100ns - front.pts_100ns <= budget {
                break;
            }
            // Pop one full GOP so the ring always starts on a keyframe.
            while let Some(packet) = self.video_ring.pop_front() {
                self.ring_bytes -= packet.data.len();
                if self.video_ring.front().is_none_or(|next| next.keyframe) {
                    break;
                }
            }
        }
        let video_front_pts = self.video_ring.front().map_or(0, |p| p.pts_100ns);
        while let Some(chunk) = self.audio_ring.front() {
            let end_frame = chunk.start_frame + (chunk.data.len() / ENCODER_BLOCK_ALIGN) as u64;
            if frames_to_100ns(end_frame) < video_front_pts {
                self.audio_ring.pop_front();
            } else {
                break;
            }
        }
    }

    /// Clones the last `replay_seconds` out of the rings, GOP-aligned.
    fn snapshot_clip(&mut self) -> Result<Option<ClipSnapshot>> {
        self.pump_encoder()?;

        // Drain audio up to this instant so the clip keeps its trailing
        // audio even when the screen has been static.
        if self.t0_qpc.is_some() {
            let now_100ns = unsafe { windows::Win32::Media::MediaFoundation::MFGetSystemTime() };
            self.pump_audio_to(now_100ns - SILENCE_GRACE_100NS);
        }

        let Some(back) = self.video_ring.back() else { return Ok(None) };
        let target_start = back.pts_100ns - self.replay_100ns;

        // Latest keyframe at or before the nominal start (ring front is
        // always a keyframe, so index 0 is a valid fallback).
        let mut base_index = 0;
        for (i, packet) in self.video_ring.iter().enumerate() {
            if packet.pts_100ns > target_start {
                break;
            }
            if packet.keyframe {
                base_index = i;
            }
        }
        let video: Vec<EncodedPacket> = self.video_ring.iter().skip(base_index).cloned().collect();
        if video.is_empty() || !video[0].keyframe {
            return Ok(None);
        }
        let base_pts = video[0].pts_100ns;

        let first_keyframe = video.iter().find(|p| p.keyframe).map(|p| p.data.as_slice());
        let video_type = self.encoder.mux_input_type(first_keyframe)?;

        // Assemble continuous PCM aligned to base_pts. Chunks are gapless and
        // consecutive by construction (AudioTimeline emits one stream).
        let mut audio_pcm = Vec::new();
        let mut audio_offset_100ns = 0i64;
        if self.settings.with_audio {
            let base_frame = dur_100ns_to_frames(base_pts);
            let mut first_chunk_frame = None;
            for chunk in &self.audio_ring {
                first_chunk_frame.get_or_insert(chunk.start_frame);
                audio_pcm.extend_from_slice(&chunk.data);
            }
            if let Some(start_frame) = first_chunk_frame {
                if start_frame < base_frame {
                    let skip = (base_frame - start_frame) as usize * ENCODER_BLOCK_ALIGN;
                    audio_pcm.drain(..skip.min(audio_pcm.len()));
                } else if start_frame > base_frame {
                    audio_offset_100ns = frames_to_100ns(start_frame - base_frame);
                }
            }
        }

        Ok(Some(ClipSnapshot {
            settings: self.settings,
            video_type,
            video,
            base_pts,
            audio_pcm,
            audio_offset_100ns,
        }))
    }
}

impl GraphicsCaptureApiHandler for ReplaySession {
    type Flags = ReplayFlags;
    type Error = anyhow::Error;

    fn new(ctx: Context<Self::Flags>) -> Result<Self> {
        let flags = ctx.flags;
        let manager = create_device_manager(&ctx.device)?;
        let converter = VideoConverter::new(
            &ctx.device,
            flags.settings.width,
            flags.settings.height,
            flags.settings.fps,
            4,
        )?;
        let encoder = H264Encoder::new(&ctx.device, &manager, &flags.settings)?;
        let stats = StatsReporter::new(&ctx.device, flags.stats_seconds);

        tracing::info!(
            width = flags.settings.width,
            height = flags.settings.height,
            fps = flags.settings.fps,
            bitrate_bps = flags.settings.bitrate_bps,
            replay_secs = flags.replay_100ns / 10_000_000,
            "replay ring ready"
        );

        Ok(Self {
            frame_duration_100ns: 10_000_000 / i64::from(flags.settings.fps.max(1)),
            settings: flags.settings,
            converter,
            encoder,
            replay_100ns: flags.replay_100ns,
            video_ring: VecDeque::new(),
            ring_bytes: 0,
            audio_ring: VecDeque::new(),
            audio_rx: flags.audio_rx,
            timeline: AudioTimeline::new(),
            t0_qpc: None,
            last_frame_qpc: 0,
            next_encode_qpc: 0,
            frames: 0,
            frames_dropped: 0,
            frames_paced: 0,
            input_size: (flags.settings.width, flags.settings.height),
            frame_latency: LatencyHistogram::new(),
            stats,
            _manager: manager,
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        _capture_control: InternalCaptureControl,
    ) -> Result<()> {
        let frame_qpc = match frame.timestamp() {
            Ok(t) => t.Duration,
            Err(e) => {
                tracing::warn!("frame without timestamp, skipped: {e}");
                return Ok(());
            }
        };
        // Pace to the configured fps: half-frame tolerance absorbs vsync
        // jitter at matching rates while skipping the surplus frames a
        // high-refresh monitor delivers — before any GPU work is issued.
        if frame_qpc + self.frame_duration_100ns / 2 < self.next_encode_qpc {
            self.frames_paced += 1;
            return Ok(());
        }

        let callback_start = Instant::now();
        let t0 = *self.t0_qpc.get_or_insert(frame_qpc);
        self.timeline.start(t0);

        if (frame.width(), frame.height()) != self.input_size {
            tracing::info!(
                from = ?self.input_size,
                to = ?(frame.width(), frame.height()),
                "capture input resized — scaling into fixed encoder resolution"
            );
            self.input_size = (frame.width(), frame.height());
        }

        self.pump_encoder()?;

        let in_flight = self.encoder.frames_in.saturating_sub(self.encoder.packets_out);
        if self.encoder.ready_for_input()
            && (in_flight as usize) < self.converter.pool_size().saturating_sub(1)
        {
            let nv12 = self.converter.convert(frame.as_raw_texture())?;
            self.encoder.encode(nv12, frame_qpc - t0, self.frame_duration_100ns)?;
            self.frames += 1;
            // Advance one nominal period; the clamp resnaps the schedule after
            // a delivery gap (static screen) instead of accepting a burst.
            self.next_encode_qpc = (self.next_encode_qpc + self.frame_duration_100ns)
                .max(frame_qpc + self.frame_duration_100ns / 2);
        } else {
            self.frames_dropped += 1;
        }
        self.last_frame_qpc = frame_qpc;

        self.pump_audio_to(frame_qpc - SILENCE_GRACE_100NS);

        self.evict();
        self.frame_latency.record(callback_start.elapsed());
        Ok(())
    }

    fn on_closed(&mut self) -> Result<()> {
        Ok(())
    }
}

fn write_clip(snapshot: &ClipSnapshot, path: &PathBuf) -> Result<()> {
    let muxer = ClipMuxer::new(path, &snapshot.settings, &snapshot.video_type)?;
    for packet in &snapshot.video {
        muxer.write_video_packet(
            packet.pts_100ns - snapshot.base_pts,
            packet.duration_100ns,
            packet.keyframe,
            &packet.data,
        )?;
    }
    // 1-second PCM buffers; the muxer's AAC encoder eats them at flush speed.
    let second = SAMPLE_RATE * ENCODER_BLOCK_ALIGN;
    for (i, chunk) in snapshot.audio_pcm.chunks(second).enumerate() {
        muxer.write_audio(
            snapshot.audio_offset_100ns + frames_to_100ns((i * SAMPLE_RATE) as u64),
            chunk,
        )?;
    }
    muxer.finish()
}

/// What one save produced: the metadata that goes on the wire and on disk,
/// plus the numbers only the console line cares about.
pub(crate) struct SavedClip {
    pub meta: ClipMeta,
    pub path: PathBuf,
    pub audio_secs: f64,
    pub packets: usize,
    pub mux_ms: u128,
}

/// Saves a clip and its sidecar. `None` means nothing is buffered yet — a
/// legitimate outcome moments after arming, not an error.
fn save_clip(
    capture: &CaptureControl<ReplaySession, anyhow::Error>,
    clip_dir: &Path,
    encoder_name: &str,
) -> Result<Option<SavedClip>> {
    let started = Instant::now();
    let snapshot = capture.callback().lock().snapshot_clip()?;
    let Some(snapshot) = snapshot else { return Ok(None) };

    let id = library::allocate_clip_id(clip_dir)?;
    let path = library::mp4_path(clip_dir, &id);
    write_clip(&snapshot, &path)?;

    let last = &snapshot.video[snapshot.video.len() - 1];
    let video_100ns = last.pts_100ns + last.duration_100ns - snapshot.base_pts;
    let audio_secs = snapshot.audio_pcm.len() as f64 / (SAMPLE_RATE * ENCODER_BLOCK_ALIGN) as f64;
    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

    let meta = ClipMeta {
        id: id.clone(),
        title: format!("clip_{id}"),
        created: library::now_rfc3339_local(),
        duration_ms: (video_100ns / 10_000).max(0) as u64,
        bytes,
        width: snapshot.settings.width,
        height: snapshot.settings.height,
        fps: snapshot.settings.fps,
        encoder: encoder_name.to_string(),
        has_audio: snapshot.settings.with_audio,
        favorite: false,
    };
    // A clip with no sidecar is still a clip — the next scan adopts it — so a
    // sidecar failure is logged, not propagated over the freshly written MP4.
    if let Err(e) = library::write_sidecar(clip_dir, &meta) {
        tracing::warn!(clip = %id, error = %format!("{e:#}"), "clip saved without a sidecar");
    }

    tracing::debug!(clip = %id, bytes, "clip written");

    Ok(Some(SavedClip {
        meta,
        path,
        audio_secs,
        packets: snapshot.video.len(),
        mux_ms: started.elapsed().as_millis(),
    }))
}

/// The console line `trix replay` has printed since Phase 5b. Kept in exactly
/// this shape — the verification workflow greps for it.
fn print_clip_line(saved: &SavedClip) {
    println!(
        "clip saved: {} ({:.1} s video / {:.1} s audio, {} packets, {:.1} MB, muxed in {} ms)",
        saved.path.display(),
        saved.meta.duration_ms as f64 / 1000.0,
        saved.audio_secs,
        saved.packets,
        saved.meta.bytes as f64 / (1024.0 * 1024.0),
        saved.mux_ms,
    );
}

/// Why a capture session ended.
enum SessionEnd {
    /// Ctrl+C or `--exit-after`: leave the process.
    Shutdown,
    /// The capture item closed or the pipeline errored (monitor unplug,
    /// display topology change, GPU driver reset): eligible for rebuild.
    Died,
}

pub fn run(config: &Config, options: ReplayOptions) -> Result<()> {
    if config.gpu_priority_low() {
        crate::capture::lower_gpu_priority();
    }
    let hotkey = control::Hotkey::parse(&config.clip_hotkey)
        .context("invalid clip_hotkey in config.toml")?;
    let hotkey_rx = control::start_hotkey(&hotkey)?;

    let (tx, rx) = channel();
    // The hotkey thread speaks `()`; the control loop speaks commands. One
    // forwarder bridges them. It discards each reply — the loop itself prints
    // the console line for the CLI.
    std::thread::Builder::new()
        .name("trix-hotkey-forward".into())
        .spawn(move || {
            while hotkey_rx.recv().is_ok() {
                let (reply, _discard) = channel();
                if tx.send(EngineCommand::Clip { reply }).is_err() {
                    return; // control loop is gone — session over
                }
            }
        })
        .context("failed to spawn the hotkey forwarder")?;

    let status = Arc::new(Mutex::new(EngineStatus::default()));
    let mut ready = None;
    let result = run_driven_inner(config, &rx, &status, &mut ready, Some(&hotkey), options);
    control::mark_finalized();
    result
}

/// Runs the replay engine driven by a command channel rather than a hotkey.
/// The daemon's engine thread body.
///
/// **The caller owns [`control::mark_finalized`], not this function.** That
/// flag is process-global and one-way: it tells a blocked console-close
/// handler that on-disk state is consistent and it may stop stalling. A
/// one-shot CLI run sets it as it exits, which is correct. An engine session
/// is not a process — calling it here would latch the flag on the first
/// disarm, and every later armed ring would lose its flush grace period on
/// logoff or console close. The daemon calls it when the daemon exits.
pub fn run_driven(
    config: &Config,
    commands: Receiver<EngineCommand>,
    status: Arc<Mutex<EngineStatus>>,
    ready: Option<Sender<Result<()>>>,
) -> Result<()> {
    if config.gpu_priority_low() {
        crate::capture::lower_gpu_priority();
    }
    let mut ready = ready;
    let options = ReplayOptions { auto_clip_secs: None, exit_after_secs: None, print_clips: false };
    run_driven_inner(config, &commands, &status, &mut ready, None, options)
}

/// The rebuild loop: one capture session at a time, restarted when the display
/// topology changes under it. Unchanged policy — only its input channel and
/// the status/readiness it threads through are new.
fn run_driven_inner(
    config: &Config,
    commands: &Receiver<EngineCommand>,
    status: &Arc<Mutex<EngineStatus>>,
    ready: &mut Option<Sender<Result<()>>>,
    hotkey: Option<&control::Hotkey>,
    options: ReplayOptions,
) -> Result<()> {
    let run_started = Instant::now();
    let mut auto_clip_fired = false;
    let mut ever_ran = false;
    let mut failures = 0u32;
    loop {
        let session_started = Instant::now();
        match run_session(
            config,
            &options,
            hotkey,
            commands,
            status,
            ready,
            run_started,
            &mut auto_clip_fired,
        ) {
            Ok(SessionEnd::Shutdown) => return Ok(()),
            Ok(SessionEnd::Died) => {
                ever_ran = true;
                // A session that held for a while earns its failure budget
                // back; one dying right after a rebuild burns it.
                if session_started.elapsed() >= Duration::from_secs(10) {
                    failures = 0;
                } else {
                    failures += 1;
                }
                // Gated on the same signal the banner at the bottom of
                // `start_session` uses. `run_driven` passes `print_clips:
                // false` precisely so the daemon stays silent on stdout, and
                // this line escaped it. Beyond the inconsistency, `println!`
                // panics when the write fails, and this is a `panic = "abort"`
                // build: a daemon launched without a valid stdout handle (a
                // tray process created with CREATE_NO_WINDOW and no
                // redirection) would abort here, holding a live replay ring,
                // the first time a display change reached this line. The CLI's
                // wording is unchanged.
                if options.print_clips {
                    println!(
                        "capture session lost (display change / device reset?) — \
                         rebuilding, replay ring restarts empty"
                    );
                } else {
                    tracing::warn!(
                        "capture session lost (display change / device reset?) — \
                         rebuilding, replay ring restarts empty"
                    );
                }
            }
            Err(e) if !ever_ran => return Err(e),
            Err(e) => {
                failures += 1;
                tracing::warn!("rebuild attempt failed: {e:#}");
                if failures >= 30 {
                    return Err(e.context("capture could not be rebuilt after repeated attempts"));
                }
            }
        }
        // Let the display settle before rebuilding; stay Ctrl+C-responsive
        // and drop clip requests queued while there was no buffer to clip
        // (their reply channels close, which `EngineHandle::clip` reports as
        // "capture is rebuilding"). A `Stop` still has to be honoured — the
        // caller that sent it is blocked waiting for this thread to end.
        let wait_until = Instant::now() + Duration::from_secs(2);
        while Instant::now() < wait_until {
            if control::shutdown_requested() {
                return Ok(());
            }
            while let Ok(command) = commands.try_recv() {
                if matches!(command, EngineCommand::Stop) {
                    return Ok(());
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// A live capture session and everything the control loop needs to drive it.
/// Split out of `run_session` so every way starting up can fail funnels
/// through one place, which is where readiness is reported.
struct LiveSession {
    capture: CaptureControl<ReplaySession, anyhow::Error>,
    encoder_name: String,
    audio_handle: Option<LoopbackCapture>,
    clip_dir: PathBuf,
    width: u32,
    height: u32,
}

/// Brings up one capture session — monitor, audio, encoder, ring. Byte for
/// byte the setup `run_session` has always done, including the banner.
fn start_session(config: &Config, hotkey: Option<&control::Hotkey>) -> Result<LiveSession> {
    let monitor = Monitor::from_index(config.monitor_index as usize + 1)
        .map_err(|e| anyhow!("monitor {} not available: {e}", config.monitor_index))?;
    let width = monitor.width().map_err(|e| anyhow!("monitor width: {e}"))?;
    let height = monitor.height().map_err(|e| anyhow!("monitor height: {e}"))?;
    let clip_dir = config.clip_dir_path();

    let (audio_handle, audio_rx) = match LoopbackCapture::start() {
        Ok((handle, rx)) => (Some(handle), Some(rx)),
        Err(e) => {
            tracing::warn!("audio capture unavailable, replay continues without: {e}");
            (None, None)
        }
    };

    let flags = ReplayFlags {
        settings: RecorderSettings {
            width,
            height,
            fps: config.fps,
            bitrate_bps: config.bitrate_bps(),
            max_bitrate_bps: config.max_bitrate_bps(),
            rate_control: config.rate_control(),
            with_audio: audio_rx.is_some(),
        },
        replay_100ns: i64::from(config.replay_seconds) * 10_000_000,
        audio_rx,
        stats_seconds: config.stats_seconds,
    };

    match hotkey {
        Some(hotkey) => println!(
            "replay buffer running: {}x{} at {} fps, {} kbps, last {} s kept — {} to clip, Ctrl+C to quit",
            width, height, config.fps, config.bitrate_kbps, config.replay_seconds, hotkey,
        ),
        None => tracing::info!(
            width,
            height,
            fps = config.fps,
            kbps = config.bitrate_kbps,
            replay_secs = config.replay_seconds,
            "replay buffer running"
        ),
    }

    let settings = Settings::new(
        monitor,
        CursorCaptureSettings::WithCursor,
        crate::capture::border_settings(),
        SecondaryWindowSettings::Default,
        crate::capture::min_update_interval(config.fps),
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        flags,
    );
    let capture = ReplaySession::start_free_threaded(settings)
        .map_err(|e| anyhow!("failed to start capture: {e}"))?;
    let encoder_name = capture.callback().lock().encoder.name().to_string();

    Ok(LiveSession { capture, encoder_name, audio_handle, clip_dir, width, height })
}

#[allow(clippy::too_many_arguments)]
fn run_session(
    config: &Config,
    options: &ReplayOptions,
    hotkey: Option<&control::Hotkey>,
    commands: &Receiver<EngineCommand>,
    status: &Arc<Mutex<EngineStatus>>,
    ready: &mut Option<Sender<Result<()>>>,
    run_started: Instant,
    auto_clip_fired: &mut bool,
) -> Result<SessionEnd> {
    // Exactly one message goes out on the ready channel, from exactly these
    // two places: the setup error below, or the success just after it. Every
    // way `start_session` can fail returns through this arm.
    let live = match start_session(config, hotkey) {
        Ok(live) => live,
        Err(e) => {
            if let Some(tx) = ready.take() {
                // The waiting caller gets the real error; this thread's own
                // Result keeps its message.
                let message = format!("{e:#}");
                let _ = tx.send(Err(e));
                return Err(anyhow!(message));
            }
            return Err(e);
        }
    };
    let LiveSession { capture, encoder_name, audio_handle, clip_dir, width, height } = live;
    if let Some(tx) = ready.take() {
        let _ = tx.send(Ok(()));
    }

    let mut session_died = false;
    loop {
        match commands.recv_timeout(Duration::from_millis(250)) {
            Ok(EngineCommand::Clip { reply }) => {
                let result = save_clip(&capture, &clip_dir, &encoder_name);
                if options.print_clips {
                    match &result {
                        Ok(Some(saved)) => print_clip_line(saved),
                        Ok(None) => println!("nothing buffered yet — try again in a moment"),
                        Err(e) => {
                            tracing::error!("clip failed: {e:#}");
                            println!("clip failed: {e}");
                        }
                    }
                }
                // The hotkey forwarder drops its receiver; the daemon reads it.
                let _ = reply.send(result.map(|opt| opt.map(|saved| saved.meta)));
            }
            Ok(EngineCommand::Stop) => break,
            Err(RecvTimeoutError::Timeout) => {
                if control::shutdown_requested() {
                    // Same gating, and the same reason, as the rebuild line in
                    // `run_driven_inner`: on the daemon's engine thread stdout
                    // may not exist, and an unguarded `println!` there is an
                    // abort with a live ring in hand. `trix replay`'s output is
                    // byte-identical.
                    if options.print_clips {
                        println!("stop requested — closing replay buffer");
                    } else {
                        tracing::info!("stop requested — closing replay buffer");
                    }
                    break;
                }
                if capture.is_finished() {
                    session_died = true;
                    break;
                }
                {
                    let handler = capture.callback();
                    let mut session = handler.lock();
                    session.idle_pump();
                    session.report_if_due();
                    // The handler lock is already held, so publishing the
                    // status snapshot here costs nothing extra.
                    if let Ok(mut status) = status.lock() {
                        status.encoder = encoder_name.clone();
                        status.monitor_index = config.monitor_index;
                        status.width = width;
                        status.height = height;
                        status.fps = config.fps;
                        status.ring_seconds_total = config.replay_seconds;
                        status.ring_seconds_used = session.ring_span_secs();
                        status.frames = session.frames;
                        status.dropped = session.frames_dropped;
                        status.paced = session.frames_paced;
                    }
                }
                if let Some(secs) = options.auto_clip_secs {
                    if !*auto_clip_fired && run_started.elapsed() >= Duration::from_secs(secs) {
                        *auto_clip_fired = true;
                        // Same two outcomes the hotkey arm reports. An
                        // --auto-clip that fires before the ring has buffered
                        // anything must say so: a verification run that
                        // silently produces no clip looks like a pass.
                        match save_clip(&capture, &clip_dir, &encoder_name)? {
                            Some(saved) => print_clip_line(&saved),
                            None => println!("nothing buffered yet — try again in a moment"),
                        }
                    }
                }
                if let Some(secs) = options.exit_after_secs {
                    if run_started.elapsed() >= Duration::from_secs(secs) {
                        break;
                    }
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    if !session_died {
        let session = capture.callback();
        let session = session.lock();
        session.log_perf("replay session closing");
        session.timeline.log_diagnostics();
    }
    if session_died {
        if let Err(e) = capture.wait() {
            tracing::warn!("capture session failed: {e:#}");
        }
    } else if let Err(e) = capture.stop() {
        tracing::warn!("failed to stop capture: {e:#}");
    }
    if let Some(handle) = audio_handle {
        if let Err(e) = handle.stop() {
            tracing::warn!("audio capture thread: {e}");
        }
    }
    Ok(if session_died { SessionEnd::Died } else { SessionEnd::Shutdown })
}
