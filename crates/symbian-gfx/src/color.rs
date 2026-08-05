//! Colour, and the RGB565 blend that the whole toolkit is built on.
//!
//! Authoring happens in 8-bit-per-channel `Color`; pixels live as `Rgb565`.
//! The device framebuffer is 16bpp, so keeping the canvas in RGB565 halves both
//! the memory and the blit cost versus a 32bpp intermediate. The panel itself is
//! 24-bit, so if a device turns out to expose `EColor16MU` we convert once at
//! blit time instead of paying for 32bpp everywhere.

/// Straight-alpha colour, packed `0xAARRGGBB`.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub struct Color(pub u32);

impl Color {
    pub const TRANSPARENT: Self = Self(0x0000_0000);
    pub const BLACK: Self = Self::rgb(0, 0, 0);
    pub const WHITE: Self = Self::rgb(255, 255, 255);

    #[inline]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self(0xFF00_0000 | ((r as u32) << 16) | ((g as u32) << 8) | b as u32)
    }

    #[inline]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self(((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | b as u32)
    }

    /// `0xRRGGBB`, fully opaque. Lets themes read like CSS.
    #[inline]
    pub const fn hex(v: u32) -> Self {
        Self(0xFF00_0000 | (v & 0x00FF_FFFF))
    }

    #[inline]
    pub const fn a(self) -> u8 {
        (self.0 >> 24) as u8
    }

    #[inline]
    pub const fn r(self) -> u8 {
        (self.0 >> 16) as u8
    }

    #[inline]
    pub const fn g(self) -> u8 {
        (self.0 >> 8) as u8
    }

    #[inline]
    pub const fn b(self) -> u8 {
        self.0 as u8
    }

    #[inline]
    pub const fn with_alpha(self, a: u8) -> Self {
        Self((self.0 & 0x00FF_FFFF) | ((a as u32) << 24))
    }

    #[inline]
    pub const fn is_opaque(self) -> bool {
        self.a() == 0xFF
    }

    #[inline]
    pub const fn to_rgb565(self) -> Rgb565 {
        Rgb565::from_rgb(self.r(), self.g(), self.b())
    }

    /// Linear interpolation in 8-bit sRGB space. Not perceptually correct, but
    /// it is what every other small UI toolkit does and it is cheap.
    #[inline]
    pub fn lerp(self, other: Self, t: u8) -> Self {
        let mix = |a: u8, b: u8| -> u8 {
            (((a as u32) * (255 - t as u32) + (b as u32) * t as u32) / 255) as u8
        };
        Self::rgba(
            mix(self.r(), other.r()),
            mix(self.g(), other.g()),
            mix(self.b(), other.b()),
            mix(self.a(), other.a()),
        )
    }
}

/// A single framebuffer pixel: `RRRRRGGG GGGBBBBB`.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default, Hash)]
pub struct Rgb565(pub u16);

impl Rgb565 {
    pub const BLACK: Self = Self(0x0000);
    pub const WHITE: Self = Self(0xFFFF);

    #[inline]
    pub const fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Self((((r as u16) & 0xF8) << 8) | (((g as u16) & 0xFC) << 3) | ((b as u16) >> 3))
    }

    /// Widen back to 8-bit channels, replicating high bits into the low ones so
    /// that 0x1F becomes 0xFF rather than 0xF8 (a plain shift would darken every
    /// colour slightly and make round-trips lossy in an obvious way).
    #[inline]
    pub const fn to_rgb(self) -> (u8, u8, u8) {
        let r = ((self.0 >> 11) & 0x1F) as u8;
        let g = ((self.0 >> 5) & 0x3F) as u8;
        let b = (self.0 & 0x1F) as u8;
        ((r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2))
    }

    #[inline]
    pub const fn to_color(self) -> Color {
        let (r, g, b) = self.to_rgb();
        Color::rgb(r, g, b)
    }
}

/// Blend `src` over `dst` with 8-bit coverage.
///
/// The red and blue channels are interleaved into one 32-bit lane and green into
/// another, so each blend costs two multiply-adds instead of six. It works
/// because masking green out of the red/blue lane leaves bits 5..10 free, which
/// is exactly the headroom blue needs when scaled by a 5-bit factor.
#[inline]
pub fn blend565(dst: u16, src: u16, coverage: u8) -> u16 {
    // Map 0..=255 onto 0..=32 so the divide below is a shift. Rounding at the
    // midpoint keeps 0 fully transparent and 255 fully opaque.
    let a = (coverage as u32 + 4) >> 3;
    if a == 0 {
        return dst;
    }
    if a >= 32 {
        return src;
    }
    let ia = 32 - a;

    let d_rb = (dst & 0xF81F) as u32;
    let d_g = (dst & 0x07E0) as u32;
    let s_rb = (src & 0xF81F) as u32;
    let s_g = (src & 0x07E0) as u32;

    let rb = ((d_rb * ia + s_rb * a) >> 5) & 0xF81F;
    let g = ((d_g * ia + s_g * a) >> 5) & 0x07E0;
    (rb | g) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_and_zero_coverage_are_exact() {
        let d = Rgb565::from_rgb(10, 20, 30).0;
        let s = Rgb565::from_rgb(200, 100, 50).0;
        assert_eq!(blend565(d, s, 0), d);
        assert_eq!(blend565(d, s, 255), s);
    }

    #[test]
    fn blend_channels_do_not_bleed_into_each_other() {
        // Pure red over pure blue must stay on the red/blue axis with no green.
        let red = Rgb565::from_rgb(255, 0, 0).0;
        let blue = Rgb565::from_rgb(0, 0, 255).0;
        for cov in 0..=255u8 {
            let out = blend565(blue, red, cov);
            assert_eq!(out & 0x07E0, 0, "green leaked at coverage {cov}: {out:#06x}");
        }
    }

    #[test]
    fn blend_is_monotonic_and_stays_in_gamut() {
        let d = Rgb565::from_rgb(0, 0, 0).0;
        let s = Rgb565::from_rgb(255, 255, 255).0;
        let mut prev = 0u16;
        for cov in 0..=255u8 {
            let out = blend565(d, s, cov);
            let (r, g, b) = Rgb565(out).to_rgb();
            // Greyscale ramp: all channels advance together, never wrapping.
            assert!(out >= prev, "not monotonic at {cov}");
            assert!(r == g || r.abs_diff(g) <= 4, "channel skew {r} {g} {b}");
            prev = out;
        }
        assert_eq!(prev, s);
    }

    #[test]
    fn midpoint_coverage_lands_near_the_middle() {
        let d = Rgb565::from_rgb(0, 0, 0).0;
        let s = Rgb565::from_rgb(255, 255, 255).0;
        let (r, _, _) = Rgb565(blend565(d, s, 128)).to_rgb();
        assert!((120..=136).contains(&r), "midpoint blend gave {r}");
    }

    #[test]
    fn rgb565_roundtrip_reaches_the_extremes() {
        assert_eq!(Rgb565::from_rgb(255, 255, 255).to_rgb(), (255, 255, 255));
        assert_eq!(Rgb565::from_rgb(0, 0, 0).to_rgb(), (0, 0, 0));
    }

    #[test]
    fn hex_matches_rgb() {
        assert_eq!(Color::hex(0x1E90FF), Color::rgb(0x1E, 0x90, 0xFF));
    }
}
