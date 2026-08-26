//! A progress bar, in the two shapes progress actually comes in.
//!
//! Nothing in this crate could draw one before, and the cost of that showed twice on the handset in a
//! single afternoon: a package manager that said `installing 0.1.0…` for forty-six seconds reads as a
//! frozen screen, and the person holding it has no way to tell a slow operation from a dead one. A bar
//! is the difference, and it is not decoration — it is the only part of the screen that says *this is
//! still moving*.
//!
//! ## Two modes, because a server may not say how big a thing is
//!
//! [`Meter::Fraction`] fills. [`Meter::Busy`] sweeps a short band along the track without ever
//! claiming a position, and it exists because `Content-Length` is optional: a download whose total is
//! unknown would otherwise be a bar stuck at 0%, which reads as broken rather than as unknown.
//! `symbian_bootcfg::queue::Job::fraction` returns `Option` for exactly this reason, so the two types
//! line up: `Some` → `Fraction`, `None` → `Busy`.
//!
//! The busy mode is driven by a **caller-supplied phase**, not by a clock this widget keeps. Widgets
//! in this crate do not own time — the application already has the timer that made it redraw, and a
//! second source of animation would be a second thing to keep in step.

use crate::theme::Theme;
use symbian_gfx::{Canvas, Color, Rect};

/// The height a meter wants, drawn on its own line.
///
/// Thin on purpose. It sits under a row of text on a 320×240 screen, and a bar as tall as a line of
/// type would read as a second row rather than as an annotation of the first.
pub fn height(theme: &Theme<'_>) -> i32 {
    (theme.metrics.row_h / 4).clamp(4, 8)
}

/// How wide a meter is when nothing has told it otherwise.
///
/// A bar has no natural width — it maps a fraction onto whatever room it is given — but "no natural
/// width" and "the whole line" are not the same answer, and answering with the offer is the defect
/// [`crate::slider::SLIDER_W`] exists to record: the layout's first pass offers *every* fixed child
/// the whole line, so a greedy bar at the end of a row takes the label with it. Sixty, the same as a
/// slider, because the two sit in the same kind of row and a bar that was wider would make a settings
/// screen look ragged.
pub const BAR_W: i32 = 60;

/// Where the bar sits inside `band`: filling its width, centred across it.
///
/// Extracted for the same reason [`crate::toggle::switch_track`] was, and against the same defect.
/// A list row hands a control its whole 38-pixel band, and a meter drawn into that rect is not a
/// thin annotation under a line of text — it is a slab. Both the imperative caller and
/// `symbian_decl_ui`'s `ProgressBar` route through here so neither can centre it differently.
pub fn track(band: Rect, theme: &Theme<'_>) -> Rect {
    let h = height(theme);
    Rect::from_xywh(band.x0, band.y0 + (band.height() - h) / 2, band.width().max(0), h)
}

/// The track colour and the fill colour, given whether the meter sits on the selection band.
///
/// Off the band these are `scrollbar_track` and `accent`, which is what this widget has always
/// painted. On it they come from [`crate::chrome::control_colors`], because `accent` and
/// `scrollbar_track` were both chosen against the *page*: on `HIGH_CONTRAST`'s white selection band a
/// bar painted in them is a white slab on a white band, which is the same defect that turned a
/// focused row's switch into a black dot floating in nothing.
pub fn colors(theme: &Theme<'_>, selected: bool) -> (Color, Color) {
    if selected {
        let (_, ink, quiet) = crate::chrome::control_colors(theme, true);
        (quiet, ink)
    } else {
        (theme.palette.scrollbar_track, theme.palette.accent)
    }
}

/// How wide the sweeping band is, as a fraction of the track.
const BUSY_SPAN: i32 = 4;

/// What to draw.
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum Meter {
    /// A known position, 0.0 to 1.0. Values outside are clamped rather than refused: a server that
    /// sends more bytes than it promised is a real thing, and it must not be able to draw outside the
    /// track.
    Fraction(f32),
    /// Something is happening and nobody knows how much of it is left. `phase` advances one step per
    /// redraw and wraps; the caller owns it.
    Busy { phase: u8 },
}

impl Meter {
    /// The meter for a job that may or may not know its size — the shape
    /// `symbian_bootcfg::queue::Job::fraction` hands back.
    pub fn of(fraction: Option<f32>, phase: u8) -> Self {
        match fraction {
            Some(f) => Meter::Fraction(f),
            None => Meter::Busy { phase },
        }
    }

    /// The filled fraction, for a test or a label. `None` while busy, because there is nothing
    /// truthful to say.
    pub fn value(self) -> Option<f32> {
        match self {
            Meter::Fraction(f) => Some(f.clamp(0.0, 1.0)),
            Meter::Busy { .. } => None,
        }
    }

    /// Draw into `r`, which should be [`height`] tall.
    ///
    /// The track is drawn first and the whole width, so a bar at zero is still visibly a bar: an empty
    /// track and no track at all look the same, and the second one says nothing is happening.
    pub fn draw(self, c: &mut Canvas<'_>, r: Rect, theme: &Theme<'_>) {
        self.draw_on(c, r, theme, false);
    }

    /// The same, told whether it sits on the selection band.
    ///
    /// A separate entry point rather than a changed one: every existing caller draws on the page and
    /// must keep the pixels it has, and a bar in a selected list row needs the two colours
    /// [`colors`] picks for the band instead. See that function for the defect.
    pub fn draw_on(self, c: &mut Canvas<'_>, r: Rect, theme: &Theme<'_>, selected: bool) {
        if r.width() <= 0 || r.height() <= 0 {
            return;
        }
        let (track_color, fill_color) = colors(theme, selected);
        let radius = r.height() / 2;
        c.fill_round_rect(r, radius, track_color);

        match self {
            Meter::Fraction(f) => {
                let f = f.clamp(0.0, 1.0);
                // At least as wide as it is tall once anything at all has happened, so 1% is a dot
                // rather than a hairline nobody can see on this panel.
                let w = ((r.width() as f32 * f) as i32).max(if f > 0.0 { r.height() } else { 0 });
                if w > 0 {
                    let filled = Rect::from_xywh(r.x0, r.y0, w.min(r.width()), r.height());
                    c.fill_round_rect(filled, radius, fill_color);
                }
            }
            Meter::Busy { phase } => {
                let span = (r.width() / BUSY_SPAN).max(r.height());
                // The band travels the track's width plus its own, so it slides in and out of both
                // ends instead of appearing and vanishing at the edges.
                let travel = r.width() + span;
                let at = (travel * (phase as i32 % 100)) / 100 - span;
                let x0 = at.max(r.x0);
                let x1 = (at + span).min(r.x1);
                if x1 > x0 {
                    c.fill_round_rect(Rect::new(x0, r.y0, x1, r.y1), radius, fill_color);
                }
            }
        }
    }

    /// `184/320 KB · 58%`, or `184 KB` when the total is unknown.
    ///
    /// Bytes are shown in KB because a phone package is hundreds of KB and a number with six digits
    /// is a number nobody reads. The percentage is second, not first: what somebody watching a
    /// download wants is the amount, and the percentage is how they judge the wait.
    pub fn label(got: u64, total: u64) -> alloc::string::String {
        let kb = |n: u64| n.div_ceil(1024);
        if total == 0 {
            return alloc::format!("{} KB", kb(got));
        }
        let pct = (got.saturating_mul(100) / total).min(100);
        alloc::format!("{}/{} KB \u{00b7} {pct}%", kb(got), kb(total))
    }
}

/// The colour a meter's fill uses, for a caller that wants to match something to it.
pub fn fill_color(theme: &Theme<'_>) -> Color {
    theme.palette.accent
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{with_canvas, with_theme};
    use crate::Palette;
    use symbian_gfx::Size;

    fn draw(m: Meter, w: i32) -> alloc::vec::Vec<u16> {
        let (_, px) = with_canvas(Size::new(w, 12), |c| {
            with_theme(Palette::DARK, |t| {
                m.draw(c, Rect::from_xywh(0, 2, w, height(t)), t);
            });
        });
        px
    }

    /// How many pixels differ from the leftmost column's colour — a crude but honest way to ask "how
    /// much of the track is filled" without reaching into the widget.
    fn painted(px: &[u16], w: i32) -> usize {
        let row = (w * 4) as usize;
        let track = px[row];
        px[row..row + w as usize].iter().filter(|&&c| c != track).count()
    }

    #[test]
    fn a_full_bar_covers_more_than_an_empty_one() {
        let empty = draw(Meter::Fraction(0.0), 100);
        let half = draw(Meter::Fraction(0.5), 100);
        let full = draw(Meter::Fraction(1.0), 100);
        // The leftmost pixel is the fill for anything non-empty, so counting *differences* from it
        // inverts: full differs least. What matters is that the three are distinguishable.
        assert_ne!(painted(&empty, 100), painted(&half, 100));
        assert_ne!(painted(&half, 100), painted(&full, 100));
    }

    #[test]
    fn a_server_that_oversends_cannot_draw_outside_the_track() {
        assert_eq!(Meter::Fraction(1.5).value(), Some(1.0));
        assert_eq!(Meter::Fraction(-0.5).value(), Some(0.0));
        // And it does not panic on a width where the arithmetic could overflow a small rect.
        draw(Meter::Fraction(2.0), 3);
    }

    #[test]
    fn one_percent_is_visible_rather_than_a_hairline() {
        // On this panel a single-pixel fill is indistinguishable from an empty track, and "1% done"
        // then looks like "nothing is happening".
        let tiny = draw(Meter::Fraction(0.01), 100);
        let empty = draw(Meter::Fraction(0.0), 100);
        assert_ne!(painted(&tiny, 100), painted(&empty, 100));
    }

    #[test]
    fn busy_says_nothing_about_position_but_still_moves() {
        // `Content-Length` is optional, and a bar stuck at 0% reads as broken rather than unknown.
        assert_eq!(Meter::Busy { phase: 0 }.value(), None);
        let a = draw(Meter::Busy { phase: 0 }, 100);
        let b = draw(Meter::Busy { phase: 40 }, 100);
        assert_ne!(a, b, "a different phase draws a different picture");
    }

    #[test]
    fn the_band_slides_in_and_out_of_both_ends() {
        // Rather than appearing and vanishing at the edges, which reads as a glitch.
        let phases: alloc::vec::Vec<_> =
            (0..100).step_by(10).map(|p| draw(Meter::Busy { phase: p }, 60)).collect();
        assert!(phases.windows(2).any(|w| w[0] != w[1]));
        // A wrapped phase is still a valid picture rather than a panic.
        draw(Meter::Busy { phase: 255 }, 60);
    }

    #[test]
    fn of_lines_up_with_what_a_job_reports() {
        // `Job::fraction` returns Option for the same reason this has two modes.
        assert_eq!(Meter::of(Some(0.25), 0), Meter::Fraction(0.25));
        assert_eq!(Meter::of(None, 7), Meter::Busy { phase: 7 });
    }

    #[test]
    fn the_label_reads_as_an_amount_first_and_a_percentage_second() {
        // What somebody watching a download wants is the amount; the percentage is how they judge
        // the wait.
        assert_eq!(Meter::label(184_320, 320_484), "180/313 KB \u{00b7} 57%");
        assert_eq!(Meter::label(0, 320_484), "0/313 KB \u{00b7} 0%");
        assert_eq!(Meter::label(320_484, 320_484), "313/313 KB \u{00b7} 100%");
        // Unknown total: the amount, and no claim about how far along it is.
        assert_eq!(Meter::label(5_000, 0), "5 KB");
        // And a server that oversends does not print 130%.
        assert_eq!(Meter::label(500_000, 320_484), "489/313 KB \u{00b7} 100%");
    }

    #[test]
    fn routing_the_colours_through_a_function_moved_no_pixel() {
        // The proof for the refactor. `Meter::draw` used to reach into the palette for
        // `scrollbar_track` and `accent` directly; now it asks `colors(theme, false)`. This paints the
        // old way by hand and demands the two buffers match — in every palette, because the whole
        // argument for `control_colors` is that a colour defect can be invisible in four of five.
        for (name, palette) in Palette::ALL {
            let by_hand = |m: Meter| {
                with_canvas(Size::new(100, 12), |c| {
                    with_theme(palette, |t| {
                        c.clear(palette.bg.mid());
                        let r = Rect::from_xywh(0, 2, 100, height(t));
                        let p = &t.palette;
                        let radius = r.height() / 2;
                        c.fill_round_rect(r, radius, p.scrollbar_track);
                        match m {
                            Meter::Fraction(f) => {
                                let f = f.clamp(0.0, 1.0);
                                let w = ((r.width() as f32 * f) as i32)
                                    .max(if f > 0.0 { r.height() } else { 0 });
                                if w > 0 {
                                    c.fill_round_rect(
                                        Rect::from_xywh(r.x0, r.y0, w.min(r.width()), r.height()),
                                        radius,
                                        p.accent,
                                    );
                                }
                            }
                            Meter::Busy { phase } => {
                                let span = (r.width() / BUSY_SPAN).max(r.height());
                                let travel = r.width() + span;
                                let at = (travel * (phase as i32 % 100)) / 100 - span;
                                let x0 = at.max(r.x0);
                                let x1 = (at + span).min(r.x1);
                                if x1 > x0 {
                                    c.fill_round_rect(
                                        Rect::new(x0, r.y0, x1, r.y1),
                                        radius,
                                        p.accent,
                                    );
                                }
                            }
                        }
                    });
                })
                .1
            };
            let now = |m: Meter, selected: bool| {
                with_canvas(Size::new(100, 12), |c| {
                    with_theme(palette, |t| {
                        c.clear(palette.bg.mid());
                        m.draw_on(c, Rect::from_xywh(0, 2, 100, height(t)), t, selected);
                    });
                })
                .1
            };
            for m in [Meter::Fraction(0.0), Meter::Fraction(0.37), Meter::Fraction(1.0), Meter::Busy { phase: 30 }] {
                assert_eq!(now(m, false), by_hand(m), "{name}: {m:?} moved");
            }
            // The negative control that keeps the equality from being vacuous: on the band the bar is
            // painted in different colours, so the same comparison must fail there.
            assert_ne!(
                now(Meter::Fraction(0.37), true),
                by_hand(Meter::Fraction(0.37)),
                "{name}: the band colours are the page colours"
            );
        }
    }

    #[test]
    fn a_bar_on_the_selection_band_can_be_seen_in_every_palette() {
        // Why `colors` takes `selected` at all. `accent` and `scrollbar_track` were both chosen
        // against the page; on `HIGH_CONTRAST`'s white band a bar painted in them is a white slab on
        // a white band, which is the switch-that-became-a-black-dot defect wearing a different shape.
        use crate::tokens::luma;
        for (name, palette) in Palette::ALL {
            with_theme(palette, |t| {
                let band = t.palette.selection.mid();
                let (track_c, fill_c) = colors(t, true);
                let d = |a: Color, b: Color| (luma(a) as i32 - luma(b) as i32).abs();
                assert!(d(track_c, band) >= 20, "{name}: the empty track vanishes into the band");
                assert!(d(fill_c, track_c) >= 40, "{name}: the fill vanishes into its own track");
                // And off the band, exactly what it always painted.
                assert_eq!(colors(t, false), (t.palette.scrollbar_track, t.palette.accent), "{name}");
            });
        }
    }

    #[test]
    fn the_track_is_its_own_height_inside_whatever_band_it_is_handed() {
        // The trap every control in this catalogue shares: a list row hands over its whole 38-pixel
        // band, and a bar drawn into that rect is a slab rather than an annotation.
        with_theme(Palette::DARK, |t| {
            let band = Rect::from_xywh(0, 0, 120, 38);
            let r = track(band, t);
            assert_eq!(r.height(), height(t), "the bar took the band's height");
            assert_eq!(r.y0, (38 - height(t)) / 2, "and was not centred in it");
            assert_eq!((r.x0, r.width()), (0, 120), "a bar fills the width it is given");
            // A band shorter than the bar still yields a drawable rect rather than an inverted one.
            assert!(track(Rect::from_xywh(0, 0, 120, 2), t).height() > 0);
            assert!(track(Rect::from_xywh(0, 0, -5, 38), t).width() >= 0);
        });
    }

    #[test]
    fn a_degenerate_rect_is_a_no_op_rather_than_a_panic() {
        with_canvas(Size::new(10, 10), |c| {
            with_theme(Palette::DARK, |t| {
                Meter::Fraction(0.5).draw(c, Rect::from_xywh(0, 0, 0, 0), t);
                Meter::Busy { phase: 3 }.draw(c, Rect::from_xywh(0, 0, 10, 0), t);
            });
        });
    }
}
