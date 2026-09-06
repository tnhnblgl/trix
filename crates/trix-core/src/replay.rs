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

use anyhow::{Context as _, Result, anyhow, bail};
use trix_proto::{ClipMeta, ShotMeta};
use windows::Win32::Graphics::Direct3D11::ID3D11Device;

use crate::{
    capture::audio::{
        AudioGains, AudioMixer, ENCODER_BLOCK_ALIGN, SAMPLE_RATE, SILENCE_GRACE_100NS,
        dur_100ns_to_frames, frames_to_100ns,
    },
    capture::source::{CaptureHandle, Flow, FrameSink, SourceFrame},
    capture::stage::{StagedFrame, stage_bgra},
    clipboard,
    config::Config,
    control,
    encode::{
        convert::VideoConverter,
        h264::{EncodedPacket, H264Encoder},
        mf::{ClipMuxer, RecorderSettings, create_device_manager},
    },
    engine::{EngineCommand, EngineStatus},
    library, shot,
    stats::{LatencyHistogram, StatsReporter, mb},
    thumb,
};

/// Extra ring depth beyond the clip length so a clip can always start on the
/// keyframe *before* its nominal start (hardware GOPs are ~2.3 s here).
const KEYFRAME_MARGIN_100NS: i64 = 30_000_000; // 3 s

/// How long `save_clip` waits for the capture thread to stage its thumbnail
/// frame. Fifteen frames at 60 fps — long enough to cover a mux that finished
/// unusually fast, short enough that a frozen screen does not visibly delay
/// the clip the user is waiting for.
const THUMB_STAGE_WAIT: Duration = Duration::from_millis(250);

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
    mixer: AudioMixer,
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

    mixer: AudioMixer,
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

    /// Set when a clip is requested; the next frame stages itself and clears
    /// it. Deliberately *not* a copy of every frame — a per-frame 9 MB GPU
    /// blit would cost exactly the gameplay impact this project exists to
    /// avoid. The cost is a thumbnail one frame (~16 ms) after the button
    /// press, which nobody can perceive.
    thumb_wanted: bool,
    /// The frame staged on the tick after a clip request, waiting for
    /// `save_clip` to encode it.
    thumb_staged: Option<StagedFrame>,

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
        let ring = &mut self.audio_ring;
        self.mixer.pump(target_100ns, &mut |start_frame, pcm| {
            ring.push_back(AudioChunk { start_frame, data: pcm.to_vec() });
        });
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

impl ReplaySession {
    /// Asks the next frame to stage itself. Called from the control thread.
    ///
    /// Any frame left over from a previous clip is dropped first. A staging
    /// frame that lands *after* its own clip has given up waiting would
    /// otherwise be served to the next clip, putting a preview from seconds
    /// ago — a different moment entirely — on it.
    fn request_thumbnail(&mut self) {
        self.thumb_staged = None;
        self.thumb_wanted = true;
    }

    /// Takes whatever the last request produced, if anything.
    fn take_staged_thumbnail(&mut self) -> Option<StagedFrame> {
        self.thumb_staged.take()
    }
}

impl FrameSink for ReplaySession {
    type Flags = ReplayFlags;

    fn new(device: &ID3D11Device, flags: ReplayFlags) -> Result<Self> {
        let manager = create_device_manager(device)?;
        let converter = VideoConverter::new(
            device,
            flags.settings.width,
            flags.settings.height,
            flags.settings.fps,
            4,
        )?;
        let encoder = H264Encoder::new(device, &manager, &flags.settings)?;
        let stats = StatsReporter::new(device, flags.stats_seconds);

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
            mixer: flags.mixer,
            t0_qpc: None,
            last_frame_qpc: 0,
            next_encode_qpc: 0,
            frames: 0,
            frames_dropped: 0,
            frames_paced: 0,
            input_size: (flags.settings.width, flags.settings.height),
            thumb_wanted: false,
            thumb_staged: None,
            frame_latency: LatencyHistogram::new(),
            stats,
            _manager: manager,
        })
    }

    fn on_frame(&mut self, frame: SourceFrame<'_>) -> Result<Flow> {
        let frame_qpc = frame.qpc_100ns;
        // Pace to the configured fps: half-frame tolerance absorbs vsync
        // jitter at matching rates while skipping the surplus frames a
        // high-refresh monitor delivers — before any GPU work is issued.
        if frame_qpc + self.frame_duration_100ns / 2 < self.next_encode_qpc {
            self.frames_paced += 1;
            return Ok(Flow::Continue);
        }

        let callback_start = Instant::now();
        let t0 = *self.t0_qpc.get_or_insert(frame_qpc);
        self.mixer.start(t0);

        if (frame.width, frame.height) != self.input_size {
            tracing::info!(
                from = ?self.input_size,
                to = ?(frame.width, frame.height),
                "capture input resized — scaling into fixed encoder resolution"
            );
            self.input_size = (frame.width, frame.height);
        }

        self.pump_encoder()?;

        let in_flight = self.encoder.frames_in.saturating_sub(self.encoder.packets_out);
        if self.encoder.ready_for_input()
            && (in_flight as usize) < self.converter.pool_size().saturating_sub(1)
        {
            let nv12 = self.converter.convert(frame.texture)?;
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

        // After the encode, so a thumbnail never delays the frame that the
        // clip is actually made of.
        if std::mem::take(&mut self.thumb_wanted) {
            match stage_bgra(frame.texture) {
                Ok(staged) => self.thumb_staged = Some(staged),
                // A thumbnail is a nicety; the clip is the product. This must
                // never fail a capture callback.
                Err(e) => tracing::warn!(error = %format!("{e:#}"), "could not stage a thumbnail"),
            }
        }

        self.pump_audio_to(frame_qpc - SILENCE_GRACE_100NS);

        self.evict();
        self.frame_latency.record(callback_start.elapsed());
        Ok(Flow::Continue)
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
/// Bounds the published ring occupancy to `0 ..= total`.
///
/// The ring evicts whole GOPs, because it cannot start a clip mid-GOP — so it
/// genuinely holds somewhere between `replay_seconds` and `replay_seconds` plus
/// one keyframe interval. Pinning the GOP to a second (see `encode/h264.rs`)
/// bounds that to under a second, but does not remove it: a 20 s ring really
/// does hand back 20.9 s.
///
/// `ring_seconds_used > ring_seconds_total` is nonetheless an invariant
/// violation for every consumer — a progress bar divides one by the other and
/// renders past 100%. The extra footage is a gift, so the fix is to report the
/// bound rather than to throw the footage away by evicting a GOP early, which
/// would make clips come out *short* of what the user configured.
///
/// `min` before `max` is deliberate and load-bearing for the non-finite cases:
/// Rust's `f64::min` returns the *other* operand when one is NaN, so a NaN span
/// reports as `total` rather than serializing to JSON `null` and silently
/// changing the wire type of this field.
fn clamped_ring_seconds(span: f64, total: u32) -> f64 {
    span.min(f64::from(total)).max(0.0)
}

fn save_clip(
    capture: &CaptureHandle<ReplaySession>,
    clip_dir: &Path,
    encoder_name: &str,
) -> Result<Option<SavedClip>> {
    let started = Instant::now();
    let callback = capture.sink();
    let snapshot = {
        let mut session = callback.lock();
        // Requested before the snapshot and read after the mux, so the capture
        // thread has the whole write to produce a frame. Ordering matters: ask
        // afterwards and the answer is always "not yet".
        session.request_thumbnail();
        session.snapshot_clip()?
    };
    let Some(snapshot) = snapshot else { return Ok(None) };

    // `allocate_clip_id` reserves the name by creating the .mp4 empty, so no
    // other saver -- or export -- can be handed the same wall-clock id. That
    // makes the file ours: `write_clip` overwrites it (`ClipMuxer::new` goes
    // through `MFCreateSinkWriterFromURL`, which creates *and* truncates), and
    // if the mux fails we take the reservation back down with us rather than
    // leaving a zero-byte .mp4 sitting on an id.
    let id = library::allocate_clip_id(clip_dir)?;
    let path = library::mp4_path(clip_dir, &id);
    if let Err(e) = write_clip(&snapshot, &path) {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            // Already gone is the outcome we wanted, not a failure.
            Err(remove) if remove.kind() == std::io::ErrorKind::NotFound => {}
            Err(remove) => tracing::warn!(
                clip = %id,
                error = %remove,
                "could not remove the reservation left by a failed clip write"
            ),
        }
        return Err(e);
    }

    // A thumbnail is never allowed to cost the clip: every failure here is a
    // warning over an MP4 that is already safely on disk. `library::scan`
    // treats a missing `.jpg` as a clip without a preview, not as a broken one.
    // The mux usually outlasts the capture thread's next frame, so the first
    // poll almost always succeeds. Almost: a fast mux on a quiet screen can
    // overtake it, and measured over ten back-to-back clips that cost two of
    // them their preview. Waiting a few frames is free when the frame is
    // already there and is the difference between a full library grid and one
    // with holes in it.
    let deadline = Instant::now() + THUMB_STAGE_WAIT;
    let staged = loop {
        if let Some(staged) = callback.lock().take_staged_thumbnail() {
            break Some(staged);
        }
        if Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };

    if let Some(staged) = staged {
        match thumb::encode_jpeg(&staged.bgra, staged.width, staged.height, staged.stride) {
            Ok(jpeg) => {
                let thumb_path = library::thumb_path(clip_dir, &id);
                match std::fs::write(&thumb_path, &jpeg) {
                    Ok(()) => tracing::debug!(clip = %id, bytes = jpeg.len(), "thumbnail written"),
                    Err(e) => {
                        tracing::warn!(path = %thumb_path.display(), %e, "thumbnail not written")
                    }
                }
            }
            Err(e) => tracing::warn!(error = %format!("{e:#}"), "thumbnail encode failed"),
        }
    } else {
        // Not an error: nothing was delivered in the whole window, which means
        // a genuinely frozen screen.
        tracing::debug!(clip = %id, "no frame was staged for a thumbnail");
    }

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

/// Stages a frame from the live session and writes it as a screenshot.
///
/// The frame arrives on the capture thread's *next* callback, so this asks and
/// then waits — the same two steps `save_clip` performs for a clip thumbnail,
/// on the same [`THUMB_STAGE_WAIT`] budget. Nothing is retained speculatively:
/// a screenshot costs one staged frame at the moment it is asked for, and
/// nothing at all the rest of the time.
fn take_screenshot(capture: &CaptureHandle<ReplaySession>, clip_dir: &Path) -> Result<ShotMeta> {
    let callback = capture.sink();
    callback.lock().request_thumbnail();

    let deadline = Instant::now() + THUMB_STAGE_WAIT;
    let staged = loop {
        if let Some(staged) = callback.lock().take_staged_thumbnail() {
            break Some(staged);
        }
        if Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let Some(staged) = staged else {
        // Unlike a clip thumbnail, this frame *is* the product, so an empty
        // window is an error the caller has to be told about rather than a
        // debug line over a file that saved fine anyway.
        bail!("no frame arrived within {THUMB_STAGE_WAIT:?} — is the screen frozen?");
    };

    let meta = write_screenshot(&shot::shots_dir(clip_dir), &staged)?;

    // The clipboard lives here rather than inside `write_screenshot`, and the
    // reason is the test suite: `write_screenshot` is the part a unit test
    // drives, and a test that reached the real clipboard would wipe whatever
    // the developer had copied every time they ran `cargo test`. That is the
    // same rule that keeps tests off the real config and clip library.
    //
    // A failed copy costs the paste, never the file — the screenshot is
    // already on disk by the time this runs.
    if let Err(e) = clipboard::copy_bgra(&staged.bgra, staged.width, staged.height, staged.stride) {
        tracing::warn!(shot = %meta.id, error = %format!("{e:#}"), "screenshot not copied to the clipboard");
    }
    Ok(meta)
}

/// Encodes an already-staged frame and writes both files.
///
/// Split from [`take_screenshot`] so everything below the capture session is
/// testable without hardware — and so the tested half touches no machine
/// state beyond the scratch directory it is given.
fn write_screenshot(shots: &Path, staged: &StagedFrame) -> Result<ShotMeta> {
    // Encoded *before* the id is reserved, so an encode that fails leaves no
    // zero-byte reservation behind for `scan` to skip forever.
    let full = thumb::encode_jpeg_sized(
        &staged.bgra,
        staged.width,
        staged.height,
        staged.stride,
        thumb::FULL_SIZE,
        thumb::SHOT_QUALITY,
    )
    .context("encoding the screenshot")?;

    let id = shot::allocate_shot_id(shots)?;
    let image = shot::image_path(shots, &id);
    if let Err(e) = std::fs::write(&image, &full) {
        // Take the reservation back down rather than leaving an empty file
        // sitting on a used id.
        let _ = std::fs::remove_file(&image);
        return Err(e).with_context(|| format!("writing {}", image.display()));
    }

    // Everything from here is a nicety over a screenshot already safely on
    // disk, and none of it may become an error the user sees.
    match thumb::encode_jpeg(&staged.bgra, staged.width, staged.height, staged.stride) {
        Ok(jpeg) => {
            let path = shot::thumb_path(shots, &id);
            if let Err(e) = std::fs::write(&path, &jpeg) {
                tracing::warn!(shot = %id, path = %path.display(), %e, "screenshot thumbnail not written");
            }
        }
        Err(e) => tracing::warn!(error = %format!("{e:#}"), "screenshot thumbnail encode failed"),
    }

    let bytes = std::fs::metadata(&image).map(|m| m.len()).unwrap_or(0);
    Ok(ShotMeta {
        created: library::created_from_id(&id),
        id,
        bytes,
        width: staged.width,
        height: staged.height,
    })
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

    let gains = AudioGains::new(config.system_volume, config.mic_volume);
    let status = Arc::new(Mutex::new(EngineStatus::default()));
    let mut ready = None;
    let result = run_driven_inner(config, &gains, &rx, &status, &mut ready, Some(&hotkey), options);
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
    gains: Arc<AudioGains>,
    commands: Receiver<EngineCommand>,
    status: Arc<Mutex<EngineStatus>>,
    ready: Option<Sender<Result<EngineStatus>>>,
) -> Result<()> {
    if config.gpu_priority_low() {
        crate::capture::lower_gpu_priority();
    }
    let mut ready = ready;
    let options = ReplayOptions { auto_clip_secs: None, exit_after_secs: None, print_clips: false };
    run_driven_inner(config, &gains, &commands, &status, &mut ready, None, options)
}

/// The rebuild loop: one capture session at a time, restarted when the display
/// topology changes under it. Unchanged policy — only its input channel and
/// the status/readiness it threads through are new.
fn run_driven_inner(
    config: &Config,
    gains: &Arc<AudioGains>,
    commands: &Receiver<EngineCommand>,
    status: &Arc<Mutex<EngineStatus>>,
    ready: &mut Option<Sender<Result<EngineStatus>>>,
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
            gains,
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
    capture: CaptureHandle<ReplaySession>,
    encoder_name: String,
    clip_dir: PathBuf,
    width: u32,
    height: u32,
}

/// Brings up one capture session — monitor, audio, encoder, ring. Byte for
/// byte the setup `run_session` has always done, including the banner.
fn start_session(
    config: &Config,
    gains: &Arc<AudioGains>,
    hotkey: Option<&control::Hotkey>,
) -> Result<LiveSession> {
    let monitor = crate::capture::source::monitor_info(config.monitor_index)?;
    let (width, height) = (monitor.width, monitor.height);
    let clip_dir = config.clip_dir_path();

    let mixer = AudioMixer::start_sources(Arc::clone(gains));

    let flags = ReplayFlags {
        settings: RecorderSettings {
            width,
            height,
            fps: config.fps,
            bitrate_bps: config.bitrate_bps(),
            max_bitrate_bps: config.max_bitrate_bps(),
            rate_control: config.rate_control(),
            with_audio: mixer.active(),
        },
        replay_100ns: i64::from(config.replay_seconds) * 10_000_000,
        mixer,
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

    let capture = crate::capture::source::start::<ReplaySession>(
        config.monitor_index,
        Some(config.fps),
        flags,
    )?;
    let encoder_name = capture.sink().lock().encoder.name().to_string();

    Ok(LiveSession { capture, encoder_name, clip_dir, width, height })
}

#[allow(clippy::too_many_arguments)]
fn run_session(
    config: &Config,
    gains: &Arc<AudioGains>,
    options: &ReplayOptions,
    hotkey: Option<&control::Hotkey>,
    commands: &Receiver<EngineCommand>,
    status: &Arc<Mutex<EngineStatus>>,
    ready: &mut Option<Sender<Result<EngineStatus>>>,
    run_started: Instant,
    auto_clip_fired: &mut bool,
) -> Result<SessionEnd> {
    // Exactly one message goes out on the ready channel, from exactly these
    // two places: the setup error below, or the success just after it. Every
    // way `start_session` can fail returns through this arm.
    let live = match start_session(config, gains, hotkey) {
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
    let LiveSession { capture, encoder_name, clip_dir, width, height } = live;
    if let Some(tx) = ready.take() {
        // Carries the session's facts, not just "ready": `EngineHandle::spawn`
        // returns the instant this lands, and the 250 ms tick below that would
        // otherwise fill them in has not run yet. Without this an `arm`
        // response reports a null encoder and a 0x0 frame.
        //
        // The counters are genuinely zero here — no frame has been encoded —
        // so this is the true state of a just-started session, not a placeholder.
        let _ = tx.send(Ok(EngineStatus {
            encoder: encoder_name.clone(),
            monitor_index: config.monitor_index,
            width,
            height,
            fps: config.fps,
            ring_seconds_used: 0.0,
            ring_seconds_total: config.replay_seconds,
            frames: 0,
            dropped: 0,
            paced: 0,
        }));
    }

    let mut session_died = false;
    // Set on a mid-loop failure that must still reach the caller. It cannot
    // propagate with `?` from inside the loop: that would jump straight out
    // of `run_session` and over the teardown below, leaking both audio
    // capture threads and the capture session (`CaptureHandle` has no `Drop`).
    // Stashing it and `break`-ing instead means every exit route — success,
    // `Stop`, disconnect, or this — runs the teardown exactly once, and the
    // error surfaces only after it has.
    let mut pending_error: Option<anyhow::Error> = None;
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
            Ok(EngineCommand::Screenshot { reply }) => {
                let result = take_screenshot(&capture, &clip_dir);
                if let Err(e) = &result {
                    tracing::warn!(error = %format!("{e:#}"), "screenshot failed");
                }
                // The daemon reads this; `trix replay` never sends the command.
                let _ = reply.send(result);
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
                    let handler = capture.sink();
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
                        status.ring_seconds_used =
                            clamped_ring_seconds(session.ring_span_secs(), config.replay_seconds);
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
                        match save_clip(&capture, &clip_dir, &encoder_name) {
                            Ok(Some(saved)) => print_clip_line(&saved),
                            Ok(None) => println!("nothing buffered yet — try again in a moment"),
                            Err(e) => {
                                pending_error = Some(e);
                                break;
                            }
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
        let session = capture.sink();
        let mut session = session.lock();
        session.log_perf("replay session closing");
        session.mixer.log_diagnostics();
        if let Err(e) = session.mixer.stop() {
            tracing::warn!(error = %format!("{e:#}"), "audio capture did not stop cleanly");
        }
    }
    if session_died {
        if let Err(e) = capture.wait() {
            tracing::warn!("capture session failed: {e:#}");
        }
    } else if let Err(e) = capture.stop() {
        tracing::warn!("failed to stop capture: {e:#}");
    }
    if let Some(e) = pending_error {
        return Err(e);
    }
    Ok(if session_died { SessionEnd::Died } else { SessionEnd::Shutdown })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ring_seconds_used` must never exceed `ring_seconds_total`: plan 4's
    /// progress bar divides one by the other, and a third-party UI binding to
    /// spec §4.3 is entitled to assume the ratio is at most 1.
    ///
    /// The ring really does hold more than its nominal length — it evicts whole
    /// GOPs, so a 20 s ring measured 20.9 s on real hardware. Reporting the
    /// bound is the fix; evicting early would make clips come out short.
    #[test]
    fn published_ring_occupancy_never_exceeds_the_configured_length() {
        assert_eq!(clamped_ring_seconds(20.9, 20), 20.0, "the measured overshoot must be capped");
        assert_eq!(clamped_ring_seconds(21.5, 20), 20.0);
        // Under the bound is reported honestly — a filling ring must still
        // animate rather than pinning to full.
        assert_eq!(clamped_ring_seconds(7.5, 20), 7.5);
        assert_eq!(clamped_ring_seconds(0.0, 20), 0.0);
    }

    /// A non-finite span must not reach the wire. `Value::from(f64)` maps NaN
    /// and infinities to JSON `null`, which would change this field's wire
    /// *type* — exactly the drift the frozen-shape test exists to prevent, and
    /// that test only covers `0.0`.
    #[test]
    fn a_non_finite_span_is_sanitised_rather_than_serialised_as_null() {
        assert_eq!(clamped_ring_seconds(f64::NAN, 20), 20.0, "NaN must not become null");
        assert_eq!(clamped_ring_seconds(f64::INFINITY, 20), 20.0);
        assert_eq!(clamped_ring_seconds(f64::NEG_INFINITY, 20), 0.0);
        for span in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(clamped_ring_seconds(span, 20).is_finite(), "{span} leaked a non-finite value");
        }
    }

    /// The screenshot writer is split so everything except the capture session is
    /// reachable from a test: staging a frame needs hardware, deciding what to
    /// write and where does not.
    #[test]
    fn a_staged_frame_becomes_two_files_and_reports_its_real_size() {
        let clips = std::env::temp_dir().join(format!("trix-replay-shot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&clips);
        let shots = crate::shot::shots_dir(&clips);
        std::fs::create_dir_all(&shots).expect("scratch shots dir");

        // A 4x2 top-down BGRA frame with driver padding -- the shape
        // `stage_thumbnail` actually produces.
        let stride = 20usize;
        let staged = StagedFrame { bgra: vec![255u8; stride * 2], width: 4, height: 2, stride };

        let meta = write_screenshot(&shots, &staged).expect("screenshot written");

        assert!(crate::shot::image_path(&shots, &meta.id).exists(), "the full-size image");
        assert!(crate::shot::thumb_path(&shots, &meta.id).exists(), "the grid thumbnail");
        assert_eq!((meta.width, meta.height), (4, 2));
        assert!(meta.bytes > 0, "bytes must come off the file, not be assumed");
        assert_eq!(meta.created, crate::library::created_from_id(&meta.id));
        let _ = std::fs::remove_dir_all(&clips);
    }
}
