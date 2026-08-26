//! A round initials tile, for a contact or a group.

use alloc::string::String;

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::{chrome, Theme};

use crate::constraints::Constraints;
use crate::widget::{hash_i32, hash_str, Widget, WidgetHash};

/// The circle of initials that begins a chat row.
///
/// A leaf over [`symbian_ui::chrome::avatar`], which already knows how to pick a stable colour from
/// a seed and how to fit one or two letters inside a circle. Nothing here re-derives any of that —
/// the whole widget is a size and a delegation, which is the shape most of this catalogue should
/// have. A widget that reimplements what the imperative toolkit already does correctly is a second
/// copy of the same arithmetic and a second place for it to drift.
///
/// It is square by construction. `chrome::avatar` centres a circle inside whatever rect it is
/// handed, so a non-square one would draw a circle with empty space beside it and no error — this
/// asks for the square instead, and lets the row's cross-axis alignment place it.
pub struct Avatar {
    initials: String,
    seed: u32,
    /// The edge, in pixels. `None` means "as tall as you will let me be", which is what a chat row
    /// wants: the avatar tracks the row height rather than being told it twice.
    size: Option<i32>,
}

impl Avatar {
    pub fn new(initials: impl Into<String>, seed: u32) -> Self {
        Self { initials: initials.into(), seed, size: None }
    }

    /// Fix the edge rather than taking it from the offer.
    pub fn size(mut self, px: i32) -> Self {
        self.size = Some(px.max(0));
        self
    }
}

impl Widget for Avatar {
    /// The initials and the seed both change the picture; the size changes the box.
    ///
    /// The seed is in here because two contacts with the same initials and different colours are
    /// the same size — but a digest that ignored it would let one keep the other's cached *size*
    /// after a reorder, which is harmless today and would stop being harmless the moment an avatar
    /// widget grows a size that depends on its content.
    fn content_hash(&self) -> WidgetHash {
        let h = hash_str(0, &self.initials);
        let h = hash_i32(h, self.seed as i32);
        hash_i32(h, self.size.unwrap_or(-1))
    }

    fn measure(&self, constraints: Constraints, theme: &Theme<'_>) -> Size {
        // Without an explicit size, take the offered height and square it — a chat row is the case
        // this exists for, and there the height is the row and the width is whatever that needs.
        let edge = match self.size {
            Some(px) => px,
            // Clamped to a row, and the clamp is the point. Taking the offered height is right in a
            // list, where a row is 38 pixels by construction — and catastrophic outside one: inside a
            // `FocusScope` column the offer is the *whole remaining page*, so an avatar with no
            // explicit size measured 180 pixels square and pushed everything below it off the screen.
            //
            // Every other widget in the catalogue that measures from the offer already clamps
            // (`switch_height` to 10..18, `mark_size` to 9..18, `track_height` to 4..10,
            // `stepper_height` to one line). This was the only one that did not, which the gallery
            // found by being the first screen to put one outside a list.
            None => constraints.max_h.min(constraints.max_w).min(theme.metrics.row_h),
        };
        constraints.constrain(Size::new(edge, edge))
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        chrome::avatar(c, rect, theme, &self.initials, self.seed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_ui::{testing, Palette};

    #[test]
    fn it_takes_the_offered_height_and_squares_it() {
        testing::with_theme(Palette::DARK, |t| {
            // A chat row: 320 wide, 38 tall. The avatar should be 38x38, not 320x38.
            let s = Avatar::new("CE", 7).measure(Constraints::loose(320, 38), t);
            assert_eq!(s, Size::new(38, 38));
        });
    }

    #[test]
    fn a_fixed_size_ignores_the_offer_until_the_offer_refuses() {
        testing::with_theme(Palette::DARK, |t| {
            assert_eq!(Avatar::new("CE", 7).size(24).measure(Constraints::loose(320, 38), t), Size::new(24, 24));
            // But it cannot escape a box smaller than itself; the clamp is the offer's, not ours.
            assert_eq!(Avatar::new("CE", 7).size(99).measure(Constraints::loose(20, 20), t), Size::new(20, 20));
        });
    }

    #[test]
    fn a_tall_offer_cannot_make_an_avatar_bigger_than_a_row() {
        // The defect the gallery found. Inside a `FocusScope` column the offer is the whole remaining
        // page, and an avatar with no explicit size took it: 180 pixels square, with the two rows
        // below it squeezed to nothing and off the screen. Asserted against `metrics.row_h` rather
        // than a number, so a theme with taller rows moves the expectation with it.
        testing::with_theme(Palette::DARK, |t| {
            let s = Avatar::new("CE", 7).measure(Constraints::loose(320, 205), t);
            assert_eq!(s, Size::new(t.metrics.row_h, t.metrics.row_h));
        });
    }

    #[test]
    fn a_narrow_offer_wins_over_a_tall_one() {
        testing::with_theme(Palette::DARK, |t| {
            // Squaring the height would overflow a narrow column; the smaller edge decides.
            assert_eq!(Avatar::new("CE", 7).measure(Constraints::loose(10, 38), t), Size::new(10, 10));
        });
    }

    #[test]
    fn nothing_offered_is_nothing_drawn_rather_than_a_negative_box() {
        testing::with_theme(Palette::DARK, |t| {
            assert_eq!(Avatar::new("CE", 7).measure(Constraints::loose(0, 0), t), Size::new(0, 0));
        });
    }

    #[test]
    fn the_digest_moves_with_everything_that_shows() {
        let a = Avatar::new("CE", 7);
        assert_ne!(a.content_hash(), Avatar::new("MP", 7).content_hash(), "initials");
        assert_ne!(a.content_hash(), Avatar::new("CE", 8).content_hash(), "colour seed");
        assert_ne!(a.content_hash(), Avatar::new("CE", 7).size(24).content_hash(), "size");
        assert_eq!(a.content_hash(), Avatar::new("CE", 7).content_hash());
    }

    #[test]
    fn it_draws_something() {
        let (_, px) = testing::with_canvas(Size::new(64, 64), |c| {
            testing::with_theme(Palette::DARK, |t| {
                Avatar::new("CE", 7).draw(c, Rect::from_xywh(4, 4, 38, 38), t);
            });
        });
        assert!(px.iter().any(|&v| v != 0));
    }
}
