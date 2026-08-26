//! A bounded number as a track with a filled part.
//!
//! [`crate::stepper`] is the other answer to the same question and the right one for a *count*:
//! four retries, nine days. A slider is for a quantity whose exact number nobody reads — volume,
//! brightness, a timeout — where the useful information is "about a third of the way along" and the
//! number is confirmation rather than content.
//!
//! # It takes a value and returns one
//!
//! Nothing here holds state. The value comes in, a new value or a clamp comes out, and the geometry
//! is a function of the two — which is what lets the declarative widget be a shell and the imperative
//! caller keep owning its own `i32`. It is also what makes every edge case here a unit test rather
//! than a screenshot.
//!
//! # An arrow at the end is consumed
//!
//! `Left` at the minimum does not move the value and **is still taken**, the same choice
//! [`ListState::handle_key`](crate::list::ListState::handle_key),
//! [`GridState::handle_key`](crate::grid::GridState::handle_key) and
//! [`EdgePolicy::Stop`](crate::focus::EdgePolicy::Stop) all make. The reason is written out in those
//! three places and is worth repeating once: an arrow that falls through *only at the ends* is an
//! arrow whose meaning depends on where the value happens to be, and a user experiences that as the
//! phone being broken rather than as a boundary.
//!
//! [`step`] reports the clamp separately so a caller that wants the other behaviour can have it —
//! what it cannot do is get it by accident.

use symbian_gfx::{Canvas, Rect};

use crate::input::{Handled, Key, KeyEvent};
use crate::theme::Theme;

/// How a slider answered a key.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Slid {
    /// The value moved. This is the new one.
    To(i32),
    /// The key was a slider key and the value was already at that end.
    ///
    /// Its own variant rather than `To(unchanged)` because the two are different things to a caller:
    /// a screen that reports a change must not report one, and a screen that pages at the boundary
    /// needs to hear about the boundary. Distinguishing them by comparing to the old value works
    /// until `step` is smaller than the rounding, which is the case a `min`/`max` of the same number
    /// already is.
    Clamped,
    /// Not a key this slider answers.
    Ignored,
}

/// Move `value` one step, within `min..=max`.
///
/// Returns [`Slid::Clamped`] rather than the unchanged value when there is nowhere to go — see the
/// module docs on why the key is still consumed by the caller.
///
/// `min > max` is not an error and not a panic: it collapses to `min`, because a range someone built
/// backwards from two model fields should show *a* value rather than take down the application. A
/// panic in a key handler on this device reports as a dialog with a number in it.
pub fn step(value: i32, min: i32, max: i32, forward: bool, by: i32) -> Slid {
    let (lo, hi) = if min <= max { (min, max) } else { (min, min) };
    let by = by.max(1);
    let at = value.clamp(lo, hi);
    let next = if forward { at.saturating_add(by).min(hi) } else { at.saturating_sub(by).max(lo) };
    if next == at {
        // Including the case where `value` arrived outside the range: the clamp above already moved
        // it, and reporting `To(at)` would tell the caller its own model was wrong through a
        // "the user changed this" channel.
        Slid::Clamped
    } else {
        Slid::To(next)
    }
}

/// [`step`] driven by a key event, for a caller that does not want to map the arrows itself.
///
/// `Left`/`Right` adjust, and `Select` steps forward with a wrap. `Up`/`Down` are deliberately
/// absent: a slider inside a vertical form must leave the vertical arrows to whatever is moving the
/// cursor, or it becomes the one field the user cannot get past.
pub fn handle_key(ev: KeyEvent, value: i32, min: i32, max: i32, by: i32) -> Slid {
    match ev.key {
        Key::Left => step(value, min, max, false, by),
        Key::Right => step(value, min, max, true, by),
        // `Select` steps forward and wraps to the minimum at the top, exactly as
        // [`crate::stepper`] does — and for a reason that only showed up once tabs existed.
        //
        // A tab strip takes `Left` and `Right` before the panel under it ever sees them, which is the
        // right ordering (a screen you cannot navigate out of is worse than a control that needs a
        // different key). But `Stepper` already had a `Select` fallback and a slider did not, so on a
        // tabbed screen a slider was drivable by **no key on the phone**. The strip was not wrong; the
        // slider was missing the escape the stepper had had all along.
        //
        // Wrapping rather than clamping, because a wrap is the only way a single key can reach a value
        // *below* the current one — and a control that can only ever go up is not a control.
        Key::Select => match step(value, min, max, true, by) {
            Slid::Clamped => Slid::To(min.min(max)),
            other => other,
        },
        _ => Slid::Ignored,
    }
}

/// Whether a [`Slid`] means the key was taken.
///
/// `Clamped` counts as taken. That is the whole decision this module documents, in one function, so
/// no caller has to remember which way it went.
pub fn consumed(slid: Slid) -> Handled {
    match slid {
        Slid::To(_) | Slid::Clamped => Handled::Consumed,
        Slid::Ignored => Handled::Ignored,
    }
}

/// A slider's natural width, when nothing has told it to fill.
///
/// # Why it has one at all
///
/// The first version had none: `measure` returned the whole offer, on the argument that mapping a
/// range onto whatever room it is given is the slider's entire job. That is true of a slider on its
/// own line and wrong everywhere else, because
/// [`measure_group`](symbian_decl_ui::layout::measure_group)'s first pass offers **every** fixed
/// child the whole line — so a slider beside a label took all of it and the label, flexing for the
/// leftover, got nothing. The row rendered as a bare track with no idea what it was for, and the
/// gallery is where that was found.
///
/// Twice a switch, because a track needs enough length for a fifth of it to be a visible difference.
/// A slider that wants the whole line asks for it with a weight, the way `Text` does.
pub const SLIDER_W: i32 = 60;

/// How tall a slider's track is inside a band `band_h` pixels high.
///
/// Thinner than a switch, and on purpose: a switch is an object you flip and wants to look
/// substantial, a track is a scale and wants to look like a line. Clamped at both ends — below four
/// pixels the fill and the track are the same object, above ten it reads as a progress bar.
pub fn track_height(band_h: i32, theme: &Theme<'_>) -> i32 {
    (band_h - theme.metrics.pad * 2).clamp(4, 10)
}

/// Where the track sits inside `band`: filling its width, centred across it.
///
/// Full width rather than a fixed one, unlike [`crate::toggle::switch_track`]. A slider has no
/// natural size — its whole job is to map a range onto whatever room it is given — so a caller that
/// wants it narrower gives it a narrower band, which is what a layout is for.
pub fn track(band: Rect, theme: &Theme<'_>) -> Rect {
    let h = track_height(band.height(), theme);
    Rect::from_xywh(band.x0, band.y0 + (band.height() - h) / 2, band.width().max(0), h)
}

/// How many pixels of a `track_w`-wide track are filled at `value`.
///
/// Rounded to nearest rather than truncated, so the halfway value fills half the track instead of
/// half minus a pixel — and so the two ends are exact: `min` fills nothing and `max` fills all of
/// it, which is the property a user checks first.
pub fn fill_width(track_w: i32, value: i32, min: i32, max: i32) -> i32 {
    let w = track_w.max(0);
    if max <= min {
        // A range of one value is entirely at its own end. Full rather than empty: a slider showing
        // the only value it can show is at that value, and an empty track reads as "unset".
        return w;
    }
    let at = value.clamp(min, max) as i64 - min as i64;
    let span = max as i64 - min as i64;
    // 64-bit: a range built from two model fields can be wide, and `at * w` on an i32 overflows long
    // before anything looks wrong on screen.
    (((at * w as i64) + span / 2) / span) as i32
}

/// Paint the track and its filled part into exactly `at`.
///
/// `focused` brightens the fill rather than adding a ring, because a slider in a list row already
/// has the selection band behind it and a ring inside a band is two cursors on one row.
pub fn draw(
    c: &mut Canvas<'_>,
    at: Rect,
    theme: &Theme<'_>,
    value: i32,
    min: i32,
    max: i32,
    focused: bool,
) {
    // A slider is focused exactly when its row is selected, so one flag serves both: the fill
    // brightens *and* the inks move onto the band. See `chrome::control_colors`.
    let (_, ink, quiet) = crate::chrome::control_colors(theme, focused);
    let h = at.height();
    let r = h / 2;
    c.fill_round_rect(at, r, if focused { quiet } else { theme.palette.scrollbar_track });
    let w = fill_width(at.width(), value, min, max);
    if w > 0 {
        let fill = if focused { ink } else { theme.palette.dim };
        // The filled part keeps the track's radius so its right edge is round: a square-ended fill
        // inside a rounded track leaves two background wedges at the left end, which reads as a
        // rendering fault at four pixels tall.
        c.fill_round_rect(Rect::from_xywh(at.x0, at.y0, w, h), r, fill);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;
    use crate::theme::Palette;
    use symbian_gfx::Size;

    const BAND: Rect = Rect { x0: 0, y0: 0, x1: 100, y1: 38 };

    #[test]
    fn a_step_moves_by_its_own_amount() {
        assert_eq!(step(40, 0, 100, true, 5), Slid::To(45));
        assert_eq!(step(40, 0, 100, false, 5), Slid::To(35));
        assert_eq!(step(40, 0, 100, true, 1), Slid::To(41));
    }

    #[test]
    fn a_step_that_would_overshoot_lands_on_the_end() {
        // Not "refuses to move": a volume at 98 with a step of 5 has to reach 100, or the last two
        // percent are unreachable and the user is left pressing a key that does nothing.
        assert_eq!(step(98, 0, 100, true, 5), Slid::To(100));
        assert_eq!(step(2, 0, 100, false, 5), Slid::To(0));
    }

    #[test]
    fn an_arrow_at_the_end_reports_a_clamp_rather_than_the_same_value() {
        assert_eq!(step(100, 0, 100, true, 5), Slid::Clamped);
        assert_eq!(step(0, 0, 100, false, 5), Slid::Clamped);
    }

    #[test]
    fn a_clamp_is_still_a_key_that_was_taken() {
        // The decision this module exists to write down once. An arrow that falls through only at the
        // ends is an arrow whose meaning depends on the value.
        assert_eq!(consumed(Slid::Clamped), Handled::Consumed);
        assert_eq!(consumed(Slid::To(5)), Handled::Consumed);
        assert_eq!(consumed(Slid::Ignored), Handled::Ignored);
    }

    #[test]
    fn a_value_arriving_outside_the_range_is_pulled_in_without_being_reported_as_a_change() {
        // A model field that drifted out of range must not come back through the "the user moved
        // this" channel — that would make `update` write a value the user never chose.
        assert_eq!(step(500, 0, 100, true, 5), Slid::Clamped);
        assert_eq!(step(-500, 0, 100, false, 5), Slid::Clamped);
        // But an inward step from outside does move, and lands inside.
        assert_eq!(step(500, 0, 100, false, 5), Slid::To(95));
    }

    #[test]
    fn the_centre_key_drives_a_slider_that_has_no_arrows_left() {
        // Found once tabs existed. A tab strip takes Left and Right before the panel under it sees
        // them — correct, because a screen you cannot navigate out of is worse than a control needing
        // another key. `Stepper` already had this fallback; a slider did not, so on a tabbed screen it
        // was drivable by no key at all.
        assert_eq!(handle_key(KeyEvent::new(Key::Select), 40, 0, 100, 5), Slid::To(45));
        // And it wraps at the top, because a wrap is the only way one key reaches a lower value. A
        // control that can only ever go up is not a control.
        assert_eq!(handle_key(KeyEvent::new(Key::Select), 100, 0, 100, 5), Slid::To(0));
        // A range of one value has nowhere to wrap to and stays there rather than inverting.
        assert_eq!(handle_key(KeyEvent::new(Key::Select), 5, 5, 5, 1), Slid::To(5));
    }

    #[test]
    fn only_the_horizontal_arrows_and_the_centre_key_belong_to_a_slider() {
        // The vertical ones are the enclosing scope's, or a slider is the field the cursor cannot
        // leave.
        for key in [Key::Up, Key::Down, Key::Backspace, Key::Char('4')] {
            assert_eq!(handle_key(KeyEvent::new(key), 40, 0, 100, 5), Slid::Ignored, "{key:?}");
        }
        assert_eq!(handle_key(KeyEvent::new(Key::Right), 40, 0, 100, 5), Slid::To(45));
        assert_eq!(handle_key(KeyEvent::new(Key::Left), 40, 0, 100, 5), Slid::To(35));
    }

    #[test]
    fn a_backwards_range_collapses_instead_of_panicking() {
        // Two model fields in the wrong order. A panic in a key handler on this device reports as a
        // dialog with a number in it, and there is nothing to be gained by it.
        assert_eq!(step(50, 100, 0, true, 5), Slid::Clamped);
        assert_eq!(step(50, 100, 0, false, 5), Slid::Clamped);
    }

    #[test]
    fn a_step_of_zero_is_read_as_one() {
        // A slider that cannot move is a label with extra machinery.
        assert_eq!(step(40, 0, 100, true, 0), Slid::To(41));
        assert_eq!(step(40, 0, 100, true, -7), Slid::To(41));
    }

    #[test]
    fn the_ends_of_the_fill_are_exact() {
        // The first thing anyone checks: nothing at the minimum, everything at the maximum. Rounding
        // that put the maximum one pixel short would show a full-looking slider that is not full.
        assert_eq!(fill_width(80, 0, 0, 100), 0);
        assert_eq!(fill_width(80, 100, 0, 100), 80);
    }

    #[test]
    fn the_fill_rounds_to_nearest_so_the_middle_is_the_middle() {
        assert_eq!(fill_width(80, 50, 0, 100), 40);
        // 1/3 of 80 is 26.67, which rounds to 27 rather than truncating to 26.
        assert_eq!(fill_width(80, 33, 0, 99), 27);
    }

    #[test]
    fn a_range_of_one_value_is_full_rather_than_empty() {
        // An empty track reads as "unset", which is a different statement from "this is the only
        // setting available".
        assert_eq!(fill_width(80, 5, 5, 5), 80);
        assert_eq!(fill_width(80, 5, 5, 4), 80, "and a backwards range does not invert it");
    }

    #[test]
    fn a_fill_never_leaves_its_track() {
        for value in [-1000, -1, 0, 1, 49, 50, 99, 100, 1000] {
            let w = fill_width(80, value, 0, 100);
            assert!((0..=80).contains(&w), "value {value} filled {w} of 80");
        }
        assert_eq!(fill_width(-5, 50, 0, 100), 0, "a track squeezed past zero fills nothing");
    }

    #[test]
    fn a_wide_range_does_not_overflow_the_multiply() {
        // `at * track_w` on an i32 would wrap long before anything looked wrong on screen.
        let w = fill_width(300, i32::MAX / 2, 0, i32::MAX);
        assert!((149..=151).contains(&w), "half of 300 is about 150, got {w}");
    }

    #[test]
    fn a_track_is_thinner_than_a_switch_and_centred_in_its_band() {
        testing::with_theme(Palette::DARK, |t| {
            let tr = track(BAND, t);
            assert_eq!(tr.width(), 100, "a slider takes the room it is given");
            assert_eq!(tr.height(), track_height(38, t));
            assert!(tr.height() < crate::toggle::switch_height(38, t), "a scale is a line, not an object");
            assert_eq!(tr.y0, (38 - tr.height()) / 2);
        });
    }

    #[test]
    fn a_squeezed_band_still_gives_a_track_with_a_visible_fill() {
        testing::with_theme(Palette::DARK, |t| {
            assert_eq!(track_height(38, t), 10, "clamped at the top");
            assert_eq!(track_height(6, t), 4, "and at the bottom");
            assert_eq!(track_height(-9, t), 4, "and never inverted");
        });
    }

    #[test]
    fn the_fill_grows_with_the_value_in_every_palette() {
        for (name, palette) in Palette::ALL {
            // Counting *fill-coloured* pixels, not ink. The first version of this test counted
            // everything that was not the background and failed on the first palette — correctly:
            // the track is painted full width whatever the value, so the total is identical at 0 and
            // at 100 and only the *colour* of part of it changes. An instrument that guesses does not
            // fail, it misdirects.
            // "How much differs from an empty slider", not "how many accent pixels". The fill's colour
            // is the band-aware ink now — `chrome::control_colors` — so a count keyed to `accent` was
            // measuring one answer rather than the property, and it went red when the answer changed
            // for a good reason. Twice, in two crates, from the same mistake.
            let render = |value: i32| {
                let (_, buf) = testing::with_canvas(Size::new(100, 38), |c| {
                    testing::with_theme(palette, |t| {
                        c.clear(palette.bg.mid());
                        draw(c, track(BAND, t), t, value, 0, 100, true);
                    });
                });
                buf
            };
            let empty = render(0);
            let filled = |value: i32| {
                render(value).iter().zip(empty.iter()).filter(|(a, b)| a != b).count()
            };
            assert!(filled(100) > filled(50), "{name}: full is not more filled than half");
            assert!(filled(50) > filled(0), "{name}: half is not more filled than empty");
            assert_eq!(filled(0), 0, "{name}: an empty slider differs from itself nowhere");
        }
    }

    #[test]
    fn focus_changes_the_fill_and_not_the_geometry() {
        // A ring would be a second cursor on a row that already has the selection band.
        let paint = |focused: bool| {
            let (_, buf) = testing::with_canvas(Size::new(100, 38), |c| {
                testing::with_theme(Palette::DARK, |t| {
                    c.clear(Palette::DARK.bg.mid());
                    draw(c, track(BAND, t), t, 50, 0, 100, focused);
                });
            });
            buf
        };
        let (a, b) = (paint(false), paint(true));
        assert_ne!(a, b, "the fill did not change colour");
        let bg = Palette::DARK.bg.mid().to_rgb565().0;
        assert_eq!(
            a.iter().filter(|&&p| p != bg).count(),
            b.iter().filter(|&&p| p != bg).count(),
            "the same pixels are painted, in a different colour"
        );
    }

    #[test]
    fn nothing_is_painted_outside_the_track() {
        // A slider in a list row sits beside a label, and a fill one pixel wide would eat it.
        let (_, buf) = testing::with_canvas(Size::new(100, 38), |c| {
            testing::with_theme(Palette::DARK, |t| {
                c.clear(Palette::DARK.bg.mid());
                draw(c, track(BAND, t), t, 70, 0, 100, true);
            });
        });
        let bg = Palette::DARK.bg.mid().to_rgb565().0;
        let tr = testing::with_theme(Palette::DARK, |t| track(BAND, t));
        for y in 0..38 {
            if (tr.y0..tr.y1).contains(&y) {
                continue;
            }
            for x in 0..100 {
                assert_eq!(buf[(y * 100 + x) as usize], bg, "ink at {x},{y}");
            }
        }
    }
}
