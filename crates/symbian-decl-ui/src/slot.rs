//! State that outlives the tree describing it.
//!
//! A declarative screen is rebuilt from scratch every frame: [`Widget`](crate::Widget) values are
//! constructed, measured, drawn and dropped. Almost everything on screen is happy with that, because
//! almost everything is a pure function of the model. Two things are not: a text field's caret and a
//! list's scroll offset. Nobody wants those in the app model — a caret is not application state, it
//! is a consequence of having drawn a text field there last frame — and yet rebuilding the widget
//! must not move the caret back to zero.
//!
//! The slot table is where that lives. A view asks for state and gets back the same state it was
//! given last frame:
//!
//! ```ignore
//! let field = slots.use_state(TextField::new);   // same field, frame after frame
//! field.handle_key(ev);
//! ```
//!
//! # Identity is position, and position is a liability
//!
//! There is no name in that call. The state is found by *where the call happened* — the Nth
//! `use_state` inside the currently open group — exactly as React's hooks do, and for the same
//! reason: the alternative is making every caller invent a unique string, and callers do not, they
//! copy-paste.
//!
//! Positional identity is only stable while the call order is. Put a `use_state` behind an `if` and
//! the calls after it shift by one the frame the condition flips, and each one silently adopts its
//! neighbour's state. This module does not pretend to prevent that — nothing without a compiler
//! plugin can — so it does three things instead:
//!
//! * When the types differ, the mismatch is detected: the slot is re-initialised rather than
//!   reinterpreted, and [`SlotTable::type_mismatches`] counts it so a test or a debug build can
//!   assert it never happened. It does not panic. A panic here is a dead application on a phone
//!   whose entire failure report is a dialog with a number in it; a text field that forgot its
//!   contents once is a bug you can survive long enough to read the counter.
//! * When the types are the same it is undetectable, the state *is* swapped, and there is a test
//!   below that says so out loud rather than a comment claiming otherwise.
//! * [`SlotTable::begin_group`] is the fix. A group has a key, and inside a group the ordinals start
//!   again from zero, so a conditional wrapped in a group of its own cannot shift anything after it.
//!
//! # Why groups are keyed
//!
//! Ordinals alone break the moment a list reorders. Twenty chats keyed by position: sort by most
//! recent, and every row's draft text, caret and expansion state slides one row up the screen with
//! the chat that used to be there. Keyed by UID, the state follows the row. That is the entire
//! reason keys exist, and [`reordering_a_keyed_list_carries_each_state_with_its_key`] is the test
//! that proves it.
//!
//! # Why nothing accumulates
//!
//! A group that is not entered this frame is dropped at the end of it, with everything under it —
//! the same rule as a React component unmounting. Without that, a screen that toggles a panel every
//! frame would grow a slot table for ever, and this device has a few megabytes of heap for the whole
//! process. The cost of the rule is that state does not survive a disappearance: hide the panel for
//! one frame and its scroll position is gone. That is the right trade — state that must survive
//! being off-screen belongs in the model, where it can be reasoned about.

use core::any::Any;

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::widget::hash_str;

/// A key derived from text, for callers whose stable identity is a name rather than a number.
///
/// Keys are `u64` so that a UID (`u32`), a database id and a hashed string all fit without the
/// caller inventing a cast — and casts are where two unrelated lists start colliding.
pub fn key_str(s: &str) -> u64 {
    hash_str(0, s)
}

/// One level of the table: the states asked for here, and the groups opened from here.
///
/// `cursor` and `child_cursor` are how far through each list this frame has got. Everything before
/// the cursor was claimed this frame; everything at or after it is last frame's leftovers, and is
/// dropped when the level closes. No generation counter is needed — the cursor *is* the generation,
/// and one fewer piece of state is one fewer way to get the reclamation subtly wrong.
struct Node {
    states: Vec<Box<dyn Any>>,
    cursor: usize,
    /// `(key, node index)`, in the order this frame visited them. Held as a flat vector rather than
    /// a map because a screen has a handful of children per level, and a linear scan that starts at
    /// the expected position finds them on the first probe in the ordinary case.
    children: Vec<(u64, usize)>,
    child_cursor: usize,
}

impl Node {
    fn new() -> Self {
        Self { states: Vec::new(), cursor: 0, children: Vec::new(), child_cursor: 0 }
    }
}

/// The root level, which is never freed and never keyed: it is the frame itself.
const ROOT: usize = 0;

/// Persistent per-widget state, addressed by position in the call order.
///
/// Created once by the application and handed to the view by `&mut` on every frame. See the module
/// documentation for what identity means here and where it stops being safe.
pub struct SlotTable {
    /// Arena. Freed nodes stay in place, emptied, and are handed out again from `free`; an index
    /// held anywhere else would be a dangling reference expressed as an integer, so nothing outside
    /// this struct ever sees one.
    nodes: Vec<Node>,
    free: Vec<usize>,
    /// Currently open groups, outermost first. `stack[0]` is always [`ROOT`].
    stack: Vec<usize>,
    in_frame: bool,
    type_mismatches: u32,
    unbalanced_groups: u32,
}

impl Default for SlotTable {
    fn default() -> Self {
        Self::new()
    }
}

impl SlotTable {
    pub fn new() -> Self {
        let mut stack = Vec::with_capacity(8);
        stack.push(ROOT);
        let mut nodes = Vec::with_capacity(16);
        nodes.push(Node::new());
        Self { nodes, free: Vec::new(), stack, in_frame: false, type_mismatches: 0, unbalanced_groups: 0 }
    }

    /// Start a frame: rewind to the first slot so the view repopulates them in the same order.
    ///
    /// Closes the previous frame first if the caller did not. That is not politeness — a view that
    /// returned early leaves groups open, and starting the next frame inside them would nest the
    /// whole screen one level deeper every frame until the heap ran out.
    pub fn begin_frame(&mut self) {
        self.end_frame();
        let root = &mut self.nodes[ROOT];
        root.cursor = 0;
        root.child_cursor = 0;
        self.in_frame = true;
    }

    /// Finish a frame: drop every slot and group the frame did not ask for.
    ///
    /// Idempotent, and called for you by [`begin_frame`](Self::begin_frame). Explicit only so a
    /// caller — or a test — can force the reclamation without pretending to draw another frame.
    pub fn end_frame(&mut self) {
        if !self.in_frame {
            return;
        }
        while self.stack.len() > 1 {
            self.unbalanced_groups += 1;
            self.close_top();
            self.stack.pop();
        }
        self.close_top();
        self.in_frame = false;
    }

    /// The state at this call position, created with `initial` the first time it is asked for.
    ///
    /// `initial` is a closure rather than a value because it is only used on the first frame, and on
    /// every frame after that evaluating it would be pure waste — an allocation per frame for a
    /// `String` field, which is the exact cost this table exists to avoid.
    pub fn use_state_with<T: 'static>(&mut self, initial: impl FnOnce() -> T) -> &mut T {
        let idx = *self.stack.last().unwrap_or(&ROOT);
        let at = self.nodes[idx].cursor;
        self.nodes[idx].cursor = at + 1;

        let states = &mut self.nodes[idx].states;
        debug_assert!(at <= states.len(), "cursor ran past the slots it allocates");
        let fresh = at >= states.len();
        // The two ways a slot needs writing, resolved before `initial` is moved: it is `FnOnce`, so
        // the compiler must see exactly one call site.
        let mismatch = !fresh && !states[at].is::<T>();
        if fresh || mismatch {
            let boxed: Box<dyn Any> = Box::new(initial());
            if fresh {
                states.push(boxed);
            } else {
                states[at] = boxed;
            }
        }
        if mismatch {
            // The call order changed under us and this slot belongs to a different widget. See the
            // module documentation: reinterpreting it is impossible, panicking is worse than useless
            // on the device, so it is reset and counted.
            self.type_mismatches += 1;
        }
        self.nodes[idx].states[at]
            .downcast_mut::<T>()
            .expect("slot was just written with T")
    }

    /// [`use_state_with`](Self::use_state_with) for a value cheap enough to build unconditionally.
    pub fn use_state<T: 'static>(&mut self, initial: T) -> &mut T {
        self.use_state_with(|| initial)
    }

    /// Open a keyed group. Ordinals inside it start again at zero, and its state is found by `key`
    /// rather than by where it sits among its siblings.
    ///
    /// Must be matched by [`end_group`](Self::end_group); prefer [`group`](Self::group), which
    /// cannot be mismatched.
    ///
    /// Two siblings sharing a key are indistinguishable, so they fall back to being told apart by
    /// order — the first `begin_group(k)` of the frame takes the first node with that key, the
    /// second takes the second. Stable while the duplicates keep their relative order, and no worse
    /// than having no key at all, which is what a caller who passes a constant deserves.
    pub fn begin_group(&mut self, key: u64) {
        let parent = *self.stack.last().unwrap_or(&ROOT);
        let start = self.nodes[parent].child_cursor;

        // Search only from the cursor onward: everything before it was claimed by an earlier call
        // this frame, and claiming it twice is how two rows end up sharing one caret.
        let found = self.nodes[parent].children[start..].iter().position(|&(k, _)| k == key);
        let idx = match found {
            Some(offset) => {
                // Swap the match into cursor position. Children end the frame in this frame's order,
                // which makes reclamation a truncate and makes the next frame's first probe a hit.
                self.nodes[parent].children.swap(start, start + offset);
                self.nodes[parent].children[start].1
            }
            None => {
                let node = self.alloc_node();
                self.nodes[parent].children.insert(start, (key, node));
                node
            }
        };
        self.nodes[parent].child_cursor = start + 1;

        let node = &mut self.nodes[idx];
        node.cursor = 0;
        node.child_cursor = 0;
        self.stack.push(idx);
    }

    /// Close the innermost group, dropping whatever it did not ask for this frame.
    ///
    /// An `end_group` with no matching `begin_group` is counted rather than obeyed: popping the root
    /// would leave the table with nowhere to put state, and one miscounted group must not take the
    /// rest of the screen with it.
    pub fn end_group(&mut self) {
        if self.stack.len() <= 1 {
            self.unbalanced_groups += 1;
            return;
        }
        self.close_top();
        self.stack.pop();
    }

    /// [`begin_group`](Self::begin_group) and [`end_group`](Self::end_group) as one call, so the
    /// pair cannot be got wrong by an early `return` or a `?` in the middle.
    pub fn group<R>(&mut self, key: u64, f: impl FnOnce(&mut Self) -> R) -> R {
        self.begin_group(key);
        let out = f(self);
        self.end_group();
        out
    }

    /// How many times a slot was found holding a different type than the caller asked for.
    ///
    /// Always zero for a view whose call order is stable. Non-zero means an unkeyed conditional is
    /// shifting ordinals — assert on it in tests, log it in a debug build.
    pub fn type_mismatches(&self) -> u32 {
        self.type_mismatches
    }

    /// How many `end_group` calls had no group to close, plus how many groups a frame left open.
    pub fn unbalanced_groups(&self) -> u32 {
        self.unbalanced_groups
    }

    /// Live groups, root included. Exists so a test can prove that vanished groups are reclaimed
    /// rather than merely unreachable.
    pub fn group_count(&self) -> usize {
        self.nodes.len() - self.free.len()
    }

    /// Live state slots across every group. Freed groups are emptied, so this is the true total.
    pub fn slot_count(&self) -> usize {
        self.nodes.iter().map(|n| n.states.len()).sum()
    }

    fn alloc_node(&mut self) -> usize {
        match self.free.pop() {
            Some(idx) => idx,
            None => {
                self.nodes.push(Node::new());
                self.nodes.len() - 1
            }
        }
    }

    /// Drop everything the innermost open group did not claim this frame.
    fn close_top(&mut self) {
        let idx = *self.stack.last().unwrap_or(&ROOT);
        let cursor = self.nodes[idx].cursor;
        self.nodes[idx].states.truncate(cursor);

        let child_cursor = self.nodes[idx].child_cursor;
        if self.nodes[idx].children.len() > child_cursor {
            let dead: Vec<usize> =
                self.nodes[idx].children.drain(child_cursor..).map(|(_, n)| n).collect();
            self.free_subtrees(dead);
        }
    }

    /// Return a set of subtrees to the free list.
    ///
    /// Iterative with an explicit worklist rather than recursive: a Symbian thread gets single-digit
    /// kilobytes of stack, and a deep tree freed by recursion is a stack overflow that presents as
    /// KERN-EXEC 3 with nothing pointing back here.
    fn free_subtrees(&mut self, mut work: Vec<usize>) {
        while let Some(idx) = work.pop() {
            let node = &mut self.nodes[idx];
            node.states.clear();
            node.cursor = 0;
            node.child_cursor = 0;
            for (_, child) in node.children.drain(..) {
                work.push(child);
            }
            self.free.push(idx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::{String, ToString};
    use alloc::vec;
    use symbian_ui::{Key, KeyEvent, ListState, TextField};

    #[test]
    fn a_slot_remembers_what_the_last_frame_put_in_it() {
        // The whole point. The widget is gone; the state it was driving is not.
        let mut t = SlotTable::new();

        t.begin_frame();
        *t.use_state(0i32) = 42;
        t.end_frame();

        t.begin_frame();
        assert_eq!(*t.use_state(0i32), 42);
        t.end_frame();
    }

    #[test]
    fn two_calls_at_different_positions_do_not_share_a_slot() {
        let mut t = SlotTable::new();

        t.begin_frame();
        *t.use_state(0i32) = 1;
        *t.use_state(0i32) = 2;
        t.end_frame();

        t.begin_frame();
        assert_eq!(*t.use_state(0i32), 1);
        assert_eq!(*t.use_state(0i32), 2);
        t.end_frame();
        assert_eq!(t.slot_count(), 2, "two call sites, two slots");
    }

    #[test]
    fn two_sibling_text_fields_keep_their_own_text() {
        // Phase 3's stated goal, with the real widget state rather than a stand-in: type into one
        // field, then the other, and neither may see the other's characters.
        let mut t = SlotTable::new();

        let frame = |t: &mut SlotTable, into: usize, ch: char| {
            t.begin_frame();
            for i in 0..2 {
                let f = t.use_state_with(TextField::new);
                if i == into {
                    f.handle_key(KeyEvent::new(Key::Char(ch)), &mut symbian_ui::NoClipboard);
                }
            }
            t.end_frame();
        };

        frame(&mut t, 0, 'a');
        frame(&mut t, 1, 'z');
        frame(&mut t, 0, 'b');

        t.begin_frame();
        assert_eq!(t.use_state_with(TextField::new).text(), "ab");
        assert_eq!(t.use_state_with(TextField::new).text(), "z");
        t.end_frame();
    }

    // ---- identity: what positional slots actually guarantee -------------------------------------

    #[test]
    fn an_unkeyed_conditional_hands_the_next_widget_the_wrong_state() {
        // The classic hooks hazard, tested for what it *does* rather than for what one would like.
        // Frame 1 allocates [header, body]; frame 2 drops the header, so `body` claims ordinal 0 —
        // the header's slot — and reads the header's value. Same type, so nothing can detect it.
        //
        // This test exists so that anyone who changes the identity rule finds out here, and so that
        // the documentation cannot quietly drift into claiming a safety this does not have.
        let mut t = SlotTable::new();

        t.begin_frame();
        *t.use_state(0i32) = 100; // header
        *t.use_state(0i32) = 200; // body
        t.end_frame();

        t.begin_frame();
        let body = *t.use_state(0i32); // header is gone; body slides into its slot
        t.end_frame();

        assert_eq!(body, 100, "same type, shifted ordinal: undetectable and wrong");
        assert_eq!(t.type_mismatches(), 0, "nothing to detect — that is precisely the problem");
        assert_eq!(t.slot_count(), 1, "and the unclaimed slot is dropped, not kept aside");
    }

    #[test]
    fn a_group_key_is_the_fix_for_that_conditional() {
        // The same view written with a group per region. The body's ordinals restart inside its own
        // group, so the header appearing and disappearing cannot move them.
        fn frame(t: &mut SlotTable, with_header: bool) -> i32 {
            t.begin_frame();
            if with_header {
                t.group(key_str("header"), |t| *t.use_state(0i32) = 100);
            }
            let body = t.group(key_str("body"), |t| *t.use_state(0i32));
            t.end_frame();
            body
        }

        let mut t = SlotTable::new();
        assert_eq!(frame(&mut t, true), 0);
        t.begin_frame();
        t.group(key_str("body"), |t| *t.use_state(0i32) = 200);
        t.end_frame();

        assert_eq!(frame(&mut t, false), 200, "the body kept its own state");
        assert_eq!(frame(&mut t, true), 200, "and keeps it when the header comes back");
        assert_eq!(t.type_mismatches(), 0);
    }

    #[test]
    fn a_shifted_slot_of_another_type_is_reset_rather_than_reinterpreted() {
        // The detectable half of the hazard. A `String` landing on an `i32`'s slot must not be
        // reinterpreted and must not panic: on the device a panic is the end of the application,
        // and this is a bug worth surviving to report.
        let mut t = SlotTable::new();

        t.begin_frame();
        *t.use_state(0i32) = 7;
        *t.use_state_with(String::new) = "draft".to_string();
        t.end_frame();

        t.begin_frame();
        let s = t.use_state_with(String::new); // now at ordinal 0, where the i32 lives
        assert_eq!(s, "", "reset to the initial value, not garbage and not a panic");
        t.end_frame();

        assert_eq!(t.type_mismatches(), 1, "and counted, so a test can forbid it");
    }

    #[test]
    fn a_stable_view_never_reports_a_mismatch() {
        let mut t = SlotTable::new();
        for i in 0..50i32 {
            t.begin_frame();
            *t.use_state(0i32) = i;
            t.group(key_str("row"), |t| *t.use_state_with(String::new) = i.to_string());
            t.end_frame();
        }
        assert_eq!(t.type_mismatches(), 0);
        assert_eq!(t.unbalanced_groups(), 0);
    }

    // ---- keyed groups ---------------------------------------------------------------------------

    /// Visit `keys` in order; each row writes its own name the first time it is seen and reports
    /// whatever it finds. A row whose state followed its key reports its own name for ever.
    fn visit(t: &mut SlotTable, keys: &[&str]) -> Vec<String> {
        t.begin_frame();
        let seen = keys
            .iter()
            .map(|name| {
                t.group(key_str(name), |t| {
                    let s = t.use_state_with(String::new);
                    if s.is_empty() {
                        *s = (*name).to_string();
                    }
                    s.clone()
                })
            })
            .collect();
        t.end_frame();
        seen
    }

    #[test]
    fn reordering_a_keyed_list_carries_each_state_with_its_key() {
        // The reason keys exist. Sort a chat list by most recent and every row moves; the drafts,
        // carets and scroll offsets must move with them and not stay behind at their old ordinals.
        let mut t = SlotTable::new();
        assert_eq!(visit(&mut t, &["a", "b", "c"]), vec!["a", "b", "c"]);

        // Rotated, reversed, and back again: each row still reports the name it was born with.
        assert_eq!(visit(&mut t, &["c", "a", "b"]), vec!["c", "a", "b"]);
        assert_eq!(visit(&mut t, &["c", "b", "a"]), vec!["c", "b", "a"]);
        assert_eq!(visit(&mut t, &["a", "b", "c"]), vec!["a", "b", "c"]);
        assert_eq!(t.group_count(), 4, "root plus three rows — no duplicates accumulated");
    }

    #[test]
    fn an_unkeyed_list_is_exactly_the_bug_keys_prevent() {
        // Same three rows without keys, to show what the keyed test is buying. State stays at its
        // ordinal, so after the rotation every row is wearing its predecessor's state.
        fn visit_unkeyed(t: &mut SlotTable, keys: &[&str]) -> Vec<String> {
            t.begin_frame();
            let seen = keys
                .iter()
                .map(|name| {
                    let s = t.use_state_with(String::new);
                    if s.is_empty() {
                        *s = (*name).to_string();
                    }
                    s.clone()
                })
                .collect();
            t.end_frame();
            seen
        }

        let mut t = SlotTable::new();
        assert_eq!(visit_unkeyed(&mut t, &["a", "b", "c"]), vec!["a", "b", "c"]);
        assert_eq!(visit_unkeyed(&mut t, &["c", "a", "b"]), vec!["a", "b", "c"], "state did not move");
    }

    #[test]
    fn a_row_inserted_in_the_middle_does_not_disturb_its_neighbours() {
        let mut t = SlotTable::new();
        assert_eq!(visit(&mut t, &["a", "c"]), vec!["a", "c"]);
        assert_eq!(visit(&mut t, &["a", "b", "c"]), vec!["a", "b", "c"]);
        assert_eq!(visit(&mut t, &["a", "c"]), vec!["a", "c"]);
    }

    #[test]
    fn a_list_state_survives_its_row_being_rebuilt() {
        // The other half of phase 3's goal: scroll position is not application state, but losing it
        // on every frame would make a list unusable.
        let mut t = SlotTable::new();
        for _ in 0..3 {
            t.begin_frame();
            t.group(key_str("chats"), |t| {
                let list = t.use_state(ListState::new());
                list.selected += 1;
                list.scroll += 10;
            });
            t.end_frame();
        }

        t.begin_frame();
        let list = t.group(key_str("chats"), |t| *t.use_state(ListState::new()));
        t.end_frame();
        assert_eq!((list.selected, list.scroll), (3, 30));
    }

    #[test]
    fn duplicate_keys_fall_back_to_order_instead_of_colliding() {
        // A caller who passes a constant key gets no worse than no key at all: the two groups are
        // told apart by the order they are opened in, and stay apart frame after frame. Sharing one
        // node would have given two rows one caret, which is far harder to explain.
        let mut t = SlotTable::new();
        for frame in 1..=3i32 {
            t.begin_frame();
            let first = t.group(7, |t| {
                let s = t.use_state(0i32);
                *s += 1;
                *s
            });
            let second = t.group(7, |t| {
                let s = t.use_state(0i32);
                *s += 10;
                *s
            });
            t.end_frame();
            assert_eq!(first, frame, "the first occurrence counts by one");
            assert_eq!(second, frame * 10, "the second counts by ten, in its own slot");
        }
        assert_eq!(t.group_count(), 3, "root plus one node per occurrence");
    }

    #[test]
    fn groups_nest() {
        let mut t = SlotTable::new();
        let frame = |t: &mut SlotTable| {
            t.begin_frame();
            let v = t.group(key_str("screen"), |t| {
                t.group(key_str("list"), |t| {
                    let s = t.use_state(0i32);
                    *s += 1;
                    *s
                })
            });
            t.end_frame();
            v
        };
        assert_eq!(frame(&mut t), 1);
        assert_eq!(frame(&mut t), 2);
        assert_eq!(t.group_count(), 3);
    }

    // ---- reclamation ----------------------------------------------------------------------------

    #[test]
    fn a_group_that_vanishes_takes_its_slots_with_it() {
        let mut t = SlotTable::new();
        visit(&mut t, &["a", "b", "c", "d"]);
        assert_eq!(t.slot_count(), 4);
        assert_eq!(t.group_count(), 5);

        visit(&mut t, &["a", "b"]);
        assert_eq!(t.slot_count(), 2, "the two dropped rows released their state");
        assert_eq!(t.group_count(), 3);
    }

    #[test]
    fn a_returning_group_starts_over_rather_than_resuming() {
        // The documented cost of reclamation, pinned by a test so it cannot be discovered by
        // surprise: hide a panel for one frame and its state is gone, exactly as an unmounted
        // component's is. State that must survive being off-screen belongs in the model.
        let mut t = SlotTable::new();
        assert_eq!(visit(&mut t, &["a"]), vec!["a"]);

        t.begin_frame();
        t.group(key_str("a"), |t| *t.use_state_with(String::new) = "edited".to_string());
        t.end_frame();
        assert_eq!(visit(&mut t, &["a"]), vec!["edited"]);

        visit(&mut t, &[]); // one frame without it
        assert_eq!(t.slot_count(), 0);
        assert_eq!(visit(&mut t, &["a"]), vec!["a"], "back to its initial value");
    }

    #[test]
    fn a_panel_toggled_for_ever_does_not_grow_the_table() {
        // A screen that flips a panel on every frame ran for a thousand frames. On a device with a
        // few megabytes of heap for the entire process, a table that grew by one node per frame
        // would be an out-of-memory some minutes into a session, blamed on whatever ran last.
        let mut t = SlotTable::new();
        for frame in 0..1000 {
            t.begin_frame();
            t.group(key_str("always"), |t| {
                let s = t.use_state(0i32);
                *s += 1;
            });
            if frame % 2 == 0 {
                t.group(key_str("panel"), |t| {
                    *t.use_state_with(String::new) = "transient".to_string();
                });
            }
            t.end_frame();
        }
        assert!(t.group_count() <= 3, "root, the panel, the constant one — got {}", t.group_count());
        assert!(t.slot_count() <= 2, "got {}", t.slot_count());
    }

    #[test]
    fn a_list_that_shrinks_and_grows_again_reuses_its_nodes() {
        // Twenty rows down to ten and back, twenty times: the freed nodes must come back out of the
        // free list rather than the arena growing by ten every cycle.
        let names: Vec<String> = (0..20).map(|i| alloc::format!("row{i}")).collect();
        let all: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        let mut t = SlotTable::new();
        for _ in 0..20 {
            visit(&mut t, &all);
            visit(&mut t, &all[..10]);
        }
        visit(&mut t, &all);
        assert_eq!(t.group_count(), 21);
        assert_eq!(t.slot_count(), 20);
    }

    #[test]
    fn a_shrinking_call_count_releases_the_slots_it_stopped_asking_for() {
        let mut t = SlotTable::new();
        t.begin_frame();
        for _ in 0..8 {
            t.use_state(0i32);
        }
        t.end_frame();
        assert_eq!(t.slot_count(), 8);

        t.begin_frame();
        t.use_state(0i32);
        t.end_frame();
        assert_eq!(t.slot_count(), 1);
    }

    // ---- misuse ---------------------------------------------------------------------------------

    #[test]
    fn a_frame_that_leaves_a_group_open_is_closed_for_it() {
        // A view that returns early — an error path, a `?` — must not nest the next frame inside the
        // group it abandoned. Left unclosed, the screen would sink one level deeper per frame.
        let mut t = SlotTable::new();
        t.begin_frame();
        t.begin_group(key_str("open"));
        *t.use_state(0i32) = 5;
        // ... and no end_group.
        t.begin_frame();
        assert_eq!(*t.use_state(0i32), 0, "back at the root, not inside the abandoned group");
        t.end_frame();
        assert_eq!(t.unbalanced_groups(), 1, "and reported rather than hidden");
    }

    #[test]
    fn an_end_group_too_many_does_not_pop_the_root() {
        let mut t = SlotTable::new();
        t.begin_frame();
        *t.use_state(0i32) = 3;
        t.end_group();
        t.end_group();
        t.use_state(1i32); // still writing to the root's second slot
        t.end_frame();

        assert_eq!(t.unbalanced_groups(), 2);
        t.begin_frame();
        assert_eq!(*t.use_state(0i32), 3, "the root survived and kept its slots");
        t.end_frame();
    }

    #[test]
    fn end_frame_is_idempotent() {
        let mut t = SlotTable::new();
        t.begin_frame();
        *t.use_state(0i32) = 1;
        t.end_frame();
        t.end_frame();
        t.end_frame();
        assert_eq!(t.slot_count(), 1);
        assert_eq!(t.unbalanced_groups(), 0);
    }
}
