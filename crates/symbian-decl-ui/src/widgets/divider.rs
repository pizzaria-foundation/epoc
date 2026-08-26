//! A hairline between two things, as a box that takes part in the layout.
//!
//! # Why this exists when `Group::border_bottom` already does
//!
//! They divide different things, and the distinction is the same one CSS makes between a
//! `border-bottom` and an `<hr>`.
//!
//! [`Group::border_bottom`](super::Group::border_bottom) is a property of a *row*: it takes no slot,
//! claims no pixel of the main axis, and is drawn after the children. That is exactly right for the
//! line under every row of a list, where the line belongs to the row and a hundred of them must not
//! cost a hundred nodes.
//!
//! This is a divider *between sections* — one line between a form's two halves, or above a row of
//! buttons. There it genuinely occupies space: the gap above and below it is part of the design, and
//! expressed as a border it would have to be an inset on one of its neighbours, which is a number in
//! the wrong place and one that goes stale when the neighbour changes.
//!
//! # It is not a stop
//!
//! A divider goes into a [`FocusScope`](super::FocusScope) through
//! [`fixed`](super::FocusScope::fixed) rather than `stop`, so no cursor ever lands on it. That is
//! declared at the call site rather than answered by a `Widget::focusable` method, deliberately: the
//! scope counts its own stops, and a second source of the same truth is a count that can disagree
//! with itself. A cursor parked on a rule is a key that does nothing.

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::{paint, Theme};

use crate::constraints::Constraints;
use crate::widget::{hash_i32, Widget, WidgetHash};
use crate::spacing::Gap;
use crate::widgets::Ink;

/// A horizontal rule with the space around it that makes it read as a division.
pub struct Divider {
    /// How the groove is coloured. [`Ink::Divider`] is the theme's own hairline; anything else is a
    /// caller insisting, which is usually a sign the palette is missing a role.
    ink: Ink,
    /// Clear space above and below the line.
    space: Gap,
    /// How far in from each end the line starts, so it can align with the text it divides rather
    /// than with the edge of the screen.
    inset: Gap,
    /// Whether to draw the second, lighter line that makes the groove read as engraved.
    engraved: bool,
}

impl Divider {
    /// A plain hairline with the theme's ordinary spacing around it.
    pub fn new() -> Self {
        Self { ink: Ink::Divider, space: Gap::None, inset: Gap::None, engraved: false }
    }

    /// Colour the line by role. Defaults to [`Ink::Divider`].
    pub fn ink(mut self, ink: Ink) -> Self {
        self.ink = ink;
        self
    }

    /// Clear space above and below. Name it — [`Gap::Snug`], [`Gap::Base`] — rather than choosing a
    /// number: a divider spaced by hand agrees with the rest of the screen everywhere it is not
    /// looked at.
    pub fn space(mut self, g: impl Into<Gap>) -> Self {
        self.space = g.into();
        self
    }

    /// Start the line this far in from each end.
    pub fn inset(mut self, g: impl Into<Gap>) -> Self {
        self.inset = g.into();
        self
    }

    /// Draw the two-line engraved groove that S60 uses between settings sections.
    ///
    /// Derived from the surface it sits on by [`paint::separator_for`], so a theme gets it without
    /// naming two more colours — and so it inverts correctly on a light palette, where the groove
    /// has to be *darker* than the surface rather than lighter. That inversion is the reason to
    /// reach for this instead of drawing two `Ink` lines by hand.
    pub fn engraved(mut self) -> Self {
        self.engraved = true;
        self
    }

    /// Total height: the line, plus the clear space on both sides.
    fn thickness(&self) -> i32 {
        if self.engraved {
            2
        } else {
            1
        }
    }
}

impl Default for Divider {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for Divider {
    fn content_hash(&self) -> WidgetHash {
        // The inset moves the line without changing the box, so it is out — the same reason
        // `Group::align` is out of a group's digest. Everything that changes the *height* is in.
        self.space.hash(hash_i32(hash_i32(0, self.thickness()), 1))
    }

    fn measure(&self, constraints: Constraints, theme: &Theme<'_>) -> Size {
        // As wide as it is offered: a rule that wrapped its content would be a rule of zero width,
        // since it has no content. This is the one widget for which filling the cross axis is not a
        // choice but the definition.
        let w = constraints.max_w;
        constraints.constrain(Size::new(w, self.thickness() + self.space.resolve(theme) * 2))
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        let inset = self.inset.resolve(theme);
        let x0 = rect.x0 + inset;
        let x1 = rect.x1 - inset;
        if x1 <= x0 {
            return;
        }
        // Centred in the box rather than anchored to the top, so the space above and below is equal
        // whatever the box was given. A rect taller than asked for — a stretched cross axis, a
        // rounding remainder — would otherwise put the line off-centre by however much it grew.
        let y = rect.y0 + (rect.height() - self.thickness()) / 2;
        if self.engraved {
            paint::separator_for(c, y, x0, x1, theme.palette.bg.mid());
        } else {
            c.hline(y, x0, x1, self.ink.resolve(theme));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_ui::{testing, Palette};

    #[test]
    fn a_divider_is_as_wide_as_it_is_offered_and_as_tall_as_it_needs() {
        testing::with_theme(Palette::DARK, |t| {
            let d = Divider::new();
            assert_eq!(d.measure(Constraints::loose(320, 240), t), Size::new(320, 1));
            // The space is on both sides, so it counts twice.
            assert_eq!(Divider::new().space(Gap::Snug).measure(Constraints::loose(320, 240), t), Size::new(320, 9));
            // Engraved is two lines, not one.
            assert_eq!(Divider::new().engraved().measure(Constraints::loose(100, 240), t), Size::new(100, 2));
        });
    }

    #[test]
    fn the_digest_moves_when_the_height_would_and_not_when_only_the_line_moves() {
        assert_ne!(Divider::new().content_hash(), Divider::new().space(Gap::Snug).content_hash());
        assert_ne!(Divider::new().content_hash(), Divider::new().engraved().content_hash());
        // An inset slides the line inside the same box — re-measuring the subtree to move it would
        // be work with nothing to show for it.
        assert_eq!(Divider::new().content_hash(), Divider::new().inset(Gap::Wide).content_hash());
        // And it is never zero, which would opt the whole thing out of the cache.
        assert_ne!(Divider::new().content_hash(), 0);
    }

    #[test]
    fn the_line_lands_in_the_middle_of_whatever_box_it_was_given() {
        // A rect taller than measured — a stretched cross axis — must not push the line off-centre.
        let (_, buf) = testing::with_canvas(Size::new(20, 9), |c| {
            testing::with_theme(Palette::DARK, |t| {
                // Cleared first: `with_canvas` hands over a zeroed buffer, and black is not this
                // palette's background — without this every pixel reads as painted.
                c.clear(symbian_ui::Palette::DARK.bg.mid());
                Divider::new().space(Gap::Snug).draw(c, Rect::from_xywh(0, 0, 20, 9), t);
            });
        });
        let bg = symbian_ui::Palette::DARK.bg.mid().to_rgb565().0;
        let painted: Vec<usize> = (0..9).filter(|y| buf[y * 20 + 10] != bg).collect();
        assert_eq!(painted, vec![4], "one line, in the middle of a nine-pixel box");
    }

    #[test]
    fn a_divider_narrower_than_its_insets_draws_nothing_rather_than_inverting() {
        // Two insets of 20 in a 30-pixel box leave a negative span; an `hline` given x1 < x0 is a
        // rect turned inside out, which on this rasteriser is a row of pixels somewhere else.
        let (_, buf) = testing::with_canvas(Size::new(30, 3), |c| {
            testing::with_theme(Palette::DARK, |t| {
                c.clear(symbian_ui::Palette::DARK.bg.mid());
                Divider::new().inset(Gap::Exact(20)).draw(c, Rect::from_xywh(0, 0, 30, 3), t);
            });
        });
        let bg = symbian_ui::Palette::DARK.bg.mid().to_rgb565().0;
        assert!(buf.iter().all(|&p| p == bg), "nothing should have been painted");
    }

    #[test]
    fn a_divider_takes_no_keys() {
        // What makes it safe to put in a form at all: `handle_key`'s default is `Ignored`, so a
        // divider between two fields cannot swallow the arrow that moves between them.
        testing::with_theme(Palette::DARK, |_t| {
            crate::widget::with_key_ctx(|cx| {
                let ev = symbian_ui::KeyEvent::new(symbian_ui::Key::Down);
                assert_eq!(
                    Divider::new().handle_key(ev, Rect::from_xywh(0, 0, 20, 3), cx),
                    symbian_ui::Handled::Ignored
                );
            });
        });
    }
}
