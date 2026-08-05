//! Helpers for testing an app on the host.
//!
//! Behind the `testing` feature, so nothing here reaches a device binary. Apps enable it
//! under `[dev-dependencies]`:
//!
//! ```toml
//! [dev-dependencies]
//! symbian-ui = { path = "...", features = ["testing"] }
//! ```
//!
//! # Why this exists
//!
//! `Theme` borrows its font atlases, so constructing one in a test means producing a
//! valid `.sbf` first. That is twenty lines of byte-packing that every app was about to
//! copy, and an app whose tests are annoying to write is an app without tests.

use alloc::vec::Vec;

use symbian_gfx::{BitmapFont, Canvas, Rect, Size};

use crate::theme::{Fonts, Palette, Theme};

/// A valid one-glyph atlas.
///
/// Synthesised rather than a real font: a test needs `measure()` and `line_height()` to
/// return something consistent, not to look like anything. Every character falls back to
/// the same advance, which makes text width exactly proportional to length — a property
/// worth having in a layout test, where a proportional font makes every expected value a
/// magic number.
pub fn atlas() -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(b"SBF1");
    v.extend_from_slice(&12u16.to_le_bytes()); // px
    v.extend_from_slice(&9i16.to_le_bytes()); // ascent
    v.extend_from_slice(&3i16.to_le_bytes()); // descent
    v.extend_from_slice(&1u16.to_le_bytes()); // glyph count
    v.push(1); // FLAG_AA
    v.push(5); // fallback advance
    v.extend_from_slice(&0u16.to_le_bytes()); // reserved
    v.extend_from_slice(&(b'a' as u32).to_le_bytes());
    v.extend_from_slice(&0u32.to_le_bytes()); // blob offset
    v.extend_from_slice(&[4, 6, 5, 0]); // w, h, advance, pad
    v.extend_from_slice(&0i16.to_le_bytes()); // bearing x
    v.extend_from_slice(&6i16.to_le_bytes()); // bearing y
    v.extend(core::iter::repeat_n(0xFFu8, 24)); // the glyph, fully inked
    v
}

/// Build a theme from [`atlas`] and hand it to `f`.
///
/// A closure because the theme borrows the fonts, which borrow the atlas — none of which
/// can be returned out of a function that owns them.
///
/// ```
/// symbian_ui::testing::with_theme(symbian_ui::Palette::DARK, |theme| {
///     assert!(theme.metrics.row_h > 0);
/// });
/// ```
pub fn with_theme<R>(palette: Palette, f: impl FnOnce(&Theme<'_>) -> R) -> R {
    let data = atlas();
    let font = BitmapFont::new(&data).expect("the built-in test atlas must parse");
    let fonts = Fonts { body: &font, strong: &font, small: &font, title: &font };
    f(&Theme::new(palette, fonts))
}

/// The E72's screen, for a test that needs a rect.
pub const SCREEN: Rect = Rect { x0: 0, y0: 0, x1: 320, y1: 240 };

/// Run `f` against a real canvas over a scratch buffer, then hand back the pixels.
///
/// Useful for asserting that something was actually drawn — a widget that silently draws
/// nothing passes every test about its return value.
pub fn with_canvas<R>(size: Size, f: impl FnOnce(&mut Canvas<'_>) -> R) -> (R, Vec<u16>) {
    let mut buf = alloc::vec![0u16; (size.w * size.h) as usize];
    let out = {
        let mut c = Canvas::from_slice(&mut buf, size);
        f(&mut c)
    };
    (out, buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_test_atlas_parses_and_measures_proportionally() {
        let data = atlas();
        let f = BitmapFont::new(&data).unwrap();
        // Every glyph falls back to the same advance, so width is length times advance.
        // A layout test relies on that; if it stopped holding, expected values across the
        // whole workspace would drift by a pixel here and there.
        use symbian_gfx::Font;
        assert_eq!(f.measure("abcd"), f.measure("wxyz"));
        assert_eq!(f.measure("aa"), f.measure("a") * 2);
        assert!(f.line_height() > 0);
    }

    #[test]
    fn with_canvas_reports_what_was_drawn() {
        let ((), px) = with_canvas(Size::new(4, 4), |c| {
            c.clear(symbian_gfx::Color::hex(0xFFFFFF));
        });
        assert!(px.iter().all(|&p| p == 0xFFFF));
    }
}
