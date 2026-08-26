//! Which of several things on a screen has the cursor, and what moves it.
//!
//! [`crate::list`] answers this for a list of identical rows and [`crate::grid`] for a block of
//! cells. Neither answers it for a *form*: a toggle, then a stepper, then a select, then a button,
//! each a different height and each already owning its own left/right keys. There the cursor is not
//! an index into rows — it is an index into whatever happens to be focusable, in the order it was
//! declared.
//!
//! # Why this is not `ListState` with a different name
//!
//! `ListState` carries a scroll offset and asks a [`Rows`](crate::list::Rows) how tall row `i` is,
//! because its job is to keep a moving selection inside a viewport. A focus ring knows neither
//! geometry nor scrolling: the stops it moves between have already been placed by whatever laid out
//! the screen, and where they are is not its business. Giving it a scroll offset it never uses is
//! how the two would drift into being the same type with half its fields ignored.
//!
//! What it *does* own is the part every hand-written form got slightly differently: what happens
//! when the cursor is already at the end. See [`EdgePolicy`].
//!
//! # It counts stops, not children
//!
//! `count` is the number of *focusable* things, not the number of things. A section header between
//! two settings rows is not a stop, and a ring told otherwise parks the cursor on a heading where
//! nothing responds — the failure looks like a dead key rather than like a miscount.

use crate::input::{Handled, Key, KeyEvent};

/// Which end of the ring a cursor ran off.
///
/// Two names rather than a `bool` for the same reason [`crate::grid::GridEdge`] has four: a caller
/// that wants to do something at the boundary — hand the key to an enclosing scope, page to the
/// next screen — is asking *which* boundary, and `false` does not say.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FocusEdge {
    /// Backwards past the first stop.
    Before,
    /// Forwards past the last stop.
    After,
}

/// Which axis's arrows this ring answers.
///
/// A form runs down the screen and a segmented control runs across it, and the two must not both
/// claim `Down` — a horizontal group of three buttons inside a vertical form has to let the form
/// have the vertical keys or the cursor can never leave the row.
///
/// Deliberately declared here rather than borrowed from the declarative layer's `Axis`: this module
/// is `no_std` arithmetic that a hand-written screen must be able to use without pulling in a widget
/// tree.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FocusAxis {
    /// `Up` moves back, `Down` moves forward. What a form wants.
    Vertical,
    /// `Left` moves back, `Right` moves forward. What a row of buttons or a tab strip wants.
    Horizontal,
}

impl FocusAxis {
    /// `Some(true)` to move forward, `Some(false)` back, `None` if this key is not ours.
    ///
    /// The keys of the *other* axis return `None` on purpose, so they fall through to whatever
    /// encloses this ring or to the focused widget itself. A stepper inside a vertical form gets its
    /// `Left` and `Right` exactly because the form declines them here.
    pub fn direction(self, key: Key) -> Option<bool> {
        match (self, key) {
            (FocusAxis::Vertical, Key::Up) => Some(false),
            (FocusAxis::Vertical, Key::Down) => Some(true),
            (FocusAxis::Horizontal, Key::Left) => Some(false),
            (FocusAxis::Horizontal, Key::Right) => Some(true),
            _ => None,
        }
    }
}

/// What a ring does with an arrow that has nowhere left to go.
///
/// The three are all defensible and they are not interchangeable, which is why this is a parameter
/// rather than a constant: a settings form and a tab strip want different ones, and a ring nested
/// inside another wants the third.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum EdgePolicy {
    /// The cursor stays put and the key is *consumed anyway*.
    ///
    /// Matches [`crate::grid::GridState::handle_key`] and [`crate::list::ListState::handle_key`],
    /// which both consume a clamped arrow deliberately: letting it fall through would give one press
    /// two meanings depending on where the cursor happened to be, which is the kind of thing a user
    /// experiences as the phone being broken.
    Stop,
    /// The cursor jumps to the other end. What a short strip of tabs wants.
    Wrap,
    /// The key is *not* consumed, so whatever encloses this ring sees it.
    ///
    /// This is what makes a horizontal row of buttons inside a vertical form work at all: the row's
    /// own ring declines `Down`, and the form's ring moves past the whole row.
    Escape,
}

/// A cursor over some number of focusable stops.
///
/// One `usize`. It is this small because everything else a form seemed to need turned out to belong
/// somewhere that already knew it: how tall each stop is belongs to the layout, whether a stop is
/// focusable belongs to the stop, and how many there are is recounted every frame from the tree
/// rather than stored — a stored count is a second copy of the truth and it is wrong on the frame a
/// row appears.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct FocusRing {
    cursor: usize,
}

impl FocusRing {
    /// A ring on its first stop.
    pub const fn new() -> Self {
        Self { cursor: 0 }
    }

    /// A ring starting somewhere other than the beginning. Clamp it before trusting it.
    pub const fn at(cursor: usize) -> Self {
        Self { cursor }
    }

    /// Which stop has the cursor.
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    /// Whether stop `i` is the focused one — what a stop's `focused` flag is built from.
    ///
    /// `count == 0` is not special-cased here: a ring with no stops has `cursor == 0` after a
    /// clamp, and a screen with no stops has no `i` to ask about.
    pub const fn is_focused(&self, i: usize) -> bool {
        self.cursor == i
    }

    /// Put the cursor on a particular stop, within range.
    pub fn select(&mut self, i: usize, count: usize) {
        self.cursor = if count == 0 { 0 } else { i.min(count - 1) };
    }

    /// Pull the cursor back inside `count` stops.
    ///
    /// Called every frame before anything reads the cursor, because the number of stops comes from
    /// the model and can shrink between two frames — a form that hides its "advanced" section loses
    /// three stops, and a cursor left past the end would focus nothing at all.
    pub fn clamp(&mut self, count: usize) {
        self.select(self.cursor, count);
    }

    /// Move one stop, without deciding what an edge means.
    ///
    /// `Ok(true)` moved, `Ok(false)` had nowhere to move because there is nothing to focus, and
    /// `Err(edge)` ran off the named end **with the cursor left where it was**. Applying a policy is
    /// [`handle_key`](Self::handle_key)'s job: this half is the arithmetic, and it is the half that
    /// can be read without knowing which of three behaviours a caller chose.
    pub fn step(&mut self, forward: bool, count: usize) -> Result<bool, FocusEdge> {
        if count == 0 {
            return Ok(false);
        }
        if forward {
            if self.cursor + 1 >= count {
                return Err(FocusEdge::After);
            }
            self.cursor += 1;
        } else {
            if self.cursor == 0 {
                return Err(FocusEdge::Before);
            }
            self.cursor -= 1;
        }
        Ok(true)
    }

    /// Apply a key under a policy.
    ///
    /// Returns whether the key was consumed, and the edge it ran into if it did run into one — the
    /// caller gets both because "consumed and clamped" and "consumed and moved" are different things
    /// to a screen that wants to page at the boundary.
    pub fn handle_key(
        &mut self,
        ev: KeyEvent,
        axis: FocusAxis,
        count: usize,
        policy: EdgePolicy,
    ) -> (Handled, Option<FocusEdge>) {
        let Some(forward) = axis.direction(ev.key) else {
            return (Handled::Ignored, None);
        };
        match self.step(forward, count) {
            Ok(true) => (Handled::Consumed, None),
            // Nothing focusable at all. Consuming here would swallow an arrow on an empty form and
            // leave the user with a key that does nothing anywhere on the screen.
            Ok(false) => (Handled::Ignored, None),
            // The edge is reported by all three policies, because *which end the cursor ran into*
            // is an observation and the policy is only the response to it. A caller that pages to
            // the next month on `Before` needs to hear about it whether the ring wrapped, held or
            // declined — and it can tell the three apart from `Handled` and the cursor without
            // being told twice.
            Err(edge) => match policy {
                // Consumed although nothing moved, matching `ListState` and `GridState`. An arrow
                // that falls through only at the ends is an arrow that means two different things
                // depending on where the cursor happens to be.
                EdgePolicy::Stop => (Handled::Consumed, Some(edge)),
                // `count > 0` here: the empty case left through `Ok(false)` above.
                EdgePolicy::Wrap => {
                    self.cursor = match edge {
                        FocusEdge::After => 0,
                        FocusEdge::Before => count - 1,
                    };
                    (Handled::Consumed, Some(edge))
                }
                // The one policy that declines, so an enclosing ring sees the key. The cursor stays
                // where it is: this ring keeps its place while the one outside moves past it.
                EdgePolicy::Escape => (Handled::Ignored, Some(edge)),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(key: Key) -> KeyEvent {
        KeyEvent::new(key)
    }

    #[test]
    fn a_ring_walks_its_stops_in_order() {
        let mut r = FocusRing::new();
        assert_eq!(r.cursor(), 0);
        assert_eq!(r.step(true, 3), Ok(true));
        assert_eq!(r.cursor(), 1);
        assert_eq!(r.step(true, 3), Ok(true));
        assert_eq!(r.cursor(), 2);
        assert_eq!(r.step(false, 3), Ok(true));
        assert_eq!(r.cursor(), 1);
    }

    #[test]
    fn step_names_the_end_it_ran_off_and_leaves_the_cursor_alone() {
        // The cursor surviving is what lets a caller choose `Stop` — a `step` that clamped by
        // moving would have already made the decision for it.
        let mut last = FocusRing::at(2);
        assert_eq!(last.step(true, 3), Err(FocusEdge::After));
        assert_eq!(last.cursor(), 2);

        let mut first = FocusRing::new();
        assert_eq!(first.step(false, 3), Err(FocusEdge::Before));
        assert_eq!(first.cursor(), 0);
    }

    #[test]
    fn a_ring_with_nothing_to_focus_reports_neither_movement_nor_an_edge() {
        // Distinct from an edge on purpose: an empty form has not run off anything, and a caller
        // that pages on `Err(After)` must not page because a list arrived empty.
        let mut r = FocusRing::new();
        assert_eq!(r.step(true, 0), Ok(false));
        assert_eq!(r.step(false, 0), Ok(false));
        assert_eq!(r.cursor(), 0);
    }

    #[test]
    fn a_single_stop_is_both_ends_at_once() {
        let mut r = FocusRing::new();
        assert_eq!(r.step(true, 1), Err(FocusEdge::After));
        assert_eq!(r.step(false, 1), Err(FocusEdge::Before));
    }

    #[test]
    fn clamping_follows_a_form_that_shrank() {
        let mut r = FocusRing::at(7);
        r.clamp(3);
        assert_eq!(r.cursor(), 2);
        r.clamp(0);
        assert_eq!(r.cursor(), 0);
    }

    #[test]
    fn selecting_out_of_range_lands_on_the_last_stop_rather_than_past_it() {
        let mut r = FocusRing::new();
        r.select(9, 4);
        assert_eq!(r.cursor(), 3);
        r.select(1, 4);
        assert_eq!(r.cursor(), 1);
    }

    #[test]
    fn each_axis_answers_its_own_arrows_and_declines_the_others() {
        // The declining is the load-bearing half: a stepper inside a vertical form only ever gets
        // its Left and Right because the form said `None` to them here.
        assert_eq!(FocusAxis::Vertical.direction(Key::Down), Some(true));
        assert_eq!(FocusAxis::Vertical.direction(Key::Up), Some(false));
        assert_eq!(FocusAxis::Vertical.direction(Key::Left), None);
        assert_eq!(FocusAxis::Vertical.direction(Key::Right), None);

        assert_eq!(FocusAxis::Horizontal.direction(Key::Right), Some(true));
        assert_eq!(FocusAxis::Horizontal.direction(Key::Left), Some(false));
        assert_eq!(FocusAxis::Horizontal.direction(Key::Up), None);
        assert_eq!(FocusAxis::Horizontal.direction(Key::Down), None);

        // And neither claims the selection key, which belongs to the focused stop.
        assert_eq!(FocusAxis::Vertical.direction(Key::Select), None);
        assert_eq!(FocusAxis::Horizontal.direction(Key::Select), None);
    }

    #[test]
    fn a_key_from_the_other_axis_is_never_consumed() {
        let mut r = FocusRing::new();
        let (h, edge) = r.handle_key(ev(Key::Left), FocusAxis::Vertical, 3, EdgePolicy::Stop);
        assert_eq!(h, Handled::Ignored);
        assert_eq!(edge, None);
        assert_eq!(r.cursor(), 0);
    }

    #[test]
    fn a_move_inside_the_ring_is_consumed_and_reports_no_edge() {
        let mut r = FocusRing::new();
        let (h, edge) = r.handle_key(ev(Key::Down), FocusAxis::Vertical, 3, EdgePolicy::Stop);
        assert_eq!(h, Handled::Consumed);
        assert_eq!(edge, None);
        assert_eq!(r.cursor(), 1);
    }

    #[test]
    fn an_arrow_on_an_empty_form_falls_through() {
        let mut r = FocusRing::new();
        let (h, edge) = r.handle_key(ev(Key::Down), FocusAxis::Vertical, 0, EdgePolicy::Stop);
        assert_eq!(h, Handled::Ignored);
        assert_eq!(edge, None);
    }

    #[test]
    fn stop_holds_the_cursor_and_eats_the_key_anyway() {
        let mut r = FocusRing::at(2);
        let (h, edge) = r.handle_key(ev(Key::Down), FocusAxis::Vertical, 3, EdgePolicy::Stop);
        assert_eq!(h, Handled::Consumed);
        assert_eq!(edge, Some(FocusEdge::After));
        assert_eq!(r.cursor(), 2);
    }

    #[test]
    fn wrap_goes_round_to_the_other_end() {
        let mut r = FocusRing::at(2);
        let (h, edge) = r.handle_key(ev(Key::Down), FocusAxis::Vertical, 3, EdgePolicy::Wrap);
        assert_eq!(h, Handled::Consumed);
        assert_eq!(edge, Some(FocusEdge::After));
        assert_eq!(r.cursor(), 0);

        let (h, edge) = r.handle_key(ev(Key::Up), FocusAxis::Vertical, 3, EdgePolicy::Wrap);
        assert_eq!(h, Handled::Consumed);
        assert_eq!(edge, Some(FocusEdge::Before));
        assert_eq!(r.cursor(), 2);
    }

    #[test]
    fn wrapping_a_single_stop_stays_on_it_rather_than_reaching_past_it() {
        // `count - 1` on a one-stop ring is 0, and it has to be: the alternative is an underflow on
        // the one shape a form of a single field takes.
        let mut r = FocusRing::new();
        let (h, _) = r.handle_key(ev(Key::Up), FocusAxis::Vertical, 1, EdgePolicy::Wrap);
        assert_eq!(h, Handled::Consumed);
        assert_eq!(r.cursor(), 0);
    }

    #[test]
    fn escape_declines_the_key_so_an_enclosing_ring_can_have_it() {
        // This is what makes a horizontal row of buttons inside a vertical form work: the row's ring
        // says Ignored on Down, and the form's ring moves past the whole row.
        let mut r = FocusRing::at(2);
        let (h, edge) = r.handle_key(ev(Key::Down), FocusAxis::Vertical, 3, EdgePolicy::Escape);
        assert_eq!(h, Handled::Ignored);
        assert_eq!(edge, Some(FocusEdge::After));
        // And it keeps its place, so coming back lands where it left.
        assert_eq!(r.cursor(), 2);
    }

    #[test]
    fn every_policy_reports_the_edge_it_ran_into() {
        // The edge is an observation, not a consequence of the policy — a screen that pages on
        // `Before` must hear about it under all three, or the policy would silently change what the
        // screen does at its boundary.
        for policy in [EdgePolicy::Stop, EdgePolicy::Wrap, EdgePolicy::Escape] {
            let mut r = FocusRing::new();
            let (_, edge) = r.handle_key(ev(Key::Up), FocusAxis::Vertical, 3, policy);
            assert_eq!(edge, Some(FocusEdge::Before), "{policy:?} lost the edge");
        }
    }

    #[test]
    fn a_policy_only_speaks_at_the_edge() {
        // A move in the middle is identical under all three. If it were not, the policy would be
        // changing ordinary navigation rather than only the boundary.
        for policy in [EdgePolicy::Stop, EdgePolicy::Wrap, EdgePolicy::Escape] {
            let mut r = FocusRing::at(1);
            let got = r.handle_key(ev(Key::Down), FocusAxis::Vertical, 3, policy);
            assert_eq!(got, (Handled::Consumed, None), "{policy:?} interfered mid-ring");
            assert_eq!(r.cursor(), 2);
        }
    }

    #[test]
    fn is_focused_is_what_a_stop_reads() {
        let r = FocusRing::at(1);
        assert!(!r.is_focused(0));
        assert!(r.is_focused(1));
        assert!(!r.is_focused(2));
    }
}
