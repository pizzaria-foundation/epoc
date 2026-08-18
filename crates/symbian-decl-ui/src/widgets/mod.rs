//! The built-in widget catalogue.
//!
//! Each module is one widget, and most of them are a thin shell over something in `symbian-ui`
//! that already works: [`scroll_list`] over [`symbian_ui::list`], [`text_field`] over
//! [`symbian_ui::edit`], [`title_bar`] and [`softkey_bar`] over [`symbian_ui::chrome`]. That is the
//! intended shape. The imperative toolkit's arithmetic is correct and tested; what it lacked was a
//! way to *declare* a screen made of it.

pub mod avatar;
pub mod badge;
pub mod button;
pub mod column;
pub mod imperative;
pub mod row;
pub mod on_key;
pub mod screen;
pub mod scroll_list;
pub mod softkey_bar;
pub mod spacer;
pub mod stack;
pub mod text;
pub mod text_field;
pub mod title_bar;

pub use avatar::Avatar;
pub use badge::Badge;
pub use button::Button;
pub use column::Column;
pub use imperative::Imperative;
pub use row::Row;
pub use on_key::OnKey;
pub use screen::Screen;
pub use scroll_list::{Edge, ScrollList};
pub use softkey_bar::SoftkeyBar;
pub use spacer::Spacer;
pub use stack::Stack;
pub use text::{Ink, Text};
pub use text_field::TextField;
pub use title_bar::TitleBar;

// ---------------------------------------------------------------- the tree
//
// # Why the tree is a `Node` and not a `Box<dyn Widget>`
//
// `Widget` can say it *has* children (`Widget::children`) and what its own weight is, but it cannot
// say how it arranges them — there is no axis in the trait, and no way to recover one from a
// `&dyn Widget`. So a layout pass handed a boxed widget knows a container is a container and still
// cannot lay it out: a row and a column look identical through the trait object, and the sketch
// this crate grew from quietly assumed every container was horizontal.
//
// A `Node` keeps that fact instead of erasing it. A leaf is a boxed widget and measures itself; a
// `Group` is a container the engine knows the axis, gap and padding of, and can therefore divide.
// Nothing about a leaf changes — a widget written against `Widget` alone drops into a tree with
// `Node::leaf` and never hears about any of this.

use alloc::boxed::Box;
use alloc::vec::Vec;

use symbian_gfx::{Canvas, Color, Edges, Rect, Size};
use symbian_ui::{Handled, KeyEvent, Theme};

use crate::cache::UiCache;
use crate::constraints::Constraints;
use crate::layout::{draw_group, layout_group, measure_group, Axis, CrossAlign, MainAlign};
use crate::length::Length;
use crate::widget::{hash_bytes, hash_i32, KeyCtx, Widget, WidgetHash};

/// One position in the tree: something that draws itself, or something that arranges others.
pub enum Node {
    /// A widget that owns its own pixels and knows nothing about layout beyond its own size.
    Leaf(Box<dyn Widget>),
    /// A row or a column: the engine places what is inside it.
    Group(Group),
}

impl Node {
    /// Put any [`Widget`] in the tree.
    pub fn leaf(w: impl Widget + 'static) -> Self {
        Node::Leaf(Box::new(w))
    }

    /// How many cache slots this subtree occupies — itself plus everything under it.
    ///
    /// A leaf is always one, even when it has children of its own: whatever a leaf does inside its
    /// rect is its own business, and the engine neither measures nor places it.
    pub fn slot_count(&self) -> usize {
        match self {
            Node::Leaf(_) => 1,
            Node::Group(g) => g.slot_count(),
        }
    }

    /// This node's digest, `0` meaning "measure me every frame".
    pub fn content_hash(&self) -> WidgetHash {
        match self {
            Node::Leaf(w) => w.content_hash(),
            Node::Group(g) => g.content_hash(),
        }
    }

    /// This node's share of its parent's leftover space.
    ///
    /// Routed through [`Length::weight`] so the rule about weightless and negative fills is written
    /// once: a child claiming `Fill(0)` or `Fill(-1)` is a mistake, and counting it would divide
    /// the screen by a number nobody chose.
    /// This node's cross-axis override, if it has one — CSS's `align-self`.
    ///
    /// Asked of the child, exactly like [`weight`](Self::weight), because a `Node` is the child and
    /// there is nowhere else to keep it.
    pub fn align_self(&self) -> Option<crate::layout::CrossAlign> {
        match self {
            Node::Leaf(w) => w.align_self(),
            Node::Group(g) => g.align_self,
        }
    }

    pub fn weight(&self) -> i32 {
        match self {
            Node::Leaf(w) => Length::Fill(w.flex_weight()).weight(),
            Node::Group(g) => g.fill.weight(),
        }
    }
}

/// A line of children: a row if [`Axis::Horizontal`], a column if [`Axis::Vertical`].
///
/// [`Row`] and [`Column`] are two constructors for this one type, and deliberately not two
/// implementations. Every line of the arithmetic in [`crate::layout`] reads "main" and "cross"
/// rather than "x" and "y" precisely so that a column is a row turned ninety degrees *by
/// construction*. The alternative — the horizontal case written out and the vertical one transposed
/// by hand — is two copies that agree on the day they are written and drift on every day after.
pub struct Group {
    pub(crate) axis: Axis,
    pub(crate) children: Vec<Node>,
    pub(crate) gap: i32,
    pub(crate) padding: Edges,
    pub(crate) width: Length,
    pub(crate) height: Length,
    /// This group's share of *its parent's* main axis, whichever axis that turns out to be.
    ///
    /// Separate from `width`/`height` because a child cannot know which way its parent runs, and
    /// [`Widget::flex_weight`] gives it exactly one number to say it with. A row inside a column
    /// that said `width: Fill(1)` would otherwise be indistinguishable from one asking for a share
    /// of the column's height, and would be stretched down the screen.
    pub(crate) fill: Length,
    pub(crate) align: CrossAlign,
    /// This group's own cross-axis override — CSS `align-self`. `None` defers to its parent.
    pub(crate) align_self: Option<CrossAlign>,
    pub(crate) justify: MainAlign,
    pub(crate) background: Option<Color>,
    /// A hairline along the bottom edge — CSS's `border-bottom`, with the inset that a list row
    /// wants. `(colour, left inset)`.
    pub(crate) border_bottom: Option<(Ink, i32)>,
    /// Whether a child may paint outside this group — CSS's `overflow: visible`.
    pub(crate) overflow_visible: bool,
    /// Subtree slot count, maintained as children are added.
    slots: usize,
    /// The children's digests, folded as they arrive.
    ///
    /// Kept incrementally rather than recomputed: `content_hash` is called at least once per node
    /// per frame, and a container that walked its whole subtree to answer it would turn the cache
    /// lookup — the thing that exists to make a frame cheap — into an O(n²) walk of the screen.
    child_hash: WidgetHash,
    /// Set when any child measures every frame. See [`Group::content_hash`].
    volatile: bool,
}

impl Group {
    pub fn new(axis: Axis) -> Self {
        Self {
            axis,
            children: Vec::new(),
            gap: 0,
            padding: Edges::ZERO,
            width: Length::WrapContent,
            height: Length::WrapContent,
            fill: Length::WrapContent,
            align: CrossAlign::Start,
            align_self: None,
            justify: MainAlign::Start,
            background: None,
            border_bottom: None,
            overflow_visible: false,
            slots: 1,
            child_hash: 0,
            volatile: false,
        }
    }

    /// Add a leaf. Anything implementing [`Widget`] is welcome.
    pub fn child(self, w: impl Widget + 'static) -> Self {
        self.node(Node::leaf(w))
    }

    /// Nest another row or column, keeping it a container rather than flattening it to a leaf.
    ///
    /// A [`Group`] added through [`child`](Self::child) still *draws* correctly — it lays itself
    /// out on the spot — but it does it without the cache and re-measures its whole subtree on
    /// every frame. This is the method that keeps a nested container inside the engine.
    pub fn group(self, g: Group) -> Self {
        self.node(Node::Group(g))
    }

    /// Add an already-built node.
    pub fn node(mut self, n: Node) -> Self {
        let h = n.content_hash();
        if h == 0 {
            // A child that wants re-measuring every frame makes this group's digest useless: a hit
            // here would return a cached size *and skip the subtree entirely*, which is exactly the
            // request the child just made being silently dropped.
            self.volatile = true;
        } else {
            self.child_hash = hash_bytes(self.child_hash, &h.to_le_bytes());
        }
        self.slots += n.slot_count();
        self.children.push(n);
        self
    }

    /// Add `n` only when `cond` holds, so a screen can branch without an `if` around the builder.
    pub fn optional(self, cond: bool, n: impl FnOnce() -> Node) -> Self {
        if cond {
            self.node(n())
        } else {
            self
        }
    }

    /// Space between children, along this group's axis. Never applied before the first child or
    /// after the last one.
    pub fn gap(mut self, px: i32) -> Self {
        self.gap = px.max(0);
        self
    }

    pub fn padding(mut self, e: Edges) -> Self {
        self.padding = e;
        self
    }

    /// The same padding on every side.
    pub fn pad(self, px: i32) -> Self {
        self.padding(Edges::all(px))
    }

    /// An exact width. Without it a group is as wide as what it holds.
    pub fn width(mut self, px: i32) -> Self {
        self.width = Length::Exact(px.max(0));
        self
    }

    /// An exact height.
    pub fn height(mut self, px: i32) -> Self {
        self.height = Length::Exact(px.max(0));
        self
    }

    /// Take a share of the parent's leftover space, by weight.
    pub fn fill(mut self, weight: i32) -> Self {
        self.fill = Length::Fill(weight);
        self
    }

    /// Be as wide as the parent offered, rather than as wide as the children.
    ///
    /// The default is to wrap, which is right along a group's *own* axis and often wrong across
    /// it: a band with a background that shrank to fit its label would paint a stripe rather than
    /// a row. Use this for the axis that runs across the parent's line — a row inside a column
    /// stretches its width — and [`fill`](Self::fill) for the parent's own axis. Saying
    /// `stretch_height` on a row inside a column would claim the whole column, which is what
    /// `fill` is for and how it would be spelled.
    pub fn stretch_width(mut self) -> Self {
        self.width = Length::Fill(1);
        self
    }

    /// Be as tall as the parent offered. The transpose of [`stretch_width`](Self::stretch_width),
    /// with the same warning: it is for the axis across the parent's line.
    pub fn stretch_height(mut self) -> Self {
        self.height = Length::Fill(1);
        self
    }

    /// Where the children sit across the line: centred in a row, stretched down a column.
    ///
    /// The one setting that turns a declared row into an S60 row. A list row is 38 pixels tall and
    /// its text is 17; left at [`CrossAlign::Start`] every row in the list draws ten pixels high,
    /// which is the single difference a pixel-for-pixel comparison against the hand-written toolkit
    /// found.
    ///
    /// Per group rather than per child: one setting covers every row this device has, and a screen
    /// that needs one child placed differently can put it in a group of its own — which is cheaper
    /// to read than an alignment on every child of every row for the sake of a case our screens do
    /// not have.
    ///
    /// Deliberately absent from [`content_hash`](Widget::content_hash): a digest exists to say
    /// whether a *size* could have changed, alignment moves a rect and never a size, and rects are
    /// recomputed every frame regardless. Including it would re-measure a whole subtree to move a
    /// child ten pixels down.
    /// Where *this* group sits across its parent's line, overriding the parent's `align`.
    ///
    /// CSS's `align-self`. Not to be confused with [`align`](Self::align), which is `align-items`
    /// and decides where this group's own children sit — the pair is easy to swap and the symptom
    /// is a child that ignores an alignment that is plainly written on it.
    pub fn align_self(mut self, align: CrossAlign) -> Self {
        self.align_self = Some(align);
        self
    }

    /// Where the children sit along this group's own axis — CSS's `justify-content`.
    ///
    /// [`MainAlign::SpaceBetween`] is the one a chat row needs: name against the top of its column,
    /// preview against the bottom, without either being anchored by hand.
    /// Let children paint outside this group's rect — CSS's `overflow: visible`.
    ///
    /// Clipping is the default and the right one: a row that paints past its band lands on the
    /// title bar, which is a real defect this project has already met. But a hand-placed layout can
    /// legitimately overlap two line boxes, and one does — an unread pill is two pixels taller than
    /// the line of text beside it, and the row it came from positions it from the bottom edge with
    /// nothing stopping it reaching into the line above. Expressed as a flex column that overlap is
    /// unrepresentable; expressed as `overflow: visible` on the line that overflows, it is exact.
    ///
    /// Use it where an overlap is intended and visible in the design, not to silence a layout that
    /// does not fit.
    pub fn overflow_visible(mut self) -> Self {
        self.overflow_visible = true;
        self
    }

    /// A one-pixel rule along the bottom edge, inset from the left — `border-bottom`, as a list
    /// row means it.
    ///
    /// A separator between rows is a property of the row, not a child of it: as a child it would
    /// take a slot, claim a pixel of the main axis, and have to be excluded from every alignment
    /// calculation by hand. As a border it is drawn after the children and takes part in nothing,
    /// which is exactly what the hand-written row does with its single `hline`.
    /// Takes an [`Ink`] rather than a `Color` for the reason that runs through this crate: a view
    /// is built without a theme, so a colour has to be named by its role and resolved at draw time.
    pub fn border_bottom(mut self, ink: Ink, inset_left: i32) -> Self {
        self.border_bottom = Some((ink, inset_left.max(0)));
        self
    }

    pub fn justify(mut self, justify: MainAlign) -> Self {
        self.justify = justify;
        self
    }

    pub fn align(mut self, align: CrossAlign) -> Self {
        self.align = align;
        self
    }

    /// Paint `color` behind the children.
    pub fn background(mut self, color: Color) -> Self {
        self.background = Some(color);
        self
    }

    pub fn axis(&self) -> Axis {
        self.axis
    }

    pub fn cross_align(&self) -> CrossAlign {
        self.align
    }

    pub fn children(&self) -> &[Node] {
        &self.children
    }

    /// Slots this group and everything under it occupy. See [`Node::slot_count`].
    pub fn slot_count(&self) -> usize {
        self.slots
    }
}

impl Widget for Group {
    /// Everything that could move a child: the axis, the spacing, the sizing, and the children's
    /// own digests.
    ///
    /// Returning `0` when any child is volatile is what makes it safe for the layout pass to stop
    /// at a cache hit instead of walking into the subtree. Fold the children in but ignore their
    /// volatility and a label that changes every second would sit under a row whose digest never
    /// moved, and the screen would freeze with the right pixels for the wrong second.
    fn content_hash(&self) -> WidgetHash {
        if self.volatile {
            return 0;
        }
        let mut h = hash_i32(0, self.axis as u8 as i32);
        h = hash_i32(h, self.gap);
        h = hash_i32(h, self.padding.left);
        h = hash_i32(h, self.padding.top);
        h = hash_i32(h, self.padding.right);
        h = hash_i32(h, self.padding.bottom);
        h = hash_length(h, self.width);
        h = hash_length(h, self.height);
        h = hash_length(h, self.fill);
        h = hash_i32(h, self.children.len() as i32);
        hash_bytes(h, &self.child_hash.to_le_bytes())
    }

    /// The size this group wants — the same two-pass division the engine does, on a scratch cache.
    ///
    /// Reached only when a group is used as a plain widget: nested through [`child`](Self::child)
    /// rather than [`group`](Self::group), or handed to code that speaks `&dyn Widget` and nothing
    /// else. It allocates one small vector and throws it away, which is the price of asking a
    /// container to size itself with no memory to do it in — correct, and slow enough that the fast
    /// path is worth reaching for.
    fn measure(&self, constraints: Constraints, theme: &Theme<'_>) -> Size {
        let mut scratch = UiCache::with_capacity(self.slots);
        measure_group(self, 0, constraints, theme, &mut scratch)
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        let mut scratch = UiCache::with_capacity(self.slots);
        let offer = Constraints::tight(rect.width(), rect.height());
        measure_group(self, 0, offer, theme, &mut scratch);
        layout_group(self, 0, rect, &mut scratch);
        draw_group(self, 0, &scratch, c, theme);
    }

    /// Offer a key to everything inside, at the rects this group would draw them at.
    ///
    /// Reached only when a group is used as a plain widget — [`Screen::content`](crate::widgets::Screen::content)
    /// takes a `Box<dyn Widget>`, so a screen whose content is a row or a column arrives here. Without
    /// it the default applied and the answer was `Ignored`: every widget below a container handed to a
    /// screen was unreachable by any key.
    ///
    /// That is not hypothetical. The launcher's settings screen is a strip of tabs above a panel of
    /// rows — one column, two bands — and the first frame of it drew correctly and answered nothing at
    /// all. Every screen before it had a single leaf for content, so the gap sat there unnoticed.
    ///
    /// The scratch layout is the same bargain [`draw`](Self::draw) makes and for the same reason: a
    /// group asked to act as a leaf has no cache of its own, so it measures and places into a
    /// throwaway one. That is a cost, not a mistake — and the rects it produces are the rects it would
    /// draw at, which is the only property a key walk needs.
    fn handle_key(&self, ev: KeyEvent, rect: Rect, cx: &mut KeyCtx<'_>) -> Handled {
        let mut scratch = UiCache::with_capacity(self.slots);
        let offer = Constraints::tight(rect.width(), rect.height());
        measure_group(self, 0, offer, cx.theme, &mut scratch);
        layout_group(self, 0, rect, &mut scratch);
        crate::layout::dispatch_key_group(self, 0, ev, &scratch, cx)
    }

    /// A group's share of its own parent's line, for the case where the parent is another group
    /// and reads it through [`Node::weight`] instead.
    ///
    /// The trait carries one weight because a widget has one parent, and the parent is the thing
    /// that knows which way its line runs. That is why this is `fill` and not `width`: a row inside
    /// a column asking for `width: Fill(1)` would be indistinguishable from one asking for a share
    /// of the column's *height*, and would be stretched down the screen.
    fn flex_weight(&self) -> i32 {
        self.fill.weight()
    }
}

/// Fold a [`Length`] into a digest, tag and all — `Exact(0)` and `Fill(0)` are different requests
/// and must not collide.
pub(crate) fn hash_length(seed: WidgetHash, l: Length) -> WidgetHash {
    let (tag, v) = match l {
        Length::Exact(px) => (1, px),
        Length::Fill(w) => (2, w),
        Length::WrapContent => (3, 0),
    };
    hash_i32(hash_i32(seed, tag), v)
}

#[cfg(test)]
mod tree_tests {
    use super::*;

    #[test]
    fn a_group_knows_how_many_slots_its_subtree_needs() {
        let g = Row::new()
            .child(Spacer::new().width(1))
            .group(Column::new().child(Spacer::new()).child(Spacer::new()))
            .child(Spacer::new());
        // root + leaf + (column + 2 leaves) + leaf
        assert_eq!(g.slot_count(), 6);
    }

    #[test]
    fn the_digest_moves_when_anything_about_the_arrangement_does() {
        let base = || Row::new().gap(2).pad(1).child(Spacer::new().width(5));
        assert_eq!(base().content_hash(), base().content_hash());
        assert_ne!(base().content_hash(), base().gap(3).content_hash());
        assert_ne!(base().content_hash(), base().pad(2).content_hash());
        assert_ne!(base().content_hash(), base().child(Spacer::new()).content_hash());
        assert_ne!(
            base().content_hash(),
            Column::new().gap(2).pad(1).child(Spacer::new().width(5)).content_hash(),
            "a row and a column with the same contents are not the same layout"
        );
        // A child that changed size must change the parent, or the parent's cache hit would skip
        // the child entirely and the row would keep last frame's arithmetic.
        assert_ne!(
            base().content_hash(),
            Row::new().gap(2).pad(1).child(Spacer::new().width(6)).content_hash()
        );
    }

    #[test]
    fn one_volatile_child_makes_the_whole_ancestry_volatile() {
        struct Volatile;
        impl Widget for Volatile {
            fn measure(&self, c: Constraints, _t: &Theme<'_>) -> Size {
                c.constrain(Size::new(1, 1))
            }
            fn draw(&self, _c: &mut Canvas<'_>, _r: Rect, _t: &Theme<'_>) {}
        }
        let inner = Row::new().child(Volatile);
        assert_eq!(inner.content_hash(), 0);
        let outer = Column::new().child(Spacer::new().width(3)).group(inner);
        assert_eq!(outer.content_hash(), 0, "an ancestor that cached would silence the child");
    }

    #[test]
    fn a_group_used_as_a_leaf_still_hands_keys_to_its_children() {
        // The gap this closes: `Screen::content` takes a widget, so a screen whose content is a column
        // arrives at `Group`'s own `Widget` impl — which had no `handle_key`, so the default said
        // `Ignored` and nothing inside a container on a screen could ever be pressed. The launcher's
        // settings screen is exactly that shape and answered no keys at all.
        use core::cell::Cell;

        struct Taker(alloc::rc::Rc<Cell<Option<Rect>>>);
        impl Widget for Taker {
            fn content_hash(&self) -> WidgetHash {
                hash_i32(0, 7)
            }
            fn measure(&self, c: Constraints, _t: &Theme<'_>) -> Size {
                c.constrain(Size::new(c.max_w, 10))
            }
            fn draw(&self, _c: &mut Canvas<'_>, _r: Rect, _t: &Theme<'_>) {}
            fn handle_key(&self, _ev: KeyEvent, rect: Rect, _cx: &mut KeyCtx<'_>) -> Handled {
                self.0.set(Some(rect));
                Handled::Consumed
            }
        }

        let seen = alloc::rc::Rc::new(Cell::new(None));
        let column = Column::new()
            .stretch_width()
            .align(CrossAlign::Stretch)
            .child(Spacer::new().height(20))
            .child(Taker(seen.clone()));

        let rect = Rect::from_xywh(0, 0, 100, 60);
        crate::widget::with_key_ctx(|cx| {
            assert_eq!(
                column.handle_key(KeyEvent::new(symbian_ui::Key::Down), rect, cx),
                Handled::Consumed,
                "the key never reached the child"
            );
        });
        // And at the rect the child would have been *drawn* at — below the spacer, not at the top of
        // the group. A key answered at the wrong rect is the failure this walk has to avoid.
        assert_eq!(seen.get(), Some(Rect::from_xywh(0, 20, 100, 10)));
    }

    #[test]
    fn a_weightless_fill_does_not_join_the_division() {
        assert_eq!(Node::Group(Row::new().fill(0)).weight(), 0);
        assert_eq!(Node::Group(Row::new().fill(-2)).weight(), 0);
        assert_eq!(Node::Group(Row::new().fill(3)).weight(), 3);
        assert_eq!(Node::leaf(Spacer::new().fill(2)).weight(), 2);
        assert_eq!(Node::leaf(Spacer::new()).weight(), 0);
    }

    #[test]
    fn optional_children_are_absent_rather_than_empty() {
        let with = Row::new().optional(true, || Node::leaf(Spacer::new().width(4)));
        let without = Row::new().optional(false, || Node::leaf(Spacer::new().width(4)));
        assert_eq!(with.children().len(), 1);
        assert_eq!(without.children().len(), 0);
        // An absent child must not leave a slot behind it either.
        assert_eq!(without.slot_count(), 1);
    }

    #[test]
    fn lengths_of_different_kinds_do_not_collide() {
        assert_ne!(hash_length(0, Length::Exact(0)), hash_length(0, Length::Fill(0)));
        assert_ne!(hash_length(0, Length::Exact(0)), hash_length(0, Length::WrapContent));
    }
}
