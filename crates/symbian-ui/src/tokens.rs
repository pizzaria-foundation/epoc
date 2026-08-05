//! The vocabulary a theme is written in.
//!
//! # Why a `Surface` and not a `Color`
//!
//! The visual unit of the S60 era was not a colour, it was a *band*: a shallow
//! vertical gradient with one pixel of lighter colour along its top edge and one
//! pixel of darker along its bottom. Every title bar, softkey bar, highlight row
//! and button was built from that one shape.
//!
//! It is easy to read that as decoration and drop it. It was not decoration. S60
//! themes were user-installable and could put *any* background behind a widget, so
//! a flat fill could land on a wallpaper of the same lightness and vanish. The
//! light-top/dark-bottom pair is self-contrast: it stays visible against anything,
//! because whatever the background does, one of the two edges differs from it. The
//! same trick is why the era's icons all had a bevel.
//!
//! It also happens to be nearly free. A gradient over an 18px bar is 18 `hline`
//! calls, and the two edges are two more.
//!
//! # Why the roles are named after jobs
//!
//! Nokia's own skin colour table (`aknsconstants.h` in the S60 3rd Edition SDK) is
//! ~60 entries named for what they colour, not what colour they are: "navi pane
//! texts", "left softkey text", "list highlight text", "settings value item text".
//! That indirection is what let a theme swap without any widget knowing. This
//! module keeps the idea and trims it to the roles a chat client actually needs —
//! Nokia's table has four separate softkey-text roles because it had four softkey
//! contexts, and we have one.

use symbian_gfx::Color;

/// A themed fill: a shallow vertical gradient plus its two edge lines.
///
/// `top == bottom` is a flat fill, and the painter skips the gradient loop for it,
/// so a flat theme costs nothing.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Surface {
    /// Gradient colour at the top edge.
    pub top: Color,
    /// Gradient colour at the bottom edge.
    pub bottom: Color,
    /// One pixel along the top. Equal to `top` means "no highlight".
    pub edge_light: Color,
    /// One pixel along the bottom. Equal to `bottom` means "no shadow".
    pub edge_dark: Color,
}

impl Surface {
    /// No gradient, no edges. For backgrounds and anywhere the era would have used
    /// plain paper.
    pub const fn flat(c: Color) -> Self {
        Self { top: c, bottom: c, edge_light: c, edge_dark: c }
    }

    /// A gradient with no edge lines.
    pub const fn gradient(top: Color, bottom: Color) -> Self {
        Self { top, bottom, edge_light: top, edge_dark: bottom }
    }

    /// Derive the whole band from one colour: lighter above, darker below, with
    /// edges pushed a step further in each direction.
    ///
    /// This is what makes a theme authorable from a handful of colours rather than
    /// four times as many, which is how `.attheme` and the S60 skin format both
    /// worked. `strength` is in 1/255ths of the way to white or black.
    pub fn raised(base: Color, strength: u8) -> Self {
        let s = strength;
        Self {
            top: lighten(base, s / 2),
            bottom: darken(base, s / 2),
            edge_light: lighten(base, s.saturating_mul(2)),
            edge_dark: darken(base, s),
        }
    }

    /// The inverse: darker above, lighter below, so the shape reads as a well
    /// pressed into the surface. Text fields and the composer use it — the era
    /// signalled "you can type here" with an inset, not with a border radius.
    pub fn sunken(base: Color, strength: u8) -> Self {
        let s = strength;
        Self {
            top: darken(base, s / 2),
            bottom: lighten(base, s / 3),
            edge_light: lighten(base, s / 2),
            edge_dark: darken(base, s.saturating_mul(2)),
        }
    }

    /// Whether the fill needs the per-row gradient loop at all.
    pub fn is_flat(&self) -> bool {
        self.top == self.bottom
    }

    /// The colour halfway down. Useful when something must pick one colour to sit
    /// against — a mask blit, say, which has no gradient of its own.
    pub fn mid(&self) -> Color {
        self.top.lerp(self.bottom, 128)
    }
}

/// Move a colour towards white by `t`/255.
pub fn lighten(c: Color, t: u8) -> Color {
    c.lerp(Color::rgb(255, 255, 255), t)
}

/// Move a colour towards black by `t`/255.
pub fn darken(c: Color, t: u8) -> Color {
    c.lerp(Color::rgb(0, 0, 0), t)
}

/// Perceived lightness, 0..=255. The weights are the usual luma coefficients
/// rounded to sum to 256, so this is one multiply-add per channel and no division.
pub fn luma(c: Color) -> u8 {
    let v = 54 * c.r() as u32 + 183 * c.g() as u32 + 19 * c.b() as u32;
    (v >> 8) as u8
}

/// Pick whichever of two colours will be more readable on `bg`.
///
/// Used by the components that draw text over a themed surface, so a theme cannot
/// accidentally produce dark-on-dark: the widget asks rather than being told.
pub fn readable_on(bg: Color, light: Color, dark: Color) -> Color {
    // The threshold is below the midpoint on purpose. RGB565 and a TN panel viewed
    // off-axis both crush the dark end, so a mid-grey background reads darker in
    // person than its luma suggests, and light text wins the tie.
    if luma(bg) < 140 {
        light
    } else {
        dark
    }
}

/// Vertical rhythm and edge distances, in whole pixels.
///
/// Absolute pixels, not scaled units: 320x240 is the only target, there is no DPI
/// to adapt to, and a scale factor would only add arithmetic that never pays off.
///
/// The steps are 1/2/4/6/10 rather than a doubling scale. On a 240px-tall screen a
/// doubling scale runs out after four steps, and the era's own layouts used a 2px
/// quantum precisely because 4px was already a visible amount of space.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Space {
    /// A separator. Always 1 — named so call sites read as intent.
    pub hair: i32,
    /// Between a glyph and the thing it labels.
    pub tight: i32,
    /// Between stacked lines of text.
    pub snug: i32,
    /// The default gap, and the side margin of list rows.
    pub base: i32,
    /// Between groups: around a bubble, or a screen's outer margin.
    pub wide: i32,
}

impl Default for Space {
    fn default() -> Self {
        Self { hair: 1, tight: 2, snug: 4, base: 6, wide: 10 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_surface_reports_flat_and_gradient_does_not() {
        assert!(Surface::flat(Color::hex(0x123456)).is_flat());
        assert!(!Surface::gradient(Color::hex(0x000000), Color::hex(0xFFFFFF)).is_flat());
    }

    #[test]
    fn raised_puts_light_above_dark() {
        let s = Surface::raised(Color::hex(0x808080), 40);
        assert!(luma(s.top) > luma(s.bottom), "gradient must run light to dark");
        assert!(luma(s.edge_light) > luma(s.top), "highlight must beat the fill");
        assert!(luma(s.edge_dark) < luma(s.bottom), "shadow must beat the fill");
    }

    #[test]
    fn sunken_is_the_mirror_of_raised() {
        let base = Color::hex(0x808080);
        let up = Surface::raised(base, 40);
        let down = Surface::sunken(base, 40);
        // The defining property: raised is lighter at the top, sunken is darker.
        assert!(luma(up.top) > luma(base));
        assert!(luma(down.top) < luma(base));
    }

    #[test]
    fn extremes_do_not_wrap_around() {
        // saturating_mul on strength is the guard; if it ever became a plain
        // multiply, a strength above 128 would wrap and invert the bevel.
        let white = Surface::raised(Color::hex(0xFFFFFF), 200);
        assert_eq!(luma(white.edge_light), 255, "cannot get lighter than white");
        let black = Surface::raised(Color::hex(0x000000), 200);
        assert_eq!(luma(black.edge_dark), 0, "cannot get darker than black");
    }

    #[test]
    fn luma_orders_the_greys() {
        let mut last = 0u8;
        for v in [0x00u32, 0x20, 0x40, 0x80, 0xC0, 0xFF] {
            let l = luma(Color::hex(v << 16 | v << 8 | v));
            assert!(l >= last, "luma must be monotonic in grey level");
            last = l;
        }
        assert_eq!(luma(Color::hex(0xFFFFFF)), 255);
        assert_eq!(luma(Color::hex(0x000000)), 0);
    }

    #[test]
    fn luma_weights_green_most() {
        let r = luma(Color::rgb(255, 0, 0));
        let g = luma(Color::rgb(0, 255, 0));
        let b = luma(Color::rgb(0, 0, 255));
        assert!(g > r && r > b, "green brightest, blue darkest: got {g} {r} {b}");
    }

    #[test]
    fn readable_on_flips_at_the_ends() {
        let light = Color::hex(0xFFFFFF);
        let dark = Color::hex(0x000000);
        assert_eq!(readable_on(Color::hex(0x000000), light, dark), light);
        assert_eq!(readable_on(Color::hex(0xFFFFFF), light, dark), dark);
    }

    #[test]
    fn mid_is_between_the_stops() {
        let s = Surface::gradient(Color::hex(0x000000), Color::hex(0xFFFFFF));
        let m = luma(s.mid());
        assert!((100..=155).contains(&m), "midpoint of black->white was {m}");
    }

    #[test]
    fn space_steps_increase() {
        let s = Space::default();
        assert!(s.hair < s.tight && s.tight < s.snug && s.snug < s.base && s.base < s.wide);
        assert_eq!(s.hair, 1, "a hairline is one pixel by definition");
    }
}
