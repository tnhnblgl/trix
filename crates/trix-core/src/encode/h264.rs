//! Manual async hardware H.264 encoder MFT (Phase 5b).
//!
//! The replay ring needs the *encoded packets themselves* — something the
//! SinkWriter never exposes. This module drives the hardware encoder MFT
//! directly: NV12 D3D11 textures in, H.264 access units (with timestamps and
//! keyframe flags) out. Enumeration is pinned to the capture adapter's LUID
//! so hybrid-GPU machines encode where the frames already live.

use anyhow::{Context as _, Result, anyhow, bail};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFAttributes, IMFDXGIDeviceManager, IMFMediaEventGenerator, IMFMediaType,
    IMFSample, IMFTransform, METransformHaveOutput, METransformNeedInput, MF_E_NO_EVENTS_AVAILABLE,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_EVENT_FLAG_NO_WAIT,
    MF_LOW_LATENCY, MF_MT_MPEG_SEQUENCE_HEADER, MF_TRANSFORM_ASYNC_UNLOCK, MFCreateAttributes,
    MFCreateDXGISurfaceBuffer, MFCreateMediaType, MFCreateSample, MFMediaType_Video,
    MFSampleExtension_CleanPoint, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_ADAPTER_LUID,
    MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER,
    MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO, MFTEnum2, MFVideoFormat_H264,
    MFVideoFormat_NV12,
};
use windows::core::Interface;

use crate::encode::mf::{RecorderSettings, allocated_string, apply_rate_control, video_type};
use windows::Win32::Media::MediaFoundation::MFT_FRIENDLY_NAME_Attribute;

/// One encoded H.264 access unit, CPU-resident (the only pixels-derived bytes
/// that ever touch system RAM in the replay pipeline).
#[derive(Clone)]
pub struct EncodedPacket {
    pub pts_100ns: i64,
    pub duration_100ns: i64,
    pub keyframe: bool,
    pub data: Vec<u8>,
}

pub struct H264Encoder {
    transform: IMFTransform,
    events: IMFMediaEventGenerator,
    /// The activate that created `transform`, kept solely so [`Drop`] can call
    /// `ShutdownObject` on it. See the `Drop` impl for why releasing the
    /// transform is not enough.
    activate: IMFActivate,
    /// NeedInput credits granted by the MFT that we haven't spent yet.
    input_credits: u32,
    /// The MFT's friendly name, e.g. "Intel® Quick Sync Video H.264 Encoder MFT".
    /// Reported in `status` and stamped into every clip's metadata.
    name: String,
    pub frames_in: u64,
    pub packets_out: u64,
}

impl Drop for H264Encoder {
    /// `IMFActivate::ActivateObject` must be paired with `ShutdownObject`.
    /// Dropping the `IMFTransform` only releases *our* reference: a hardware
    /// MFT keeps its D3D device reference, its async work queue and its driver
    /// allocations alive until the activate is shut down. Without this, every
    /// arm/disarm cycle left ~50 MB of GPU memory, one Media Foundation
    /// work-queue thread and ~37 handles behind for the life of the process.
    fn drop(&mut self) {
        // Best effort: a failure here is not actionable by the caller, and a
        // disarm must not fail because teardown was untidy.
        if let Err(e) = unsafe { self.activate.ShutdownObject() } {
            tracing::warn!("encoder MFT ShutdownObject failed: {e}");
        }
    }
}

// SAFETY: same contract as MfRecorder — the encoder is moved into the capture
// thread and driven from one thread at a time; the D3D device it touches has
// multithread protection enabled.
unsafe impl Send for H264Encoder {}

impl H264Encoder {
    pub fn new(
        device: &ID3D11Device,
        manager: &IMFDXGIDeviceManager,
        settings: &RecorderSettings,
    ) -> Result<Self> {
        crate::encode::mf::ensure_mf_started()?;
        unsafe {
            let (transform, activate, name) = activate_hardware_encoder(device)?;

            // Async MFTs refuse ProcessInput/Output until unlocked.
            let attrs = transform.GetAttributes().context("MFT attributes")?;
            attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
            // No B-frames/lookahead: smaller internal surface pool, 1-in-1-out.
            let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);

            transform
                .ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)
                .context("MFT SET_D3D_MANAGER")?;

            // Encoders require output type before input type.
            let out_type = video_type(settings, &MFVideoFormat_H264, true)?;
            transform.SetOutputType(0, &out_type, 0).context("SetOutputType(H264)")?;
            let in_type = video_type(settings, &MFVideoFormat_NV12, false)?;
            transform.SetInputType(0, &in_type, 0).context("SetInputType(NV12)")?;

            // Force the rate-control mode + bitrate ceiling now, before
            // streaming — the media-type hint alone lets AMF overshoot ~4×,
            // which would also blow the replay ring's RAM budget.
            apply_rate_control(&transform, settings);

            let info = transform.GetOutputStreamInfo(0)?;
            if info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 == 0 {
                bail!("hardware encoder does not provide output samples (unexpected)");
            }

            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;

            let events: IMFMediaEventGenerator =
                transform.cast().context("encoder MFT is not async")?;

            Ok(Self {
                transform,
                events,
                activate,
                input_credits: 0,
                name,
                frames_in: 0,
                packets_out: 0,
            })
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Pumps the MFT event queue: collects NeedInput credits and drains every
    /// available output packet into `sink`.
    pub fn pump(&mut self, sink: &mut impl FnMut(EncodedPacket)) -> Result<()> {
        loop {
            let event = unsafe { self.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) };
            let event = match event {
                Ok(e) => e,
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(()),
                Err(e) => return Err(anyhow!("MFT event queue: {e}")),
            };
            let event_type = unsafe { event.GetType()? } as i32;
            if event_type == METransformNeedInput.0 {
                self.input_credits += 1;
            } else if event_type == METransformHaveOutput.0 {
                if let Some(packet) = self.drain_one()? {
                    sink(packet);
                }
            }
        }
    }

    /// True if the MFT can accept a frame right now.
    pub const fn ready_for_input(&self) -> bool {
        self.input_credits > 0
    }

    /// Submits one NV12 texture. Caller must check [`ready_for_input`] and
    /// drop the frame otherwise (backpressure = drop, never block).
    pub fn encode(&mut self, nv12: &ID3D11Texture2D, pts_100ns: i64, dur_100ns: i64) -> Result<()> {
        unsafe {
            let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, nv12, 0, false)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(pts_100ns)?;
            sample.SetSampleDuration(dur_100ns)?;
            self.transform.ProcessInput(0, &sample, 0).context("encoder ProcessInput")?;
        }
        self.input_credits = self.input_credits.saturating_sub(1);
        self.frames_in += 1;
        Ok(())
    }

    fn drain_one(&mut self) -> Result<Option<EncodedPacket>> {
        let mut buffers = [MFT_OUTPUT_DATA_BUFFER::default()];
        let mut status = 0u32;
        let result = unsafe { self.transform.ProcessOutput(0, &mut buffers, &mut status) };
        let [buffer] = &mut buffers;
        let sample = unsafe { std::mem::ManuallyDrop::take(&mut buffer.pSample) };
        let events = unsafe { std::mem::ManuallyDrop::take(&mut buffer.pEvents) };
        drop(events);
        match result {
            Ok(()) => {
                let Some(sample) = sample else { return Ok(None) };
                let packet = unsafe { packet_from_sample(&sample)? };
                self.packets_out += 1;
                Ok(Some(packet))
            }
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(None),
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                // The MFT finalized its real output type (now carrying the
                // sequence header). Accept it; the pending output will be
                // re-announced with a fresh METransformHaveOutput event.
                unsafe {
                    let new_type = self.transform.GetOutputAvailableType(0, 0)?;
                    self.transform.SetOutputType(0, &new_type, 0)?;
                }
                Ok(None)
            }
            Err(e) => Err(anyhow!("encoder ProcessOutput: {e}")),
        }
    }

    /// A copy of the encoder's *negotiated* output type, guaranteed to carry
    /// the H.264 sequence header — exactly what the clip muxer must declare
    /// as its input for passthrough. Falls back to scanning `first_keyframe`
    /// for SPS/PPS if the MFT didn't publish the header on the type.
    pub fn mux_input_type(&self, first_keyframe: Option<&[u8]>) -> Result<IMFMediaType> {
        unsafe {
            let current = self.transform.GetOutputCurrentType(0).context("output type")?;
            let copy = MFCreateMediaType()?;
            current.CopyAllItems(&copy).context("CopyAllItems")?;
            if copy.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER).unwrap_or(0) == 0 {
                let header = first_keyframe
                    .and_then(extract_sps_pps)
                    .ok_or_else(|| anyhow!("no sequence header available for mux"))?;
                copy.SetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &header)?;
            }
            Ok(copy)
        }
    }
}

unsafe fn packet_from_sample(sample: &IMFSample) -> Result<EncodedPacket> {
    unsafe {
        let pts_100ns = sample.GetSampleTime().unwrap_or(0);
        let duration_100ns = sample.GetSampleDuration().unwrap_or(0);
        let keyframe = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) == 1;
        let buffer = sample.ConvertToContiguousBuffer()?;
        let mut data_ptr = std::ptr::null_mut();
        let mut length = 0u32;
        buffer.Lock(&mut data_ptr, None, Some(&mut length))?;
        let data = std::slice::from_raw_parts(data_ptr, length as usize).to_vec();
        buffer.Unlock()?;
        Ok(EncodedPacket { pts_100ns, duration_100ns, keyframe, data })
    }
}

/// Scans an annex-B access unit for SPS (type 7) and PPS (type 8) NALs and
/// returns them concatenated with 4-byte start codes.
fn extract_sps_pps(au: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut found_sps = false;
    let mut found_pps = false;
    let mut i = 0;
    while i + 4 < au.len() {
        let (start, code_len) = if au[i..].starts_with(&[0, 0, 0, 1]) {
            (i + 4, 4)
        } else if au[i..].starts_with(&[0, 0, 1]) {
            (i + 3, 3)
        } else {
            i += 1;
            continue;
        };
        // find next start code
        let mut end = au.len();
        let mut j = start;
        while j + 3 <= au.len() {
            if au[j..].starts_with(&[0, 0, 1]) || au[j..].starts_with(&[0, 0, 0, 1]) {
                end = if j > start && au[j - 1] == 0 { j - 1 } else { j };
                end = end.min(au.len());
                break;
            }
            j += 1;
        }
        let nal_type = au.get(start).map(|b| b & 0x1f);
        match nal_type {
            Some(7) => {
                out.extend_from_slice(&[0, 0, 0, 1]);
                out.extend_from_slice(&au[start..end]);
                found_sps = true;
            }
            Some(8) => {
                out.extend_from_slice(&[0, 0, 0, 1]);
                out.extend_from_slice(&au[start..end]);
                found_pps = true;
            }
            _ => {}
        }
        if found_sps && found_pps {
            return Some(out);
        }
        i = start.max(i + code_len);
        i = end.max(i);
    }
    None
}

/// Returns the transform *and* the activate that produced it — the caller owns
/// both, because shutting the MFT down requires the activate, not the transform.
fn activate_hardware_encoder(device: &ID3D11Device) -> Result<(IMFTransform, IMFActivate, String)> {
    unsafe {
        // Pin enumeration to the capture adapter so hybrid-GPU machines
        // encode on the GPU that already holds the frames (no PCIe copies).
        let luid = device
            .cast::<IDXGIDevice>()?
            .GetAdapter()?
            .GetDesc()
            .map(|d| d.AdapterLuid)
            .context("adapter desc")?;
        let mut luid_bytes = [0u8; 8];
        luid_bytes[..4].copy_from_slice(&luid.LowPart.to_le_bytes());
        luid_bytes[4..].copy_from_slice(&luid.HighPart.to_le_bytes());

        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 1)?;
        let attrs = attrs.unwrap();
        attrs.SetBlob(&MFT_ENUM_ADAPTER_LUID, &luid_bytes)?;

        let input = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_NV12,
        };
        let output = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_H264,
        };

        let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        MFTEnum2(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input),
            Some(&output),
            Some(&attrs),
            &mut activates,
            &mut count,
        )
        .context("MFTEnum2")?;
        if count == 0 || activates.is_null() {
            bail!("no hardware H.264 encoder on the capture adapter");
        }

        // Take ownership of the first (best) activate; free the rest + array.
        let mut chosen: Option<IMFActivate> = None;
        for i in 0..count as usize {
            let activate = std::ptr::read(activates.add(i));
            if i == 0 {
                chosen = activate;
            }
        }
        windows::Win32::System::Com::CoTaskMemFree(Some(activates as *const _));
        let activate = chosen.ok_or_else(|| anyhow!("null encoder activate"))?;

        let name =
            allocated_string(&activate.cast::<IMFAttributes>()?, &MFT_FRIENDLY_NAME_Attribute)
                .unwrap_or_else(|| "<unnamed>".into());
        tracing::info!(encoder = %name, "hardware H.264 MFT activated");

        let transform = activate.ActivateObject::<IMFTransform>().context("ActivateObject")?;
        Ok((transform, activate, name))
    }
}
