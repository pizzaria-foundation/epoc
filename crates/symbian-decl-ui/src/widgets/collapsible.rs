//! A heading that folds what is under it away.
//!
//! A settings screen with four sections and thirty rows does not fit on 205 pixels of content band.
//! Folding is how the era solved that, and it is what jQuery Mobile calls a collapsible and S60 calls
//! a form section.
//!
//! ```ignore
//! let section = Collapsible::new_open(slots, "Connectivity")
//!     .child(ListItem::new("Wi-Fi").trailing_value("On").build())
//!     .child(ListItem::new("Bluetooth").trailing_value("Off").build());
//!
//! let mut scope = FocusScope::vertical(slots).stop(|f| Node::leaf(section.head(f)));
//! for row in section.body() {
//!     scope = scope.stop(|_| row);
//! }
//! scope.build()
//! ```
//!
//! # Open or closed is the slot's, not the model's
//!
//! Whether a section is folded is a consequence of having drawn it here — like a scroll offset, and
//! unlike a selection. It is not what a [`Cmd`](crate::Cmd) is made of, and putting it in the model
//! would mean a message per section in `update`, which is exactly the routing this layer exists to
//! delete. [`Select`](super::Select) made the same call for its popup and for the same reason.
//!
//! **The cost is real and is stated rather than hidden:** [`crate::slot`]'s rule is that a group not
//! entered on a frame is dropped with everything under it. So leaving the screen and coming back finds
//! every section closed again. For a settings screen that is fine — the sections are short and the
//! user is passing through. For a long reference document it would be irritating, and that is the
//! screen that should keep the state in its model and pass it in with
//! [`open`](Collapsible::open).
//!
//! # Closed means *not built*, not hidden
//!
//! A closed section does not add its children to the tree at all. That is the point of folding: a
//! hidden child still costs a slot, a measure and a rect, and thirty of them cost thirty of each on a
//! screen showing four.
//!
//! It also means a closed section's children lose whatever they were keeping — a half-typed field, a
//! scroll offset. Same rule as above, arriving through the same door, and the same answer: state that
//! must survive being folded away belongs in the model.
//!
//! # The heading is the stop, the children are not
//!
//! Inside a [`FocusScope`](super::FocusScope) a collapsible contributes **one** stop when closed and
//! its heading plus its children's stops when open. The builder handles that by adding the heading
//! through `stop` and the children through the scope directly, which is why it takes the scope rather
//! than returning a bare `Node`: a section that returned one node would be one stop whatever it held,
//! and the cursor could never reach inside it.

use alloc::string::String;
use alloc::vec::Vec;
use core::cell::Cell;

use alloc::rc::Rc;

use symbian_gfx::{Align, Canvas, Rect, Size};
use symbian_ui::{icon, paint, Handled, Key, KeyEvent, Theme};

use crate::constraints::Constraints;
use crate::slot::SlotTable;
use crate::widget::{hash_i32, hash_str, KeyCtx, Widget, WidgetHash};
use crate::widgets::{Ink, Node};

/// The heading row of a collapsible: a label and a chevron that turns.
///
/// Its own widget rather than a [`SectionHeader`](super::SectionHeader) with an icon, because it does
/// something a heading never does — it takes a key. A `SectionHeader` is deliberately not a stop; this
/// is one, and conflating them would put the cursor on every heading in the SDK.
pub struct CollapsibleHead {
    label: String,
    open: Rc<Cell<bool>>,
    focused: bool,
}

impl CollapsibleHead {
    /// How tall a heading row is: a line of body text with air around it, like a list row but shorter.
    ///
    /// Public for the same reason `SectionHeader::height` is: a screen putting one in a mixed-height
    /// list has to reserve the band, and a guess one pixel out is a list that scrolls a fraction short
    /// of its last row for ever.
    pub fn height(theme: &Theme<'_>) -> i32 {
        theme.fonts.body.line_height() + theme.metrics.space.snug * 2
    }

    pub fn is_open(&self) -> bool {
        self.open.get()
    }
}

impl Widget for CollapsibleHead {
    fn content_hash(&self) -> WidgetHash {
        // The label, because it is the width. Not `open` and not `focused`: the row is the same box
        // folded or not — what changes is which way the chevron points and what is *below* it, and
        // what is below it is a different subtree with its own slots.
        hash_i32(hash_str(0, &self.label), 1)
    }

    fn measure(&self, constraints: Constraints, theme: &Theme<'_>) -> Size {
        constraints.constrain(Size::new(constraints.max_w, Self::height(theme)))
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        // Its own height, centred — the `CrossAlign::Stretch` trap. A heading drawn into a stretched
        // band would be a slab with a chevron floating in the middle of it.
        let h = Self::height(theme).min(rect.height());
        let band = Rect::from_xywh(rect.x0, rect.y0 + (rect.height() - h) / 2, rect.width(), h);

        if self.focused {
            symbian_ui::chrome::selection(c, band, theme);
        }
        // A hairline under it whether folded or not, so a closed section still reads as a section and
        // not as a stray row.
        paint::separator_for(c, band.y1 - 1, band.x0, band.x1, theme.palette.bg.mid());

        let (_, ink, _) = symbian_ui::chrome::control_colors(theme, self.focused);
        let text_ink =
            if self.focused { Ink::Selection.resolve(theme) } else { Ink::Text.resolve(theme) };

        let pad = theme.metrics.space.base;
        let size = theme.metrics.icon_sm;
        // Down when open, right when closed — the direction the content will go, which is the
        // convention every list on this platform uses and the only one a user does not have to learn.
        let glyph = if self.open.get() { icon::Icon::ChevronDown } else { icon::Icon::ChevronRight };
        let w = icon::width_for(glyph, size);
        let at = Rect::from_xywh(band.x0 + pad, band.y0 + (band.height() - size) / 2, w, size);
        icon::draw(c, at, glyph, ink);

        let text = Rect {
            x0: at.x1 + theme.metrics.space.snug,
            x1: band.x1 - pad,
            ..band
        };
        c.draw_text_in(text, &self.label, theme.fonts.strong, text_ink, Align::Start);
    }

    fn handle_key(&self, ev: KeyEvent, _rect: Rect, _cx: &mut KeyCtx<'_>) -> Handled {
        if !self.focused {
            return Handled::Ignored;
        }
        match ev.key {
            // `Select` folds. `Left`/`Right` also do, because a chevron pointing right is an invitation
            // to press right — and a section that ignored it would be a widget whose own picture lies.
            Key::Select => {
                self.open.set(!self.open.get());
                Handled::Consumed
            }
            Key::Right if !self.open.get() => {
                self.open.set(true);
                Handled::Consumed
            }
            Key::Left if self.open.get() => {
                self.open.set(false);
                Handled::Consumed
            }
            // Everything else falls through, including `Left` on a closed section and `Right` on an
            // open one: those are the presses that should reach whatever encloses this, and a section
            // that ate them would be one the cursor could not leave sideways.
            _ => Handled::Ignored,
        }
    }
}

/// A foldable section: a heading, and children that exist only while it is open.
pub struct Collapsible {
    label: String,
    open: Rc<Cell<bool>>,
    children: Vec<Node>,
}

impl Collapsible {
    /// A section that starts folded.
    pub fn new(slots: &mut SlotTable, label: impl Into<String>) -> Self {
        Self::with_default(slots, label, false)
    }

    /// A section that starts open.
    ///
    /// # Why the default is a constructor and not a builder method
    ///
    /// Because "apply this once, on the first frame" is exactly what
    /// [`SlotTable::use_state_with`](crate::slot::SlotTable::use_state_with) already does: its closure
    /// runs when the slot is created and never again. Passing the default *into* it makes the rule
    /// structural.
    ///
    /// The first version of this was `.open_by_default(true)` on the builder, which had to ask "has
    /// this been set before?" — and the slot table has no way to answer that. What it did instead was
    /// check whether the value was still `false`, which meant a section the user had just closed was
    /// reopened on the very next frame. A section that cannot be closed reads as a dead key, and the
    /// mistake was invisible until a test pressed it twice.
    pub fn new_open(slots: &mut SlotTable, label: impl Into<String>) -> Self {
        Self::with_default(slots, label, true)
    }

    fn with_default(slots: &mut SlotTable, label: impl Into<String>, open: bool) -> Self {
        let open = slots.use_state_with(|| Rc::new(Cell::new(open))).clone();
        Self { label: label.into(), open, children: Vec::new() }
    }

    /// A section whose folded state the caller keeps, for a screen that must remember across
    /// navigation. See the module docs on what the slot loses.
    pub fn open(mut self, state: Rc<Cell<bool>>) -> Self {
        self.open = state;
        self
    }

    /// Add a child, shown only while the section is open.
    pub fn child(mut self, node: Node) -> Self {
        self.children.push(node);
        self
    }

    // No `gap`: a section does not space its own children. They go into the enclosing `FocusScope`,
    // which already has one, and a second spacing here would be two numbers deciding one distance.

    /// The shared flag, readable while the tree is still being built.
    ///
    /// For a screen that has to know before it finishes the tree — a softkey label that says "Expand"
    /// or "Collapse", say. The same shape [`FocusStops`](super::FocusStops) has and for the same
    /// reason: the answer lives in a slot and the caller needs it during `view`.
    pub fn state(&self) -> Rc<Cell<bool>> {
        Rc::clone(&self.open)
    }

    pub fn is_open(&self) -> bool {
        self.open.get()
    }

    /// The heading, as a focusable widget for a scope's `stop`.
    pub fn head(&self, focused: bool) -> CollapsibleHead {
        CollapsibleHead { label: self.label.clone(), open: Rc::clone(&self.open), focused }
    }

    /// The children, or nothing at all when folded.
    ///
    /// **Nothing at all**, not hidden: a folded child still costs a slot, a measure and a rect, and
    /// thirty of them cost thirty of each on a screen showing four.
    pub fn body(self) -> Vec<Node> {
        if self.open.get() {
            self.children
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::with_key_ctx;
    use crate::widgets::{ListItem, Text};
    use symbian_ui::{testing, Palette};

    const BAND: Rect = Rect { x0: 0, y0: 0, x1: 320, y1: 40 };

    fn section(slots: &mut SlotTable) -> Collapsible {
        Collapsible::new(slots, "Connectivity")
            .child(ListItem::new("Wi-Fi").trailing_value("On").build())
            .child(ListItem::new("Bluetooth").trailing_value("Off").build())
    }

    fn press(h: &CollapsibleHead, key: Key) -> Handled {
        testing::with_theme(Palette::DARK, |_t| {
            with_key_ctx(|cx| h.handle_key(KeyEvent::new(key), BAND, cx))
        })
    }

    #[test]
    fn a_closed_section_builds_none_of_its_children() {
        // Folding is for the cost, not only for the look: a hidden child still costs a slot, a measure
        // and a rect. If this ever returns them, a screen with four sections pays for all thirty rows.
        let mut slots = SlotTable::new();
        let s = section(&mut slots);
        assert!(!s.is_open());
        assert!(s.body().is_empty());
    }

    #[test]
    fn an_open_section_builds_all_of_them() {
        let mut slots = SlotTable::new();
        let s = Collapsible::new_open(&mut slots, "Connectivity")
            .child(ListItem::new("Wi-Fi").trailing_value("On").build())
            .child(ListItem::new("Bluetooth").trailing_value("Off").build());
        assert!(s.is_open());
        assert_eq!(s.body().len(), 2);
    }

    #[test]
    fn the_heading_folds_on_select_and_the_state_is_shared() {
        // The head holds an `Rc` to the same cell the section does, which is what lets a key on the
        // heading change what the *next* `view` builds. Without the sharing the chevron would turn and
        // nothing underneath would move.
        let mut slots = SlotTable::new();
        let s = section(&mut slots);
        let state = s.state();
        let head = s.head(true);
        assert_eq!(press(&head, Key::Select), Handled::Consumed);
        assert!(state.get(), "the section's own flag moved");
        press(&head, Key::Select);
        assert!(!state.get());
    }

    #[test]
    fn the_chevron_points_where_the_key_would_take_you() {
        // Right opens, Left closes, and each is ignored when it would do nothing — so the press falls
        // through to whatever encloses the section rather than being eaten by a no-op.
        let mut slots = SlotTable::new();
        let s = section(&mut slots);
        let head = s.head(true);
        assert_eq!(press(&head, Key::Left), Handled::Ignored, "already closed");
        assert_eq!(press(&head, Key::Right), Handled::Consumed);
        assert_eq!(press(&head, Key::Right), Handled::Ignored, "already open");
        assert_eq!(press(&head, Key::Left), Handled::Consumed);
    }

    #[test]
    fn an_unfocused_heading_answers_nothing() {
        // Four sections on one screen and one press: without the flag every heading would fold.
        let mut slots = SlotTable::new();
        let s = section(&mut slots);
        let head = s.head(false);
        for key in [Key::Select, Key::Left, Key::Right] {
            assert_eq!(press(&head, key), Handled::Ignored, "{key:?}");
        }
        assert!(!s.is_open());
    }

    #[test]
    fn the_state_survives_the_tree_being_rebuilt() {
        // The reason it lives in the slot table at all: `view` runs again on every change, and a flag
        // held in the widget would refold the section on the frame after it was opened.
        let mut slots = SlotTable::new();
        let first = section(&mut slots);
        press(&first.head(true), Key::Select);
        drop(first);

        slots.begin_frame();
        let second = section(&mut slots);
        assert!(second.is_open(), "it folded itself back");
        assert_eq!(slots.type_mismatches(), 0);
    }

    #[test]
    fn a_default_does_not_fight_the_user() {
        // The default applies when the slot is created and never again, because it is the slot's
        // initial value rather than something the builder re-applies. The first version of this widget
        // asked "has this been set before?", which the slot table cannot answer, and reopened a section
        // the user had just closed — on every frame, so it could not be closed at all.
        let mut slots = SlotTable::new();
        let s = Collapsible::new_open(&mut slots, "Connectivity");
        assert!(s.is_open());
        press(&s.head(true), Key::Select);
        assert!(!s.is_open());
        drop(s);

        slots.begin_frame();
        let again = Collapsible::new_open(&mut slots, "Connectivity");
        assert!(!again.is_open(), "the default overrode what the user did");
    }

    #[test]
    fn a_caller_can_keep_the_state_itself() {
        // For a screen that must remember across navigation, where the slot's rule — a group not
        // entered is dropped — would lose it.
        let mut slots = SlotTable::new();
        let mine = Rc::new(Cell::new(true));
        let s = Collapsible::new(&mut slots, "Sound").open(Rc::clone(&mine));
        assert!(s.is_open());
        press(&s.head(true), Key::Select);
        assert!(!mine.get(), "the caller's cell is the one that moved");
    }

    #[test]
    fn the_heading_is_shorter_than_a_row_and_taller_than_nothing() {
        testing::with_theme(Palette::DARK, |t| {
            let h = CollapsibleHead::height(t);
            assert!(h > 0 && h < t.metrics.row_h, "a heading the height of a row reads as one");
        });
    }

    #[test]
    fn a_stretched_band_does_not_make_a_slab() {
        // The trap every widget in this catalogue shares.
        let mut slots = SlotTable::new();
        let s = section(&mut slots);
        let head = s.head(false);
        let (_, buf) = testing::with_canvas(Size::new(320, 40), |c| {
            testing::with_theme(Palette::DARK, |t| {
                c.clear(Palette::DARK.bg.mid());
                head.draw(c, BAND, t);
            });
        });
        let bg = Palette::DARK.bg.mid().to_rgb565().0;
        let rows: Vec<i32> =
            (0..40).filter(|&y| (0..320).any(|x| buf[(y * 320 + x) as usize] != bg)).collect();
        let h = testing::with_theme(Palette::DARK, CollapsibleHead::height);
        assert!(!rows.is_empty(), "it drew something");
        assert!(
            rows.last().unwrap() - rows.first().unwrap() < h,
            "the heading spread over {} rows, its own height is {h}",
            rows.last().unwrap() - rows.first().unwrap()
        );
    }

    #[test]
    fn open_and_closed_look_different() {
        let mut slots = SlotTable::new();
        let s = section(&mut slots);
        let paint = |open: bool| {
            s.state().set(open);
            let head = s.head(false);
            let (_, buf) = testing::with_canvas(Size::new(320, 40), |c| {
                testing::with_theme(Palette::DARK, |t| {
                    c.clear(Palette::DARK.bg.mid());
                    head.draw(c, BAND, t);
                });
            });
            buf
        };
        assert_ne!(paint(false), paint(true), "the chevron did not turn");
    }

    #[test]
    fn the_digest_is_the_label_and_never_zero() {
        let mut a = SlotTable::new();
        let mut b = SlotTable::new();
        let one = Collapsible::new(&mut a, "Sound").head(false);
        let two = Collapsible::new(&mut b, "Network").head(false);
        assert_ne!(one.content_hash(), two.content_hash());
        assert_ne!(one.content_hash(), 0);

        // Not focus and not open: the row is the same box either way, and folding it in would
        // re-measure a heading to turn a chevron.
        let mut c = SlotTable::new();
        let s = Collapsible::new(&mut c, "Sound");
        assert_eq!(s.head(false).content_hash(), s.head(true).content_hash());
        s.state().set(true);
        assert_eq!(s.head(false).content_hash(), one.content_hash());
        let _ = Text::new("");
    }
}
