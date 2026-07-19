//! `trix replay` (Phase 5b) — the Medal-style replay buffer.
//!
//! Continuously encodes the screen into an in-RAM ring of *compressed* H.264
//! packets (GOP-aligned) plus a PCM audio ring on the same timeline. On
//! Alt+F10 the last `replay_seconds` are muxed to `clip_<timestamp>.mp4` —
//! video in passthrough (no re-encode), audio AAC-encoded at flush time.
//! Only compressed video and a few seconds of PCM ever touch system RAM.

use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::mpsc::{Receiver, RecvTimeoutError},
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
};

/// Extra ring depth beyond the clip length so a clip can always start on the
/// keyframe *before* its nominal start (hardware GOPs are ~2.3 s here).
const KEYFRAME_MARGIN_100NS: i64 = 30_000_000; // 3 s

pub struct ReplayOptions {
    /// Testing hook: save a clip automatically N seconds after start.
    pub auto_clip_secs: Option<u64>,
    /// Testing hook: exit after N seconds instead of running forever.
    pub exit_after_secs: Option<u64>,
}

struct ReplayFlags {
    settings: RecorderSettings,
    replay_100ns: i64,
    audio_rx: Option<Receiver<AudioPacket>>,
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
    /// Continuous PCM starting exactly at `base_pts`.
    audio_pcm: Vec<u8>,
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

    frames: u64,
    frames_dropped: u64,

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

        // Audio keeps flowing while the screen is static (no frames, so
        // on_frame_arrived stops pumping); drain everything captured up to
        // this instant or the clip loses its trailing audio.
        if let Some(rx) = &self.audio_rx {
            let now_100ns =
                unsafe { windows::Win32::Media::MediaFoundation::MFGetSystemTime() };
            let ring = &mut self.audio_ring;
            self.timeline.pump(rx, now_100ns - SILENCE_GRACE_100NS, &mut |start_frame, pcm| {
                ring.push_back(AudioChunk { start_frame, data: pcm.to_vec() });
            });
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
        let video: Vec<EncodedPacket> =
            self.video_ring.iter().skip(base_index).cloned().collect();
        if video.is_empty() || !video[0].keyframe {
            return Ok(None);
        }
        let base_pts = video[0].pts_100ns;

        let first_keyframe = video.iter().find(|p| p.keyframe).map(|p| p.data.as_slice());
        let video_type = self.encoder.mux_input_type(first_keyframe)?;

        // Assemble continuous PCM aligned to base_pts. Chunks are gapless and
        // consecutive by construction (AudioTimeline emits one stream).
        let mut audio_pcm = Vec::new();
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
                    let pad = (start_frame - base_frame) as usize * ENCODER_BLOCK_ALIGN;
                    audio_pcm.splice(..0, std::iter::repeat_n(0u8, pad));
                }
            }
        }

        Ok(Some(ClipSnapshot { settings: self.settings, video_type, video, base_pts, audio_pcm }))
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
            frames: 0,
            frames_dropped: 0,
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
        let t0 = *self.t0_qpc.get_or_insert(frame_qpc);
        self.timeline.start(t0);

        self.pump_encoder()?;

        let in_flight = self.encoder.frames_in.saturating_sub(self.encoder.packets_out);
        if self.encoder.ready_for_input()
            && (in_flight as usize) < self.converter.pool_size().saturating_sub(1)
        {
            let nv12 = self.converter.convert(frame.as_raw_texture())?;
            self.encoder.encode(nv12, frame_qpc - t0, self.frame_duration_100ns)?;
            self.frames += 1;
        } else {
            self.frames_dropped += 1;
        }
        self.last_frame_qpc = frame_qpc;

        if let Some(rx) = &self.audio_rx {
            let ring = &mut self.audio_ring;
            self.timeline.pump(rx, frame_qpc - SILENCE_GRACE_100NS, &mut |start_frame, pcm| {
                ring.push_back(AudioChunk { start_frame, data: pcm.to_vec() });
            });
        }

        self.evict();
        Ok(())
    }

    fn on_closed(&mut self) -> Result<()> {
        Ok(())
    }
}

fn clip_path() -> PathBuf {
    let now = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    PathBuf::from(format!(
        "clip_{:04}{:02}{:02}_{:02}{:02}{:02}.mp4",
        now.wYear, now.wMonth, now.wDay, now.wHour, now.wMinute, now.wSecond
    ))
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
        muxer.write_audio(frames_to_100ns((i * SAMPLE_RATE) as u64), chunk)?;
    }
    muxer.finish()
}

fn save_clip(capture: &CaptureControl<ReplaySession, anyhow::Error>) -> Result<()> {
    let started = Instant::now();
    let snapshot = capture.callback().lock().snapshot_clip()?;
    let Some(snapshot) = snapshot else {
        println!("nothing buffered yet — try again in a moment");
        return Ok(());
    };
    let path = clip_path();
    write_clip(&snapshot, &path)?;

    let last = &snapshot.video[snapshot.video.len() - 1];
    let video_secs =
        (last.pts_100ns + last.duration_100ns - snapshot.base_pts) as f64 / 10_000_000.0;
    let audio_secs =
        snapshot.audio_pcm.len() as f64 / (SAMPLE_RATE * ENCODER_BLOCK_ALIGN) as f64;
    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    println!(
        "clip saved: {} ({:.1} s video / {:.1} s audio, {} packets, {:.1} MB, muxed in {} ms)",
        path.display(),
        video_secs,
        audio_secs,
        snapshot.video.len(),
        bytes as f64 / (1024.0 * 1024.0),
        started.elapsed().as_millis(),
    );
    Ok(())
}

pub fn run(config: &Config, options: ReplayOptions) -> Result<()> {
    let monitor = Monitor::from_index(config.monitor_index as usize + 1)
        .map_err(|e| anyhow!("monitor {} not available: {e}", config.monitor_index))?;
    let width = monitor.width().map_err(|e| anyhow!("monitor width: {e}"))?;
    let height = monitor.height().map_err(|e| anyhow!("monitor height: {e}"))?;

    let (audio_handle, audio_rx) =
        match LoopbackCapture::start() {
            Ok((handle, rx)) => (Some(handle), Some(rx)),
            Err(e) => {
                tracing::warn!("audio capture unavailable, replay continues without: {e}");
                (None, None)
            }
        };

    let hotkey_rx = control::start_hotkey()?;

    let flags = ReplayFlags {
        settings: RecorderSettings {
            width,
            height,
            fps: config.fps,
            bitrate_bps: config.bitrate_kbps.saturating_mul(1000),
            with_audio: audio_rx.is_some(),
        },
        replay_100ns: i64::from(config.replay_seconds) * 10_000_000,
        audio_rx,
    };

    println!(
        "replay buffer running: {}x{} at {} fps, {} kbps, last {} s kept — Alt+F10 to clip, Ctrl+C to quit",
        width, height, config.fps, config.bitrate_kbps, config.replay_seconds,
    );

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
    let capture = ReplaySession::start_free_threaded(settings)
        .map_err(|e| anyhow!("failed to start capture: {e}"))?;

    let started = Instant::now();
    let mut auto_clip_fired = false;
    let mut session_died = false;
    loop {
        match hotkey_rx.recv_timeout(Duration::from_millis(250)) {
            Ok(()) => {
                if let Err(e) = save_clip(&capture) {
                    tracing::error!("clip failed: {e:#}");
                    println!("clip failed: {e}");
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if capture.is_finished() {
                    session_died = true;
                    break;
                }
                if let Some(secs) = options.auto_clip_secs {
                    if !auto_clip_fired && started.elapsed() >= Duration::from_secs(secs) {
                        auto_clip_fired = true;
                        save_clip(&capture)?;
                    }
                }
                if let Some(secs) = options.exit_after_secs {
                    if started.elapsed() >= Duration::from_secs(secs) {
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
        tracing::info!(
            frames = session.frames,
            dropped = session.frames_dropped,
            ring_bytes = session.ring_bytes,
            ring_packets = session.video_ring.len(),
            "replay session closing"
        );
        session.timeline.log_diagnostics();
    }
    let capture_result = if session_died {
        capture.wait().map_err(|e| anyhow!("capture session failed: {e}"))
    } else {
        capture.stop().map_err(|e| anyhow!("failed to stop capture: {e}"))
    };
    if let Some(handle) = audio_handle {
        if let Err(e) = handle.stop() {
            tracing::warn!("audio capture thread: {e}");
        }
    }
    capture_result
}
