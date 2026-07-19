//! GPU BGRA → NV12 conversion via the D3D11 video processor (Phase 5).
//!
//! Doing this conversion ourselves (instead of letting the sink writer insert
//! MF's Video Processor MFT) matters twice: the encoder can be fed NV12
//! directly — which the Phase 5 replay path's manual MFT requires — and it
//! removes the VP MFT's opaque sample pools. On UMA iGPUs every GPU surface
//! is system RAM charged to our working set, so each avoided pool is real
//! memory back.

use anyhow::{Context as _, Result};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11_VIDEO_PROCESSOR_COLOR_SPACE, D3D11_VIDEO_PROCESSOR_CONTENT_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_STREAM, D3D11_VIDEO_USAGE_PLAYBACK_NORMAL, D3D11_VPIV_DIMENSION_TEXTURE2D,
    D3D11_VPOV_DIMENSION_TEXTURE2D, ID3D11Device, ID3D11Texture2D,
    ID3D11VideoContext, ID3D11VideoDevice, ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC};
use windows::core::Interface;

/// One reusable BGRA→NV12 GPU blit path plus a small pool of NV12 targets.
pub struct VideoConverter {
    video_context: ID3D11VideoContext,
    processor: ID3D11VideoProcessor,
    enumerator: ID3D11VideoProcessorEnumerator,
    video_device: ID3D11VideoDevice,
    pool: Vec<ID3D11Texture2D>,
    next: usize,
}

// SAFETY: used from one thread at a time (moved into the capture thread);
// the underlying D3D11 device has multithread protection enabled.
unsafe impl Send for VideoConverter {}


impl VideoConverter {
    pub fn new(
        device: &ID3D11Device,
        width: u32,
        height: u32,
        fps: u32,
        pool_size: usize,
    ) -> Result<Self> {
        let video_device: ID3D11VideoDevice =
            device.cast().context("device has no ID3D11VideoDevice")?;
        let context = unsafe { device.GetImmediateContext().context("no immediate context")? };
        let video_context: ID3D11VideoContext =
            context.cast().context("context has no ID3D11VideoContext")?;

        let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: Default::default(), // progressive
            InputFrameRate: DXGI_RATIONAL { Numerator: fps, Denominator: 1 },
            InputWidth: width,
            InputHeight: height,
            OutputFrameRate: DXGI_RATIONAL { Numerator: fps, Denominator: 1 },
            OutputWidth: width,
            OutputHeight: height,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        let enumerator = unsafe {
            video_device.CreateVideoProcessorEnumerator(&desc).context("VP enumerator")?
        };
        let processor =
            unsafe { video_device.CreateVideoProcessor(&enumerator, 0).context("VP create")? };

        unsafe {
            // Input: full-range RGB. Output: BT.709 studio-range YCbCr —
            // what every H.264 consumer expects for HD content.
            let input_cs = D3D11_VIDEO_PROCESSOR_COLOR_SPACE::default();
            video_context.VideoProcessorSetStreamColorSpace(&processor, 0, &input_cs);
            let mut output_cs = D3D11_VIDEO_PROCESSOR_COLOR_SPACE::default();
            // bitfield: YCbCr_Matrix (bit 2) = 1 → BT.709,
            //           Nominal_Range (bits 4-5) = 1 → 16..235
            output_cs._bitfield = (1 << 2) | (1 << 4);
            video_context.VideoProcessorSetOutputColorSpace(&processor, &output_cs);
        }

        let tex_desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut pool = Vec::with_capacity(pool_size);
        for _ in 0..pool_size {
            let mut texture: Option<ID3D11Texture2D> = None;
            unsafe {
                device
                    .CreateTexture2D(&tex_desc, None, Some(&mut texture))
                    .context("CreateTexture2D(NV12)")?;
            }
            pool.push(texture.unwrap());
        }

        Ok(Self { video_context, processor, enumerator, video_device, pool, next: 0 })
    }

    pub const fn pool_size(&self) -> usize {
        self.pool.len()
    }

    /// Blits `source` (BGRA) into the next NV12 pool texture and returns it.
    /// The returned texture is reused after `pool_size` further calls — the
    /// caller must apply backpressure before then.
    pub fn convert(&mut self, source: &ID3D11Texture2D) -> Result<&ID3D11Texture2D> {
        let target = &self.pool[self.next];
        self.next = (self.next + 1) % self.pool.len();
        unsafe {
            let mut in_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                ..Default::default()
            };
            in_desc.Anonymous.Texture2D.MipSlice = 0;
            in_desc.Anonymous.Texture2D.ArraySlice = 0;
            let mut input_view = None;
            self.video_device
                .CreateVideoProcessorInputView(source, &self.enumerator, &in_desc, Some(&mut input_view))
                .context("VP input view")?;
            let input_view = input_view.unwrap();

            let mut out_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                ..Default::default()
            };
            out_desc.Anonymous.Texture2D.MipSlice = 0;
            let mut output_view = None;
            self.video_device
                .CreateVideoProcessorOutputView(target, &self.enumerator, &out_desc, Some(&mut output_view))
                .context("VP output view")?;
            let output_view = output_view.unwrap();

            let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: std::mem::ManuallyDrop::new(Some(input_view)),
                ..Default::default()
            };
            let result = self
                .video_context
                .VideoProcessorBlt(&self.processor, &output_view, 0, std::slice::from_ref(&stream))
                .context("VideoProcessorBlt");
            // The struct holds a ManuallyDrop'd COM ref we own — release it.
            std::mem::ManuallyDrop::drop(&mut stream.pInputSurface);
            result?;
        }
        Ok(target)
    }
}
