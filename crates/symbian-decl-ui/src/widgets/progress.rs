//! A known fraction, as the thin bar under the line of text that names it.
//!
//! ```ignore
//! Column::new().gap(Gap::Snug)
//!     .child(Text::new(&job.name))
//!     .child(Text::new(ProgressBar::label(job.got, job.total)).font(FontRole::Small).ink(Ink::Dim))
//!     .child(Node::leaf(ProgressBar::of(job.got, job.total).flex(1)))
//! ```
//!
//! # This file contains no arithmetic
//!
//! All of it is [`symbian_ui::meter`]'s: the clamp on a server that oversends, the floor that keeps
//! one percent from being a hairline nobody can see on this panel, the rounding of the fill, the
//! radius. What lives here is the shell — where the fraction comes from, how wide the box is, and
//! how a bar four pixels tall survives being handed a 38-pixel band.
//!
//! # It is the bar, not the row
//!
//! There is no label inside it and no percentage drawn on it. [`Text`](super::Text) already sets a
//! caption and [`ListItem`](super::ListItem) already owns a row's band and margins, both with parity
//! tests behind them; a bar that drew its own caption would be a second piece of typography that
//! agrees with theirs on the day it was written. [`ProgressBar::label`] hands back the string so the
//! caption is still `symbian_ui`'s wording — `184/313 KB · 58%`, the amount first — rather than each
//! screen inventing one.
//!
//! # The width is sixty, not the line
//!
//! A bar has no natural width, and "no natural width" is not the same answer as "all of it". The
//! first [`Slider`](super::Slider) returned the offer and ate the label off every row it sat in,
//! because the layout's first pass offers *every* fixed child the whole line and a greedy fixed
//! child leaves nothing for the flexible ones. So this takes [`symbian_ui::meter::BAR_W`] and grows
//! only when told to, with `.flex(1)`.

use alloc::string::String;

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::meter::{self, Meter};
use symbian_ui::Theme;

use crate::constraints::Constraints;
use crate::widget::{hash_i32, hash_str, Widget, WidgetHash};

/// How much of something is done, as a filled track.
pub struct ProgressBar {
    /// `0.0` to `1.0`. Not clamped here — [`Meter::Fraction`] clamps at the draw, and clamping twice
    /// would mean two places that decide what an over-sending server looks like.
    fraction: f32,
    /// Whether this sits on the selection band. The fill and track colours differ there, and
    /// [`symbian_ui::meter::colors`] owns that choice for this and for the imperative caller both.
    selected: bool,
    /// This bar's share of its parent's leftover space. `0` means it takes `BAR_W` and leaves the
    /// rest of the line alone — see the module docs.
    flex: i32,
}

impl ProgressBar {
    /// A bar filled to `fraction`, `0.0` to `1.0`.
    pub fn new(fraction: f32) -> Self {
        Self { fraction, selected: false, flex: 0 }
    }

    /// A bar at `pct` percent, for a model that counts in whole numbers.
    ///
    /// Worth having as its own constructor because `ProgressBar::new(57)` does not compile and
    /// `ProgressBar::new(57.0)` draws a full bar — a mistake that is silent, since a clamped
    /// fraction of 57 looks exactly like a finished download.
    pub fn percent(pct: i32) -> Self {
        Self::new(pct as f32 / 100.0)
    }

    /// A bar for `got` of `total` bytes.
    ///
    /// A `total` of zero is a full bar's worth of nothing to say, so it reads as empty rather than
    /// as complete: `Content-Length` is optional, and a division by it is where an unknown size
    /// becomes a lie. A job that does not know its total wants [`Spinner`](super::Spinner), which is
    /// what [`Meter::of`] returns `None` for.
    pub fn of(got: u64, total: u64) -> Self {
        if total == 0 {
            return Self::new(0.0);
        }
        Self::new(got as f32 / total as f32)
    }

    /// Whether this bar is on the selected row.
    pub fn selected(mut self, on: bool) -> Self {
        self.selected = on;
        self
    }

    /// Take a share of the parent's leftover space instead of a fixed width.
    ///
    /// A bar on its own line wants `flex(1)`; a bar at the end of a labelled row wants the default.
    /// See the module docs for what taking the line by default cost the first `Slider`.
    pub fn flex(mut self, weight: i32) -> Self {
        self.flex = weight.max(0);
        self
    }

    /// The caption that goes beside it: `184/313 KB · 58%`, or `5 KB` when the total is unknown.
    ///
    /// Forwarded to [`Meter::label`] rather than reimplemented, so a declarative download screen and
    /// the hand-written one word the same wait the same way. The amount is first and the percentage
    /// second on purpose — the percentage is how a person judges the wait, not what they wanted.
    pub fn label(got: u64, total: u64) -> String {
        Meter::label(got, total)
    }

    /// What the model said, before the draw clamps it.
    pub fn fraction(&self) -> f32 {
        self.fraction
    }

    /// The meter this draws — the single place the two modes are chosen between.
    fn meter(&self) -> Meter {
        Meter::Fraction(self.fraction)
    }
}

impl Widget for ProgressBar {
    fn content_hash(&self) -> WidgetHash {
        // The fraction is *not* in here, and that is the whole point of the widget being a fixed box:
        // a bar is the same size at 0% and at 100%, and a digest that folded the fraction in would
        // re-measure the row on every packet that arrived — the download screen being the one screen
        // where that happens sixty times a second. `selected` is out for the same reason, since it
        // only chooses two colours.
        //
        // What *does* change the size is `flex`, because a flexed bar answers with the offer and a
        // fixed one answers with `BAR_W`. And it is a constant rather than zero: zero means
        // "re-measure me every frame", which would put the whole row on the slow path for ever.
        hash_i32(hash_str(0, "progress"), self.flex)
    }

    fn measure(&self, constraints: Constraints, theme: &Theme<'_>) -> Size {
        // The height is the theme's, not the offer's: `meter::height` derives it from `row_h` and
        // clamps 4..8 already, so there is nothing here to clamp a second time. That is the
        // difference from `Switch` and `Slider`, whose heights *are* functions of the band — a bar
        // that scaled with its band would be a slab in a list row.
        let h = meter::height(theme);
        let w = if self.flex > 0 { constraints.max_w } else { meter::BAR_W };
        constraints.constrain(Size::new(w, h))
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        // Through `meter::track` and not into `rect`: `CrossAlign::Stretch` on a list row hands this
        // the whole 38-pixel band, and a four-pixel annotation drawn into that is a slab across the
        // row. The same helper the imperative caller uses, so neither can centre it differently.
        self.meter().draw_on(c, meter::track(rect, theme), theme, self.selected);
    }

    fn flex_weight(&self) -> i32 {
        self.flex
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_ui::{testing, Palette};

    const ROW: Rect = Rect { x0: 0, y0: 0, x1: 120, y1: 38 };

    fn paint(bar: &ProgressBar) -> Vec<u16> {
        let (_, buf) = testing::with_canvas(Size::new(120, 38), |c| {
            testing::with_theme(Palette::DARK, |t| {
                c.clear(Palette::DARK.bg.mid());
                bar.draw(c, ROW, t);
            });
        });
        buf
    }

    #[test]
    fn a_plain_bar_takes_its_own_width_and_leaves_the_rest_of_the_row_alone() {
        // The `Slider` defect, which this widget would have repeated for free: the layout's first
        // pass offers every fixed child the whole line, so a bar that answered with the offer took
        // the label with it and the row rendered as a bare track with no idea what it was for.
        testing::with_theme(Palette::DARK, |t| {
            let got = ProgressBar::new(0.5).measure(Constraints::loose(120, 38), t);
            assert_eq!(got, Size::new(meter::BAR_W, meter::height(t)));
            // And never wider than the offer, however small it is.
            assert_eq!(ProgressBar::new(0.5).measure(Constraints::loose(20, 38), t).w, 20);
        });
    }

    #[test]
    fn a_flexed_bar_takes_the_line() {
        testing::with_theme(Palette::DARK, |t| {
            let got = ProgressBar::new(0.5).flex(1).measure(Constraints::loose(120, 38), t);
            assert_eq!(got.w, 120);
            assert_eq!(ProgressBar::new(0.5).flex(1).flex_weight(), 1);
            assert_eq!(ProgressBar::new(0.5).flex_weight(), 0);
            assert_eq!(ProgressBar::new(0.5).flex(-3).flex_weight(), 0, "a negative share is none");
        });
    }

    #[test]
    fn the_height_is_the_themes_and_not_the_bands() {
        // A bar is an annotation of the line above it, not a second row. `Switch` and `Slider` scale
        // with the band they are offered; this one deliberately does not, and a 38-pixel offer must
        // not produce a 30-pixel bar.
        testing::with_theme(Palette::DARK, |t| {
            let tall = ProgressBar::new(0.5).measure(Constraints::loose(120, 240), t);
            let short = ProgressBar::new(0.5).measure(Constraints::loose(120, 12), t);
            assert_eq!(tall.h, meter::height(t));
            assert_eq!(tall.h, short.h);
            assert!(tall.h <= 8, "a meter is thin: {}", tall.h);
        });
    }

    #[test]
    fn the_stretch_a_list_row_applies_does_not_fatten_the_bar() {
        // The trap every control in this catalogue shares: the row hands over 38 pixels.
        let buf = paint(&ProgressBar::new(0.5));
        let bg = Palette::DARK.bg.mid().to_rgb565().0;
        let rows: Vec<i32> =
            (0..38).filter(|&y| (0..120).any(|x| buf[(y * 120 + x) as usize] != bg)).collect();
        let h = testing::with_theme(Palette::DARK, meter::height);
        assert_eq!(rows.len() as i32, h, "the bar is its own height, not the band's");
        assert_eq!(rows[0], (38 - h) / 2, "and centred in it");
    }

    #[test]
    fn more_progress_is_more_fill_in_every_palette() {
        // Counted as "pixels that differ from the empty bar", not as "pixels of the accent". Two
        // tests in this repo counted `Palette::accent` to prove this property and both went red the
        // moment the fill's colour changed for a good reason — the count was measuring one of the
        // property's answers rather than the property.
        for (name, palette) in Palette::ALL {
            let paint = |f: f32| {
                let (_, buf) = testing::with_canvas(Size::new(120, 38), |c| {
                    testing::with_theme(palette, |t| {
                        c.clear(palette.bg.mid());
                        ProgressBar::new(f).flex(1).draw(c, ROW, t);
                    });
                });
                buf
            };
            let empty = paint(0.0);
            let differs = |f: f32| paint(f).iter().zip(empty.iter()).filter(|(a, b)| a != b).count();
            assert_eq!(differs(0.0), 0, "{name}: an empty bar differs from itself somewhere");
            assert!(differs(0.5) > 0, "{name}: half a bar is indistinguishable from none");
            assert!(differs(1.0) > differs(0.5), "{name}: a full bar is no fuller than half");
        }
    }

    #[test]
    fn the_band_changes_the_colours_and_not_one_pixel_of_the_geometry() {
        // `chrome::control_colors`, reached through `meter::colors`. A bar painted in the page's
        // accent on the selection band is three colours picked for a ground that is not there — on
        // `HIGH_CONTRAST` a white slab on a white band.
        // Cleared to a colour no palette contains rather than to `bg`, and that is not fussiness:
        // in `Light` the meter's own track *is* the page colour, so a count of "pixels unlike the
        // background" reports the fill alone off the band and the whole track on it, and the two
        // differ by the width of the track for a reason that has nothing to do with geometry.
        const GROUND: symbian_gfx::Color = symbian_gfx::Color::hex(0xFF00FF);
        for (name, palette) in Palette::ALL {
            let paint = |selected: bool| {
                let (_, buf) = testing::with_canvas(Size::new(120, 38), |c| {
                    testing::with_theme(palette, |t| {
                        c.clear(GROUND);
                        ProgressBar::new(0.5).flex(1).selected(selected).draw(c, ROW, t);
                    });
                });
                buf
            };
            let (off, on) = (paint(false), paint(true));
            assert_ne!(off, on, "{name}: the band changed nothing");
            let ground = GROUND.to_rgb565().0;
            assert_eq!(
                off.iter().filter(|&&p| p != ground).count(),
                on.iter().filter(|&&p| p != ground).count(),
                "{name}: the same pixels in different colours — a ring would change the count"
            );
        }
    }

    #[test]
    fn a_server_that_oversends_cannot_draw_outside_the_track() {
        // Clamped once, in `symbian_ui::meter`, and not a second time here. What this asserts is that
        // the shell does not defeat it: an over-full bar is a full bar and not a panic.
        let full = paint(&ProgressBar::new(1.0));
        assert_eq!(paint(&ProgressBar::new(2.5)), full);
        assert_eq!(paint(&ProgressBar::new(-1.0)), paint(&ProgressBar::new(0.0)));
        assert_ne!(paint(&ProgressBar::new(0.0)), full, "the comparison tells nothing apart");
        let _ = paint(&ProgressBar::new(f32::NAN));
    }

    #[test]
    fn a_job_with_no_known_total_reads_as_empty_rather_than_as_finished() {
        // `got as f32 / 0` is infinity, which clamps to a full bar — a download that has not started
        // drawn as one that has finished. `Content-Length` is optional, so this is the ordinary case
        // and not the exotic one; the honest answer is a `Spinner`.
        assert_eq!(ProgressBar::of(0, 0).fraction(), 0.0);
        assert_eq!(ProgressBar::of(5_000, 0).fraction(), 0.0);
        assert_eq!(ProgressBar::of(50, 100).fraction(), 0.5);
    }

    #[test]
    fn percent_takes_whole_numbers_because_new_would_take_them_silently() {
        assert_eq!(ProgressBar::percent(0).fraction(), 0.0);
        assert_eq!(ProgressBar::percent(57).fraction(), 0.57);
        assert_eq!(ProgressBar::percent(100).fraction(), 1.0);
    }

    #[test]
    fn the_caption_is_the_toolkits_wording_and_not_a_second_one() {
        // So a declarative download screen and the hand-written one word the same wait the same way.
        assert_eq!(ProgressBar::label(184_320, 320_484), Meter::label(184_320, 320_484));
        assert_eq!(ProgressBar::label(5_000, 0), "5 KB");
    }

    #[test]
    fn the_fraction_is_not_in_the_digest_but_the_flex_is_and_it_is_never_zero() {
        // The fraction moves no pixel of the box, and it changes sixty times a second on the one
        // screen this widget exists for — folding it in would re-measure the row on every packet.
        let a = ProgressBar::new(0.0);
        let b = ProgressBar::new(0.99).selected(true);
        assert_eq!(a.content_hash(), b.content_hash());
        // The flex *does* change the box: fixed answers `BAR_W`, flexed answers the offer.
        assert_ne!(a.content_hash(), ProgressBar::new(0.0).flex(1).content_hash());
        assert_ne!(a.content_hash(), 0);
    }

    #[test]
    fn a_degenerate_rect_is_a_no_op_rather_than_a_panic() {
        testing::with_canvas(Size::new(10, 10), |c| {
            testing::with_theme(Palette::DARK, |t| {
                ProgressBar::new(0.5).draw(c, Rect::from_xywh(0, 0, 0, 0), t);
                ProgressBar::new(0.5).draw(c, Rect::from_xywh(0, 0, 10, 1), t);
                ProgressBar::new(0.5).measure(Constraints::loose(0, 0), t);
            });
        });
    }

    #[test]
    fn a_label_beside_a_bar_keeps_its_room() {
        // The defect at the level it appeared at. The label flexes, the bar does not, and the label's
        // rect is most of the line rather than nothing.
        use crate::widgets::{ListItem, Node};
        let root = ListItem::new("Downloading")
            .trailing_node(Node::leaf(ProgressBar::new(0.4)))
            .build();
        let label = testing::with_theme(Palette::DARK, |theme| {
            let mut cache = crate::UiCache::with_capacity(root.slot_count());
            crate::layout::place_frame(&root, ROW, &mut cache, theme);
            cache.rect(2).expect("the label was placed")
        });
        assert!(label.width() > 40, "the label got {}px of 120", label.width());
    }
}
