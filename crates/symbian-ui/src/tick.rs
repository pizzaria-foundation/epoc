//! The two marks that say "chosen": a checkbox and a radio button.
//!
//! # Why these are one module and not two
//!
//! Because the only difference is the outline. Both are a small box of the same size in the same
//! place with the same states, and both have to line up with the text beside them — so the size, the
//! centring and the ink are one set of rules with a shape parameter, rather than two files that
//! agree by inspection.
//!
//! The *meanings* differ and that is the caller's business: a square is one of many, a circle is one
//! of few. Nothing here enforces it, because a widget cannot see its siblings.
//!
//! # Drawn as geometry, like the icons
//!
//! No glyph and no bitmap, for the reason [`crate::icon`] gives: at eleven pixels a font's tick is a
//! smudge, and a bitmap would need one per size and per theme. The tick itself is
//! [`icon::Icon::Check`], already drawn as two strokes and already legible at nine pixels.

use symbian_gfx::{Canvas, Rect};

use crate::icon::{self, Icon};
use crate::theme::Theme;

/// Which outline a mark wears.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Mark {
    /// A square: one of many may be chosen.
    Check,
    /// A circle: one of few.
    Radio,
}

/// How big a mark is, inside a band `band_h` pixels high.
///
/// Square, and derived from the band the same way [`crate::toggle::switch_height`] is — so a mark in
/// a list row and one in a dialog line are both proportionate to their line. Clamped at both ends:
/// below nine pixels the tick inside stops being a tick, and above eighteen the box starts
/// outweighing the label it belongs to.
pub fn mark_size(band_h: i32, theme: &Theme<'_>) -> i32 {
    (band_h - theme.metrics.pad * 2).clamp(9, 18)
}

/// Where the mark sits inside `band`: against its left edge, centred across it.
///
/// Left, unlike a switch, and that is the convention rather than an accident. A checkbox precedes
/// what it labels because the eye reads the state before the text; a switch follows it because the
/// text is the question and the switch is the answer. Getting it backwards is the sort of thing that
/// reads as a foreign application rather than as a bug.
pub fn mark_box(band: Rect, theme: &Theme<'_>) -> Rect {
    let s = mark_size(band.height(), theme);
    Rect::from_xywh(band.x0, band.y0 + (band.height() - s) / 2, s, s)
}

/// Paint a mark into exactly `at`.
///
/// `checked` fills it in the "on" ink and puts a tick inside; unchecked is an empty outline in the
/// quiet one. The tick is cut out in the *ground* — whatever is behind the control — so it reads as a
/// hole in the fill rather than as a second colour that has to contrast with it.
///
/// `selected` says whether the control sits on the selection band, which changes all three colours.
/// See [`crate::chrome::control_colors`].
pub fn draw_mark(
    c: &mut Canvas<'_>,
    at: Rect,
    theme: &Theme<'_>,
    mark: Mark,
    checked: bool,
    selected: bool,
) {
    // See `chrome::control_colors`: on the selection band the page's inks are the wrong three colours.
    let (ground, ink, quiet) = crate::chrome::control_colors(theme, selected);
    let radius = match mark {
        // A one-pixel radius rather than none: a hard square corner at this size reads as an artefact
        // of the rasteriser next to the round switch and the round avatar.
        Mark::Check => 1,
        Mark::Radio => at.width() / 2,
    };
    if checked {
        c.fill_round_rect(at, radius, ink);
        match mark {
            // Inset by two so the tick sits inside the fill rather than touching its edge.
            Mark::Check => icon::draw(c, at.inset(2), Icon::Check, ground),
            // A dot, not a tick: a radio button's chosen state is a smaller concentric circle, and a
            // tick in a circle reads as a checkbox someone drew wrong.
            Mark::Radio => {
                let d = (at.width() / 2).max(2);
                let inner = Rect::from_xywh(
                    at.x0 + (at.width() - d) / 2,
                    at.y0 + (at.height() - d) / 2,
                    d,
                    d,
                );
                c.fill_round_rect(inner, d / 2, ground);
            }
        }
    } else {
        // Outline only. Drawn with the rounded fill and then hollowed, so the corner treatment is the
        // same as the checked state's — a stroked rect would have square corners and the box would
        // change shape when it was ticked.
        c.fill_round_rect(at, radius, quiet);
        c.fill_round_rect(at.inset(1), (radius - 1).max(0), ground);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;
    use crate::theme::Palette;
    use symbian_gfx::Size;

    const BAND: Rect = Rect { x0: 0, y0: 0, x1: 40, y1: 38 };

    fn paint(mark: Mark, checked: bool, palette: Palette) -> Vec<u16> {
        let (_, buf) = testing::with_canvas(Size::new(40, 38), |c| {
            testing::with_theme(palette, |t| {
                c.clear(palette.bg.mid());
                draw_mark(c, mark_box(BAND, t), t, mark, checked, false);
            });
        });
        buf
    }

    #[test]
    fn a_mark_is_square_and_centred_in_its_band() {
        testing::with_theme(Palette::DARK, |t| {
            let b = mark_box(BAND, t);
            assert_eq!(b.width(), b.height(), "square, or a radio is an ellipse");
            assert_eq!(b.width(), mark_size(38, t));
            assert_eq!(b.x0, 0, "against the left edge");
            assert_eq!(b.y0, (38 - b.height()) / 2, "centred across the band");
        });
    }

    #[test]
    fn a_short_band_shrinks_the_mark_but_not_below_a_legible_tick() {
        testing::with_theme(Palette::DARK, |t| {
            assert_eq!(mark_size(38, t), 18, "clamped at the top");
            assert_eq!(mark_size(11, t), 9, "and at the bottom");
            // And never inverted, whatever a squeezed group offers.
            assert_eq!(mark_size(-5, t), 9);
        });
    }

    #[test]
    fn checked_and_unchecked_are_different_in_every_palette() {
        for (name, palette) in Palette::ALL {
            assert_ne!(paint(Mark::Check, false, palette), paint(Mark::Check, true, palette), "{name}");
            assert_ne!(paint(Mark::Radio, false, palette), paint(Mark::Radio, true, palette), "{name}");
        }
    }

    #[test]
    fn a_square_and_a_circle_are_different_marks() {
        // Both checked, both filled in the accent: the outline and the inner shape are what tell them
        // apart, and if they came out identical one of the two radii is wrong.
        assert_ne!(paint(Mark::Check, true, Palette::DARK), paint(Mark::Radio, true, Palette::DARK));
        assert_ne!(paint(Mark::Check, false, Palette::DARK), paint(Mark::Radio, false, Palette::DARK));
    }

    #[test]
    fn an_unchecked_mark_is_hollow() {
        // The middle of an unchecked box is the background, or it reads as a filled box in a dim
        // colour — which on a dark palette is indistinguishable from checked at a glance.
        let buf = paint(Mark::Check, false, Palette::DARK);
        let bg = Palette::DARK.bg.mid().to_rgb565().0;
        let mid = testing::with_theme(Palette::DARK, |t| {
            let b = mark_box(BAND, t);
            (b.y0 + b.height() / 2) * 40 + b.x0 + b.width() / 2
        });
        assert_eq!(buf[mid as usize], bg, "the middle should be background");
    }

    #[test]
    fn a_checked_mark_is_not_hollow() {
        // The negative control for the test above: if `draw_mark` stopped filling, `an_unchecked_mark_is_hollow`
        // would keep passing and say nothing.
        let buf = paint(Mark::Check, true, Palette::DARK);
        let bg = Palette::DARK.bg.mid().to_rgb565().0;
        let painted = buf.iter().filter(|&&p| p != bg).count();
        assert!(painted > 0);
        let hollow = paint(Mark::Check, false, Palette::DARK);
        let hollow_painted = hollow.iter().filter(|&&p| p != bg).count();
        assert!(painted > hollow_painted, "a filled box has more ink than an outline");
    }

    #[test]
    fn nothing_is_painted_outside_the_mark() {
        // A mark in a list row sits beside text, and a fill one pixel wide would eat the first letter
        // of the label — invisible in a glance and wrong on every row.
        let buf = paint(Mark::Check, true, Palette::DARK);
        let bg = Palette::DARK.bg.mid().to_rgb565().0;
        let b = testing::with_theme(Palette::DARK, |t| mark_box(BAND, t));
        for y in 0..38 {
            for x in 0..40 {
                if (b.x0..b.x1).contains(&x) && (b.y0..b.y1).contains(&y) {
                    continue;
                }
                assert_eq!(buf[(y * 40 + x) as usize], bg, "ink at {x},{y}");
            }
        }
    }
}
