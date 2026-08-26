//! A line that starts a new one when it runs out of room.
//!
//! [`Row`](super::Row) divides one line among its children and [`Column`](super::Column) does the
//! same turned ninety degrees. Neither describes a set of chips, a list of tags, or a legend of key
//! hints: items of unlike widths whose *count* is not known when the screen is designed, laid along
//! a line and continuing onto the next.
//!
//! This is the one genuinely new layout concept the component library needed. Everything else it
//! wanted — proportion, alignment across a line, padding, overflow — the engine already had.
//!
//! ```ignore
//! Flow::new()
//!     .gap(theme.metrics.space.snug)
//!     .line_gap(theme.metrics.space.snug)
//!     .child(Chip::new("unread", Tone::Fresh))
//!     .child(Chip::new("muted", Tone::Calm))
//!     .child(Chip::new("failed to send", Tone::Warn))
//! ```
//!
//! # Flex weights do nothing here
//!
//! A [`Length::Fill`](crate::Length) child asks for a share of what is left of a line. In a wrapping
//! group there is nothing left: a child that does not fit opens a line of its own rather than
//! competing for this one, so there is no leftover to divide and no meaning to a weight.
//!
//! This is stated rather than enforced, and that is a real cost — a `.fill(1)` on a chip is silently
//! ignored. Enforcing it would mean either a panic in a `view` (a dead app on a phone whose entire
//! failure report is a dialog with a number in it) or a branch through the flex arithmetic that
//! exists only to reject. The honest middle is to say so here and in
//! [`crate::layout::measure_wrapping`], where the decision is made.
//!
//! # `justify` does nothing here either
//!
//! `justify-content` distributes a line's slack, and a wrapping line's slack is whatever the break
//! left over — a number that is different on every line and means nothing on the last one. CSS
//! answers this with `align-content` and a per-line justification; this device has one screen shape
//! and no case that wants it. When one appears, per-line `justify` is the place it goes.
//!
//! # What alignment means
//!
//! [`Group::align`](super::Group::align) applies **within each line**, not across the block. A row of
//! chips of two heights centres each chip in its own line; measured against the whole block, the
//! short chips on the last line would drift toward the middle of the thing rather than of their row.

use symbian_gfx::Color;

use crate::spacing::{Gap, Pad};

use crate::layout::{Axis, CrossAlign};
use crate::widget::Widget;
use crate::widgets::{Group, Node};

/// A wrapping run of children.
///
/// Builds a [`Group`], so it stays a container the engine can measure and cache — the same reason
/// [`FocusScope`](super::FocusScope) does. A flow that laid out its own children as a leaf would be
/// the `Group: Widget` trap, and a row of twenty chips would re-measure all twenty every frame with
/// nothing on screen to show it.
pub struct Flow {
    group: Group,
}

impl Flow {
    /// A run that fills across the screen and continues below.
    pub fn new() -> Self {
        Self::along(Axis::Horizontal)
    }

    /// A run that fills down the screen and continues to the right. Rare, and here for symmetry
    /// rather than for a screen that wants it — the engine is written in main and cross, so the
    /// vertical case costs nothing but a constructor.
    pub fn vertical() -> Self {
        Self::along(Axis::Vertical)
    }

    fn along(axis: Axis) -> Self {
        let mut group = Group::new(axis);
        group.wrap = true;
        Self { group }
    }

    /// Space between two children on the same line.
    ///
    /// Counted against the line's room, not only against the children: two 48-pixel chips fit a
    /// 100-pixel line and the same two with an 8-pixel join do not. A packer that charged only the
    /// items would overflow every line by exactly one gap.
    pub fn gap(mut self, g: impl Into<Gap>) -> Self {
        self.group = self.group.gap(g);
        self
    }

    /// Space between one line and the next.
    ///
    /// Its own setting rather than reusing [`gap`](Self::gap), because the two are different
    /// distances in the design: chips sit close together side by side and want more air between rows,
    /// which is exactly the CSS `row-gap`/`column-gap` split.
    pub fn line_gap(mut self, g: impl Into<Gap>) -> Self {
        self.group.cross_gap = g.into();
        self
    }

    /// Add a leaf.
    pub fn child(mut self, w: impl Widget + 'static) -> Self {
        self.group = self.group.child(w);
        self
    }

    /// Add an already-built node.
    pub fn node(mut self, n: Node) -> Self {
        self.group = self.group.node(n);
        self
    }

    /// Add a child only when `cond` holds.
    pub fn optional(mut self, cond: bool, n: impl FnOnce() -> Node) -> Self {
        self.group = self.group.optional(cond, n);
        self
    }

    /// Where each child sits within **its own line**. See the module docs.
    pub fn align(mut self, align: CrossAlign) -> Self {
        self.group = self.group.align(align);
        self
    }

    pub fn padding(mut self, p: impl Into<Pad>) -> Self {
        self.group = self.group.padding(p);
        self
    }

    /// The same padding on every side.
    pub fn pad(mut self, g: impl Into<Gap>) -> Self {
        self.group = self.group.pad(g);
        self
    }

    pub fn background(mut self, color: Color) -> Self {
        self.group = self.group.background(color);
        self
    }

    /// Be as wide as the parent offered rather than as wide as the widest line.
    ///
    /// Usually what a flow wants: a block that shrank to its content would break at its own width
    /// rather than at the screen's, which reads as the chips having chosen to stack.
    pub fn stretch_width(mut self) -> Self {
        self.group = self.group.stretch_width();
        self
    }

    /// Take a share of the parent's leftover space, by weight. This is the flow's *own* weight in
    /// its parent, which is meaningful — unlike a weight on one of its children.
    pub fn fill(mut self, weight: i32) -> Self {
        self.group = self.group.fill(weight);
        self
    }

    pub fn build(self) -> Node {
        Node::Group(self.group)
    }
}

impl Default for Flow {
    fn default() -> Self {
        Self::new()
    }
}

impl From<Flow> for Node {
    fn from(f: Flow) -> Node {
        f.build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::UiCache;
    use crate::layout;
    use crate::widgets::Spacer;
    use symbian_gfx::Rect;
    use symbian_ui::{testing, Palette};

    /// A flow of fixed boxes, so the arithmetic is the only thing under test.
    fn flow_of(widths: &[i32], h: i32, gap: i32, line_gap: i32) -> Node {
        let mut f = Flow::new().gap(gap).line_gap(line_gap).stretch_width();
        for w in widths {
            f = f.child(Spacer::new().width(*w).height(h));
        }
        f.build()
    }

    /// Measure and place `root` in `area`, and report every child's rect in order.
    fn rects(root: &Node, area: Rect) -> Vec<Rect> {
        testing::with_theme(Palette::DARK, |theme| {
            let mut cache = UiCache::with_capacity(root.slot_count());
            layout::place_frame(root, area, &mut cache, theme);
            (1..root.slot_count()).map(|s| cache.rect(s).unwrap_or(Rect::from_xywh(0, 0, 0, 0))).collect()
        })
    }

    #[test]
    fn children_that_fit_stay_on_one_line() {
        let root = flow_of(&[20, 20, 20], 10, 4, 6);
        let got = rects(&root, Rect::from_xywh(0, 0, 100, 50));
        assert_eq!(got[0], Rect::from_xywh(0, 0, 20, 10));
        assert_eq!(got[1], Rect::from_xywh(24, 0, 20, 10));
        assert_eq!(got[2], Rect::from_xywh(48, 0, 20, 10));
    }

    #[test]
    fn a_child_that_does_not_fit_starts_the_next_line() {
        // 40 + 4 + 40 = 84 fits in 100; the third would need 128.
        let root = flow_of(&[40, 40, 40], 10, 4, 6);
        let got = rects(&root, Rect::from_xywh(0, 0, 100, 50));
        assert_eq!(got[0].y0, 0);
        assert_eq!(got[1].y0, 0);
        assert_eq!(got[2], Rect::from_xywh(0, 16, 40, 10), "line two, below the line gap");
    }

    #[test]
    fn the_line_gap_goes_between_lines_and_not_after_the_last() {
        // Three lines of 10 with a 6-pixel join is 42, not 48.
        let root = flow_of(&[90, 90, 90], 10, 4, 6);
        let measured = testing::with_theme(Palette::DARK, |theme| {
            let mut cache = UiCache::with_capacity(root.slot_count());
            layout::measure_tree(&root, crate::Constraints::loose(100, 200), theme, &mut cache)
        });
        assert_eq!(measured.h, 42);
    }

    #[test]
    fn a_flow_is_as_tall_as_the_lines_it_needs() {
        // The property that makes it usable inside a column: the height comes back from the measure
        // pass, so whatever encloses it can reserve the right band without knowing the item count.
        let one = flow_of(&[20, 20], 10, 4, 6);
        let two = flow_of(&[20, 20, 20, 20, 20, 20], 10, 4, 6);
        let (h1, h2) = testing::with_theme(Palette::DARK, |theme| {
            let mut c1 = UiCache::with_capacity(one.slot_count());
            let mut c2 = UiCache::with_capacity(two.slot_count());
            let offer = crate::Constraints::loose(60, 200);
            (
                layout::measure_tree(&one, offer, theme, &mut c1).h,
                layout::measure_tree(&two, offer, theme, &mut c2).h,
            )
        });
        assert_eq!(h1, 10);
        assert_eq!(h2, 42, "six 20-wide boxes in 60 pixels is three lines");
    }

    #[test]
    fn a_line_is_as_tall_as_its_own_tallest_child_and_not_the_blocks() {
        // Line one holds a 30-tall box, line two only 10-tall ones. If the second line took the
        // block's tallest, the flow would be 30 + 30 rather than 30 + 10.
        let root = Flow::new()
            .gap(0)
            .line_gap(0)
            .stretch_width()
            .child(Spacer::new().width(60).height(30))
            .child(Spacer::new().width(60).height(10))
            .child(Spacer::new().width(60).height(10))
            .build();
        let got = rects(&root, Rect::from_xywh(0, 0, 100, 100));
        assert_eq!(got[0], Rect::from_xywh(0, 0, 60, 30));
        assert_eq!(got[1], Rect::from_xywh(0, 30, 60, 10), "line two starts under line one");
        assert_eq!(got[2], Rect::from_xywh(0, 40, 60, 10));
    }

    #[test]
    fn alignment_is_within_the_line_and_not_across_the_block() {
        // Line one is 30 tall, line two is 10. Centring the short child in *its line* puts it at
        // 30..40; centring it in the block would put it near 10, which is inside line one.
        let root = Flow::new()
            .gap(0)
            .line_gap(0)
            .stretch_width()
            .align(CrossAlign::Center)
            .child(Spacer::new().width(60).height(30))
            .child(Spacer::new().width(60).height(4))
            .build();
        let got = rects(&root, Rect::from_xywh(0, 0, 100, 100));
        assert_eq!(got[1], Rect::from_xywh(0, 30, 60, 4), "centred in a 4-tall line is at its top");
    }

    #[test]
    fn a_child_wider_than_the_line_is_placed_rather_than_looping() {
        let root = flow_of(&[10, 500], 10, 4, 0);
        let got = rects(&root, Rect::from_xywh(0, 0, 100, 50));
        assert_eq!(got[1].x0, 0, "it broke onto its own line");
        assert_eq!(got[1].y0, 10);
        assert!(got[1].x1 <= 100, "and it is clamped to the line rather than running off it");
    }

    #[test]
    fn padding_comes_out_of_the_line_before_the_break_is_decided() {
        // A flow padded by 10 on each side breaks at 80, not 100. Charging padding after the break
        // would put the last child on each line inside the padding.
        let mut f = Flow::new().gap(0).line_gap(0).stretch_width().pad(10);
        for _ in 0..3 {
            f = f.child(Spacer::new().width(40).height(10));
        }
        let root = f.build();
        let got = rects(&root, Rect::from_xywh(0, 0, 100, 50));
        assert_eq!(got[0], Rect::from_xywh(10, 10, 40, 10));
        assert_eq!(got[1], Rect::from_xywh(50, 10, 40, 10));
        assert_eq!(got[2], Rect::from_xywh(10, 20, 40, 10), "the third does not fit in 80");
    }

    #[test]
    fn an_empty_flow_is_a_box_of_nothing_rather_than_a_panic() {
        let root = Flow::new().stretch_width().build();
        let got = rects(&root, Rect::from_xywh(0, 0, 100, 50));
        assert!(got.is_empty());
    }

    #[test]
    fn wrapping_is_in_the_digest_because_it_changes_the_shape() {
        // Unlike `align`, which moves a child inside a box it does not resize. A flow and a row with
        // the same children are different heights, and a digest that ignored it would hand the flow
        // the row's cached size.
        let flow = flow_of(&[40, 40, 40], 10, 4, 6);
        let mut row = crate::widgets::Row::new().gap(4);
        for _ in 0..3 {
            row = row.child(Spacer::new().width(40).height(10));
        }
        assert_ne!(flow.content_hash(), Node::Group(row).content_hash());
    }

    #[test]
    fn a_named_gap_reaches_the_same_pixels_the_toolkit_would() {
        // The point of `Gap` existing: a declarative screen saying `Gap::Snug` and an imperative one
        // saying `theme.metrics.space.snug` must place things identically. Asserted against the theme
        // rather than against `4`, so a theme that changed the scale moves the expectation with it.
        let snug = testing::with_theme(Palette::DARK, |t| t.metrics.space.snug);
        let named = Flow::new()
            .gap(crate::Gap::Snug)
            .stretch_width()
            .child(Spacer::new().width(20).height(10))
            .child(Spacer::new().width(20).height(10))
            .build();
        let got = rects(&named, Rect::from_xywh(0, 0, 100, 50));
        assert_eq!(got[1].x0, 20 + snug);

        // And a number still says the same thing, so no existing screen moved.
        let numbered = flow_of(&[20, 20], 10, snug, 0);
        assert_eq!(rects(&numbered, Rect::from_xywh(0, 0, 100, 50)), got);
    }

    #[test]
    fn a_role_and_a_number_that_agree_today_are_still_different_declarations() {
        // Same pixels, different digests — so a theme that ever separated them would not serve one
        // the other's cached size. A cache miss is the safe direction; a stale size is not.
        let named = Flow::new().gap(crate::Gap::Snug).child(Spacer::new().width(20).height(10)).build();
        let numbered = Flow::new().gap(4).child(Spacer::new().width(20).height(10)).build();
        assert_ne!(named.content_hash(), numbered.content_hash());
    }

    #[test]
    fn a_flow_stays_a_group_the_engine_can_cache() {
        let root = flow_of(&[20, 20], 10, 4, 6);
        assert!(matches!(root, Node::Group(_)));
        assert_eq!(root.slot_count(), 3);
    }

    #[test]
    fn a_still_flow_measures_once_and_then_not_again() {
        // The reason it is a `Group` and not a leaf. Measured twice with the same offer, the second
        // pass must touch nothing: a flow that re-measured would re-measure every chip in it.
        let root = flow_of(&[40, 40, 40], 10, 4, 6);
        testing::with_theme(Palette::DARK, |theme| {
            let mut cache = UiCache::with_capacity(root.slot_count());
            let offer = crate::Constraints::loose(100, 200);
            layout::measure_tree(&root, offer, theme, &mut cache);
            let after_first = cache.measure_calls();
            layout::measure_tree(&root, offer, theme, &mut cache);
            assert_eq!(cache.measure_calls(), after_first, "the second pass hit the cache");
        });
    }
}
