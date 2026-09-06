//! BGRA↔JPEG via WIC: encoding a staged frame for clip thumbnails and
//! full-size screenshots, and decoding a saved screenshot back to BGRA for
//! `shots.copy`. PNG comes out of the same encoder, for the capture probe's
//! snapshot.
//!
//! Spec §5.3 takes the thumbnail from the frame at the hotkey press rather
//! than from a decoded MP4 — the frame is already in VRAM, so no decoder is
//! involved and no file is re-read. That requires one staging copy to CPU
//! memory per clip (~9 MB at 1920x1200, freed immediately), which is the
//! **deliberate, documented exception** to PLAN.md's Decision 1 ("frames never
//! touch the CPU"). It is paid once per clip, not once per frame.

use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use windows::Win32::Foundation::{GENERIC_READ, HGLOBAL};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatJpeg, GUID_ContainerFormatPng,
    GUID_WICPixelFormat32bppBGRA, IWICImagingFactory, WICBitmapDitherTypeNone,
    WICBitmapEncoderNoCache, WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom,
    WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::StructuredStorage::{
    CreateStreamOnHGlobal, IPropertyBag2, PROPBAG2,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CoCreateInstance, IStream, STATFLAG_NONAME, STATSTG, STREAM_SEEK_SET,
};
use windows::Win32::System::Variant::{VARIANT, VT_R4};
use windows::core::{GUID, PCWSTR, PWSTR};

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

/// Passed as `max_width` to ask for no downscaling at all. A screenshot is the
/// capture resolution by definition — that is the whole difference between it
/// and the thumbnail this module was written for.
pub const FULL_SIZE: u32 = u32::MAX;

/// Quality for a full-size screenshot. Higher than the thumbnail's 0.82
/// because this one is the artifact the user keeps and shares rather than a
/// tile in a grid: roughly 400 KB for a 1080p frame against ~250 KB at 0.82,
/// for visibly cleaner HUD text.
pub const SHOT_QUALITY: f32 = 0.92;

/// Encodes a top-down BGRA buffer to JPEG bytes at a caller-chosen size and
/// quality.
///
/// `stride` is the row pitch in bytes, which for a staged D3D texture is
/// whatever the driver chose and is frequently larger than `width * 4`.
/// Passing `width * 4` when the real pitch is bigger produces a sheared image,
/// so it is an explicit parameter rather than a derived one.
pub fn encode_jpeg_sized(
    bgra: &[u8],
    width: u32,
    height: u32,
    stride: usize,
    max_width: u32,
    quality: f32,
) -> Result<Vec<u8>> {
    encode_wic(&GUID_ContainerFormatJpeg, Some(quality), bgra, width, height, stride, max_width)
}

/// Encodes a top-down BGRA buffer to PNG bytes at its own size.
///
/// PNG rather than JPEG for exactly one caller: `trix probe capture
/// --snapshot`, which exists to show what the capture stack actually produced.
/// A lossy snapshot of a capture under diagnosis would be a poor tool — the
/// artefact you are looking for could be the encoder's.
pub fn encode_png(bgra: &[u8], width: u32, height: u32, stride: usize) -> Result<Vec<u8>> {
    encode_wic(&GUID_ContainerFormatPng, None, bgra, width, height, stride, FULL_SIZE)
}

/// The shared WIC path. `quality` is JPEG's alone: PNG is lossless and has no
/// property to set, and passing one it ignores would only invite the reader to
/// believe it did something.
fn encode_wic(
    container: &GUID,
    quality: Option<f32>,
    bgra: &[u8],
    width: u32,
    height: u32,
    stride: usize,
    max_width: u32,
) -> Result<Vec<u8>> {
    if width == 0 || height == 0 {
        bail!("cannot encode a {width}x{height} image");
    }
    if stride < (width as usize).saturating_mul(4) {
        bail!("stride {stride} is too small for a {width}px BGRA row");
    }
    let needed = stride.checked_mul(height as usize).context("image geometry overflows")?;
    if bgra.len() < needed {
        bail!("frame buffer is {} bytes, need {needed} for {width}x{height}", bgra.len());
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
        let stream = CreateStreamOnHGlobal(HGLOBAL::default(), true).context("encoder stream")?;
        let encoder = factory.CreateEncoder(container, std::ptr::null()).context("WIC encoder")?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache).context("encoder init")?;

        let mut frame = None;
        let mut props = None;
        encoder.CreateNewFrame(&mut frame, &mut props).context("encoder frame")?;
        let frame = frame.context("WIC returned no frame")?;
        // Quality goes into the property bag before `Initialize`, or it is
        // silently ignored and every thumbnail ships at the default.
        if let (Some(props), Some(quality)) = (&props, quality) {
            set_quality(props, quality);
        }
        frame.Initialize(props.as_ref()).context("frame init")?;
        let (out_w, out_h) = scaled_size(width, height, max_width);
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
            frame.WriteSource(&source, std::ptr::null()).context("writing image pixels")?;
        } else {
            let scaler = factory.CreateBitmapScaler().context("bitmap scaler")?;
            // Fant is WIC's best downscale filter — a box or nearest filter on
            // a 3x reduction aliases text and UI edges badly, and a clip
            // preview is mostly text and UI edges.
            scaler
                .Initialize(&source, out_w, out_h, WICBitmapInterpolationModeFant)
                .context("scaler init")?;
            frame.WriteSource(&scaler, std::ptr::null()).context("writing image pixels")?;
        }
        frame.Commit().context("frame commit")?;
        encoder.Commit().context("encoder commit")?;

        read_stream(&stream)
    }
}

/// Encodes a top-down BGRA buffer to a JPEG thumbnail: at most [`MAX_WIDTH`]
/// wide, at [`QUALITY`].
///
/// A thin wrapper over [`encode_jpeg_sized`], so the clip path keeps its
/// established defaults in one place rather than repeating two constants at
/// every call site.
pub fn encode_jpeg(bgra: &[u8], width: u32, height: u32, stride: usize) -> Result<Vec<u8>> {
    encode_jpeg_sized(bgra, width, height, stride, MAX_WIDTH, QUALITY)
}

/// Fits `width` under `max_width`, keeping the aspect ratio. Never scales
/// *up* — a small capture stays its own size rather than being blown up into
/// a blurrier, larger file.
fn scaled_size(width: u32, height: u32, max_width: u32) -> (u32, u32) {
    if width <= max_width {
        return (width, height);
    }
    // Rounded, and floored at 1: an extreme aspect ratio must not produce a
    // zero-height image, which WIC rejects outright.
    let height = ((u64::from(height) * u64::from(max_width) + u64::from(width) / 2)
        / u64::from(width)) as u32;
    (max_width, height.max(1))
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

/// Decodes a JPEG file back to a top-down BGRA buffer.
///
/// Returns `(bgra, width, height, stride)` with `stride == width * 4` — this
/// buffer is ours, so it carries no driver padding.
///
/// Used only by `shots.copy`, which puts an already-saved screenshot back on
/// the clipboard. Decoding on demand rather than caching frames is deliberate:
/// a cache of full-resolution BGRA is precisely the memory cost this product
/// exists not to have.
pub fn decode_jpeg_bgra(path: &Path) -> Result<(Vec<u8>, u32, u32, usize)> {
    crate::encode::mf::ensure_mf_started()?;
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                .context("WIC factory")?;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let decoder = factory
            .CreateDecoderFromFilename(
                PCWSTR(wide.as_ptr()),
                None,
                GENERIC_READ,
                WICDecodeMetadataCacheOnDemand,
            )
            .with_context(|| format!("decoding {}", path.display()))?;
        let frame = decoder.GetFrame(0).context("first frame")?;
        let converter = factory.CreateFormatConverter().context("format converter")?;
        converter
            .Initialize(
                &frame,
                &GUID_WICPixelFormat32bppBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeCustom,
            )
            .context("converting to BGRA")?;

        let mut width = 0u32;
        let mut height = 0u32;
        converter.GetSize(&mut width, &mut height).context("image size")?;
        // Mirrors `encode_jpeg_sized`'s own guard: a degenerate size here
        // would otherwise sail through to a zero-length buffer handed to
        // `clipboard::copy_bgra`, rather than being refused where the bad
        // geometry is actually known.
        if width == 0 || height == 0 {
            bail!("cannot decode a {width}x{height} screenshot");
        }
        let stride = (width as usize).checked_mul(4).context("image geometry overflows")?;
        let len = stride.checked_mul(height as usize).context("image geometry overflows")?;
        let mut bgra = vec![0u8; len];
        let stride_u32 = u32::try_from(stride).context("image geometry overflows")?;
        converter.CopyPixels(std::ptr::null(), stride_u32, &mut bgra).context("copying pixels")?;
        Ok((bgra, width, height, stride))
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

    /// The PNG container is a second WIC encoder behind the same code path, so
    /// it is worth proving it produces a real PNG rather than a JPEG with a
    /// different extension: the signature bytes are what an image viewer reads.
    ///
    /// Full size, not scaled: the snapshot exists to show the capture exactly
    /// as it arrived.
    #[test]
    fn encodes_a_bgra_buffer_into_a_real_full_size_png() {
        let (w, h) = (64u32, 32u32);
        // Driver padding on the row pitch, which is what a staged capture
        // frame actually carries.
        let stride = (w * 4) as usize + 16;
        let bgra = vec![0x40u8; stride * h as usize];

        let png = encode_png(&bgra, w, h, stride).expect("WIC must encode a plain BGRA buffer");
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A], "PNG signature");
        // `IHDR` carries the dimensions big-endian, right after the signature
        // and the chunk length: proof nothing scaled it on the way through.
        assert_eq!(&png[16..24], &[0, 0, 0, 64, 0, 0, 0, 32], "64x32, unscaled");
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
        assert_eq!(scaled_size(1920, 1200, MAX_WIDTH), (640, 400), "16:10 keeps its ratio");
        assert_eq!(scaled_size(2560, 1440, MAX_WIDTH), (640, 360), "16:9 keeps its ratio");
        // Never upscaled: a smaller capture stays its own size.
        assert_eq!(scaled_size(320, 200, MAX_WIDTH), (320, 200));
        // A pathological ratio must not round to a zero-height image, which
        // WIC refuses.
        assert_eq!(scaled_size(20_000, 1, MAX_WIDTH), (640, 1));
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

    /// The 640 px ceiling and the new full-size mode must agree about everything
    /// except the scaling, because the clip thumbnail and the screenshot are now
    /// the same encoder called twice.
    #[test]
    fn full_size_encoding_keeps_the_capture_resolution() {
        assert_eq!(scaled_size(1920, 1200, MAX_WIDTH), (640, 400));
        assert_eq!(scaled_size(1920, 1200, FULL_SIZE), (1920, 1200));
        // A capture smaller than the ceiling is never blown up.
        assert_eq!(scaled_size(320, 200, MAX_WIDTH), (320, 200));
        // An extreme aspect ratio must not floor the height to zero, which WIC
        // rejects outright.
        assert_eq!(scaled_size(20_000, 3, MAX_WIDTH).1, 1);
    }

    /// The only test that reaches `decode_jpeg_bgra`'s COM sequencing, stride
    /// arithmetic and buffer sizing at all. `shots_copy`'s traversal test in
    /// `trix-daemon` only ever reaches the id-whitelist rejection branch, so
    /// without this, a stride or channel-order mistake in the decoder would
    /// ship and only a user's clipboard would ever reveal it.
    ///
    /// Writes only to a scratch temp dir it creates and removes -- never the
    /// developer's real clip library or config, per the rule every test in
    /// this workspace follows.
    #[test]
    fn decoding_recovers_the_encoded_image() {
        let (w, h) = (64u32, 32u32);
        let stride = (w * 4) as usize;
        // The same gradient as `encodes_a_bgra_buffer_into_a_real_jpeg`:
        // smooth enough that JPEG's quantization does not move a sampled
        // pixel far from its source value, but varying per-pixel so a stride
        // or channel-order mistake lands on a visibly wrong value rather than
        // an accidental match against a solid colour.
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

        let jpeg = encode_jpeg(&bgra, w, h, stride).expect("encode a known buffer");

        let dir = std::env::temp_dir().join(format!("trix-thumb-decode-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join("shot.jpg");
        std::fs::write(&path, &jpeg).expect("write scratch jpeg");

        let result = decode_jpeg_bgra(&path);
        let _ = std::fs::remove_dir_all(&dir);
        let (bgra_out, dw, dh, dstride) = result.expect("decode the jpeg back");

        assert_eq!((dw, dh), (w, h), "decode must recover the encoded dimensions");
        assert_eq!(dstride, w as usize * 4, "decode's own buffer carries no driver padding");
        assert_eq!(bgra_out.len(), dstride * dh as usize);

        // A point away from every edge, so a stride mistake lands on an
        // obviously wrong value rather than an edge pixel smeared by the
        // encoder's own block padding.
        let (x, y) = (32usize, 16usize);
        let p = y * dstride + x * 4;
        let expected = [(x * 4) as u8, (y * 8) as u8, 0x80u8, 0xFFu8];
        for (channel, want) in expected.iter().enumerate() {
            let got = bgra_out[p + channel];
            assert!(
                (i32::from(got) - i32::from(*want)).abs() <= 12,
                "channel {channel} at ({x},{y}): expected ~{want}, got {got} -- \
                 a stride or channel-order bug misses by far more than JPEG noise"
            );
        }
    }
}
