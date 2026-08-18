//! The layout mistake a screenshot does not show.
//!
//! A widget is asked how big it wants to be and then given a rectangle. Those two numbers are
//! supposed to agree, and when they do not, nothing complains: the child draws at the size it
//! measured, the canvas clips whatever falls off the surface, and what is left is a list whose last
//! row is painted over the softkey bar. On a 320x240 screen the overlap is a few pixels of a font
//! that was already dark on dark, so the screen looks *slightly* wrong in a way a human reviewing a
//! PNG will read as an antialiasing artefact and move on.
//!
//! That is the failure this module exists to turn into a message. It is the mistake migration will
//! actually make — a hand-written screen that used to compute its own bands is handed a
//! [`Constraints`] instead, keeps returning the size it always returned, and is now a child of
//! something that believed it.
//!
//! # Why a debug assertion and not a clamp
//!
//! The layout pass clamps anyway: a rect is a rect and the child cannot draw outside the canvas.
//! Clamping *silently* is what produces the bug — the screen composes, nothing panics, and the
//! wrongness is a few pixels rather than an error. Failing loudly in a debug build and clamping in
//! a release one is the only arrangement where the developer finds out and the user does not lose
//! their phone to a panic dialog.
//!
//! # Why the whole call disappears in release
//!
//! [`check`] measures the child a second time to compare. That is not free — measuring text is the
//! expensive half of a frame — so the body is behind `cfg!(debug_assertions)` rather than only the
//! assertion inside it. In a release build the call is a function whose body is `return`, and the
//! optimiser removes it along with the `measure` that would have fed it.

use symbian_gfx::{Rect, Size};
use symbian_ui::Theme;

use crate::constraints::Constraints;
use crate::widget::Widget;

/// Whether a measured size sits inside the rectangle it was given.
///
/// An empty or inverted rect has no room for anything, so only a zero size fits it — `max(0)`
/// rather than the raw width, because an inverted rect's width is negative and every size is
/// larger than a negative number, which would make this report the opposite of the truth.
pub fn fits(size: Size, rect: Rect) -> bool {
    size.w <= rect.width().max(0) && size.h <= rect.height().max(0)
}

/// Assert, in a debug build, that `child` fits the rectangle it is about to be drawn into.
///
/// `what` names the child in the panic message. Nothing else can: a `&dyn Widget` has no name, and
/// "a widget overflowed" is a message that sends someone to the wrong file.
///
/// The offer is [`Constraints::loose`] over the rect — "up to this, and you may be smaller" — which
/// is the most generous reading of what the parent is giving. A child that returns something larger
/// than *that* is not disagreeing about tightness; it is ignoring its constraints.
#[track_caller]
pub fn check(what: &str, child: &dyn Widget, rect: Rect, theme: &Theme<'_>) {
    if !cfg!(debug_assertions) {
        return;
    }
    let offer = Constraints::loose(rect.width().max(0), rect.height().max(0));
    let size = child.measure(offer, theme);
    debug_assert!(
        fits(size, rect),
        "{what} measured {}x{} but was given {}x{} at ({},{}) — it will draw over whatever is \
         below it. A widget must return a size inside the constraints it was offered; see \
         Constraints::constrain.",
        size.w,
        size.h,
        rect.width(),
        rect.height(),
        rect.x0,
        rect.y0
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_gfx::Canvas;
    use symbian_ui::{testing, Palette};

    /// A widget that answers with a fixed size whatever it is offered — the shape a hand-written
    /// screen has after it is dropped into a layout that did not exist when it was written.
    struct Stubborn(i32, i32);

    impl Widget for Stubborn {
        fn measure(&self, _c: Constraints, _t: &Theme<'_>) -> Size {
            Size::new(self.0, self.1)
        }
        fn draw(&self, _c: &mut Canvas<'_>, _r: Rect, _t: &Theme<'_>) {}
    }

    /// A widget that does what the contract says.
    struct Polite(i32, i32);

    impl Widget for Polite {
        fn measure(&self, c: Constraints, _t: &Theme<'_>) -> Size {
            c.constrain(Size::new(self.0, self.1))
        }
        fn draw(&self, _c: &mut Canvas<'_>, _r: Rect, _t: &Theme<'_>) {}
    }

    #[test]
    fn fits_is_about_room_not_about_signs() {
        let r = Rect::from_xywh(0, 0, 100, 50);
        assert!(fits(Size::new(100, 50), r), "exactly filling is fitting");
        assert!(fits(Size::new(0, 0), r));
        assert!(!fits(Size::new(101, 50), r));
        assert!(!fits(Size::new(100, 51), r));

        // An inverted rect has negative width. Comparing against it raw would say every size fits,
        // which is the opposite of the truth and would make this check useless exactly where the
        // layout has already gone wrong.
        let inverted = Rect::new(30, 10, 5, 2);
        assert!(inverted.width() < 0);
        assert!(!fits(Size::new(1, 1), inverted));
        assert!(fits(Size::new(0, 0), inverted));
    }

    #[test]
    fn a_child_that_respects_its_offer_passes_quietly() {
        testing::with_theme(Palette::DARK, |t| {
            let band = Rect::from_xywh(0, 18, 320, 205);
            check("content", &Polite(9999, 9999), band, t);
            check("content", &Polite(10, 10), band, t);
            check("content", &Polite(0, 0), Rect::EMPTY, t);
        });
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "draw over whatever is below it")]
    fn a_child_that_ignores_its_offer_says_so() {
        // 205 pixels of content band, a child that insists on 240: on a real screen the extra 35
        // land on the softkey bar, and nothing anywhere reports it.
        testing::with_theme(Palette::DARK, |t| {
            check("content", &Stubborn(320, 240), Rect::from_xywh(0, 18, 320, 205), t);
        });
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "content measured 320x240 but was given 320x205")]
    fn the_message_names_the_child_and_both_sizes() {
        // The message is the deliverable. "A widget overflowed" sends someone to the wrong file.
        testing::with_theme(Palette::DARK, |t| {
            check("content", &Stubborn(320, 240), Rect::from_xywh(0, 18, 320, 205), t);
        });
    }
}
