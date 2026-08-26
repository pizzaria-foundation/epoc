//! One cursor moving between unlike controls.
//!
//! [`ScrollList`](super::ScrollList) moves a cursor down identical rows and [`Grid`](super::Grid)
//! moves one around a block of cells. Neither is what a *form* needs: a switch, then a stepper,
//! then a select, then a button — four different heights, four different key appetites, and one
//! D-pad. Before this, every screen that wanted that wrote its own cursor and decided for itself
//! what `Down` does on the last field.
//!
//! # This file contains no traversal arithmetic
//!
//! It is in [`symbian_ui::focus`], pure and unit-tested, for the reason the whole crate is built
//! on: a widget that grows its own `i32` calculations is a second implementation of the same bugs.
//! What lives here is *where the cursor lives* and *when the children learn they are focused* —
//! the same split [`ScrollList`](super::ScrollList) makes against `symbian_ui::list`.
//!
//! # A stop is told, not asked
//!
//! The children are built by the app's `view` before any key is dispatched, so a control cannot
//! discover its own focus at draw time — it has to be handed the flag while it is being built. That
//! is why a stop arrives as a closure:
//!
//! ```ignore
//! FocusScope::vertical(slots)
//!     .fixed(Node::leaf(SectionHeader::new("Network")))     // not a stop: no cursor lands here
//!     .stop(|f| Node::leaf(Switch::new(model.wifi).focused(f)))
//!     .stop(|f| Node::leaf(Stepper::new(model.retries, 0, 9).focused(f)))
//!     .stop(|f| Node::leaf(Button::new("Save").focused(f)))
//! ```
//!
//! `fixed` and `stop` are two methods and not one flag because the count that matters is the number
//! of *stops*. A ring told that a section header is a stop parks the cursor on a heading where
//! nothing answers, and the symptom is a dead key rather than a miscount.
//!
//! # The cursor is the slot's, not the model's
//!
//! Same rule as a scroll offset, and for a nearly identical reason. Which field has focus is a
//! consequence of having drawn this form here — it is not what a `Cmd` is made of, and an `update`
//! that moved it would be maintaining a second copy of a number the tree already knows. What *is*
//! the model's is the value in each field.
//!
//! The cost of that rule is the one [`crate::slot`] states plainly: a scope not entered on a frame
//! is dropped with everything under it, so hiding a form for one frame forgets which field had the
//! cursor. State that must survive being off-screen belongs in the model.
//!
//! # Innermost first
//!
//! [`crate::layout::dispatch_key_group`] offers a key to a scope's children *before* the scope's own
//! ring. This is the ordering [`OnKey`](super::OnKey) already uses, and the reason is the same: a
//! control must not have to know what encloses it.
//!
//! It is also what makes nesting work at all. A horizontal row of buttons inside a vertical form,
//! `Down` pressed: the inner ring is horizontal, so it declines vertical keys outright and the key
//! bubbles to the form. And a vertical [`RadioGroup`] inside a vertical form only gets to move
//! between its own options because it is asked first — outer-first, the form would move past the
//! whole group and the options would be unreachable.

use alloc::rc::Rc;
use core::cell::Cell;

use symbian_ui::focus::{EdgePolicy, FocusAxis, FocusEdge, FocusRing};
use symbian_ui::{Handled, KeyEvent};

use crate::layout::Axis;
use crate::slot::SlotTable;
use crate::widgets::{Group, Node};

/// A scope's cursor, as the layout pass sees it.
///
/// Lives on [`Group`] rather than on a widget because the engine has to be able to see into the
/// structure — see the field's own comment. `Cell` and not `RefCell` because [`FocusRing`] is
/// `Copy`: no borrow flag to get wrong, and no runtime panic path in a key dispatch on a device
/// whose whole failure report is a dialog with a number in it. That is the same choice
/// [`ScrollList`](super::ScrollList) made for its `ListState`.
pub struct FocusHook {
    ring: Rc<Cell<FocusRing>>,
    axis: FocusAxis,
    stops: usize,
    policy: EdgePolicy,
}

impl FocusHook {
    /// Apply a key to this scope's cursor. Called by the layout pass after the children declined.
    pub fn handle_key(&self, ev: KeyEvent) -> (Handled, Option<FocusEdge>) {
        let mut ring = self.ring.get();
        let out = ring.handle_key(ev, self.axis, self.stops, self.policy);
        self.ring.set(ring);
        out
    }

    /// How many stops this scope has.
    pub fn stops(&self) -> usize {
        self.stops
    }

    /// Which stop has the cursor.
    pub fn cursor(&self) -> usize {
        self.ring.get().cursor()
    }
}

/// The cursor of a scope being built, so a caller can read it before the scope exists.
///
/// Handed out by [`FocusScope::stops`] for the case the builder's `stop` closure cannot serve: a
/// screen whose softkey label depends on which field has focus — "Toggle" over a switch, "Edit" over
/// a text field — needs the cursor while it is assembling `Softkeys`, which happens outside the
/// tree entirely.
#[derive(Clone)]
pub struct FocusStops {
    ring: Rc<Cell<FocusRing>>,
}

impl FocusStops {
    /// Which stop has the cursor.
    pub fn cursor(&self) -> usize {
        self.ring.get().cursor()
    }

    /// Whether stop `i` is the focused one.
    pub fn is_focused(&self, i: usize) -> bool {
        self.ring.get().is_focused(i)
    }
}

/// A [`Group`] that moves one cursor between its focusable children.
///
/// Builds a group, so it stays a container the engine can measure and cache. `Vertical` is a column
/// and `Horizontal` a row, because a form runs down the screen and a segmented control runs across
/// it — and because the axis decides which arrows the scope answers and which it leaves to whatever
/// has the cursor.
pub struct FocusScope {
    group: Group,
    ring: Rc<Cell<FocusRing>>,
    axis: FocusAxis,
    stops: usize,
    policy: EdgePolicy,
}

impl FocusScope {
    /// A column of stops: `Up` and `Down` move the cursor, `Left` and `Right` go to whatever has it.
    pub fn vertical(slots: &mut SlotTable) -> Self {
        Self::new(slots, Axis::Vertical)
    }

    /// A row of stops: `Left` and `Right` move the cursor, `Up` and `Down` are left alone.
    pub fn horizontal(slots: &mut SlotTable) -> Self {
        Self::new(slots, Axis::Horizontal)
    }

    fn new(slots: &mut SlotTable, axis: Axis) -> Self {
        let ring = slots.use_state_with(|| Rc::new(Cell::new(FocusRing::new()))).clone();
        Self {
            group: Group::new(axis),
            ring,
            axis: match axis {
                Axis::Vertical => FocusAxis::Vertical,
                Axis::Horizontal => FocusAxis::Horizontal,
            },
            stops: 0,
            // `Stop` by default, matching `ListState` and `GridState`: an arrow that falls through
            // only at the ends is an arrow whose meaning depends on where the cursor happens to be.
            // The two policies that do something else are both deliberate choices about a boundary,
            // and a default that quietly wrapped or escaped would make them invisible.
            policy: EdgePolicy::Stop,
        }
    }

    /// What an arrow does when the cursor has nowhere left to go. Defaults to
    /// [`EdgePolicy::Stop`].
    ///
    /// [`EdgePolicy::Escape`] is the one a nested scope wants: it declines the key so the scope
    /// outside moves past this one.
    pub fn policy(mut self, policy: EdgePolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Add a focusable child. The closure is told whether it has the cursor.
    pub fn stop(mut self, child: impl FnOnce(bool) -> Node) -> Self {
        let focused = self.ring.get().is_focused(self.stops);
        self.stops += 1;
        self.group = self.group.node(child(focused));
        self
    }

    /// Add a child no cursor lands on — a heading, a divider, a line of help text.
    pub fn fixed(mut self, child: Node) -> Self {
        self.group = self.group.node(child);
        self
    }

    /// Add a stop only when `cond` holds.
    ///
    /// The stop count follows, which is the whole point: a form that hides its advanced section
    /// loses those stops, and [`build`](Self::build) clamps the cursor back inside what is left.
    /// A conditional stop still shifts the *slot* ordinals of anything after it — wrap the branch
    /// in [`SlotTable::group`](crate::slot::SlotTable::group) if it holds state of its own.
    pub fn optional_stop(self, cond: bool, child: impl FnOnce(bool) -> Node) -> Self {
        if cond {
            self.stop(child)
        } else {
            self
        }
    }

    /// The cursor, readable before the tree is finished.
    ///
    /// For the caller that needs it outside the tree — a softkey label that names what the focused
    /// field does. Inside the tree, take the flag the `stop` closure is handed instead: it is the
    /// same number and it cannot be off by one.
    pub fn stops(&self) -> FocusStops {
        FocusStops { ring: Rc::clone(&self.ring) }
    }

    /// Space between the stops, along the scope's axis.
    pub fn gap(mut self, g: impl Into<crate::spacing::Gap>) -> Self {
        self.group = self.group.gap(g);
        self
    }

    /// Take a share of the parent's leftover space, by weight.
    pub fn fill(mut self, weight: i32) -> Self {
        self.group = self.group.fill(weight);
        self
    }

    /// Be as wide as the parent offered — what a column of settings rows wants.
    pub fn stretch_width(mut self) -> Self {
        self.group = self.group.stretch_width();
        self
    }

    /// Reach the underlying [`Group`] for anything this builder does not forward: padding,
    /// alignment, a background.
    pub fn group(mut self, f: impl FnOnce(Group) -> Group) -> Self {
        self.group = f(self.group);
        self
    }

    /// Finish the scope.
    ///
    /// Clamping happens here rather than in `stop`, because the stop count is only final once every
    /// child has been added — and it has to happen at all because the count comes from the model and
    /// can shrink between two frames. A cursor left past the end focuses nothing, and the symptom is
    /// a form where no key does anything.
    pub fn build(mut self) -> Node {
        let mut ring = self.ring.get();
        ring.clamp(self.stops);
        self.ring.set(ring);

        self.group.focus = Some(FocusHook {
            ring: self.ring,
            axis: self.axis,
            stops: self.stops,
            policy: self.policy,
        });
        Node::Group(self.group)
    }
}

impl From<FocusScope> for Node {
    fn from(s: FocusScope) -> Node {
        s.build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout;
    use crate::widget::with_key_ctx;
    use crate::widgets::{Button, Text};

    /// The one message the test form sends. A button holds a value, not a callback.
    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Msg {
        One,
        Two,
        Three,
    }
    use crate::UiCache;
    use symbian_gfx::Rect;
    use symbian_ui::{testing, Key};

    /// A form of three buttons, one of which is focused. Returns the tree and the cursor handle.
    fn form(slots: &mut SlotTable, policy: EdgePolicy) -> (Node, FocusStops) {
        let scope = FocusScope::vertical(slots)
            .policy(policy)
            .fixed(Node::leaf(Text::new("Network")))
            .stop(|f| Node::leaf(Button::new("One", Msg::One).focused(f)))
            .stop(|f| Node::leaf(Button::new("Two", Msg::Two).focused(f)))
            .stop(|f| Node::leaf(Button::new("Three", Msg::Three).focused(f)));
        let stops = scope.stops();
        (scope.build(), stops)
    }

    /// Lay the tree out, then press `key` at it. Returns whether it was consumed.
    fn press(root: &Node, key: Key) -> Handled {
        testing::with_theme(symbian_ui::Palette::DARK, |theme| {
            let mut cache = UiCache::with_capacity(root.slot_count());
            layout::place_frame(root, testing::SCREEN, &mut cache, theme);
            with_key_ctx(|cx| layout::dispatch_key(root, KeyEvent::new(key), &cache, cx))
        })
    }

    #[test]
    fn a_scope_moves_its_cursor_on_its_own_axis() {
        let mut slots = SlotTable::new();
        let (root, stops) = form(&mut slots, EdgePolicy::Stop);
        assert_eq!(stops.cursor(), 0);
        assert_eq!(press(&root, Key::Down), Handled::Consumed);
        assert_eq!(stops.cursor(), 1);
        assert_eq!(press(&root, Key::Up), Handled::Consumed);
        assert_eq!(stops.cursor(), 0);
    }

    #[test]
    fn a_fixed_child_is_not_a_stop() {
        // Three buttons and a heading: four children, three stops. A ring told otherwise would park
        // the cursor on the heading, where nothing answers a key.
        let mut slots = SlotTable::new();
        let (root, _) = form(&mut slots, EdgePolicy::Stop);
        let Node::Group(g) = &root else { panic!("a scope builds a group") };
        assert_eq!(g.children().len(), 4);
        assert_eq!(g.focus().expect("a scope has a hook").stops(), 3);
    }

    #[test]
    fn the_cursor_survives_the_tree_being_rebuilt() {
        // The whole reason it lives in the slot table: `view` runs again every change, and a cursor
        // held in the widget would go back to the first field mid-form.
        let mut slots = SlotTable::new();
        let (first, _) = form(&mut slots, EdgePolicy::Stop);
        press(&first, Key::Down);
        drop(first);

        slots.begin_frame();
        let (_second, stops) = form(&mut slots, EdgePolicy::Stop);
        assert_eq!(stops.cursor(), 1);
        assert_eq!(slots.type_mismatches(), 0);
    }

    #[test]
    fn the_cursor_follows_a_form_that_shrank() {
        let mut slots = SlotTable::new();
        let (root, stops) = form(&mut slots, EdgePolicy::Stop);
        press(&root, Key::Down);
        press(&root, Key::Down);
        assert_eq!(stops.cursor(), 2);
        drop(root);

        // Next frame the last two stops are gone. Clamping in `build` is what keeps the cursor on
        // something rather than past the end.
        slots.begin_frame();
        let scope = FocusScope::vertical(&mut slots).stop(|f| Node::leaf(Button::new("Only", Msg::One).focused(f)));
        let stops = scope.stops();
        let _root = scope.build();
        assert_eq!(stops.cursor(), 0);
    }

    #[test]
    fn a_vertical_scope_leaves_the_horizontal_arrows_to_whatever_has_the_cursor() {
        // The declining is the load-bearing half: a stepper inside a form gets its Left and Right
        // only because the form does not claim them.
        let mut slots = SlotTable::new();
        let (root, stops) = form(&mut slots, EdgePolicy::Stop);
        assert_eq!(press(&root, Key::Left), Handled::Ignored);
        assert_eq!(press(&root, Key::Right), Handled::Ignored);
        assert_eq!(stops.cursor(), 0);
    }

    #[test]
    fn stop_eats_the_arrow_at_the_end_and_escape_hands_it_on() {
        let mut slots = SlotTable::new();
        let (stopping, _) = form(&mut slots, EdgePolicy::Stop);
        press(&stopping, Key::Down);
        press(&stopping, Key::Down);
        assert_eq!(press(&stopping, Key::Down), Handled::Consumed);

        let mut slots = SlotTable::new();
        let (escaping, cursor) = form(&mut slots, EdgePolicy::Escape);
        press(&escaping, Key::Down);
        press(&escaping, Key::Down);
        assert_eq!(press(&escaping, Key::Down), Handled::Ignored);
        // And it keeps its place, so coming back lands where it left.
        assert_eq!(cursor.cursor(), 2);
    }

    #[test]
    fn a_key_the_scope_declines_still_reaches_the_focused_stop() {
        // Innermost first: the children are asked before the ring, so the focused button gets
        // `Select` even though the scope is sitting in front of it.
        let mut slots = SlotTable::new();
        let (root, _) = form(&mut slots, EdgePolicy::Stop);
        assert_eq!(press(&root, Key::Select), Handled::Consumed);
    }

    #[test]
    fn only_the_focused_stop_answers() {
        // Three buttons on one screen and one press: without the flag, all three would fire. The
        // broadcast walk behaves like focused dispatch precisely because the others veto.
        let mut slots = SlotTable::new();
        let (root, _) = form(&mut slots, EdgePolicy::Stop);
        let Node::Group(g) = &root else { panic!() };
        let focused: usize = g
            .children()
            .iter()
            .filter(|c| {
                testing::with_theme(symbian_ui::Palette::DARK, |_t| {
                    with_key_ctx(|cx| {
                        c.slot_count() == 1
                            && matches!(c, Node::Leaf(w)
                                if w.handle_key(KeyEvent::new(Key::Select), Rect::from_xywh(0, 0, 40, 20), cx)
                                    == Handled::Consumed)
                    })
                })
            })
            .count();
        assert_eq!(focused, 1);
    }

    #[test]
    fn a_scope_stays_a_group_the_engine_can_cache() {
        // The `Group: Widget` trap: a scope built as a leaf around a group would be one opaque node,
        // and the whole form would re-measure every frame with nothing in the picture to look at.
        let mut slots = SlotTable::new();
        let (root, _) = form(&mut slots, EdgePolicy::Stop);
        assert!(matches!(root, Node::Group(_)));
        assert_eq!(root.slot_count(), 5, "the scope plus its four children");
    }
}
