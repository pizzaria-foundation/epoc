//! The escape hatch: keys the convention does not cover.
//!
//! [`Softkeys`] covers the three keys every screen has, and covers them well enough that a screen
//! cannot label one and handle another. It does not cover everything, and it was never going to.
//! The recent-apps drawer kills an app on `Delete`. The icon probe cycles a bitmap index on `Left`.
//! A composer wants `Backspace` before anything else sees it.
//!
//! Without somewhere to put those, the first screen that needs one either abandons this crate or
//! bolts a key handler onto the bridge — and a handler bolted onto the bridge is the imperative
//! routing this layer exists to replace, reintroduced at the one point where nothing can see it.
//!
//! # The order, and why it is this one
//!
//! ```text
//!   1. the softkey bar          always, and unconditionally
//!   2. the innermost hatch      then outward, one enclosing scope at a time
//!   3. the widget itself        a text field's own editing keys
//! ```
//!
//! **The bar wins over everything, and it does not win by being asked first — it wins because a
//! hatch cannot bind its keys at all.** See [`OnKey::on`]. Ordering would have been enough to make
//! the bar work today and not enough to keep it working: an ordering rule is a thing a later
//! refactor can get backwards, and the failure mode is a screen a user cannot leave. Refusing the
//! binding makes it structural. There is no order of evaluation that produces a trapped screen,
//! because the trapping binding does not exist.
//!
//! **Below the bar, innermost first.** A text field that eats `Backspace` must not have to know
//! what encloses it; if the outer scope won, every container would have to enumerate the keys its
//! children might want and carefully not bind them, which is a coupling that grows with the tree.
//! Inner-first means a widget's own keys are its own business, and an enclosing scope catches only
//! what fell through.
//!
//! # Nesting is flattening
//!
//! Two hatches around the same subtree are one hatch with its bindings in order — see
//! [`OnKey::wrap`]. There is no chain to walk at dispatch time and no second resolution rule to
//! remember, because inner-first is just the order the vector is already in.
//!
//! # Why this is not a `Widget`
//!
//! It has no pixels, and it must not pretend to. A wrapper that implemented [`Widget`] would be a
//! [`Node::Leaf`] to the layout engine, and the engine cannot see inside a leaf — so the entire
//! subtree beneath it would fall onto the same uncached path `Group`'s `Widget` impl documents,
//! re-measuring itself from a throwaway cache every frame. A key binding is not worth a screen's
//! layout cache. So the hatch carries its child through untouched: [`OnKey::into_node`] hands back
//! exactly the node it was given.

use alloc::vec::Vec;

use symbian_ui::{Key, KeyEvent};

use crate::keys::Softkeys;
use crate::widgets::Node;

/// One binding: a key, and what it means here.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Binding<M> {
    key: Key,
    msg: M,
}

/// A key-to-message map for one scope, optionally carrying the subtree it applies to.
pub struct OnKey<M> {
    /// Innermost first. [`Self::dispatch`] takes the first match, so the order in this vector *is*
    /// the resolution rule — there is nowhere else for it to be written down and get out of step.
    bindings: Vec<Binding<M>>,
    child: Option<Node>,
    refused: u32,
}

impl<M: Clone> OnKey<M> {
    /// A hatch with nothing under it, for an app that only wants the mapping.
    ///
    /// Typical use is a helper an app calls from both `view` and `on_key`, since a binding has to
    /// be declared in one place to be worth anything.
    pub fn new() -> Self {
        Self { bindings: Vec::new(), child: None, refused: 0 }
    }

    /// A hatch around `child`. The child is carried through untouched — see the module note on why
    /// this is not a [`Widget`](crate::Widget).
    pub fn around(child: Node) -> Self {
        Self { bindings: Vec::new(), child: Some(child), refused: 0 }
    }

    /// Put another hatch inside this one.
    ///
    /// The inner bindings come first and stay first, which is the whole of the inner-first rule.
    /// The inner hatch's child becomes this one's, because a hatch adds no pixels and two of them
    /// around one subtree are still one subtree.
    pub fn wrap(inner: OnKey<M>) -> Self {
        Self { bindings: inner.bindings, child: inner.child, refused: inner.refused }
    }

    /// Bind `key` to `msg` in this scope.
    ///
    /// # A key the softkey bar owns is refused
    ///
    /// `Select`, `Enter`, `Softkey(..)` and `End` are the convention's, and binding one here would
    /// be writing a second action that the bar does not advertise — the label-lies-about-the-key
    /// defect in a new costume, and harder to see than the original because the label is not even
    /// wrong, it is absent.
    ///
    /// The binding is dropped and counted rather than panicking. A panic is a dead application on
    /// a phone whose whole failure report is a dialog with a number in it; a key that does nothing
    /// is a bug you can survive long enough to read [`refused`](Self::refused).
    ///
    /// **Assert `refused() == 0` in a test of any screen that uses a hatch.** Nothing else will
    /// tell you. The refusal is deliberately quiet, so the symptom of binding `Select` is a handler
    /// that never fires — which reads as the crate being broken rather than as the crate having
    /// declined you. This counter is the only thing that distinguishes the two, and a counter
    /// nobody reads is a comment.
    pub fn on(mut self, key: Key, msg: M) -> Self {
        if belongs_to_the_bar(key) {
            self.refused += 1;
            return self;
        }
        self.bindings.push(Binding { key, msg });
        self
    }

    /// What this key press means in this scope, if it means anything.
    ///
    /// `None` means "not mine", which is what lets the key carry on to whatever encloses this — and
    /// what lets the bridge skip `update` entirely for a key nobody wanted.
    pub fn dispatch(&self, ev: KeyEvent) -> Option<M> {
        self.bindings.iter().find(|b| b.key == ev.key).map(|b| b.msg.clone())
    }

    /// How many bindings were refused for belonging to the softkey bar.
    ///
    /// Always zero for a screen that respects the convention. Non-zero means somebody tried to
    /// rebind the action key, and the binding is not there.
    pub fn refused(&self) -> u32 {
        self.refused
    }

    /// How many bindings this scope actually holds.
    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// The subtree, exactly as it was handed in.
    ///
    /// A hatch built with [`new`](Self::new) has none, and yields a node that measures to nothing —
    /// it says it has nothing to draw and then draws nothing. Put a `new()` hatch in a tree and you
    /// get an empty box, which is the honest result of asking a key map to render itself.
    pub fn into_node(self) -> Node {
        self.child.unwrap_or_else(|| Node::leaf(Nothing))
    }
}

impl<M: Clone> Default for OnKey<M> {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether the softkey convention could route this key.
///
/// Asked of [`Softkeys`] rather than answered with a list, because a list here would be a second
/// definition of the convention and the two would drift — which is the exact failure this crate was
/// built around. A bar with all three slots filled routes precisely the keys the convention owns,
/// so if it has an answer for a key, that key is not the hatch's to take.
fn belongs_to_the_bar(key: Key) -> bool {
    let every_slot = Softkeys::new().options("", ()).action("", ()).back("", ());
    every_slot.dispatch(KeyEvent::new(key)).is_some()
}

/// A node that occupies no space, for a hatch that was never given a child.
struct Nothing;

impl crate::widget::Widget for Nothing {
    fn measure(&self, c: crate::constraints::Constraints, _t: &symbian_ui::Theme<'_>) -> symbian_gfx::Size {
        c.constrain(symbian_gfx::Size::new(0, 0))
    }
    fn draw(&self, _c: &mut symbian_gfx::Canvas<'_>, _r: symbian_gfx::Rect, _t: &symbian_ui::Theme<'_>) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_ui::Softkey;

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Msg {
        Close,
        Rename,
        Cycle,
        Inner,
        Outer,
    }

    fn press(k: Key) -> KeyEvent {
        KeyEvent::new(k)
    }

    fn drawer() -> OnKey<Msg> {
        OnKey::new().on(Key::Delete, Msg::Close).on(Key::Char('r'), Msg::Rename)
    }

    #[test]
    fn a_mapped_key_produces_its_message() {
        assert_eq!(drawer().dispatch(press(Key::Delete)), Some(Msg::Close));
        assert_eq!(drawer().dispatch(press(Key::Char('r'))), Some(Msg::Rename));
    }

    #[test]
    fn an_unmapped_key_falls_through() {
        // `None` is a real answer: it is what lets the key reach the widget inside, and what lets
        // the bridge skip `update` for a key nobody wanted.
        let d = drawer();
        assert_eq!(d.dispatch(press(Key::Up)), None);
        assert_eq!(d.dispatch(press(Key::Char('x'))), None);
        assert_eq!(d.dispatch(press(Key::Backspace)), None);
    }

    #[test]
    fn the_action_key_cannot_be_rebound_here() {
        // The requirement this widget is most likely to be misused against. Binding `Select` would
        // be a second action the bar does not advertise — the label-lies-about-the-key bug with the
        // label removed entirely.
        let sneaky = OnKey::new().on(Key::Select, Msg::Close);
        assert_eq!(sneaky.dispatch(press(Key::Select)), None, "the action key is not for sale");
        assert_eq!(sneaky.refused(), 1);
        assert_eq!(sneaky.len(), 0, "the binding is absent, not merely outranked");
    }

    #[test]
    fn no_key_the_bar_owns_is_stealable() {
        // Every key `Softkeys::dispatch` can route, refused — asked of the same function the bar
        // asks, so this cannot drift away from the convention.
        for k in [
            Key::Select,
            Key::Enter,
            Key::Softkey(Softkey::Left),
            Key::Softkey(Softkey::Middle),
            Key::Softkey(Softkey::Right),
            Key::End,
        ] {
            let h = OnKey::new().on(k, Msg::Close);
            assert_eq!(h.dispatch(press(k)), None, "{k:?} was stealable");
            assert_eq!(h.refused(), 1, "{k:?} was taken without complaint");
        }
    }

    #[test]
    fn the_keys_that_are_not_the_bars_are_all_available() {
        // The other half: the hatch must actually be useful. These are the real cases — the
        // drawer's Delete, the probe's Left, the composer's Backspace.
        for k in [Key::Delete, Key::Backspace, Key::Up, Key::Down, Key::Left, Key::Right, Key::Char('a'), Key::Call, Key::Raw(0xB4)] {
            let h = OnKey::new().on(k, Msg::Close);
            assert_eq!(h.dispatch(press(k)), Some(Msg::Close), "{k:?} should have been bindable");
            assert_eq!(h.refused(), 0);
        }
    }

    #[test]
    fn nesting_resolves_innermost_first() {
        // Both scopes want Delete. The inner one is closer to the widget, so it wins — a text
        // field's own keys must not depend on what encloses it.
        let inner = OnKey::new().on(Key::Delete, Msg::Inner);
        let outer = OnKey::wrap(inner).on(Key::Delete, Msg::Outer);
        assert_eq!(outer.dispatch(press(Key::Delete)), Some(Msg::Inner));
    }

    #[test]
    fn an_outer_scope_catches_what_the_inner_one_let_through() {
        // The other half of inner-first, and the reason it is useful rather than merely defined.
        let inner = OnKey::new().on(Key::Delete, Msg::Inner);
        let outer = OnKey::wrap(inner).on(Key::Char('c'), Msg::Cycle);
        assert_eq!(outer.dispatch(press(Key::Delete)), Some(Msg::Inner));
        assert_eq!(outer.dispatch(press(Key::Char('c'))), Some(Msg::Cycle));
        assert_eq!(outer.dispatch(press(Key::Up)), None);
    }

    #[test]
    fn nesting_three_deep_keeps_the_order() {
        let a = OnKey::new().on(Key::Delete, Msg::Inner);
        let b = OnKey::wrap(a).on(Key::Delete, Msg::Cycle);
        let c = OnKey::wrap(b).on(Key::Delete, Msg::Outer);
        assert_eq!(c.dispatch(press(Key::Delete)), Some(Msg::Inner), "depth must not reorder");
        assert_eq!(c.len(), 3, "every scope kept its binding — the outer ones are shadowed, not lost");
    }

    #[test]
    fn a_refusal_survives_being_wrapped() {
        // Otherwise wrapping would launder it: an inner hatch that tried to take the action key
        // would come out of `wrap` looking clean, and the counter is the only way anyone finds out.
        let inner = OnKey::new().on(Key::Select, Msg::Inner);
        let outer = OnKey::wrap(inner).on(Key::Delete, Msg::Outer);
        assert_eq!(outer.refused(), 1);
        assert_eq!(outer.dispatch(press(Key::Select)), None);
    }

    #[test]
    fn the_child_comes_out_exactly_as_it_went_in() {
        // The hatch is not a layout boundary. If it wrapped the child in anything, the subtree
        // below it would stop being visible to the engine and would re-measure itself every frame.
        let child = Node::leaf(Nothing);
        let before = child.slot_count();
        let node = OnKey::<Msg>::around(child).on(Key::Delete, Msg::Close).into_node();
        assert_eq!(node.slot_count(), before);
    }

    #[test]
    fn a_hatch_with_no_child_draws_nothing_rather_than_panicking() {
        let node = OnKey::<Msg>::new().on(Key::Delete, Msg::Close).into_node();
        assert_eq!(node.slot_count(), 1, "one empty leaf, which is what a key map looks like");
    }

    #[test]
    fn wrapping_carries_the_child_up() {
        let inner = OnKey::<Msg>::around(Node::leaf(Nothing)).on(Key::Delete, Msg::Inner);
        let outer = OnKey::wrap(inner).on(Key::Up, Msg::Outer);
        assert_eq!(outer.dispatch(press(Key::Delete)), Some(Msg::Inner));
        assert_eq!(outer.into_node().slot_count(), 1, "the subtree was not dropped on the way out");
    }
}
