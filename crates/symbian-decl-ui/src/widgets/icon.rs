//! One of the toolkit's twenty glyphs, as a box in the layout.
//!
//! [`symbian_ui::icon`] draws them as geometry — no font, no bitmap, no atlas — which is why there
//! are twenty and not two hundred, and why each one is legible at nine pixels. This is the shell
//! that lets one sit in a row and be measured with everything else.
//!
//! # Its size is a role, not a number
//!
//! `theme.metrics` names two: `icon_sm` (9) beside small text, `icon_md` (11) beside body text. An
//! icon written as `.size(9)` agrees with the theme until the theme changes, which is the same
//! problem [`Gap`](crate::Gap) exists to solve one level up — so the same answer is used here.
//!
//! # Width is asked, never assumed
//!
//! [`symbian_ui::icon::width_for`] is the authority: a chevron is narrower than its height and a
//! double check is wider. Reconstructing that here — even as `height` — is the mistake
//! [`Badge`](super::Badge) already made once, where a widget's `measure` computed a width its own
//! `draw` did not use and the row truncated a character early.

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::icon::{self, Icon as Glyph};
use symbian_ui::Theme;

use crate::constraints::Constraints;
use crate::widget::{hash_i32, Widget, WidgetHash};
use crate::widgets::Ink;

/// How big an icon is, named by what it sits beside.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum IconSize {
    /// `metrics.icon_sm` — beside small text: a timestamp's tick, a muted bell in a list row.
    Small,
    /// `metrics.icon_md` — beside body text. The default, because that is what a row is set in.
    #[default]
    Medium,
    /// A size the caller insists on. Three of these with the same number is a role the theme is
    /// missing.
    Exact(i32),
}

impl IconSize {
    pub fn resolve(self, theme: &Theme<'_>) -> i32 {
        match self {
            IconSize::Small => theme.metrics.icon_sm,
            IconSize::Medium => theme.metrics.icon_md,
            IconSize::Exact(px) => px,
        }
        .max(0)
    }

    fn hash(self, seed: WidgetHash) -> WidgetHash {
        let (tag, px) = match self {
            IconSize::Small => (0, 0),
            IconSize::Medium => (1, 0),
            IconSize::Exact(px) => (2, px),
        };
        hash_i32(hash_i32(seed, tag), px)
    }
}

/// A drawn glyph, coloured by role.
pub struct Icon {
    glyph: Glyph,
    size: IconSize,
    ink: Ink,
}

impl Icon {
    pub fn new(glyph: Glyph) -> Self {
        Self { glyph, size: IconSize::Medium, ink: Ink::Text }
    }

    /// The chevron a row ends with when it leads somewhere.
    ///
    /// A named constructor because it is the commonest icon in any list and because *which* chevron
    /// is a fact about the writing direction rather than a choice: a row that navigates forward ends
    /// with the one pointing forward.
    pub fn arrow() -> Self {
        Self::new(Glyph::ChevronRight).ink(Ink::Dim)
    }

    pub fn size(mut self, size: IconSize) -> Self {
        self.size = size;
        self
    }

    /// Beside small text.
    pub fn small(mut self) -> Self {
        self.size = IconSize::Small;
        self
    }

    pub fn ink(mut self, ink: Ink) -> Self {
        self.ink = ink;
        self
    }

    pub fn dim(mut self) -> Self {
        self.ink = Ink::Dim;
        self
    }

    pub fn glyph(&self) -> Glyph {
        self.glyph
    }
}

impl Widget for Icon {
    fn content_hash(&self) -> WidgetHash {
        // The glyph and the size, because both change the box. The ink does not — a recoloured
        // chevron is the same chevron in the same place.
        self.size.hash(hash_i32(0, self.glyph as u8 as i32))
    }

    fn measure(&self, constraints: Constraints, theme: &Theme<'_>) -> Size {
        let h = self.size.resolve(theme);
        // Asked, not reconstructed. See the module docs.
        constraints.constrain(Size::new(icon::width_for(self.glyph, h), h))
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        icon::draw(c, rect, self.glyph, self.ink.resolve(theme));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_ui::{testing, Palette};

    #[test]
    fn the_size_comes_from_the_theme_and_not_from_a_number() {
        testing::with_theme(Palette::DARK, |t| {
            assert_eq!(Icon::new(Glyph::Check).small().measure(Constraints::loose(50, 50), t).h, t.metrics.icon_sm);
            assert_eq!(Icon::new(Glyph::Check).measure(Constraints::loose(50, 50), t).h, t.metrics.icon_md);
            assert_eq!(
                Icon::new(Glyph::Check).size(IconSize::Exact(20)).measure(Constraints::loose(50, 50), t).h,
                20
            );
        });
    }

    #[test]
    fn the_width_is_the_one_the_drawing_uses() {
        // The `Badge` mistake, inverted: a measure that reconstructed the width would disagree with
        // the draw, and the symptom would be a row truncating a character early rather than a
        // misshapen icon.
        testing::with_theme(Palette::DARK, |t| {
            for glyph in [Glyph::ChevronRight, Glyph::CheckDouble, Glyph::Dot, Glyph::Menu] {
                let h = t.metrics.icon_md;
                let got = Icon::new(glyph).measure(Constraints::loose(100, 100), t);
                assert_eq!(got, Size::new(icon::width_for(glyph, h), h), "{glyph:?}");
            }
        });
    }

    #[test]
    fn the_glyph_and_the_size_are_in_the_digest_and_the_colour_is_not() {
        assert_ne!(Icon::new(Glyph::Check).content_hash(), Icon::new(Glyph::Warning).content_hash());
        assert_ne!(Icon::new(Glyph::Check).content_hash(), Icon::new(Glyph::Check).small().content_hash());
        // A recoloured chevron is the same chevron in the same box.
        assert_eq!(Icon::new(Glyph::Check).content_hash(), Icon::new(Glyph::Check).dim().content_hash());
        assert_ne!(Icon::new(Glyph::Check).content_hash(), 0);
    }

    #[test]
    fn an_arrow_is_the_chevron_that_points_the_way_a_row_goes() {
        assert_eq!(Icon::arrow().glyph(), Glyph::ChevronRight);
    }

    #[test]
    fn it_paints_something_inside_its_box_and_nothing_outside() {
        let (_, buf) = testing::with_canvas(Size::new(30, 30), |c| {
            testing::with_theme(Palette::DARK, |t| {
                c.clear(Palette::DARK.bg.mid());
                Icon::new(Glyph::Check).size(IconSize::Exact(10)).draw(c, Rect::from_xywh(10, 10, 10, 10), t);
            });
        });
        let bg = Palette::DARK.bg.mid().to_rgb565().0;
        let mut any = false;
        for y in 0..30 {
            for x in 0..30 {
                if buf[y * 30 + x] != bg {
                    any = true;
                    assert!((10..20).contains(&x) && (10..20).contains(&y), "ink at {x},{y}");
                }
            }
        }
        assert!(any, "a check mark should have drawn something");
    }
}
