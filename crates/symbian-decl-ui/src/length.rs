//! How a widget asks for space.

/// A dimension, stated the way a layout wants to hear it.
///
/// Three answers cover everything this SDK's screens do, and the third is the interesting one:
/// `Fill` carries a *weight*, so two children with `Fill(1)` split the leftover evenly and
/// `Fill(2)` beside `Fill(1)` takes two thirds. That is proportion without arithmetic at the call
/// site, which is the whole point of describing a screen rather than computing one.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Length {
    /// Exactly this many pixels, whatever else wants room.
    Exact(i32),
    /// A share of what the parent has left over, by weight. `Fill(0)` is the same as asking for
    /// nothing, which is treated as `WrapContent` — a weightless share is not a share.
    Fill(i32),
    /// As small as the content allows.
    WrapContent,
}

impl Length {
    /// The fixed part of this length, for a first pass that hands out the space nobody is
    /// competing for. `Fill` contributes nothing here: it is paid from the remainder.
    pub fn fixed(&self, wrap: i32) -> i32 {
        match self {
            Length::Exact(px) => (*px).max(0),
            Length::WrapContent => wrap.max(0),
            Length::Fill(_) => 0,
        }
    }

    /// This length's share of the leftover space, `0` for anything not filling.
    ///
    /// A non-positive weight is not a share: `Fill(0)` and `Fill(-1)` are mistakes, and treating
    /// them as "some" would silently divide the screen by a number nobody chose.
    pub fn weight(&self) -> i32 {
        match self {
            Length::Fill(w) if *w > 0 => *w,
            _ => 0,
        }
    }

    /// Whether this length takes its size from what is inside it.
    pub fn is_wrap(&self) -> bool {
        matches!(self, Length::WrapContent)
    }
}

impl Default for Length {
    /// Wrapping, because a widget that has not been told a size should take the space its content
    /// needs and no more — the answer that is never surprising.
    fn default() -> Self {
        Length::WrapContent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_is_fixed_and_wrap_takes_its_content() {
        assert_eq!(Length::Exact(10).fixed(99), 10);
        assert_eq!(Length::WrapContent.fixed(37), 37);
    }

    #[test]
    fn fill_costs_nothing_up_front_and_everything_later() {
        // The two-pass rule: fixed children are paid first, and what is left is split by weight.
        assert_eq!(Length::Fill(3).fixed(99), 0);
        assert_eq!(Length::Fill(3).weight(), 3);
        assert_eq!(Length::Exact(10).weight(), 0);
        assert_eq!(Length::WrapContent.weight(), 0);
    }

    #[test]
    fn a_weightless_fill_is_not_a_share() {
        // Otherwise `Fill(0)` would join the division and quietly take a slice of the screen that
        // nobody asked for — or, worse, divide by zero further up.
        assert_eq!(Length::Fill(0).weight(), 0);
        assert_eq!(Length::Fill(-1).weight(), 0);
    }

    #[test]
    fn a_negative_exact_is_nothing_rather_than_a_hole() {
        // Sizes flow into rects; a negative one would invert a rectangle rather than shrink it.
        assert_eq!(Length::Exact(-5).fixed(0), 0);
        assert_eq!(Length::WrapContent.fixed(-5), 0);
    }

    #[test]
    fn the_default_is_the_unsurprising_one() {
        assert_eq!(Length::default(), Length::WrapContent);
        assert!(Length::default().is_wrap());
    }
}
