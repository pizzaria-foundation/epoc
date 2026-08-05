//! Getting the canvas onto a screen whose pixels are not the canvas's pixels.
//!
//! The E72 reports `EColor16MU` — 32 bits per pixel — not the `EColor64K` we
//! assumed. Measured on the device, not read in a datasheet: the app printed
//! `display mode=11 bpp=32` and `EColor64K=7`.
//!
//! That leaves two options, and the obvious one is the wrong one.
//!
//! Rendering natively in 32bpp would avoid this conversion, but it doubles the
//! memory traffic of *every* drawing operation, and a UI touches most pixels more
//! than once — fill the background, draw a bubble over it, draw text over that.
//! Rendering in RGB565 and expanding once at present time moves half as many
//! bytes during drawing and pays a single linear pass at the end. On a 320x240
//! screen that pass is 76800 pixels of shift-and-or with no branches.
//!
//! So the canvas stays 16bpp and this module bridges the gap. If a device turns
//! out to report `EColor64K`, the shim skips this entirely and hands the canvas
//! buffer straight to the window server.

/// Expand RGB565 into `0x00RRGGBB`, the layout `EColor16MU` uses.
///
/// `dst_stride` and `src_stride` are in pixels, not bytes: Symbian aligns
/// `CFbsBitmap` scanlines to 4 bytes, so a 320-pixel row is not guaranteed to be
/// contiguous with the next one in either buffer.
///
/// Channels are widened by replicating their high bits into the low ones, so 5-bit
/// 0x1F becomes 0xFF rather than 0xF8. A plain shift would darken every colour
/// slightly and make white come out as 0xF8F8F8.
pub fn rgb565_to_xrgb8888(
    dst: &mut [u32],
    dst_stride: usize,
    src: &[u16],
    src_stride: usize,
    width: usize,
    height: usize,
) {
    debug_assert!(dst_stride >= width && src_stride >= width);
    for y in 0..height {
        let s = &src[y * src_stride..y * src_stride + width];
        let d = &mut dst[y * dst_stride..y * dst_stride + width];
        for (o, &p) in d.iter_mut().zip(s) {
            let r = ((p >> 11) & 0x1F) as u32;
            let g = ((p >> 5) & 0x3F) as u32;
            let b = (p & 0x1F) as u32;
            let r = (r << 3) | (r >> 2);
            let g = (g << 2) | (g >> 4);
            let b = (b << 3) | (b >> 2);
            *o = (r << 16) | (g << 8) | b;
        }
    }
}

/// The pixel formats the shim can hand us, as reported by the device rather than
/// assumed. The numeric values match Symbian's `TDisplayMode`.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ScreenFormat {
    /// 16bpp RGB565. The canvas format, so presenting is a straight copy.
    Rgb565 = 7,
    /// 32bpp `0x00RRGGBB`. What the E72 actually reports.
    Xrgb8888 = 11,
}

impl ScreenFormat {
    /// Map a raw `TDisplayMode`. Anything else is a device we have not seen and
    /// the caller should refuse rather than render garbage.
    pub fn from_display_mode(mode: i32) -> Option<Self> {
        match mode {
            7 => Some(ScreenFormat::Rgb565),
            11 => Some(ScreenFormat::Xrgb8888),
            _ => None,
        }
    }

    pub fn bytes_per_pixel(self) -> usize {
        match self {
            ScreenFormat::Rgb565 => 2,
            ScreenFormat::Xrgb8888 => 4,
        }
    }

    /// True when the canvas can be handed to the window server with no pass over
    /// the pixels at all.
    pub fn is_zero_copy(self) -> bool {
        matches!(self, ScreenFormat::Rgb565)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Rgb565;
    use alloc::vec;

    #[test]
    fn primaries_expand_to_full_range() {
        let src = [
            Rgb565::from_rgb(255, 0, 0).0,
            Rgb565::from_rgb(0, 255, 0).0,
            Rgb565::from_rgb(0, 0, 255).0,
            Rgb565::from_rgb(255, 255, 255).0,
            Rgb565::from_rgb(0, 0, 0).0,
        ];
        let mut dst = vec![0u32; 5];
        rgb565_to_xrgb8888(&mut dst, 5, &src, 5, 5, 1);
        assert_eq!(dst[0], 0x00FF0000, "red");
        assert_eq!(dst[1], 0x0000FF00, "green");
        assert_eq!(dst[2], 0x000000FF, "blue");
        assert_eq!(dst[3], 0x00FFFFFF, "white must reach FF, not F8");
        assert_eq!(dst[4], 0x00000000, "black");
    }

    #[test]
    fn top_byte_stays_clear() {
        // EColor16MU ignores the high byte, but leaving junk there has bitten
        // people on devices that quietly treat it as alpha.
        let src: alloc::vec::Vec<u16> = (0..=255u16).map(|i| i.wrapping_mul(257)).collect();
        let mut dst = vec![0xFFFF_FFFFu32; src.len()];
        let n = src.len();
        rgb565_to_xrgb8888(&mut dst, n, &src, n, n, 1);
        assert!(dst.iter().all(|p| p >> 24 == 0));
    }

    #[test]
    fn honours_differing_strides() {
        // 2x2 visible inside wider buffers; the padding must be left alone.
        let src = [
            Rgb565::from_rgb(255, 0, 0).0, Rgb565::from_rgb(0, 255, 0).0, 0xDEAD,
            Rgb565::from_rgb(0, 0, 255).0, Rgb565::from_rgb(255, 255, 255).0, 0xBEEF,
        ];
        let mut dst = vec![0xAAAA_AAAAu32; 8];
        rgb565_to_xrgb8888(&mut dst, 4, &src, 3, 2, 2);
        assert_eq!(dst[0], 0x00FF0000);
        assert_eq!(dst[1], 0x0000FF00);
        assert_eq!(dst[2], 0xAAAA_AAAA, "padding untouched");
        assert_eq!(dst[4], 0x000000FF);
        assert_eq!(dst[5], 0x00FFFFFF);
    }

    #[test]
    fn display_mode_mapping_matches_the_device() {
        // Measured on the E72: it reports 11, and EColor64K is 7.
        assert_eq!(ScreenFormat::from_display_mode(11), Some(ScreenFormat::Xrgb8888));
        assert_eq!(ScreenFormat::from_display_mode(7), Some(ScreenFormat::Rgb565));
        assert_eq!(ScreenFormat::from_display_mode(4), None);
        assert!(!ScreenFormat::Xrgb8888.is_zero_copy());
        assert!(ScreenFormat::Rgb565.is_zero_copy());
    }
}
