//! Putting a captured frame on the Windows clipboard.

use anyhow::{Context as _, Result, bail};
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::CF_DIBV5;

/// `size_of::<BITMAPV5HEADER>()`, stated once so the writer and the tests
/// cannot disagree about where the pixels start.
pub const BITMAPV5HEADER_BYTES: usize = 124;

/// How long to wait before the single retry in [`copy_bgra`].
const RETRY_AFTER: std::time::Duration = std::time::Duration::from_millis(30);

/// Packs a top-down BGRA buffer into a `CF_DIBV5` blob.
///
/// Two transformations, and both are load-bearing:
///
/// - **The rows are reversed.** The capture is top-down; a DIB with a positive
///   height is bottom-up, which is what clipboard consumers overwhelmingly
///   assume. Declaring a negative height instead is legal and is mishandled by
///   enough applications to be the wrong choice.
/// - **The driver's row padding is dropped.** A staged D3D texture's stride is
///   whatever the driver chose and is frequently wider than `width * 4`;
///   copying it verbatim shears the image.
pub fn dibv5(bgra: &[u8], width: u32, height: u32, stride: usize) -> Result<Vec<u8>> {
    if width == 0 || height == 0 {
        bail!("cannot copy a {width}x{height} image to the clipboard");
    }
    let row = (width as usize).checked_mul(4).context("image geometry overflows")?;
    if stride < row {
        bail!("stride {stride} is too small for a {width}px BGRA row");
    }
    let needed = stride.checked_mul(height as usize).context("image geometry overflows")?;
    if bgra.len() < needed {
        bail!("buffer is {} bytes, need {needed} for {width}x{height}", bgra.len());
    }

    // Plain multiplication, unlike the two `checked_mul`s above: it is safe
    // rather than checked-again, because `stride >= row` was just enforced and
    // `stride.checked_mul(height)` already succeeded as `needed`, so
    // `row * height <= stride * height` cannot overflow either.
    let mut dib = vec![0u8; BITMAPV5HEADER_BYTES + row * height as usize];
    {
        let mut put = |at: usize, value: u32| {
            dib[at..at + 4].copy_from_slice(&value.to_le_bytes());
        };
        put(0, BITMAPV5HEADER_BYTES as u32); // bV5Size
        put(4, width); // bV5Width
        put(8, height); // bV5Height, positive == bottom-up
        put(16, 3); // bV5Compression = BI_BITFIELDS
        put(20, (row * height as usize) as u32); // bV5SizeImage
        put(40, 0x00FF_0000); // bV5RedMask
        put(44, 0x0000_FF00); // bV5GreenMask
        put(48, 0x0000_00FF); // bV5BlueMask
        put(52, 0xFF00_0000); // bV5AlphaMask
        put(56, 0x7352_4742); // bV5CSType = 'sRGB', little-endian
    }
    dib[12..14].copy_from_slice(&1u16.to_le_bytes()); // bV5Planes
    dib[14..16].copy_from_slice(&32u16.to_le_bytes()); // bV5BitCount

    for y in 0..height as usize {
        let source = (height as usize - 1 - y) * stride;
        let dest = BITMAPV5HEADER_BYTES + y * row;
        dib[dest..dest + row].copy_from_slice(&bgra[source..source + row]);
    }
    Ok(dib)
}

/// Puts a captured frame on the clipboard.
///
/// `OpenClipboard` genuinely fails in normal use, because any other process can
/// hold the clipboard for as long as it likes. One retry, then give up: the
/// caller has already written the screenshot to disk, so a failure here costs
/// the paste, not the file.
pub fn copy_bgra(bgra: &[u8], width: u32, height: u32, stride: usize) -> Result<()> {
    let dib = dibv5(bgra, width, height, stride)?;
    unsafe {
        if OpenClipboard(Some(HWND::default())).is_err() {
            std::thread::sleep(RETRY_AFTER);
            OpenClipboard(Some(HWND::default()))
                .context("another program is holding the clipboard")?;
        }
        let result = set_dib(&dib);
        // Unconditional. An early return between open and close would leave
        // the clipboard locked against every application on the desktop until
        // this process exits -- far worse than a screenshot that did not copy.
        let _ = CloseClipboard();
        result
    }
}

/// The body between `OpenClipboard` and `CloseClipboard`.
///
/// Split out so the close above can be unconditional.
unsafe fn set_dib(dib: &[u8]) -> Result<()> {
    unsafe {
        EmptyClipboard().context("emptying the clipboard")?;
        let global: HGLOBAL =
            GlobalAlloc(GMEM_MOVEABLE, dib.len()).context("clipboard allocation")?;
        let locked = GlobalLock(global);
        if locked.is_null() {
            let _ = GlobalFree(Some(global));
            bail!("could not lock the clipboard allocation");
        }
        std::ptr::copy_nonoverlapping(dib.as_ptr(), locked.cast::<u8>(), dib.len());
        let _ = GlobalUnlock(global);

        // Ownership transfers to the clipboard on success only, so the free
        // below belongs to the failure path alone. Freeing after a successful
        // SetClipboardData hands every pasting application a dangling handle.
        match SetClipboardData(CF_DIBV5.0 as u32, Some(HANDLE(global.0))) {
            Ok(_) => Ok(()),
            Err(e) => {
                let _ = GlobalFree(Some(global));
                Err(e).context("SetClipboardData")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Byte written into the driver's row padding. Distinctive and non-zero
    /// on purpose: padding filled with zeros would be indistinguishable from
    /// correctly-dropped padding in the output.
    const PAD: u8 = 0xAB;

    /// Two rows of two pixels, with a stride deliberately wider than the
    /// pixels — which is what a staged D3D texture actually looks like.
    fn sample() -> (Vec<u8>, u32, u32, usize) {
        let stride = 12; // 2px * 4 bytes = 8, plus 4 bytes of driver padding
        let mut bgra = vec![PAD; stride * 2];
        bgra[0..8].copy_from_slice(&[255, 0, 0, 255, 0, 255, 0, 255]);
        bgra[stride..stride + 8].copy_from_slice(&[0, 0, 255, 255, 255, 255, 255, 255]);
        (bgra, 2, 2, stride)
    }

    #[test]
    fn the_dib_is_a_header_plus_exactly_the_pixels() {
        let (bgra, w, h, stride) = sample();
        let dib = dibv5(&bgra, w, h, stride).expect("dib");
        assert_eq!(dib.len(), BITMAPV5HEADER_BYTES + (w as usize * 4 * h as usize));
    }

    #[test]
    fn the_header_declares_a_bottom_up_bitmap_of_the_right_size() {
        let (bgra, w, h, stride) = sample();
        let dib = dibv5(&bgra, w, h, stride).expect("dib");
        let read = |at: usize| i32::from_le_bytes([dib[at], dib[at + 1], dib[at + 2], dib[at + 3]]);
        assert_eq!(read(0) as usize, BITMAPV5HEADER_BYTES, "bV5Size");
        assert_eq!(read(4), 2, "bV5Width");
        // Positive height means bottom-up, which is what clipboard consumers
        // overwhelmingly assume. Our source is top-down, so the rows must
        // actually be reversed -- see the next test.
        assert_eq!(read(8), 2, "bV5Height must be positive: bottom-up");
        assert_eq!(u16::from_le_bytes([dib[14], dib[15]]), 32, "bV5BitCount");
    }

    #[test]
    fn rows_are_flipped_because_the_capture_is_top_down() {
        let (bgra, w, h, stride) = sample();
        let dib = dibv5(&bgra, w, h, stride).expect("dib");
        let pixels = &dib[BITMAPV5HEADER_BYTES..];
        // The last source row must come first in a bottom-up DIB. Pasting a
        // vertically mirrored screenshot is the likeliest bug here and the one
        // nobody notices until a user complains.
        assert_eq!(&pixels[0..8], &[0, 0, 255, 255, 255, 255, 255, 255]);
        assert_eq!(&pixels[8..16], &[255, 0, 0, 255, 0, 255, 0, 255]);
    }

    #[test]
    fn the_driver_padding_is_dropped_rather_than_copied() {
        let (bgra, w, h, stride) = sample();
        let dib = dibv5(&bgra, w, h, stride).expect("dib");
        // Copying the stride verbatim would shear the image. The padding is a
        // distinctive non-zero byte, so this asserts on its *absence* rather
        // than on the output length -- a length check would only repeat what
        // `the_dib_is_a_header_plus_exactly_the_pixels` already proves, and
        // would pass just as happily if padding were copied and a real pixel
        // dropped to compensate.
        assert!(
            !dib[BITMAPV5HEADER_BYTES..].contains(&PAD),
            "a padding byte reached the clipboard: the row copy is using stride, not width"
        );
    }

    #[test]
    fn a_buffer_too_small_for_its_geometry_is_refused() {
        assert!(dibv5(&[0u8; 4], 100, 100, 400).is_err(), "buffer shorter than the geometry");
        assert!(dibv5(&[0u8; 16], 0, 2, 8).is_err(), "zero width");
        assert!(dibv5(&[0u8; 16], 2, 2, 4).is_err(), "stride narrower than one row");
    }
}
