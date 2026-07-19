//! Hand-rolled Media Foundation SinkWriter encoder (Phase 5).
//!
//! Replaces `windows_capture`'s MediaStreamSource + MediaTranscoder pipeline
//! (~199 MB working set) with a direct `IMFSinkWriter`: capture textures are
//! handed to the hardware H.264 MFT through a DXGI device manager without
//! ever leaving VRAM (the sink writer inserts the GPU video processor for the
//! BGRA→NV12 conversion), and PCM audio goes to the AAC encoder MFT with
//! explicit caller-controlled timestamps — the piece the crate encoder never
//! allowed.

use std::path::Path;
use std::sync::OnceLock;

use anyhow::{Context as _, Result, anyhow};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Multithread, ID3D11Texture2D};
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMFAttributes, IMFDXGIDeviceManager, IMFMediaType, IMFSinkWriter, IMFTransform,
    MF_API_VERSION, MF_MT_ALL_SAMPLES_INDEPENDENT, MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
    MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_BLOCK_ALIGNMENT, MF_MT_AUDIO_NUM_CHANNELS,
    MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE,
    MF_LOW_LATENCY, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
    MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS,
    MF_SDK_VERSION, MF_SINK_WRITER_D3D_MANAGER, MFSampleExtension_CleanPoint,
    MF_SINK_WRITER_DISABLE_THROTTLING, MF_SINK_WRITER_STATISTICS, MFAudioFormat_AAC,
    MFAudioFormat_PCM, MFCreateAttributes, MFCreateDXGIDeviceManager, MFCreateDXGISurfaceBuffer,
    MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample, MFCreateSinkWriterFromURL, MFMediaType_Audio,
    MFMediaType_Video, MFSTARTUP_NOSOCKET, MFStartup, MFT_ENUM_HARDWARE_URL_Attribute,
    MFT_FRIENDLY_NAME_Attribute, MFVideoFormat_H264, MFVideoFormat_NV12,
    MFVideoInterlace_Progressive,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::core::{GUID, HSTRING, Interface, PWSTR};

use crate::capture::audio::{CHANNELS, ENCODER_BLOCK_ALIGN, SAMPLE_RATE};
use crate::encode::convert::VideoConverter;

const MF_VERSION: u32 = ((MF_SDK_VERSION as u32) << 16) | MF_API_VERSION as u32;

/// Media Foundation startup is process-wide; do it exactly once. `MFShutdown`
/// is deliberately skipped — the OS reclaims everything at process exit.
pub fn ensure_mf_started() -> Result<()> {
    static STARTED: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    STARTED
        .get_or_init(|| unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET).map_err(|e| e.to_string())
        })
        .clone()
        .map_err(|e| anyhow!("MFStartup failed: {e}"))
}

#[derive(Clone, Copy)]
pub struct RecorderSettings {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_bps: u32,
    pub with_audio: bool,
}

/// Creates the DXGI device manager the MF pipeline uses to reach the GPU,
/// enabling D3D11 multithread protection on the way (the capture thread and
/// MF worker threads share this device).
pub(crate) fn create_device_manager(device: &ID3D11Device) -> Result<IMFDXGIDeviceManager> {
    ensure_mf_started()?;
    unsafe {
        let _ = device
            .cast::<ID3D11Multithread>()
            .context("ID3D11Multithread")?
            .SetMultithreadProtected(true);
        let mut reset_token = 0u32;
        let mut manager: Option<IMFDXGIDeviceManager> = None;
        MFCreateDXGIDeviceManager(&mut reset_token, &mut manager)
            .context("MFCreateDXGIDeviceManager")?;
        let manager = manager.unwrap();
        manager.ResetDevice(device, reset_token).context("ResetDevice")?;
        Ok(manager)
    }
}

/// Direct `IMFSinkWriter` MP4 recorder: BGRA D3D11 textures in, hardware
/// H.264 + AAC out. All timestamps are caller-supplied 100 ns units relative
/// to the start of the recording.
pub struct MfRecorder {
    writer: IMFSinkWriter,
    video_stream: u32,
    audio_stream: Option<u32>,
    /// BGRA→NV12 GPU blit; its NV12 pool doubles as the staging pool (the
    /// WGC frame texture is only valid during the capture callback, and the
    /// blit is the copy that outlives it).
    converter: VideoConverter,
    frame_duration_100ns: i64,
    video_samples_sent: u64,
    pub frames_dropped: u64,
    // Keeps the device manager (and thus GPU access for the MFTs) alive.
    _dxgi_manager: IMFDXGIDeviceManager,
}

// SAFETY: windows-rs leaves Win32 COM pointers !Send, but every object held
// here tolerates the cross-thread use this type actually performs: the sink
// writer serializes all its methods internally (MF readwrite object), the
// D3D11 device/context have multithread protection enabled, and the rest are
// free-threaded refcounted handles. The recorder is only ever *moved* to the
// capture thread and used from one thread at a time.
unsafe impl Send for MfRecorder {}

impl MfRecorder {
    pub fn new(output: &Path, device: &ID3D11Device, settings: &RecorderSettings) -> Result<Self> {
        ensure_mf_started()?;
        unsafe {
            let manager = create_device_manager(device)?;

            let mut attrs: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attrs, 4).context("MFCreateAttributes")?;
            let attrs = attrs.unwrap();
            attrs.SetUnknown(&MF_SINK_WRITER_D3D_MANAGER, &manager)?;
            attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
            // We do our own backpressure via the texture pool; the writer
            // must never block the capture callback.
            attrs.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;
            // Propagates to the encoder MFT: no B-frames, no lookahead —
            // right for a live recorder, and it shrinks the encoder's
            // internal surface pool (system RAM on UMA iGPUs).
            attrs.SetUINT32(&MF_LOW_LATENCY, 1)?;

            let writer =
                MFCreateSinkWriterFromURL(&HSTRING::from(output.as_os_str()), None, Some(&attrs))
                    .context("MFCreateSinkWriterFromURL")?;

            // -- video stream: H.264 out, NV12 in (we convert BGRA→NV12 on
            // the GPU ourselves, so the writer connects the encoder MFT
            // directly with no converter MFT and none of its sample pools) --
            let out_type = video_type(settings, &MFVideoFormat_H264, true)?;
            let video_stream = writer.AddStream(&out_type).context("AddStream(video)")?;
            let in_type = video_type(settings, &MFVideoFormat_NV12, false)?;
            writer
                .SetInputMediaType(video_stream, &in_type, None)
                .context("SetInputMediaType(NV12)")?;

            // -- audio stream: AAC out, PCM i16 in --
            let audio_stream = if settings.with_audio {
                let out_type = audio_type(&MFAudioFormat_AAC)?;
                // 192 kbps AAC, matching the Phase 4 recordings.
                out_type.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 24_000)?;
                let audio_stream = writer.AddStream(&out_type).context("AddStream(audio)")?;
                let in_type = audio_type(&MFAudioFormat_PCM)?;
                in_type.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, ENCODER_BLOCK_ALIGN as u32)?;
                in_type.SetUINT32(
                    &MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
                    (SAMPLE_RATE * ENCODER_BLOCK_ALIGN) as u32,
                )?;
                in_type.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
                writer
                    .SetInputMediaType(audio_stream, &in_type, None)
                    .context("SetInputMediaType(audio PCM)")?;
                Some(audio_stream)
            } else {
                None
            };

            writer.BeginWriting().context("BeginWriting")?;
            log_video_transform(&writer, video_stream);

            // 3 NV12 targets ≈ 50 ms of pipeline depth at 60 fps; measured
            // drain is fast enough that even 2 never dropped a frame.
            let converter = VideoConverter::new(
                device,
                settings.width,
                settings.height,
                settings.fps,
                3,
            )?;

            Ok(Self {
                writer,
                video_stream,
                audio_stream,
                converter,
                frame_duration_100ns: 10_000_000 / i64::from(settings.fps.max(1)),
                video_samples_sent: 0,
                frames_dropped: 0,
                _dxgi_manager: manager,
            })
        }
    }

    /// Queues one BGRA frame at `timestamp_100ns`. Returns `false` if the
    /// frame was dropped because the encoder pipeline still holds the whole
    /// staging pool (backpressure = drop, never block the capture thread).
    pub fn write_frame(&mut self, source: &ID3D11Texture2D, timestamp_100ns: i64) -> Result<bool> {
        if self.in_flight()? >= self.converter.pool_size() as u64 {
            self.frames_dropped += 1;
            return Ok(false);
        }
        let nv12 = self.converter.convert(source)?;
        unsafe {
            let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, nv12, 0, false)
                .context("MFCreateDXGISurfaceBuffer")?;
            let length = buffer.cast::<IMF2DBuffer>()?.GetContiguousLength()?;
            buffer.SetCurrentLength(length)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(timestamp_100ns)?;
            sample.SetSampleDuration(self.frame_duration_100ns)?;
            self.writer.WriteSample(self.video_stream, &sample).context("WriteSample(video)")?;
        }
        self.video_samples_sent += 1;
        Ok(true)
    }

    /// Writes interleaved i16 stereo PCM starting at `timestamp_100ns`.
    pub fn write_audio(&mut self, timestamp_100ns: i64, pcm: &[u8]) -> Result<()> {
        let Some(audio_stream) = self.audio_stream else { return Ok(()) };
        write_pcm_sample(&self.writer, audio_stream, timestamp_100ns, pcm)
    }

    /// Finalizes the MP4 (writes the moov atom). Must be called — an
    /// unfinalized file is corrupt.
    pub fn finish(self) -> Result<()> {
        unsafe { self.writer.Finalize().context("SinkWriter Finalize") }
    }

    /// Video samples queued to the writer but not yet muxed into the file.
    fn in_flight(&self) -> Result<u64> {
        let mut stats = MF_SINK_WRITER_STATISTICS {
            cb: size_of::<MF_SINK_WRITER_STATISTICS>() as u32,
            ..Default::default()
        };
        unsafe { self.writer.GetStatistics(self.video_stream, &mut stats)? };
        Ok(self.video_samples_sent.saturating_sub(stats.qwNumSamplesProcessed))
    }
}

pub(crate) fn video_type(
    settings: &RecorderSettings,
    subtype: &GUID,
    with_bitrate: bool,
) -> Result<IMFMediaType> {
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        t.SetUINT64(
            &MF_MT_FRAME_SIZE,
            (u64::from(settings.width) << 32) | u64::from(settings.height),
        )?;
        t.SetUINT64(&MF_MT_FRAME_RATE, (u64::from(settings.fps) << 32) | 1)?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        if with_bitrate {
            t.SetUINT32(&MF_MT_AVG_BITRATE, settings.bitrate_bps)?;
        }
        Ok(t)
    }
}

fn write_pcm_sample(
    writer: &IMFSinkWriter,
    stream: u32,
    timestamp_100ns: i64,
    pcm: &[u8],
) -> Result<()> {
    if pcm.is_empty() {
        return Ok(());
    }
    unsafe {
        let buffer = MFCreateMemoryBuffer(pcm.len() as u32)?;
        let mut data = std::ptr::null_mut();
        buffer.Lock(&mut data, None, None)?;
        std::ptr::copy_nonoverlapping(pcm.as_ptr(), data, pcm.len());
        buffer.Unlock()?;
        buffer.SetCurrentLength(pcm.len() as u32)?;
        let sample = MFCreateSample()?;
        sample.AddBuffer(&buffer)?;
        sample.SetSampleTime(timestamp_100ns)?;
        let frames = (pcm.len() / ENCODER_BLOCK_ALIGN) as i64;
        sample.SetSampleDuration(frames * 10_000_000 / SAMPLE_RATE as i64)?;
        writer.WriteSample(stream, &sample).context("WriteSample(audio)")?;
    }
    Ok(())
}

/// Muxes already-encoded H.264 packets (from the replay ring) plus a PCM
/// timeline into an MP4. Video is written in passthrough — no re-encode; the
/// only encoding work at clip time is AAC over a few seconds of PCM.
pub struct ClipMuxer {
    writer: IMFSinkWriter,
    video_stream: u32,
    audio_stream: Option<u32>,
}

impl ClipMuxer {
    pub fn new(
        output: &Path,
        settings: &RecorderSettings,
        video_input_type: &IMFMediaType,
    ) -> Result<Self> {
        ensure_mf_started()?;
        unsafe {
            // Throttling must be off: we write the whole video track before any
            // audio, and a throttled writer blocks WriteSample waiting for the
            // lagging stream to catch up.
            let mut attrs: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attrs, 1)?;
            let attrs = attrs.unwrap();
            attrs.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;
            let writer =
                MFCreateSinkWriterFromURL(&HSTRING::from(output.as_os_str()), None, Some(&attrs))
                    .context("MFCreateSinkWriterFromURL(clip)")?;

            // The encoder's own negotiated H.264 type (with sequence header)
            // declared as both stream and input type = guaranteed passthrough.
            let video_stream =
                writer.AddStream(video_input_type).context("AddStream(clip video)")?;
            writer
                .SetInputMediaType(video_stream, video_input_type, None)
                .context("SetInputMediaType(H264 passthrough)")?;

            let audio_stream = if settings.with_audio {
                let out_type = audio_type(&MFAudioFormat_AAC)?;
                out_type.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 24_000)?;
                let audio_stream = writer.AddStream(&out_type).context("AddStream(clip audio)")?;
                let in_type = audio_type(&MFAudioFormat_PCM)?;
                in_type.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, ENCODER_BLOCK_ALIGN as u32)?;
                in_type.SetUINT32(
                    &MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
                    (SAMPLE_RATE * ENCODER_BLOCK_ALIGN) as u32,
                )?;
                in_type.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
                writer
                    .SetInputMediaType(audio_stream, &in_type, None)
                    .context("SetInputMediaType(clip PCM)")?;
                Some(audio_stream)
            } else {
                None
            };

            writer.BeginWriting().context("BeginWriting(clip)")?;
            Ok(Self { writer, video_stream, audio_stream })
        }
    }

    pub fn write_video_packet(
        &self,
        pts_100ns: i64,
        duration_100ns: i64,
        keyframe: bool,
        data: &[u8],
    ) -> Result<()> {
        unsafe {
            let buffer = MFCreateMemoryBuffer(data.len() as u32)?;
            let mut ptr = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None)?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(data.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(pts_100ns)?;
            sample.SetSampleDuration(duration_100ns)?;
            if keyframe {
                sample.SetUINT32(&MFSampleExtension_CleanPoint, 1)?;
            }
            self.writer
                .WriteSample(self.video_stream, &sample)
                .context("WriteSample(clip video)")
        }
    }

    pub fn write_audio(&self, timestamp_100ns: i64, pcm: &[u8]) -> Result<()> {
        let Some(audio_stream) = self.audio_stream else { return Ok(()) };
        write_pcm_sample(&self.writer, audio_stream, timestamp_100ns, pcm)
    }

    pub fn finish(self) -> Result<()> {
        unsafe { self.writer.Finalize().context("clip Finalize") }
    }
}

fn audio_type(subtype: &GUID) -> Result<IMFMediaType> {
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, SAMPLE_RATE as u32)?;
        t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, CHANNELS as u32)?;
        t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        Ok(t)
    }
}

/// Best-effort: name the encoder MFT the sink writer picked, so the log
/// proves the hardware path is active.
fn log_video_transform(writer: &IMFSinkWriter, video_stream: u32) {
    unsafe {
        let mut raw: *mut core::ffi::c_void = std::ptr::null_mut();
        if writer
            .GetServiceForStream(video_stream, &GUID::zeroed(), &IMFTransform::IID, &mut raw)
            .is_err()
            || raw.is_null()
        {
            return;
        }
        let transform = IMFTransform::from_raw(raw);
        let Ok(attrs) = transform.GetAttributes() else {
            tracing::info!("video encoder MFT active (no attributes exposed)");
            return;
        };
        let name = allocated_string(&attrs, &MFT_FRIENDLY_NAME_Attribute)
            .unwrap_or_else(|| "<unnamed>".into());
        let hardware = allocated_string(&attrs, &MFT_ENUM_HARDWARE_URL_Attribute).is_some();
        tracing::info!(encoder = %name, hardware, "sink writer video encoder");
    }
}

pub(crate) fn allocated_string(attrs: &IMFAttributes, key: &GUID) -> Option<String> {
    let mut value = PWSTR::null();
    let mut length = 0u32;
    unsafe {
        attrs.GetAllocatedString(key, &mut value, &mut length).ok()?;
        if value.is_null() {
            return None;
        }
        let s = value.to_string().ok();
        CoTaskMemFree(Some(value.as_ptr() as *const _));
        s
    }
}
