//! Deliberate emptiness.

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::Theme;

use crate::constraints::Constraints;
use crate::widget::{hash_i32, Widget, WidgetHash};

/// A gap that takes part in the layout.
///
/// Two uses, and the second is the reason it exists. `Spacer::new().width(8)` is a fixed gap
/// between two things that a container's uniform `gap` cannot express. `Spacer::new().fill(1)` is
/// the *push*: put one between two children of a row and the first goes left, the second goes
/// right, with no arithmetic at the call site and nothing to correct when the screen or the labels
/// change size. Written by hand that is a width computed from the parent's width minus two measured
/// strings, which is exactly the calculation this layer was built to stop people from writing.
///
/// Draws nothing, ever.
#[derive(Copy, Clone, Debug, Default)]
pub struct Spacer {
    w: i32,
    h: i32,
    weight: i32,
}

impl Spacer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn width(mut self, px: i32) -> Self {
        self.w = px.max(0);
        self
    }

    pub fn height(mut self, px: i32) -> Self {
        self.h = px.max(0);
        self
    }

    /// The same in both axes, for a spacer used in a row today and a column tomorrow.
    pub fn square(self, px: i32) -> Self {
        self.width(px).height(px)
    }

    /// Take a share of the parent's leftover space instead of a fixed size.
    pub fn fill(mut self, weight: i32) -> Self {
        self.weight = weight;
        self
    }
}

impl Widget for Spacer {
    fn content_hash(&self) -> WidgetHash {
        hash_i32(hash_i32(hash_i32(0, self.w), self.h), self.weight)
    }

    /// Its own size, forced into the offer.
    ///
    /// The clamp is what makes a filling spacer work at all: the layout pass hands a flexible child
    /// a *tight* main axis holding the share it won, so a spacer that asks for nothing is given
    /// exactly that share and reports it back.
    fn measure(&self, constraints: Constraints, _theme: &Theme<'_>) -> Size {
        constraints.constrain(Size::new(self.w, self.h))
    }

    fn draw(&self, _c: &mut Canvas<'_>, _rect: Rect, _theme: &Theme<'_>) {}

    fn flex_weight(&self) -> i32 {
        self.weight
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_ui::{testing, Palette};

    #[test]
    fn a_spacer_is_the_size_it_was_told_to_be() {
        testing::with_theme(Palette::DARK, |t| {
            let offer = Constraints::loose(100, 50);
            assert_eq!(Spacer::new().width(10).measure(offer, t), Size::new(10, 0));
            assert_eq!(Spacer::new().square(6).measure(offer, t), Size::new(6, 6));
        });
    }

    #[test]
    fn a_tight_offer_is_not_a_suggestion() {
        testing::with_theme(Palette::DARK, |t| {
            // This is how a filling spacer receives its share: the container decided, and the
            // widget's own preference has nothing left to say.
            assert_eq!(
                Spacer::new().width(10).measure(Constraints::tight(40, 12), t),
                Size::new(40, 12)
            );
            let share = Constraints::tight(40, 0);
            assert_eq!(Spacer::new().fill(1).measure(share, t), Size::new(40, 0));
        });
    }

    #[test]
    fn a_negative_size_is_refused_at_the_door() {
        testing::with_theme(Palette::DARK, |t| {
            // A negative width would flow into a rect with `x1 < x0`, which draws as nothing and
            // reports nothing.
            assert_eq!(
                Spacer::new().width(-5).height(-5).measure(Constraints::loose(10, 10), t),
                Size::ZERO
            );
        });
    }

    #[test]
    fn the_digest_tells_a_gap_from_a_push() {
        // Same pixels, different behaviour in a row: if these collided, turning a fixed gap into a
        // filling one would leave the old layout on screen.
        assert_ne!(Spacer::new().width(4).content_hash(), Spacer::new().height(4).content_hash());
        assert_ne!(Spacer::new().fill(1).content_hash(), Spacer::new().fill(2).content_hash());
        assert_ne!(Spacer::new().content_hash(), Spacer::new().fill(1).content_hash());
        assert_eq!(Spacer::new().width(4).content_hash(), Spacer::new().width(4).content_hash());
    }

    #[test]
    fn the_default_takes_no_room_and_no_share() {
        assert_eq!(Spacer::new().flex_weight(), 0);
        assert_eq!(Spacer::new().fill(-1).flex_weight(), -1, "the container is what rejects it");
    }
}
