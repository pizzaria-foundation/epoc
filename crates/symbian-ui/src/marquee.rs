//! How far along a line of text has slid, for text too long for its box.
//!
//! [`Font::fit`](symbian_gfx::Font::fit) is the other answer to the same problem and the right one
//! almost everywhere: cut the string and put an ellipsis on it. That fails in exactly one place —
//! the row that has the cursor. A user who has selected a row and still cannot read its name has no
//! way to find out what it says, and truncation has turned a label into a puzzle.
//!
//! # It takes a phase and returns a number
//!
//! No timer, no state, no interior mutability. The caller has a tick — [`Meter::Busy`](crate::Meter)
//! already works this way, and a screen that animates anything already has one — and this turns that
//! tick into an offset. Which means it can be unit-tested for every phase of the cycle, and a
//! screenshot of frame 37 is reproducible.
//!
//! # The cycle pauses at both ends
//!
//! Sliding continuously means the beginning of the label is only ever readable in passing. The
//! S60 behaviour, and the one here, is: hold still long enough to read the start, slide to the end,
//! hold still long enough to read that, then snap back. The snap is deliberate and not a slide: a
//! reverse slide reads as the text having changed direction for a reason, and there is no reason.

/// How the offset is paced, in ticks of whatever clock the caller is driving.
///
/// A value rather than constants so a screen can slow a long label down without every other marquee
/// on the screen changing with it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Pace {
    /// Ticks held still at each end of the travel.
    pub pause: u32,
    /// Pixels moved per tick. `0` is treated as `1`: a marquee that never moves is a truncation with
    /// extra machinery, and the caller that wants that should not have built one.
    pub step: i32,
}

impl Pace {
    /// Roughly a second of pause at a four-hertz tick, two pixels a step.
    ///
    /// Two rather than one because a one-pixel step at a readable rate takes eleven seconds to cross
    /// a 320-pixel screen, by which time the user has pressed something.
    pub const DEFAULT: Self = Self { pause: 4, step: 2 };

    /// The step, with zero read as one. See the field's own note.
    fn step(self) -> i32 {
        self.step.max(1)
    }
}

impl Default for Pace {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// How far left the text has slid, in pixels, at `phase`.
///
/// The cycle is: `pace.pause` ticks held at zero, one tick per `pace.step` until the end of the text
/// reaches the end of the box, then `pace.pause` ticks held there, then back to zero. A `pause` of
/// zero therefore never shows the start of the label at all — the first tick has already moved — which
/// is the honest reading of "do not hold at the start" and a reason not to ask for it.
///
/// Always `0..=travel`, where `travel` is how much of the text does not fit. Text that fits returns
/// `0` for every phase, so a caller can build a marquee unconditionally and let the arithmetic decide
/// whether anything moves — which is the point, because whether a label fits depends on a font
/// measurement the caller would otherwise have to repeat.
pub fn offset(text_w: i32, box_w: i32, phase: u32, pace: Pace) -> i32 {
    let travel = (text_w - box_w).max(0);
    if travel == 0 {
        return 0;
    }
    // Ceiling division: a travel that is not a whole number of steps still has to arrive, and
    // rounding down leaves the last few pixels of the label permanently off the edge.
    let moving = (travel + pace.step() - 1) / pace.step();
    let cycle = pace.pause + moving as u32 + pace.pause;
    let at = phase % cycle;

    if at < pace.pause {
        0
    } else if at < pace.pause + moving as u32 {
        // `+ 1` because the tick that *ends* the pause is the first tick that has moved. Without it
        // the first slide tick still shows offset zero, so a `pause` of two holds for three.
        //
        // Clamped as well as computed: the last step would otherwise overshoot by the remainder of
        // the ceiling division above, and the text would slide one step too far left.
        ((at - pace.pause + 1) as i32 * pace.step()).min(travel)
    } else {
        travel
    }
}

/// Whether a label of `text_w` in a box of `box_w` has anything to slide.
///
/// The condition to build a marquee at all, exposed so a screen can answer it without duplicating
/// the subtraction — and so a test can assert the two agree.
pub fn scrolls(text_w: i32, box_w: i32) -> bool {
    text_w > box_w
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every offset of one full cycle, for a 40-pixel overhang.
    fn cycle(text_w: i32, box_w: i32, pace: Pace, n: u32) -> Vec<i32> {
        (0..n).map(|p| offset(text_w, box_w, p, pace)).collect()
    }

    #[test]
    fn text_that_fits_never_moves() {
        // The property that lets a caller build one unconditionally.
        let pace = Pace::DEFAULT;
        for phase in 0..50 {
            assert_eq!(offset(50, 100, phase, pace), 0);
            assert_eq!(offset(100, 100, phase, pace), 0, "exactly fitting is fitting");
        }
        assert!(!scrolls(100, 100));
        assert!(scrolls(101, 100));
    }

    #[test]
    fn it_holds_still_slides_holds_still_and_snaps_back() {
        // 10 pixels over, 2 per step, 2 ticks of pause: hold 0,0 — slide 2,4,6,8,10 — hold 10,10.
        let pace = Pace { pause: 2, step: 2 };
        assert_eq!(cycle(110, 100, pace, 9), vec![0, 0, 2, 4, 6, 8, 10, 10, 10]);
        // And the tenth tick is the start of the next cycle, not a slide backwards.
        assert_eq!(offset(110, 100, 9, pace), 0);
    }

    #[test]
    fn the_offset_never_exceeds_the_overhang() {
        // The end of the label must land on the right edge of the box and stop there. One pixel more
        // and the last character is off screen with blank space behind it, which reads as a bug in
        // the font rather than in the animation.
        let pace = Pace { pause: 1, step: 3 };
        for phase in 0..200 {
            let got = offset(107, 100, phase, pace);
            assert!((0..=7).contains(&got), "phase {phase} gave {got}");
        }
    }

    #[test]
    fn a_travel_that_is_not_a_whole_number_of_steps_still_arrives() {
        // 7 over at 3 a step is 2.33 steps. Rounding down would stop at 6 and leave a pixel of the
        // label permanently hidden — invisible in a glance and wrong on every long label.
        let pace = Pace { pause: 0, step: 3 };
        let seen = cycle(107, 100, pace, 6);
        assert!(seen.contains(&7), "the end is reached: {seen:?}");
    }

    #[test]
    fn a_zero_step_is_read_as_one_rather_than_dividing_by_it() {
        // One pixel per tick, and the fourth tick has moved four — not a division by zero, and not a
        // marquee that stands still.
        let pace = Pace { pause: 0, step: 0 };
        assert_eq!(offset(105, 100, 3, pace), 4);
    }

    #[test]
    fn no_pause_slides_from_the_very_first_tick() {
        // Nothing is held, so offset zero is never shown: the first tick has already moved. Worth
        // asserting rather than leaving implied, because it is the surprising half of `pause: 0` and
        // the reason the default is not zero.
        let pace = Pace { pause: 0, step: 1 };
        assert_eq!(cycle(105, 100, pace, 5), vec![1, 2, 3, 4, 5]);
        // And it repeats immediately, with no hold at the end either.
        assert_eq!(offset(105, 100, 5, pace), 1);
    }

    #[test]
    fn the_cycle_repeats_exactly() {
        // A phase counter on this device is a `u32` that runs for weeks. If the cycle drifted, a
        // long-lived screen would end up with every marquee on it out of step with every other.
        let pace = Pace::DEFAULT;
        let first: Vec<i32> = (0..64).map(|p| offset(180, 100, p, pace)).collect();
        // 48 is the cycle length here: an 80-pixel overhang at 2 a step is 40 moving ticks, plus the
        // 4-tick pause at each end. A hundred whole cycles later the offsets must be the same ones.
        let later: Vec<i32> = (0..64).map(|p| offset(180, 100, p + 48 * 100, pace)).collect();
        assert_eq!(first, later);
    }

    #[test]
    fn a_negative_box_is_all_overhang_rather_than_a_rect_turned_inside_out() {
        // A group squeezed past zero. The label is entirely outside its box, and the offset must stay
        // a forward slide rather than becoming a number larger than the text.
        let pace = Pace { pause: 0, step: 1000 };
        assert_eq!(offset(50, -20, 1, pace), 70);
    }
}
