//! A rounded square with one letter in it: the icon a row has when it has no icon.

use alloc::string::String;

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::{tile, Theme};

use crate::constraints::Constraints;
use crate::widget::{hash_i32, hash_str, Widget, WidgetHash};

/// The letter tile, as something a row can carry.
///
/// A leaf over [`symbian_ui::tile::letter_tile`], which already picks a stable colour from a seed
/// and centres the caption's first letter in a rounded square. Nothing here re-derives any of that —
/// the whole widget is a size and a delegation, the shape [`Avatar`](super::Avatar) argues for and
/// most of this catalogue should have.
///
/// # Why this exists beside `Avatar`, which looks like the same thing
///
/// They are two drawings, not one, and the difference is not decoration:
///
/// | | [`Avatar`](super::Avatar) | `Tile` |
/// |---|---|---|
/// | shape | a circle | a rounded square |
/// | palette | eight **muted** hues, picked by hand | eight **saturated** ones |
/// | letters | up to two, supplied by the caller | one, taken from the caption |
///
/// A circle of initials is a *person* — that is what `tg`'s chat rows use it for. A square is a
/// *thing you can open*, which is what a launcher draws when an application has no icon of its own,
/// and what a list of subjects wants. The same seed gives different colours in the two, so they are
/// not interchangeable even where both would fit.
///
/// # Square by construction
///
/// `letter_tile` fills whatever rect it is handed and does **not** square it — unlike
/// `chrome::avatar`, which does. So a non-square rect draws a stretched lozenge with no error at
/// all. This asks for the square instead and lets the row's cross-axis alignment place it.
pub struct Tile {
    caption: String,
    seed: u32,
    /// The edge, in pixels. `None` means "as tall as you will let me be", which is what a list row
    /// wants: the tile tracks the row height rather than being told it twice.
    size: Option<i32>,
}

impl Tile {
    /// A tile whose letter is the first character of `caption`, coloured by `seed`.
    ///
    /// The seed is used raw, modulo the palette — there is no hashing — so a caller wanting eight
    /// distinguishable tiles should pass 0..8 rather than, say, eight UIDs that may collide.
    pub fn new(caption: impl Into<String>, seed: u32) -> Self {
        Self { caption: caption.into(), seed, size: None }
    }

    /// Fix the edge rather than taking it from the offer.
    pub fn size(mut self, px: i32) -> Self {
        self.size = Some(px.max(0));
        self
    }
}

impl Widget for Tile {
    /// The caption and the seed both change the picture; the size changes the box.
    ///
    /// The whole caption and not just its first letter: two captions that begin alike draw the same
    /// tile *today*, and a digest that folded in only the letter would be encoding that as a
    /// promise. See [`Avatar::content_hash`](super::Avatar) for the same argument about its seed.
    fn content_hash(&self) -> WidgetHash {
        let h = hash_str(0, &self.caption);
        let h = hash_i32(h, self.seed as i32);
        hash_i32(h, self.size.unwrap_or(-1))
    }

    fn measure(&self, constraints: Constraints, theme: &Theme<'_>) -> Size {
        let edge = match self.size {
            Some(px) => px,
            // Clamped to a row, and the clamp is the point — the lesson `Avatar` paid for. Taking
            // the offered height is right in a list, where a row is 38 pixels by construction, and
            // catastrophic outside one: in a column the offer is the whole remaining page, and an
            // unclamped tile would measure a page-sized square and push everything below it off the
            // screen. That is not hypothetical; it is what an avatar did in `apps/uigallery`.
            None => constraints.max_h.min(constraints.max_w).min(theme.metrics.row_h),
        };
        constraints.constrain(Size::new(edge, edge))
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        // Squared and centred here, not just asked for in `measure` — the `CrossAlign::Stretch`
        // trap every control in this catalogue shares. A list row applies `Stretch`, which hands the
        // leading widget the **whole** row band whatever it measured, and `letter_tile` fills what it
        // is given without squaring. So a tile in a 34-pixel row drew a 34-pixel tall lozenge that
        // touched the tiles above and below it, and three of them read as one striped column.
        //
        // `chrome::avatar` does this internally, which is why `Avatar` never had to. `letter_tile`
        // does not, so this widget is where it belongs.
        let edge = rect.width().min(rect.height()).min(self.size.unwrap_or(i32::MAX));
        let box_ = Rect::from_xywh(
            rect.x0 + (rect.width() - edge) / 2,
            rect.y0 + (rect.height() - edge) / 2,
            edge,
            edge,
        );
        tile::letter_tile(c, box_, &self.caption, self.seed, theme);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_ui::{testing, Palette};

    fn paint(t: &Tile, w: i32, h: i32) -> alloc::vec::Vec<u16> {
        let (_, buf) = testing::with_canvas(Size::new(w, h), |c| {
            testing::with_theme(Palette::DARK, |th| {
                c.clear(Palette::DARK.bg.mid());
                t.draw(c, Rect::from_xywh(0, 0, w, h), th);
            });
        });
        buf
    }

    #[test]
    fn it_is_square_and_never_larger_than_a_row() {
        // The defect this clamp exists for, asserted rather than remembered: handed a whole page —
        // which is what a column offers — an unclamped tile measures a page-sized square. `Avatar`
        // shipped that and the gallery found it.
        testing::with_theme(Palette::DARK, |t| {
            let page = Tile::new("Boot", 0).measure(Constraints::loose(320, 205), t);
            assert_eq!(page.w, page.h, "square, or it is a lozenge");
            assert_eq!(page.w, t.metrics.row_h, "and no taller than a row");

            let row = Tile::new("Boot", 0).measure(Constraints::loose(320, 38), t);
            assert_eq!(row, Size::new(38, 38), "in a list it is the row");
        });
    }

    #[test]
    fn an_explicit_size_wins_over_the_offer() {
        testing::with_theme(Palette::DARK, |t| {
            let s = Tile::new("Boot", 0).size(20).measure(Constraints::loose(320, 205), t);
            assert_eq!(s, Size::new(20, 20));
        });
    }

    #[test]
    fn the_seed_changes_the_colour() {
        // The half the one-glyph test atlas can see: colour is geometry-free.
        assert_ne!(paint(&Tile::new("Boot", 0), 24, 24), paint(&Tile::new("Boot", 1), 24, 24));
    }

    #[test]
    fn the_caption_changes_the_letter() {
        // The half it cannot. `symbian_ui::testing`'s atlas has **one glyph**, so `B` and `P` draw
        // the same picture and this assertion passes no matter what — I wrote it that way first and
        // it failed for the opposite reason, which is the only reason I noticed. This crate's own
        // Cargo.toml says it outright: "the unit tests prove the arithmetic against a one-glyph test
        // atlas; only the real fonts can prove the pixels."
        let atlases = symbian_preview::Atlases::load();
        atlases.with_themes(|theme, _light| {
            let paint_real = |t: &Tile| {
                let (_, buf) = testing::with_canvas(Size::new(24, 24), |c| {
                    c.clear(Palette::DARK.bg.mid());
                    t.draw(c, Rect::from_xywh(0, 0, 24, 24), theme);
                });
                buf
            };
            assert_ne!(paint_real(&Tile::new("Boot", 0)), paint_real(&Tile::new("Packages", 0)));
        });
    }

    #[test]
    fn the_same_seed_is_the_same_colour_every_time() {
        // `letter_tile` indexes its palette with `seed % 8` and hashes nothing, so this is a
        // property rather than a coincidence — and a caller picking seeds needs to be able to rely
        // on it.
        assert_eq!(paint(&Tile::new("Boot", 3), 24, 24), paint(&Tile::new("Boot", 11), 24, 24));
    }

    #[test]
    fn the_digest_moves_with_everything_that_moves_the_picture() {
        let a = Tile::new("Boot", 0);
        assert_ne!(a.content_hash(), Tile::new("Boot", 1).content_hash(), "seed");
        assert_ne!(a.content_hash(), Tile::new("Packages", 0).content_hash(), "caption");
        assert_ne!(a.content_hash(), Tile::new("Boot", 0).size(20).content_hash(), "size");
        assert_ne!(a.content_hash(), 0);
    }

    #[test]
    fn a_stretched_row_does_not_make_a_lozenge() {
        // The trap every control in this catalogue shares, and this one shipped with it: a list row
        // applies `CrossAlign::Stretch`, so the leading widget is handed the whole band whatever it
        // measured. `letter_tile` fills what it is given, so the tile drew the full row height and
        // three of them touched. Found by looking at the render, not by reading the code.
        let (w, h) = (60, 34);
        let buf = paint(&Tile::new("Boot", 0), w, h);
        let bg = Palette::DARK.bg.mid().to_rgb565().0;
        let rows: alloc::vec::Vec<i32> =
            (0..h).filter(|&y| (0..w).any(|x| buf[(y * w + x) as usize] != bg)).collect();
        let cols: alloc::vec::Vec<i32> =
            (0..w).filter(|&x| (0..h).any(|y| buf[(y * w + x) as usize] != bg)).collect();
        // Square is the property. The *height* filling the band is correct with no explicit size —
        // "as tall as you will let me be" is what `None` means — and the bug was the **width**
        // following it, which is what made a 60x34 rect draw a 60-wide lozenge.
        assert_eq!(rows.len(), cols.len(), "square, or it is a lozenge");
        assert_eq!(rows.len(), h as usize, "with no size it is the band's height");
        assert_eq!(cols[0], (w - cols.len() as i32) / 2, "and centred across the width");

        // With a size it is that size, centred both ways — which is what a row of tiles needs so
        // they do not touch each other.
        let buf = paint(&Tile::new("Boot", 0).size(24), w, h);
        let rows: alloc::vec::Vec<i32> =
            (0..h).filter(|&y| (0..w).any(|x| buf[(y * w + x) as usize] != bg)).collect();
        assert_eq!(rows.len(), 24);
        assert_eq!(rows[0], (h - 24) / 2, "centred down the band, so tiles do not touch");
    }
}
