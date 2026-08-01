//! Clip thumbnails: a staged BGRA frame, WIC-encoded to JPEG.
//!
//! Spec §5.3 takes the thumbnail from the frame at the hotkey press rather
//! than from a decoded MP4 — the frame is already in VRAM, so no decoder is
//! involved and no file is re-read. That requires one staging copy to CPU
//! memory per clip (~9 MB at 1920x1200, freed immediately), which is the
//! **deliberate, documented exception** to PLAN.md's Decision 1 ("frames never
//! touch the CPU"). It is paid once per clip, not once per frame.

use anyhow::{Context as _, Result, bail};
use windows::Win32::Foundation::HGLOBAL;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatJpeg, GUID_WICPixelFormat32bppBGRA,
    IWICImagingFactory, WICBitmapEncoderNoCache, WICBitmapInterpolationModeFant,
};
use windows::Win32::System::Com::StructuredStorage::{
    CreateStreamOnHGlobal, IPropertyBag2, PROPBAG2,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CoCreateInstance, IStream, STATFLAG_NONAME, STATSTG, STREAM_SEEK_SET,
};
use windows::Win32::System::Variant::{VARIANT, VT_R4};
use windows::core::PWSTR;

/// JPEG quality. 0.82 is the knee: visually clean in a grid at any thumbnail
/// size, and roughly a third the bytes of 0.95.
const QUALITY: f32 = 0.82;

/// Widest thumbnail written. A capture frame is whatever the monitor is —
/// 1920x1200 here — and encoding that verbatim produced **703 KB per clip**,
/// measured. A library grid on the low-end machines this project targets would
/// then decode tens of megabytes to draw one screen of tiles, which is the
/// opposite of the point. 640 px is roughly 60 KB, still sharp at a 320 px
/// tile on a 2x display and large enough for a hover preview.
const MAX_WIDTH: u32 = 640;

/// Encodes a top-down BGRA buffer to JPEG bytes.
///
/// `stride` is the row pitch in bytes, which for a staged D3D texture is
/// whatever the driver chose and is frequently larger than `width * 4`.
/// Passing `width * 4` when the real pitch is bigger produces a sheared image,
/// so it is an explicit parameter rather than a derived one.
pub fn encode_jpeg(bgra: &[u8], width: u32, height: u32, stride: usize) -> Result<Vec<u8>> {
    if width == 0 || height == 0 {
        bail!("cannot encode a {width}x{height} thumbnail");
    }
    if stride < (width as usize).saturating_mul(4) {
        bail!("stride {stride} is too small for a {width}px BGRA row");
    }
    let needed = stride.checked_mul(height as usize).context("thumbnail geometry overflows")?;
    if bgra.len() < needed {
        bail!("thumbnail buffer is {} bytes, need {needed} for {width}x{height}", bgra.len());
    }

    // WIC is COM, and this is reachable from the capture thread and from a
    // socket command. `ensure_mf_started` is what already guarantees the
    // process has a COM apartment on every path that reaches here.
    crate::encode::mf::ensure_mf_started()?;
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                .context("WIC factory")?;
        // A null HGLOBAL asks the runtime to allocate and grow one for us; the
        // `true` hands ownership of it to the stream.
        let stream = CreateStreamOnHGlobal(HGLOBAL::default(), true).context("thumbnail stream")?;
        let encoder = factory
            .CreateEncoder(&GUID_ContainerFormatJpeg, std::ptr::null())
            .context("JPEG encoder")?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache).context("encoder init")?;

        let mut frame = None;
        let mut props = None;
        encoder.CreateNewFrame(&mut frame, &mut props).context("encoder frame")?;
        let frame = frame.context("WIC returned no frame")?;
        // Quality goes into the property bag before `Initialize`, or it is
        // silently ignored and every thumbnail ships at the default.
        if let Some(props) = &props {
            set_quality(props, QUALITY);
        }
        frame.Initialize(props.as_ref()).context("frame init")?;
        let (out_w, out_h) = scaled_size(width, height);
        frame.SetSize(out_w, out_h).context("frame size")?;
        let mut format = GUID_WICPixelFormat32bppBGRA;
        frame.SetPixelFormat(&mut format).context("frame pixel format")?;

        let source = factory
            .CreateBitmapFromMemory(
                width,
                height,
                &GUID_WICPixelFormat32bppBGRA,
                stride as u32,
                &bgra[..needed],
            )
            .context("wrapping the staged frame")?;
        if (out_w, out_h) == (width, height) {
            frame.WriteSource(&source, std::ptr::null()).context("writing thumbnail pixels")?;
        } else {
            let scaler = factory.CreateBitmapScaler().context("bitmap scaler")?;
            // Fant is WIC's best downscale filter — a box or nearest filter on
            // a 3x reduction aliases text and UI edges badly, and a clip
            // preview is mostly text and UI edges.
            scaler
                .Initialize(&source, out_w, out_h, WICBitmapInterpolationModeFant)
                .context("scaler init")?;
            frame.WriteSource(&scaler, std::ptr::null()).context("writing thumbnail pixels")?;
        }
        frame.Commit().context("frame commit")?;
        encoder.Commit().context("encoder commit")?;

        read_stream(&stream)
    }
}

/// Fits `width` under [`MAX_WIDTH`], keeping the aspect ratio. Never scales
/// *up* — a small capture stays its own size rather than being blown up into
/// a blurrier, larger file.
fn scaled_size(width: u32, height: u32) -> (u32, u32) {
    if width <= MAX_WIDTH {
        return (width, height);
    }
    // Rounded, and floored at 1: an extreme aspect ratio must not produce a
    // zero-height image, which WIC rejects outright.
    let height = ((u64::from(height) * u64::from(MAX_WIDTH) + u64::from(width) / 2)
        / u64::from(width)) as u32;
    (MAX_WIDTH, height.max(1))
}

/// Writes the `ImageQuality` float into WIC's encoder property bag.
///
/// Hand-built VARIANT for the same reason as `encode/mf.rs`'s `variant_u32`:
/// windows-rs exposes only the raw union, with no `From<f32>`. Best-effort —
/// a rejected quality hint means a larger thumbnail, not a failed clip.
unsafe fn set_quality(props: &IPropertyBag2, quality: f32) {
    unsafe {
        // Must outlive the `Write` call below; `PROPBAG2` borrows it.
        let mut name: Vec<u16> = "ImageQuality\0".encode_utf16().collect();
        let bag = PROPBAG2 { pstrName: PWSTR(name.as_mut_ptr()), ..Default::default() };
        let mut value: VARIANT = core::mem::zeroed();
        let inner = &mut *value.Anonymous.Anonymous;
        inner.vt = VT_R4;
        inner.Anonymous.fltVal = quality;
        if let Err(e) = props.Write(1, &bag, &value) {
            tracing::debug!(error = %e, "WIC rejected the thumbnail quality hint");
        }
    }
}

/// Reads the finished JPEG back out of the in-memory stream.
unsafe fn read_stream(stream: &IStream) -> Result<Vec<u8>> {
    unsafe {
        let mut stat = STATSTG::default();
        // `STATFLAG_NONAME` so WIC does not allocate a name string this
        // function would then have to free.
        stream.Stat(&mut stat, STATFLAG_NONAME).context("thumbnail stream stat")?;
        let size = usize::try_from(stat.cbSize).context("thumbnail is implausibly large")?;
        stream.Seek(0, STREAM_SEEK_SET, None).context("thumbnail stream seek")?;

        let mut buf = vec![0u8; size];
        let mut read = 0u32;
        stream
            .Read(buf.as_mut_ptr().cast(), size as u32, Some(&mut read))
            .ok()
            .context("reading the thumbnail stream")?;
        buf.truncate(read as usize);
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real JPEG, not just bytes: the SOI/EOI markers are what a `<video>`
    /// poster or an `<img>` in the clip grid will actually parse.
    #[test]
    fn encodes_a_bgra_buffer_into_a_real_jpeg() {
        let (w, h) = (64u32, 32u32);
        let stride = (w * 4) as usize;
        // A gradient rather than a solid colour: a solid image would still
        // encode if the stride handling were wrong.
        let mut bgra = vec![0u8; stride * h as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let p = y * stride + x * 4;
                bgra[p] = (x * 4) as u8;
                bgra[p + 1] = (y * 8) as u8;
                bgra[p + 2] = 0x80;
                bgra[p + 3] = 0xFF;
            }
        }

        let jpeg = encode_jpeg(&bgra, w, h, stride).expect("WIC must encode a plain BGRA buffer");
        assert!(jpeg.len() > 256, "suspiciously small for a 64x32 gradient: {}", jpeg.len());
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8], "JPEG SOI marker");
        assert_eq!(&jpeg[jpeg.len() - 2..], &[0xFF, 0xD9], "JPEG EOI marker");
    }

    /// A short buffer must be refused rather than read past the end. This runs
    /// on a socket-reachable path under `panic = "abort"`, so an out-of-bounds
    /// read is a process kill, not an exception.
    #[test]
    fn a_buffer_too_small_for_its_geometry_is_refused() {
        assert!(encode_jpeg(&[0u8; 16], 64, 32, 256).is_err());
    }

    /// A capture frame is monitor-sized. Encoding one verbatim measured 703 KB
    /// per clip, which a grid on a low-end PC would pay for on every tile.
    #[test]
    fn a_monitor_sized_frame_is_scaled_down_to_a_thumbnail() {
        assert_eq!(scaled_size(1920, 1200), (640, 400), "16:10 keeps its ratio");
        assert_eq!(scaled_size(2560, 1440), (640, 360), "16:9 keeps its ratio");
        // Never upscaled: a smaller capture stays its own size.
        assert_eq!(scaled_size(320, 200), (320, 200));
        // A pathological ratio must not round to a zero-height image, which
        // WIC refuses.
        assert_eq!(scaled_size(20_000, 1), (640, 1));
    }

    /// The scaled path must still produce a decodable JPEG, and a much smaller
    /// one — the whole reason the scaler is there.
    #[test]
    fn scaling_produces_a_smaller_but_still_valid_jpeg() {
        let (w, h) = (1280u32, 800u32);
        let stride = (w * 4) as usize;
        let mut bgra = vec![0u8; stride * h as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let p = y * stride + x * 4;
                bgra[p] = (x % 256) as u8;
                bgra[p + 1] = (y % 256) as u8;
                bgra[p + 2] = ((x + y) % 256) as u8;
                bgra[p + 3] = 0xFF;
            }
        }

        let scaled = encode_jpeg(&bgra, w, h, stride).expect("scaled encode");
        assert_eq!(&scaled[..2], &[0xFF, 0xD8], "JPEG SOI marker");
        assert_eq!(&scaled[scaled.len() - 2..], &[0xFF, 0xD9], "JPEG EOI marker");
        assert!(
            scaled.len() < 120_000,
            "a 640px thumbnail should be tens of KB, got {}",
            scaled.len()
        );
    }
}
