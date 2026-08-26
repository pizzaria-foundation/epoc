//! A state, as a coloured pill instead of words at the end of a row.
//!
//! ## What was actually missing
//!
//! Not the drawing. [`chrome::badge`] already paints a pill with a fill and a text colour, and this
//! calls it — inventing a second pill would be the opposite mistake. What was missing was that
//! **nobody had given the states names**, so every screen spelled them into the end of a row label:
//!
//! ```text
//! Launcher  0.1.0  · ok
//! browser   0.1.0  ↺ rebuild
//! gpsprobe  0.1.0  ⚠ UID clash · gpsprobe.sis
//! ```
//!
//! Three different vocabularies, one column, and a row whose meaning depends on reading to the end of
//! a 40-column string. A [`Tone`] is the semantic half — *this is good, this needs attention, this is
//! new* — and the colour comes from the theme rather than from each caller picking one.
//!
//! ## Colour is never the only signal
//!
//! Every chip carries a word. The panel on this handset is a TN screen read in sunlight, the palette
//! has both a light and a dark variant, and a person may not distinguish the accent from the warning
//! hue at all. So the colour is an accelerant for something already legible, never the message —
//! which is the same reason `Offer::describe` exists in words.

use crate::chrome;
use crate::theme::Theme;
use symbian_gfx::{Canvas, Color, Point, Rect};

/// How tall a chip is: a line of small text plus two.
///
/// Named because it was written out three times — once in [`Chip::width`] as the floor that keeps a
/// two-character chip from becoming a slot, once in [`Chip::draw_right`] as the number it centres
/// vertically by, and once again the moment `symbian_decl_ui` needed a chip's *box* rather than its
/// width. Three copies of one number is how a measured size and a drawn size come to disagree, and
/// the symptom of that is never a misshapen pill — it is the row beside it truncating a character
/// early, which is the defect `Badge` shipped with.
pub fn height(theme: &Theme<'_>) -> i32 {
    theme.fonts.small.line_height() + 2
}

/// What a state *means*, which is what decides its colour.
///
/// Deliberately four and not one per state: a palette with a colour for every condition is a palette
/// nobody can learn. "Needs attention" covers a rebuild, an unknown, and a rate limit, because the
/// action they call for is the same — look at this one.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Tone {
    /// Nothing to do. Quiet on purpose: the ordinary state should not be the loudest thing on screen.
    Calm,
    /// Something is on offer, or something arrived.
    Fresh,
    /// Worth a look before acting.
    Warn,
    /// Something is happening right now.
    Busy,
}

impl Tone {
    /// The fill and text colours, from the theme.
    ///
    /// `Calm` uses the divider colour rather than the accent, so a screen of healthy rows reads as
    /// calm and the one row that is not draws the eye. A list where everything is coloured is a list
    /// where nothing stands out.
    pub fn colors(self, theme: &Theme<'_>) -> (Color, Color) {
        let p = &theme.palette;
        match self {
            Tone::Calm => (p.divider, p.text),
            Tone::Fresh => (p.accent, p.accent_text),
            // `warn` exists because of this line. The first version borrowed `unread` — the
            // palette's existing "you should look at this" — and a test found that `unread` and
            // `accent` are the *same colour* in the dark palette, so "on offer" and "be careful"
            // painted identically. A design system with an accent and no caution colour was missing
            // one, and forcing an existing token would have hidden that.
            Tone::Warn => (p.warn, p.warn_text),
            Tone::Busy => (p.selection.top, p.selection_text),
        }
    }

    /// The fill and text colours for a chip that may be on the selection band.
    ///
    /// Off the band, exactly [`Tone::colors`]. On it, one pair for all four tones — see
    /// [`Chip::draw_right_on`] for why the distinction is the thing that gives way rather than the
    /// legibility.
    pub fn colors_on(self, theme: &Theme<'_>, selected: bool) -> (Color, Color) {
        if selected {
            chrome::unread_colors(theme, true)
        } else {
            self.colors(theme)
        }
    }
}

/// A word and a tone, drawn as a pill.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Chip<'a> {
    pub text: &'a str,
    pub tone: Tone,
}

impl<'a> Chip<'a> {
    pub const fn new(text: &'a str, tone: Tone) -> Self {
        Self { text, tone }
    }

    pub const fn calm(text: &'a str) -> Self {
        Self::new(text, Tone::Calm)
    }

    pub const fn fresh(text: &'a str) -> Self {
        Self::new(text, Tone::Fresh)
    }

    pub const fn warn(text: &'a str) -> Self {
        Self::new(text, Tone::Warn)
    }

    pub const fn busy(text: &'a str) -> Self {
        Self::new(text, Tone::Busy)
    }

    /// How wide this will be, so a caller can reserve the space *before* laying out the text beside
    /// it.
    ///
    /// Measured rather than assumed, because the alternative is what every row in this project did
    /// until now: put the state at the end of the label and let a long name push it off the screen.
    pub fn width(&self, theme: &Theme<'_>) -> i32 {
        // The eight and the floor are [`chrome::badge`]'s own, not a second sizing rule that happens
        // to agree: what this reports is what that paints, or the caller reserves the wrong room.
        (theme.fonts.small.measure(self.text) + 8).max(height(theme))
    }

    /// Draw right-aligned inside `row`, and answer how much width was used.
    ///
    /// Right-aligned because the name is what a person scans down and the state is what they glance
    /// at — and because a chip on the left would move every name by a different amount.
    pub fn draw_right(&self, c: &mut Canvas<'_>, row: Rect, theme: &Theme<'_>) -> i32 {
        self.draw_right_on(c, row, theme, false)
    }

    /// The same, told whether the row it sits on is the selected one.
    ///
    /// A second entry point rather than a changed one, because every caller that exists draws on the
    /// page and must keep its pixels. What the band needs is different colours: `Tone::Calm` fills
    /// with `divider` and `Tone::Busy` with `selection.top`, and neither of those is a colour you can
    /// see *on* the selection surface — a calm chip on the highlighted row is a pill-shaped hole.
    ///
    /// On the band all four tones collapse to the inverted pill [`chrome::unread_colors`] already
    /// picks for a badge there, which the palette guarantees is legible. That loses the tone
    /// distinction on exactly one row, and it is the right trade for the reason this module opens
    /// with: every chip carries a word, so the colour was never the message.
    pub fn draw_right_on(
        &self,
        c: &mut Canvas<'_>,
        row: Rect,
        theme: &Theme<'_>,
        selected: bool,
    ) -> i32 {
        let (fill, fg) = self.tone.colors_on(theme, selected);
        let h = height(theme);
        // Vertically centred in the row rather than sitting on its top edge. This is also what makes
        // a chip survive `CrossAlign::Stretch`: a list row hands over its whole 38-pixel band, and
        // without the centring the pill would sit on the band's top edge.
        let y = row.y0 + ((row.height() - h) / 2).max(0);
        chrome::badge(c, Point::new(row.x1, y), theme, self.text, fill, fg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{with_canvas, with_theme};
    use crate::Palette;
    use symbian_gfx::Size;

    fn drawn(chip: Chip<'_>, palette: Palette) -> (i32, alloc::vec::Vec<u16>) {
        with_canvas(Size::new(160, 20), |c| {
            with_theme(palette, |t| chip.draw_right(c, Rect::from_xywh(0, 0, 160, 20), t))
        })
    }

    #[test]
    fn a_chip_reports_the_width_it_will_take_before_it_is_drawn() {
        // The whole point: a caller can reserve the space, instead of putting the state at the end of
        // a label where a long name pushes it off a 320-pixel screen.
        with_theme(Palette::DARK, |t| {
            let short = Chip::calm("ok").width(t);
            let long = Chip::warn("UID clash").width(t);
            assert!(long > short);
            assert!(short > 0);
        });
    }

    #[test]
    fn the_width_reported_is_the_width_used() {
        let (used, _) = drawn(Chip::fresh("new"), Palette::DARK);
        with_theme(Palette::DARK, |t| assert_eq!(used, Chip::fresh("new").width(t)));
    }

    #[test]
    fn tones_differ_from_each_other_in_both_palettes() {
        // If two tones painted the same, the semantic distinction would exist only in the source.
        for palette in [Palette::DARK, Palette::LIGHT] {
            with_theme(palette, |t| {
                let colors: alloc::vec::Vec<_> =
                    [Tone::Calm, Tone::Fresh, Tone::Warn, Tone::Busy]
                        .iter()
                        .map(|x| x.colors(t).0)
                        .collect();
                for i in 0..colors.len() {
                    for j in i + 1..colors.len() {
                        assert_ne!(colors[i], colors[j], "{palette:?}: tones {i} and {j} match");
                    }
                }
            });
        }
    }

    #[test]
    fn the_calm_tone_is_the_quiet_one() {
        // A list where everything is coloured is a list where nothing stands out, so the ordinary
        // state must not use the accent.
        with_theme(Palette::DARK, |t| {
            assert_ne!(Tone::Calm.colors(t).0, t.palette.accent);
            assert_eq!(Tone::Fresh.colors(t).0, t.palette.accent);
        });
    }

    #[test]
    fn a_chip_always_carries_a_word() {
        // Colour is an accelerant for something already legible, never the message: this is a TN
        // panel read in sunlight, and a person may not tell the hues apart at all.
        for c in [Chip::calm("ok"), Chip::fresh("new"), Chip::warn("UID"), Chip::busy("57%")] {
            assert!(!c.text.is_empty());
        }
    }

    #[test]
    fn something_is_actually_painted() {
        let (_, dark) = drawn(Chip::fresh("new"), Palette::DARK);
        let blank = with_canvas(Size::new(160, 20), |_| {}).1;
        assert_ne!(dark, blank);
    }

    #[test]
    fn extracting_the_height_moved_no_pixel_and_no_reported_width() {
        // The proof for the refactor that named `height`. Both `width` and `draw_right` used to spell
        // `f.line_height() + 2` inline; this reconstructs the *old* arithmetic and demands the new
        // code agrees on both the number and the buffer.
        //
        // The negative control is at the bottom: a deliberately wrong height must fail the comparison,
        // or this test proves only that two calls to the same function agree.
        for (name, palette) in Palette::ALL {
            with_theme(palette, |t| {
                let old_h = t.fonts.small.line_height() + 2;
                assert_eq!(height(t), old_h, "{name}");
                for chip in [Chip::calm("ok"), Chip::warn("UID clash"), Chip::busy("a")] {
                    let old_w = (t.fonts.small.measure(chip.text) + 8).max(old_h);
                    assert_eq!(chip.width(t), old_w, "{name}: {}", chip.text);
                }
            });
        }

        let row = Rect::from_xywh(0, 0, 160, 38);
        let paint = |h: i32| {
            with_canvas(Size::new(160, 38), |c| {
                with_theme(Palette::DARK, |t| {
                    c.clear(Palette::DARK.bg.mid());
                    let (fill, fg) = Tone::Warn.colors(t);
                    let y = row.y0 + ((row.height() - h) / 2).max(0);
                    chrome::badge(c, Point::new(row.x1, y), t, "UID clash", fill, fg);
                });
            })
            .1
        };
        let now = with_canvas(Size::new(160, 38), |c| {
            with_theme(Palette::DARK, |t| {
                c.clear(Palette::DARK.bg.mid());
                Chip::warn("UID clash").draw_right(c, row, t);
            });
        })
        .1;
        let old_h = with_theme(Palette::DARK, |t| t.fonts.small.line_height() + 2);
        assert_eq!(now, paint(old_h), "the pill moved");
        // The negative control. If a wrong height painted the same buffer, the comparison above would
        // be vacuous.
        assert_ne!(now, paint(old_h + 4), "the comparison cannot tell two pills apart");
    }

    #[test]
    fn drawing_on_the_page_is_exactly_what_it_always_was() {
        // `draw_right` now delegates to `draw_right_on(.., false)`. The delegation must be invisible.
        for (name, palette) in Palette::ALL {
            let go = |selected: bool| {
                with_canvas(Size::new(160, 20), |c| {
                    with_theme(palette, |t| {
                        c.clear(palette.bg.mid());
                        Chip::calm("a").draw_right_on(c, Rect::from_xywh(0, 0, 160, 20), t, selected);
                    });
                })
                .1
            };
            let plain = with_canvas(Size::new(160, 20), |c| {
                with_theme(palette, |t| {
                    c.clear(palette.bg.mid());
                    Chip::calm("a").draw_right(c, Rect::from_xywh(0, 0, 160, 20), t);
                });
            })
            .1;
            assert_eq!(plain, go(false), "{name}: the page pill changed");
            // The control that makes the equality mean something: the band pill is a different pill.
            assert_ne!(plain, go(true), "{name}: the band changed nothing");
        }
    }

    #[test]
    fn a_calm_chip_on_the_selection_band_is_not_a_pill_shaped_hole() {
        // `Tone::Calm` fills with `divider`, which was chosen against the page. On the band it can be
        // the band's own colour, and then the chip is a word floating in nothing — the same defect
        // `chrome::control_colors` was written for, arriving at a different control.
        for (name, palette) in Palette::ALL {
            with_theme(palette, |t| {
                let band = t.palette.selection.mid();
                for tone in [Tone::Calm, Tone::Fresh, Tone::Warn, Tone::Busy] {
                    let (fill, fg) = tone.colors_on(t, true);
                    let d = |a: Color, b: Color| {
                        (crate::tokens::luma(a) as i32 - crate::tokens::luma(b) as i32).abs()
                    };
                    assert!(d(fill, band) >= 40, "{name} {tone:?}: the pill vanishes into the band");
                    assert!(d(fg, fill) >= 40, "{name} {tone:?}: the word vanishes into the pill");
                }
                // And off the band nothing changed at all.
                for tone in [Tone::Calm, Tone::Fresh, Tone::Warn, Tone::Busy] {
                    assert_eq!(tone.colors_on(t, false), tone.colors(t), "{name} {tone:?}");
                }
            });
        }
    }

    #[test]
    fn a_row_with_no_room_does_not_panic() {
        with_canvas(Size::new(8, 8), |c| {
            with_theme(Palette::DARK, |t| {
                Chip::calm("ok").draw_right(c, Rect::from_xywh(0, 0, 4, 4), t);
                Chip::calm("ok").draw_right(c, Rect::from_xywh(0, 0, 0, 0), t);
            });
        });
    }
}
