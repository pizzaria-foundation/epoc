//! A heading between groups of rows. KaiUI calls it a Separator; S60 draws it as a band.
//!
//! # Why it is not a [`ListItem`] with different colours
//!
//! Because the difference that matters is not visual. A heading is **not a stop**: no cursor lands on
//! it, no key reaches it, and it is added to a [`FocusScope`](super::FocusScope) through
//! [`fixed`](super::FocusScope::fixed) rather than `stop`. Built as a row it would look right and be
//! reachable, and the symptom of that is a D-pad press that appears to do nothing — the cursor is
//! sitting on a word.
//!
//! It is also shorter than a row, deliberately. A heading the height of the rows it introduces reads
//! as one of them.
//!
//! # Inside a [`ScrollList`](super::ScrollList), it costs a row
//!
//! [`ScrollList::mixed`](super::ScrollList::mixed) takes a
//! [`RowHeight`](crate::spacing::RowHeight) per entry, so a heading goes in as
//! `RowHeight::Header` and the list reserves the right band for it without the screen resolving
//! anything. What the screen still owns is the **mapping**: which index is a heading and which is a
//! row, since that is a fact about its own data. A list that worked it out for itself would be a
//! second model of the same thing, living where it cannot be tested.
//!
//! The cursor is the other half, and it is not solved here. `ListState` moves one row at a time and
//! does not know that a heading is unselectable, so a screen with headings inside the scroll has to
//! skip them in its own `update`. That is a real gap, written down rather than papered over — when a
//! second screen wants it, the arithmetic belongs beside `symbian_ui::focus`.

use alloc::string::String;

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::{paint, Theme};

use crate::constraints::Constraints;
use crate::spacing::Gap;
use crate::theme::FontRole;
use crate::widget::{hash_i32, hash_str, Widget, WidgetHash};
use crate::widgets::Ink;

/// A label introducing the rows below it.
pub struct SectionHeader {
    label: String,
    /// Whether to draw the palette's chrome band behind it, the way S60 does — or leave it on the
    /// screen background with only a rule under it.
    banded: bool,
    pad: Gap,
}

impl SectionHeader {
    pub fn new(label: impl Into<String>) -> Self {
        Self { label: label.into(), banded: true, pad: Gap::Base }
    }

    /// Draw it flat on the background with a hairline under it instead of on a band.
    ///
    /// The quieter of the two, and the right one when the headings are frequent: a band every four
    /// rows turns a list into stripes.
    pub fn plain(mut self) -> Self {
        self.banded = false;
        self
    }

    /// Side padding. Defaults to the row margin, so a heading's text lines up with the rows under it
    /// rather than with the edge of the screen.
    pub fn pad(mut self, g: impl Into<Gap>) -> Self {
        self.pad = g.into();
        self
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    /// How tall a heading is: its small text plus a little air.
    ///
    /// **Delegates to [`RowHeight::Header`](crate::spacing::RowHeight::Header)** rather than holding
    /// the expression, so the two cannot drift. A list told a heading is one height while the heading
    /// measures another scrolls a fraction short of its last row for ever, and the symptom is a row
    /// nobody can quite reach — the kind of thing that gets blamed on the scroll arithmetic.
    ///
    /// Still public, because a screen assembling a mixed list may want the number for its own
    /// reasons; the kind is the better thing to pass to
    /// [`ScrollList::mixed`](super::ScrollList::mixed).
    pub fn height(theme: &Theme<'_>) -> i32 {
        crate::spacing::RowHeight::Header.resolve(theme)
    }
}

impl Widget for SectionHeader {
    fn content_hash(&self) -> WidgetHash {
        self.pad.hash(hash_i32(hash_str(0, &self.label), self.banded as i32))
    }

    fn measure(&self, constraints: Constraints, theme: &Theme<'_>) -> Size {
        // As wide as offered: a band that shrank to its label would be a stripe ending mid-screen.
        constraints.constrain(Size::new(constraints.max_w, Self::height(theme)))
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        if self.banded {
            paint::band(c, rect, &theme.palette.chrome);
        } else {
            // A rule along the bottom rather than a fill. Derived from the background by
            // `separator_for`, so it inverts correctly on a light palette instead of being a lighter
            // line on a lighter surface.
            paint::separator_for(c, rect.y1 - 1, rect.x0, rect.x1, theme.palette.bg.mid());
        }
        let ink = if self.banded { Ink::Chrome } else { Ink::Dim };
        let inset = self.pad.resolve(theme);
        let band = Rect { x0: rect.x0 + inset, x1: rect.x1 - inset, ..rect };
        c.draw_text_in(
            band,
            &self.label,
            FontRole::Small.font(theme),
            ink.resolve(theme),
            symbian_gfx::Align::Start,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_ui::{testing, Handled, Key, KeyEvent, Palette};

    #[test]
    fn a_heading_is_shorter_than_a_row() {
        // A heading the height of the rows it introduces reads as one of them.
        testing::with_theme(Palette::DARK, |t| {
            assert!(SectionHeader::height(t) < t.metrics.row_h);
            let got = SectionHeader::new("Network").measure(Constraints::loose(320, 240), t);
            assert_eq!(got, Size::new(320, SectionHeader::height(t)));
        });
    }

    #[test]
    fn the_header_role_is_the_headers_own_height() {
        // The pin between `RowHeight::Header` and this widget. If these ever part company, a mixed
        // list reserves one number and the heading paints another.
        testing::with_theme(Palette::DARK, |t| {
            assert_eq!(SectionHeader::height(t), crate::spacing::RowHeight::Header.resolve(t));
        });
    }

    #[test]
    fn the_published_height_is_the_height_it_measures() {
        // A screen putting headings in a `ScrollList` builds the `Varying` list from
        // `SectionHeader::height`, so the two must be the same number — a divergence is a list that
        // scrolls a fraction short of its last row.
        testing::with_theme(Palette::DARK, |t| {
            for h in [SectionHeader::new("A"), SectionHeader::new("A").plain(), SectionHeader::new("A").pad(0)] {
                assert_eq!(h.measure(Constraints::loose(320, 240), t).h, SectionHeader::height(t));
            }
        });
    }

    #[test]
    fn it_is_as_wide_as_it_is_offered() {
        testing::with_theme(Palette::DARK, |t| {
            assert_eq!(SectionHeader::new("Network").measure(Constraints::loose(120, 240), t).w, 120);
        });
    }

    #[test]
    fn a_heading_takes_no_keys() {
        // The property that makes it safe between two settings rows: it cannot swallow the arrow that
        // moves between them. `FocusScope::fixed` is the other half, at the call site.
        testing::with_theme(Palette::DARK, |_t| {
            crate::widget::with_key_ctx(|cx| {
                let h = SectionHeader::new("Network");
                for key in [Key::Up, Key::Down, Key::Select, Key::Left, Key::Right] {
                    assert_eq!(
                        h.handle_key(KeyEvent::new(key), Rect::from_xywh(0, 0, 100, 16), cx),
                        Handled::Ignored,
                        "{key:?}"
                    );
                }
            });
        });
    }

    #[test]
    fn banded_and_plain_are_different_pixels_and_different_digests() {
        let (_, banded) = testing::with_canvas(Size::new(100, 20), |c| {
            testing::with_theme(Palette::DARK, |t| {
                c.clear(Palette::DARK.bg.mid());
                SectionHeader::new("Network").draw(c, Rect::from_xywh(0, 0, 100, 16), t);
            });
        });
        let (_, plain) = testing::with_canvas(Size::new(100, 20), |c| {
            testing::with_theme(Palette::DARK, |t| {
                c.clear(Palette::DARK.bg.mid());
                SectionHeader::new("Network").plain().draw(c, Rect::from_xywh(0, 0, 100, 16), t);
            });
        });
        assert_ne!(banded, plain);
        assert_ne!(
            SectionHeader::new("Network").content_hash(),
            SectionHeader::new("Network").plain().content_hash()
        );
    }

    #[test]
    fn the_digest_moves_with_the_label_and_is_never_zero() {
        assert_ne!(SectionHeader::new("A").content_hash(), SectionHeader::new("B").content_hash());
        assert_ne!(SectionHeader::new("A").content_hash(), 0);
    }

    #[test]
    fn nothing_is_painted_outside_the_heading() {
        // A band is a fill, and a fill that ran a pixel over would eat the first row under it.
        let (_, buf) = testing::with_canvas(Size::new(60, 30), |c| {
            testing::with_theme(Palette::DARK, |t| {
                c.clear(Palette::DARK.bg.mid());
                SectionHeader::new("Net").draw(c, Rect::from_xywh(0, 5, 60, 14), t);
            });
        });
        let bg = Palette::DARK.bg.mid().to_rgb565().0;
        for y in 0..30 {
            if (5..19).contains(&y) {
                continue;
            }
            for x in 0..60 {
                assert_eq!(buf[y * 60 + x], bg, "ink at {x},{y}");
            }
        }
    }
}
