//! Copying one capture frame out of VRAM.
//!
//! Two paths need a frame in system memory: the clip thumbnail and the
//! screenshot in `replay.rs`, and the single-frame snapshot in
//! [`super::video`]. Both used to be written against a WGC `Frame`, which
//! could save itself to disk. A [`FrameSink`](super::source::FrameSink) is
//! handed a bare texture instead, so the staging copy lives here, once, and
//! works whichever backend produced the texture.

use anyhow::{Context as _, Result, bail};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_STAGING, ID3D11Device, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

/// One frame copied out of VRAM, tightly packed, waiting to be encoded.
pub struct StagedFrame {
    pub bgra: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Row pitch of `bgra`, not of the texture it came from — the driver's
    /// padding is dropped during the copy.
    pub stride: usize,
}

/// Copies one capture frame out of VRAM into a tightly packed BGRA buffer.
///
/// The `Map` below waits for the GPU to finish the copy, so this stalls the
/// capture callback for a few milliseconds — once, on the frame after a
/// clip request. That is the deliberate trade: the alternative is either a
/// per-frame copy (the gameplay cost this project exists to avoid) or
/// decoding the finished MP4 (a decoder the engine does not otherwise
/// need). A single dropped frame at the exact moment of a button press is
/// the worst case, and the ring already holds the seconds before it.
pub fn stage_bgra(source: &ID3D11Texture2D) -> Result<StagedFrame> {
    unsafe {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        source.GetDesc(&mut desc);
        if desc.Width == 0 || desc.Height == 0 {
            bail!("capture frame is {}x{}", desc.Width, desc.Height);
        }
        // Both backends hand us BGRA. Refusing anything else is what stops a
        // surprising format from becoming a silently wrong-coloured
        // thumbnail rather than a log line.
        if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
            bail!("capture frame is DXGI format {}, expected BGRA", desc.Format.0);
        }

        // From the texture rather than from the caller's converter, which
        // holds only the *video* device and context. Same underlying device.
        let device: ID3D11Device =
            source.GetDevice().context("the capture texture has no device")?;
        let context = device.GetImmediateContext().context("no immediate context")?;

        let staging_desc = D3D11_TEXTURE2D_DESC {
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
            ..desc
        };
        let mut staging: Option<ID3D11Texture2D> = None;
        device
            .CreateTexture2D(&staging_desc, None, Some(&mut staging))
            .context("CreateTexture2D(staging)")?;
        let staging = staging.context("no staging texture")?;

        context.CopyResource(&staging, source);

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        context
            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            .context("mapping the staged frame")?;

        // Copied row by row: the driver's `RowPitch` is usually larger
        // than `width * 4`, and handing that padding to the JPEG encoder
        // as if it were pixels is what produces a sheared image.
        let row = desc.Width as usize * 4;
        let mut bgra = vec![0u8; row * desc.Height as usize];
        for y in 0..desc.Height as usize {
            let src = (mapped.pData as *const u8).add(y * mapped.RowPitch as usize);
            std::ptr::copy_nonoverlapping(src, bgra.as_mut_ptr().add(y * row), row);
        }
        context.Unmap(&staging, 0);

        Ok(StagedFrame { bgra, width: desc.Width, height: desc.Height, stride: row })
    }
}
