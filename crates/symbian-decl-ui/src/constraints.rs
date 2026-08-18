//! What a parent offers a child, and what the child may answer.

use symbian_gfx::Size;

/// The room a parent is willing to give, as a range in each axis.
///
/// A child measures itself against this and returns a size inside it. Two shapes cover most uses:
/// [`Constraints::tight`], which says "exactly this" and leaves no choice, and
/// [`Constraints::loose`], which says "up to this" and lets the child be smaller.
///
/// Every constructor keeps the invariant `min <= max` and both non-negative. That is not
/// defensiveness for its own sake: constraints are computed by subtracting padding, gaps and
/// siblings from a parent's box, and on a 320x240 screen those subtractions go negative regularly.
/// A negative maximum would flow into a rect with `x1 < x0`, and an inverted rectangle draws as
/// nothing at all — a blank screen with no error anywhere, which is the worst failure this layer
/// can produce.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Constraints {
    pub min_w: i32,
    pub max_w: i32,
    pub min_h: i32,
    pub max_h: i32,
}

impl Constraints {
    /// Exactly `w` by `h`: no freedom in either axis.
    pub fn tight(w: i32, h: i32) -> Self {
        let (w, h) = (w.max(0), h.max(0));
        Self { min_w: w, max_w: w, min_h: h, max_h: h }
    }

    /// Up to `max_w` by `max_h`, and as small as nothing.
    pub fn loose(max_w: i32, max_h: i32) -> Self {
        Self { min_w: 0, max_w: max_w.max(0), min_h: 0, max_h: max_h.max(0) }
    }

    /// Loose in both axes with no ceiling worth speaking of — for measuring what a widget *wants*
    /// before deciding what it gets.
    pub fn unbounded() -> Self {
        Self::loose(i32::MAX / 4, i32::MAX / 4)
    }

    /// The same, with the maxima reduced by `w`/`h` and never below the minima.
    ///
    /// This is the subtraction that padding, gaps and already-placed siblings all go through, and
    /// the clamp is what keeps a too-small parent from producing a negative offer.
    pub fn shrink(&self, w: i32, h: i32) -> Self {
        Self {
            min_w: self.min_w,
            max_w: (self.max_w - w).max(self.min_w),
            min_h: self.min_h,
            max_h: (self.max_h - h).max(self.min_h),
        }
    }

    /// This offer with its minima dropped: "up to what you were given, but you may be smaller".
    pub fn loosen(&self) -> Self {
        Self { min_w: 0, max_w: self.max_w, min_h: 0, max_h: self.max_h }
    }

    /// Force a size into the range. Both axes independently, so a wide short answer to a tall
    /// narrow offer is corrected in both directions rather than rejected.
    pub fn constrain(&self, size: Size) -> Size {
        Size::new(
            size.w.clamp(self.min_w, self.max_w),
            size.h.clamp(self.min_h, self.max_h),
        )
    }

    /// Whether the offer leaves the child no choice.
    pub fn is_tight(&self) -> bool {
        self.min_w == self.max_w && self.min_h == self.max_h
    }
}

impl Default for Constraints {
    fn default() -> Self {
        Self::loose(0, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tight_leaves_no_choice_and_loose_leaves_all_of_it() {
        let t = Constraints::tight(100, 50);
        assert!(t.is_tight());
        assert_eq!(t.constrain(Size::new(999, 1)), Size::new(100, 50));

        let l = Constraints::loose(100, 50);
        assert!(!l.is_tight());
        assert_eq!(l.constrain(Size::new(10, 10)), Size::new(10, 10));
        assert_eq!(l.constrain(Size::new(999, 999)), Size::new(100, 50));
    }

    #[test]
    fn shrinking_past_the_end_stops_at_the_minimum() {
        // The case this exists for: padding and siblings eating more than the parent has. The
        // offer must bottom out, not go negative — a negative max becomes an inverted rect, which
        // draws as nothing and reports nothing.
        let c = Constraints::loose(40, 20).shrink(100, 100);
        assert_eq!((c.max_w, c.max_h), (0, 0));
        assert!(c.max_w >= c.min_w && c.max_h >= c.min_h);
    }

    #[test]
    fn shrinking_a_tight_offer_cannot_break_its_own_minimum() {
        let c = Constraints::tight(50, 50).shrink(30, 30);
        assert_eq!((c.min_w, c.max_w), (50, 50), "a tight minimum is a promise");
        assert!(c.max_w >= c.min_w);
    }

    #[test]
    fn negative_input_becomes_nothing_rather_than_an_inverted_box() {
        assert_eq!(Constraints::tight(-10, -10), Constraints::tight(0, 0));
        assert_eq!(Constraints::loose(-10, -10), Constraints::loose(0, 0));
    }

    #[test]
    fn loosening_keeps_the_ceiling_and_drops_the_floor() {
        let c = Constraints::tight(30, 40).loosen();
        assert_eq!((c.min_w, c.min_h), (0, 0));
        assert_eq!((c.max_w, c.max_h), (30, 40));
    }

    #[test]
    fn unbounded_can_be_shrunk_without_wrapping_round() {
        // Measuring what a widget wants uses a huge ceiling; subtracting from it must not overflow
        // into a negative, which is why it is MAX/4 rather than MAX.
        let c = Constraints::unbounded().shrink(1000, 1000);
        assert!(c.max_w > 0 && c.max_h > 0);
    }

    #[test]
    fn constraining_fixes_both_axes_not_just_the_offending_one() {
        let c = Constraints { min_w: 10, max_w: 20, min_h: 5, max_h: 8 };
        assert_eq!(c.constrain(Size::new(1, 100)), Size::new(10, 8));
    }
}
