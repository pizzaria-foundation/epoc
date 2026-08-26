//! Something is happening and nobody knows how much of it is left.
//!
//! ```ignore
//! // in the model, advanced by whatever already made the screen redraw:
//! self.phase = self.phase.wrapping_add(3);
//! // in the view:
//! Node::leaf(Spinner::new(self.phase).flex(1))
//! ```
//!
//! # Why this is not a `ProgressBar` at zero
//!
//! Because `Content-Length` is optional. A download whose total the server never sent would be a bar
//! stuck at 0%, and a bar stuck at 0% reads as *broken* rather than as *unknown* — which is the worse
//! of the two, since the person holding the phone stops waiting. [`symbian_ui::meter::Meter::of`]
//! takes an `Option<f32>` for exactly this reason, and `symbian_bootcfg::queue::Job::fraction`
//! returns one.
//!
//! # There is no timer in here
//!
//! The phase is a `u8` the caller advances and this widget never touches. That is not a limitation
//! working around a missing clock — it is the rule the whole toolkit runs on: the application already
//! owns the timer that made the screen redraw, and a widget with a second source of animation is a
//! second thing to keep in step with the first. `symbian_ui::meter` says the same in its own words,
//! and [`Marquee`](super::Marquee) is the other widget in this catalogue that works this way.
//!
//! It also means a spinner on a screen nobody is redrawing simply stops, which is honest: nothing is
//! being redrawn because nothing is happening.
//!
//! # The phase is not in the digest
//!
//! It changes every tick and it never changes the box. See [`Spinner::content_hash`].

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::meter::{self, Meter};
use symbian_ui::Theme;

use crate::constraints::Constraints;
use crate::widget::{hash_i32, hash_str, Widget, WidgetHash};

/// A band sweeping along a track, claiming no position.
pub struct Spinner {
    /// Where the band is, `0..=255`, wrapping. The caller's, always.
    phase: u8,
    /// Whether this sits on the selection band — see [`ProgressBar`](super::ProgressBar).
    selected: bool,
    /// This spinner's share of its parent's leftover space. `0` takes [`meter::BAR_W`].
    flex: i32,
}

impl Spinner {
    /// A spinner at `phase`.
    ///
    /// Advance it with `wrapping_add`: the arithmetic inside takes `phase % 100`, so any `u8` is a
    /// valid picture and a caller that lets it roll over past 255 gets a jump of four frames rather
    /// than a panic. That is the right failure — an animation that stutters once a wrap is invisible,
    /// and a `u8` that had to be kept under 100 would be a rule every caller could forget.
    pub fn new(phase: u8) -> Self {
        Self { phase, selected: false, flex: 0 }
    }

    /// Whether this spinner is on the selected row.
    pub fn selected(mut self, on: bool) -> Self {
        self.selected = on;
        self
    }

    /// Take a share of the parent's leftover space instead of a fixed width.
    ///
    /// Same default and same reason as [`ProgressBar::flex`](super::ProgressBar::flex): the layout's
    /// first pass offers every fixed child the whole line, so a spinner that took the offer would
    /// take the label beside it too.
    pub fn flex(mut self, weight: i32) -> Self {
        self.flex = weight.max(0);
        self
    }

    pub fn phase(&self) -> u8 {
        self.phase
    }

    fn meter(&self) -> Meter {
        Meter::Busy { phase: self.phase }
    }
}

impl Widget for Spinner {
    /// # Why the phase is not in the digest
    ///
    /// It moves every tick and it never changes the size: the box is the box, and what slides is the
    /// band inside it. A digest that folded the phase in would re-measure the row on every frame of
    /// the animation — turning the widget that exists to say "this is still moving" into the one that
    /// makes a waiting screen expensive. [`Marquee`](super::Marquee) records the identical decision.
    ///
    /// `flex` *is* in it, because a flexed spinner answers with the offer and a fixed one answers
    /// with `BAR_W`, and those are two different boxes.
    ///
    /// A constant, never zero: zero means "re-measure me every frame", which is the slow path this
    /// paragraph exists to keep the row off.
    fn content_hash(&self) -> WidgetHash {
        hash_i32(hash_str(0, "spinner"), self.flex)
    }

    fn measure(&self, constraints: Constraints, theme: &Theme<'_>) -> Size {
        // Exactly a `ProgressBar`'s box, and deliberately: a job whose total arrives mid-download
        // switches from `Busy` to `Fraction`, and a size that changed with it would make the row
        // twitch at the moment the screen is being watched most closely.
        let h = meter::height(theme);
        let w = if self.flex > 0 { constraints.max_w } else { meter::BAR_W };
        constraints.constrain(Size::new(w, h))
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        // Through `meter::track`: `CrossAlign::Stretch` on a list row hands this the whole 38-pixel
        // band, and the sweeping band drawn into that is a slab sliding across the row.
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

    fn paint_in(palette: Palette, s: &dyn Widget) -> Vec<u16> {
        let (_, buf) = testing::with_canvas(Size::new(120, 38), |c| {
            testing::with_theme(palette, |t| {
                c.clear(palette.bg.mid());
                s.draw(c, ROW, t);
            });
        });
        buf
    }

    fn paint(s: &dyn Widget) -> Vec<u16> {
        paint_in(Palette::DARK, s)
    }

    #[test]
    fn a_plain_spinner_takes_its_own_width_and_leaves_the_rest_of_the_row_alone() {
        testing::with_theme(Palette::DARK, |t| {
            let got = Spinner::new(0).measure(Constraints::loose(120, 38), t);
            assert_eq!(got, Size::new(meter::BAR_W, meter::height(t)));
            assert_eq!(Spinner::new(0).measure(Constraints::loose(20, 38), t).w, 20);
            assert_eq!(Spinner::new(0).flex(1).measure(Constraints::loose(120, 38), t).w, 120);
        });
    }

    #[test]
    fn a_spinner_is_the_same_box_as_the_bar_it_may_become() {
        // A job whose `Content-Length` arrives mid-download switches from `Busy` to `Fraction`. If the
        // two measured differently the row would twitch at the moment it is being watched hardest.
        use super::super::ProgressBar;
        testing::with_theme(Palette::DARK, |t| {
            let c = Constraints::loose(120, 38);
            assert_eq!(Spinner::new(7).measure(c, t), ProgressBar::new(0.3).measure(c, t));
            assert_eq!(
                Spinner::new(7).flex(1).measure(c, t),
                ProgressBar::new(0.3).flex(1).measure(c, t)
            );
        });
    }

    #[test]
    fn the_phase_the_caller_advances_is_the_only_thing_that_moves_the_band() {
        // No timer in here: two draws at one phase are one picture, and only the model moving moves
        // it. A widget that animated itself would drift from whatever redraw schedule the app has.
        let a = paint(&Spinner::new(20).flex(1));
        assert_eq!(a, paint(&Spinner::new(20).flex(1)), "it animated itself");
        assert_ne!(a, paint(&Spinner::new(60).flex(1)), "the phase moved nothing");
    }

    #[test]
    fn every_phase_a_u8_can_hold_is_a_picture_rather_than_a_panic() {
        // The wrap is the whole point of taking a `u8` the caller advances with `wrapping_add`. A
        // phase that had to be kept under 100 would be a rule every caller could forget.
        for phase in [0u8, 1, 50, 99, 100, 199, 200, 255] {
            let _ = paint(&Spinner::new(phase).flex(1));
        }
        // And the sweep visits genuinely different places, rather than being a still picture that
        // happens not to crash.
        let frames: Vec<_> = (0..100).step_by(10).map(|p| paint(&Spinner::new(p).flex(1))).collect();
        assert!(frames.windows(2).any(|w| w[0] != w[1]), "the band never moved");
    }

    #[test]
    fn the_band_never_fills_the_whole_track_because_that_would_be_a_claim() {
        // The one thing a busy meter must not do is look like a finished bar. A sweep that covered the
        // track at some phase would say "done" once per cycle.
        use super::super::ProgressBar;
        let full = paint(&ProgressBar::new(1.0).flex(1));
        for phase in 0..100u8 {
            assert_ne!(paint(&Spinner::new(phase).flex(1)), full, "phase {phase} claimed completion");
        }
    }

    #[test]
    fn the_stretch_a_list_row_applies_does_not_fatten_the_spinner() {
        // The trap every control in this catalogue shares: the row hands over 38 pixels.
        let buf = paint(&Spinner::new(50).flex(1));
        let bg = Palette::DARK.bg.mid().to_rgb565().0;
        let rows: Vec<i32> =
            (0..38).filter(|&y| (0..120).any(|x| buf[(y * 120 + x) as usize] != bg)).collect();
        let h = testing::with_theme(Palette::DARK, meter::height);
        assert_eq!(rows.len() as i32, h, "the spinner is its own height, not the band's");
        assert_eq!(rows[0], (38 - h) / 2, "and centred in it");
    }

    #[test]
    fn the_band_is_visible_against_its_track_in_every_palette() {
        // Counted as "pixels that differ from an empty bar", not as pixels of a named colour: two
        // tests in this repo keyed a count to `Palette::accent` and both went red when the fill's
        // colour changed for a good reason.
        use super::super::ProgressBar;
        for (name, palette) in Palette::ALL {
            let empty = paint_in(palette, &ProgressBar::new(0.0).flex(1));
            for selected in [false, true] {
                let empty = if selected {
                    paint_in(palette, &ProgressBar::new(0.0).flex(1).selected(true))
                } else {
                    empty.clone()
                };
                let moved = |p: u8| {
                    paint_in(palette, &Spinner::new(p).flex(1).selected(selected))
                        .iter()
                        .zip(empty.iter())
                        .filter(|(a, b)| a != b)
                        .count()
                };
                assert!(moved(50) > 0, "{name} selected={selected}: the band is invisible");
                // The negative control: an empty bar differs from itself nowhere, so a comparison
                // that reported movement there would be measuring noise.
                assert_eq!(
                    paint_in(palette, &ProgressBar::new(0.0).flex(1).selected(selected))
                        .iter()
                        .zip(empty.iter())
                        .filter(|(a, b)| a != b)
                        .count(),
                    0,
                    "{name} selected={selected}: the comparison sees differences that are not there"
                );
            }
        }
    }

    #[test]
    fn the_band_changes_the_colours_and_not_the_geometry() {
        // Cleared to a colour no palette contains: in `Light` the track is the page colour, so a
        // count of "pixels unlike the background" would be counting the band's visibility rather
        // than the geometry. See `ProgressBar`'s twin of this test.
        const GROUND: symbian_gfx::Color = symbian_gfx::Color::hex(0xFF00FF);
        for (name, palette) in Palette::ALL {
            let shot = |selected: bool| {
                let (_, buf) = testing::with_canvas(Size::new(120, 38), |c| {
                    testing::with_theme(palette, |t| {
                        c.clear(GROUND);
                        Spinner::new(40).flex(1).selected(selected).draw(c, ROW, t);
                    });
                });
                buf
            };
            let (off, on) = (shot(false), shot(true));
            assert_ne!(off, on, "{name}: the selection band changed nothing");
            let ground = GROUND.to_rgb565().0;
            assert_eq!(
                off.iter().filter(|&&p| p != ground).count(),
                on.iter().filter(|&&p| p != ground).count(),
                "{name}: the same pixels in different colours"
            );
        }
    }

    #[test]
    fn the_phase_is_not_in_the_digest_and_the_digest_is_never_zero() {
        // If it were, the row would re-measure on every frame of the animation — which is the one
        // thing a widget that exists to say "still moving" must not cost.
        let a = Spinner::new(0);
        let b = Spinner::new(255).selected(true);
        assert_eq!(a.content_hash(), b.content_hash());
        assert_ne!(a.content_hash(), Spinner::new(0).flex(1).content_hash(), "flex changes the box");
        assert_ne!(a.content_hash(), 0);
    }

    #[test]
    fn a_degenerate_rect_is_a_no_op_rather_than_a_panic() {
        testing::with_canvas(Size::new(10, 10), |c| {
            testing::with_theme(Palette::DARK, |t| {
                Spinner::new(3).draw(c, Rect::from_xywh(0, 0, 0, 0), t);
                Spinner::new(3).draw(c, Rect::from_xywh(0, 0, 10, 1), t);
                Spinner::new(3).measure(Constraints::loose(0, 0), t);
            });
        });
    }
}
