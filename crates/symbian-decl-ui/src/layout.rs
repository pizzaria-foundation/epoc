//! Measure, place, draw — in that order, once each.
//!
//! # The three passes, and why they are three
//!
//! ```text
//! measure_tree   asks every node how big it wants to be, and remembers the answer
//! layout_tree    turns those sizes into rectangles
//! draw_tree      paints into the rectangles
//! ```
//!
//! Only the first can be expensive, and only the first is cached. That split is what makes the
//! cache work at all: **the layout pass never measures**. It takes no theme and has no way to call
//! [`Widget::measure`], because everything a placement needs — including each flexible child's
//! share of the leftover — was already decided and stored while measuring. The obvious alternative,
//! re-asking the cache during layout, sounds free and is not: layout derives its offers from the
//! rectangle it was given rather than from the offer the parent made, the two differ by a hair, and
//! a cache that keys on the offer would then miss on *every* node of *every* frame. The version of
//! this file that did that measured the whole screen twice per frame and looked like it worked.
//!
//! # Two passes inside a line
//!
//! A row hands out space in the order the space is claimed:
//!
//! ```text
//! |<-- pad -->|<-- fixed -->|<-gap->|<------ leftover, by weight ------>|<-- pad -->|
//! ```
//!
//! Padding and gaps come off the top, the children that asked for an exact size are paid next, and
//! only what survives that is divided among the `Fill` children. Dividing first and subtracting the
//! gaps afterwards is the classic version of this bug: every row is a few pixels too wide, the last
//! column is pushed off the right edge, and it only shows up once a screen has enough children for
//! the gaps to add up.
//!
//! # Integers, and where the odd pixel goes
//!
//! There is no floating point on this handset worth using. Three children splitting 100 pixels get
//! 33, 33 and 34 — never 33, 33, 33, which would leave a one-pixel gutter that moves as the screen
//! resizes. Each boundary is placed at the nearest whole pixel to where the exact division would
//! have put it (a running total, not a per-child rounding), so the error never accumulates and the
//! last child absorbs whatever is left.

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::{Handled, KeyEvent, Theme};

use crate::cache::UiCache;
use crate::constraints::Constraints;
use crate::length::Length;
use crate::widget::{KeyCtx, Widget};
use crate::widgets::{Group, Node};

/// Which way a line runs.
///
/// The engine is written entirely in terms of *main* (along the line) and *cross* (across it), so
/// there is exactly one copy of the arithmetic and a column cannot drift from a row.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Axis {
    Horizontal,
    Vertical,
}

impl Axis {
    /// Pick the value belonging to this axis. The whole transposition trick lives here.
    ///
    /// Generic because the same choice is made for pixel counts and for declared [`Length`]s, and
    /// two copies of it would be two chances to write `y` where `x` was meant.
    #[inline]
    pub fn pick<T>(self, horizontal: T, vertical: T) -> T {
        match self {
            Axis::Horizontal => horizontal,
            Axis::Vertical => vertical,
        }
    }

    #[inline]
    pub fn main(self, s: Size) -> i32 {
        self.pick(s.w, s.h)
    }

    #[inline]
    pub fn cross(self, s: Size) -> i32 {
        self.pick(s.h, s.w)
    }

    /// Rebuild a size from its main and cross extents.
    #[inline]
    pub fn size(self, main: i32, cross: i32) -> Size {
        match self {
            Axis::Horizontal => Size::new(main, cross),
            Axis::Vertical => Size::new(cross, main),
        }
    }

    /// A rect from a position along the line and a position across it.
    #[inline]
    pub fn rect(self, main0: i32, cross0: i32, main: i32, cross: i32) -> Rect {
        match self {
            Axis::Horizontal => Rect::from_xywh(main0, cross0, main, cross),
            Axis::Vertical => Rect::from_xywh(cross0, main0, cross, main),
        }
    }

    /// What to offer a child: `main_min..main_max` along the line, `0..cross_max` across it.
    ///
    /// Built by hand rather than through [`Constraints::tight`]/[`loose`](Constraints::loose)
    /// because a flexible child gets a *tight* main and a *loose* cross at the same time — it is
    /// told exactly how much of the line it won, and left to be as thin as it likes across it.
    #[inline]
    pub fn offer(self, main_min: i32, main_max: i32, cross_max: i32) -> Constraints {
        let main_min = main_min.max(0);
        let main_max = main_max.max(main_min);
        let cross_max = cross_max.max(0);
        match self {
            Axis::Horizontal => {
                Constraints { min_w: main_min, max_w: main_max, min_h: 0, max_h: cross_max }
            }
            Axis::Vertical => {
                Constraints { min_w: 0, max_w: cross_max, min_h: main_min, max_h: main_max }
            }
        }
    }
}

/// Where a child sits across its parent's line.
///
/// The main axis is divided; the cross axis is *placed*, and until this existed it was always
/// placed at the start. That is the wrong answer for the shape this device is made of: an S60 list
/// row is 38 pixels tall and holds a 17-pixel line of text, and top-anchoring it draws every row in
/// the list ten pixels high. A pixel-for-pixel comparison against the hand-written toolkit found
/// exactly one difference between the two, and it was this.
///
/// Alignment never touches the main axis, never changes a measured size, and is therefore not part
/// of any digest — see [`Group::align`](crate::widgets::Group::align).
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
#[repr(u8)]
pub enum MainAlign {
    /// Packed against the start of the line, the way a paragraph sets. The default, and what every
    /// screen written before this existed assumed.
    #[default]
    Start,
    /// Packed against the end — the bottom of a column, the right of a row.
    End,
    /// Packed in the middle, leftover split evenly on both sides.
    Center,
    /// First child at the start, last at the end, the leftover shared between the joins.
    ///
    /// CSS calls this `space-between`, and it is here because a Symbian list row *is* one: the
    /// name sits at the top of its column and the preview at the bottom, the timestamp at the top
    /// of its column and the unread badge at the bottom. The hand-written row expresses that as
    /// four rects anchored to two different edges — `r.y0 + 3` for the name, `r.y1 - 4` for the
    /// preview — which is correct and is also four places to get an inset wrong. Stated as an
    /// alignment it is one word, and the two columns of a chat row stop being unrelated
    /// arithmetic that happens to line up.
    ///
    /// With one child it degenerates to [`Start`](Self::Start): there is no second edge to reach.
    SpaceBetween,
}

/// Where the children sit along the axis the group runs on — CSS's `justify-content`.
///
/// The cross axis is [`CrossAlign`]; this is the main one. They are separate types rather than one
/// because the two axes offer genuinely different choices: a child can be *stretched* across the
/// cross axis, and there is no meaning to stretching one along the main axis when its siblings are
/// competing for the same line.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum CrossAlign {
    /// Against the top of a row, the left of a column. The default, because it is what every
    /// existing screen was laid out against.
    #[default]
    Start,
    /// Centred, with the odd pixel below or to the right.
    ///
    /// The truncating division is deliberate and matches
    /// [`Canvas::draw_text_in`](symbian_gfx::Canvas::draw_text_in), which centres a line in its box
    /// the same way. A 17-pixel line in a 38-pixel row lands at 10, leaving 10 above and 11 below,
    /// whether it got there by being centred here or by being stretched and centring its own text —
    /// which is what makes the two routes to an S60 row produce the same pixels rather than two
    /// nearly-right answers a pixel apart.
    Center,
    /// Against the bottom of a row, the right of a column.
    End,
    /// Filling the cross axis: the child is given the whole band whatever it measured.
    ///
    /// Applied when the rect is handed out, not when the child is measured. Tightening the measure
    /// offer instead would mean measuring twice on the frames a child changes — once loose to find
    /// the line's cross size and once tight to impose it — and the second measurement would
    /// overwrite the first in the cache, so the next frame would miss and do it all again. A widget
    /// paints into the rect it is given; that is all a stretch has to change.
    Stretch,
}

/// Hands out `leftover` pixels by weight, one child at a time, without losing any.
///
/// Each call returns the difference between two running totals rather than a rounded-down share, so
/// the boundaries land where exact arithmetic would have put them and the shares always add up to
/// exactly `leftover`. Rounding each share on its own instead loses up to one pixel per child, and
/// on a five-item row that is a visible strip of background down the right-hand side.
struct Shares {
    leftover: i64,
    total: i64,
    used_weight: i64,
    used_px: i32,
}

impl Shares {
    fn new(leftover: i32, total_weight: i32) -> Self {
        Self {
            leftover: leftover.max(0) as i64,
            total: total_weight.max(0) as i64,
            used_weight: 0,
            used_px: 0,
        }
    }

    fn take(&mut self, weight: i32) -> i32 {
        if self.total <= 0 || weight <= 0 {
            return 0;
        }
        self.used_weight += weight as i64;
        // 64-bit on purpose: `Constraints::unbounded` offers a quarter of `i32::MAX`, and
        // `leftover * weight` on a widget measured against it overflows a signed 32-bit multiply
        // long before anything looks wrong on screen.
        let upto = (self.leftover * self.used_weight / self.total) as i32;
        let share = upto - self.used_px;
        self.used_px = upto;
        share.max(0)
    }
}

/// Measure a whole tree against `offer`, reusing everything that has not changed.
///
/// Returns the root's size. Must run before [`layout_tree`], which reads what this pass stored.
pub fn measure_tree(
    root: &Node,
    offer: Constraints,
    theme: &Theme<'_>,
    cache: &mut UiCache,
) -> Size {
    measure_node(root, 0, offer, theme, cache)
}

/// [`measure_tree`] for a subtree whose slots start at `slot`.
pub fn measure_node(
    node: &Node,
    slot: usize,
    offer: Constraints,
    theme: &Theme<'_>,
    cache: &mut UiCache,
) -> Size {
    match node {
        Node::Leaf(w) => cache.measure_or_compute(slot, w.as_ref(), offer, theme),
        Node::Group(g) => measure_group(g, slot, offer, theme, cache),
    }
}

/// The two-pass division, and the only place a `Fill` becomes a number of pixels.
pub fn measure_group(
    g: &Group,
    slot: usize,
    offer: Constraints,
    theme: &Theme<'_>,
    cache: &mut UiCache,
) -> Size {
    let hash = g.content_hash();
    if let Some(size) = cache.lookup(slot, hash, offer) {
        // The subtree is not visited at all. That is the entire point of the cache, and it is only
        // sound because a group's digest folds in its children's — see `Group::content_hash`.
        return size;
    }

    let axis = g.axis;
    let padding = g.padding.resolve(theme);
    let gap = g.gap.resolve(theme);
    let (pad_main, pad_cross) = (
        axis.pick(padding.horizontal(), padding.vertical()),
        axis.pick(padding.vertical(), padding.horizontal()),
    );
    let gaps = gap * (g.children.len() as i32 - 1).max(0);

    // What the parent is offering, before anything is spent.
    let outer_main = resolve(axis.pick(g.width, g.height), axis.main(max_size(offer)));
    let outer_cross = resolve(axis.pick(g.height, g.width), axis.cross(max_size(offer)));
    // Gaps and padding are spent here — before the division, never after it.
    let avail_main = (outer_main - pad_main - gaps).max(0);
    let avail_cross = (outer_cross - pad_cross).max(0);

    if g.wraps() {
        let size =
            measure_wrapping(g, slot, offer, theme, cache, outer_main, outer_cross, pad_main, pad_cross);
        cache.store(slot, hash, offer, size);
        return size;
    }

    // Pass one: the children that asked for a size get it.
    //
    // Each is offered the whole line rather than what its predecessors left, and that is a cache
    // decision before it is a layout one: an offer that depended on the siblings before it would
    // make every child after a changed one a cache miss, so re-labelling the first cell of a row
    // would re-measure the entire row. Overflow is dealt with where it belongs — at placement,
    // where a child that no longer fits is clamped flat instead of being handed a negative offer.
    let mut fixed_main = 0;
    let mut content_cross = 0;
    let mut total_weight = 0;
    let mut child_slot = slot + 1;
    for child in g.children() {
        let weight = child.weight();
        total_weight += weight;
        if weight == 0 {
            let offer = axis.offer(0, avail_main, avail_cross);
            let size = measure_node(child, child_slot, offer, theme, cache);
            fixed_main += axis.main(size).clamp(0, avail_main);
            content_cross = content_cross.max(axis.cross(size).clamp(0, avail_cross));
        }
        child_slot += child.slot_count();
    }

    // Pass two: whatever survived is divided by weight, and each flexible child is measured against
    // a *tight* main. That is what makes the stored size the final size, so the layout pass has
    // nothing left to work out.
    let leftover = (avail_main - fixed_main).max(0);
    let mut shares = Shares::new(leftover, total_weight);
    let mut child_slot = slot + 1;
    for child in g.children() {
        let weight = child.weight();
        if weight > 0 {
            let share = shares.take(weight);
            let offer = axis.offer(share, share, avail_cross);
            let size = measure_node(child, child_slot, offer, theme, cache);
            content_cross = content_cross.max(axis.cross(size).clamp(0, avail_cross));
        }
        child_slot += child.slot_count();
    }

    let content_main = fixed_main + if total_weight > 0 { leftover } else { 0 } + gaps;
    let main = if axis.pick(g.width, g.height).is_wrap() {
        content_main + pad_main
    } else {
        outer_main
    };
    let cross = if axis.pick(g.height, g.width).is_wrap() {
        content_cross + pad_cross
    } else {
        outer_cross
    };

    let size = offer.constrain(axis.size(main, cross));
    cache.store(slot, hash, offer, size);
    size
}

/// [`measure_group`] for a group that wraps — see [`crate::widgets::Flow`].
///
/// # Why this is a separate function and not a branch inside the line arithmetic
///
/// A wrapping group divides nothing. The flex pass exists to share one line's leftover space by
/// weight, and there is no leftover when a child that does not fit opens a line of its own instead
/// of competing for this one. Threading `wrap` through the two-pass division would mean a `if wrap`
/// beside every use of `avail_main`, `Shares` and `total_weight` — five branches in the most
/// delicate arithmetic in the crate, four of which say "not this".
///
/// So flex weights are ignored here, deliberately and documented on
/// [`Flow`](crate::widgets::Flow): a share of a line is meaningless when lines are created on
/// demand.
#[allow(clippy::too_many_arguments)]
fn measure_wrapping(
    g: &Group,
    slot: usize,
    offer: Constraints,
    theme: &Theme<'_>,
    cache: &mut UiCache,
    outer_main: i32,
    outer_cross: i32,
    pad_main: i32,
    pad_cross: i32,
) -> Size {
    use alloc::vec::Vec;
    use symbian_ui::flow::{stack_extent, Packer};

    let axis = g.axis;
    // The line's own room, with no gap subtracted up front: gaps here are *within* a line and the
    // packer charges them as it goes, which is the whole reason it counts them against its limit.
    let limit = (outer_main - pad_main).max(0);
    let avail_cross = (outer_cross - pad_cross).max(0);

    let mut packer = Packer::new(limit, g.gap.resolve(theme));
    // One entry per line, so the placement pass can find where each line begins across the axis.
    let mut line_crosses: Vec<i32> = Vec::new();
    let mut widest_line = 0;
    let mut current_line = 0usize;
    let mut current_cross = 0;

    let mut child_slot = slot + 1;
    for child in g.children() {
        // Offered the whole line, never what its predecessors left — the same cache decision the
        // straight-line pass makes, and it matters more here: an offer that shrank as a line filled
        // would make the last chip in a row a cache miss every time one before it changed.
        let size = measure_node(child, child_slot, axis.offer(0, limit, avail_cross), theme, cache);
        let placed = packer.place(axis.main(size));
        if placed.line != current_line {
            line_crosses.push(current_cross);
            current_line = placed.line;
            current_cross = 0;
        }
        current_cross = current_cross.max(axis.cross(size).clamp(0, avail_cross));
        widest_line = widest_line.max(packer.line_extent());
        child_slot += child.slot_count();
    }
    line_crosses.push(current_cross);

    let main = if axis.pick(g.width, g.height).is_wrap() {
        widest_line + pad_main
    } else {
        outer_main
    };
    let cross = if axis.pick(g.height, g.width).is_wrap() {
        stack_extent(&line_crosses, g.cross_gap.resolve(theme)) + pad_cross
    } else {
        outer_cross
    };
    offer.constrain(axis.size(main, cross))
}

/// [`layout_group`] for a group that wraps.
///
/// # Two packers, one rule
///
/// The line breaks are recomputed here rather than carried over from the measure pass, because the
/// cache stores sizes and rects and nothing in between — and a third kind of entry, holding line
/// numbers, would be a second thing to invalidate correctly.
///
/// Recomputing is safe for a reason the packer's own tests pin down: the same feed gives the same
/// lines. This pass feeds it the *cached* main sizes in the same order the measure pass fed it the
/// freshly measured ones, and those are the same numbers — the measure pass stored exactly what it
/// packed. `the_same_feed_gives_the_same_lines_twice` in `symbian_ui::flow` is the assertion that
/// keeps that true.
fn layout_wrapping(g: &Group, slot: usize, inner: Rect, cache: &mut UiCache, theme: &Theme<'_>) {
    use alloc::vec::Vec;
    use symbian_ui::flow::Packer;

    let axis = g.axis;
    let gap = g.gap.resolve(theme);
    let cross_gap = g.cross_gap.resolve(theme);
    let limit = axis.pick(inner.width(), inner.height()).max(0);
    let main0 = axis.pick(inner.x0, inner.y0);
    let cross0 = axis.pick(inner.y0, inner.x0);

    // Pass one: how tall each line is, from what the measure pass stored. Nothing is placed yet,
    // because a line's cross origin is the sum of the lines above it and the last child of line 0
    // is walked before the first child of line 1.
    let mut line_crosses: Vec<i32> = Vec::new();
    {
        let mut packer = Packer::new(limit, gap);
        let mut current_line = 0usize;
        let mut current_cross = 0;
        let mut child_slot = slot + 1;
        for child in g.children() {
            let size = cache.size(child_slot).unwrap_or(Size::ZERO);
            let placed = packer.place(axis.main(size));
            if placed.line != current_line {
                line_crosses.push(current_cross);
                current_line = placed.line;
                current_cross = 0;
            }
            current_cross = current_cross.max(axis.cross(size));
            child_slot += child.slot_count();
        }
        line_crosses.push(current_cross);
    }

    // Where each line begins across the axis, accumulated once so the placement below is a lookup.
    let mut line_origins: Vec<i32> = Vec::with_capacity(line_crosses.len());
    let mut at = cross0;
    for h in &line_crosses {
        line_origins.push(at);
        at += h + cross_gap;
    }

    let mut packer = Packer::new(limit, gap);
    let mut child_slot = slot + 1;
    for child in g.children() {
        let size = cache.size(child_slot).unwrap_or(Size::ZERO);
        let placed = packer.place(axis.main(size));
        let band = line_crosses[placed.line];
        let band0 = line_origins[placed.line];

        // Clamped against the line's own end, exactly as the straight-line pass clamps against the
        // group's: an over-wide first child on a line comes out flat inside its parent rather than
        // sitting off the edge with a positive width.
        let start = (main0 + placed.offset).min(main0 + limit);
        let main = axis.main(size).clamp(0, (main0 + limit - start).max(0));
        let cross = axis.cross(size).clamp(0, band);
        // Alignment is within the child's own *line*, not the whole group. A row of chips of two
        // heights centres each chip in its line; measured against the block, the short chips on the
        // last line would drift toward the middle of the whole thing.
        let (cross_at, cross) = match child.align_self().unwrap_or(g.align) {
            CrossAlign::Start => (band0, cross),
            CrossAlign::Center => (band0 + (band - cross) / 2, cross),
            CrossAlign::End => (band0 + band - cross, cross),
            CrossAlign::Stretch => (band0, band),
        };
        layout_node(child, child_slot, axis.rect(start, cross_at, main, cross), cache, theme);
        child_slot += child.slot_count();
    }
}

/// Turn the measured sizes into rectangles, starting from `area`.
///
/// Takes no constraints: it cannot measure, only place. See the module docs.
///
/// # It does take a theme, and that is not a measurement
///
/// This signature used to end at `cache`, and its comment said so with some pride. What changed is
/// [`Gap`](crate::Gap): a group's padding and gaps are now named by role — `Gap::Snug`, not `4` —
/// for the same reason a colour is, and turning a role into a pixel count needs the theme that
/// defines the scale.
///
/// The rule this pass obeys is unchanged, because the rule was never "no theme". It is that this
/// pass **cannot measure**: it may not ask a widget how big it is, consult a font, or look at a
/// string. Resolving `Gap::Snug` does none of those — it is a lookup with the same answer every
/// time, and the measure pass resolves it identically from the same theme, which is what keeps the
/// two passes agreeing about where a line starts.
pub fn layout_tree(root: &Node, area: Rect, cache: &mut UiCache, theme: &Theme<'_>) {
    layout_node(root, 0, area, cache, theme);
}

/// [`layout_tree`] for a subtree whose slots start at `slot`.
pub fn layout_node(node: &Node, slot: usize, area: Rect, cache: &mut UiCache, theme: &Theme<'_>) {
    match node {
        Node::Leaf(_) => cache.set_rect(slot, area),
        Node::Group(g) => layout_group(g, slot, area, cache, theme),
    }
}

/// Walk the line, laying each child down where the previous one ended.
pub fn layout_group(g: &Group, slot: usize, area: Rect, cache: &mut UiCache, theme: &Theme<'_>) {
    cache.set_rect(slot, area);

    let axis = g.axis;
    let gap = g.gap.resolve(theme);
    let inner = sane(area.inset_edges(g.padding.resolve(theme)));
    if g.wraps() {
        layout_wrapping(g, slot, inner, cache, theme);
        return;
    }
    let limit = axis.pick(inner.x1, inner.y1);
    let cross0 = axis.pick(inner.y0, inner.x0);
    let cross_room = axis.pick(inner.height(), inner.width()).max(0);
    // Where the line starts, and how much room opens between joins, once the children's own sizes
    // are known. This is the whole of `justify-content`: the main axis is not divided here — the
    // flex pass already did that — so what is left over is a gap to distribute, not space to take.
    let content_main: i32 = {
        let mut total = 0;
        let mut s = slot + 1;
        for child in g.children() {
            total += axis.main(cache.size(s).unwrap_or(Size::ZERO));
            s += child.slot_count();
        }
        total + gap * (g.children.len() as i32 - 1).max(0)
    };
    let room = axis.pick(inner.width(), inner.height()).max(0);
    let slack = (room - content_main).max(0);
    let joins = (g.children.len() as i32 - 1).max(0);
    // `SpaceBetween` with one child has no second edge to reach, so it packs at the start — the
    // same degenerate case CSS has, and the same answer.
    let (lead, extra_gap) = match g.justify {
        MainAlign::Start => (0, 0),
        MainAlign::End => (slack, 0),
        MainAlign::Center => (slack / 2, 0),
        MainAlign::SpaceBetween if joins > 0 => (0, slack / joins),
        MainAlign::SpaceBetween => (0, 0),
    };
    let mut cursor = axis.pick(inner.x0, inner.y0) + lead;

    let mut child_slot = slot + 1;
    for child in g.children() {
        let size = cache.size(child_slot).unwrap_or(Size::ZERO);
        // Clamped against what is actually left rather than trusted: a group whose padding is
        // wider than the group itself, or a screen too small for its own contents, must produce a
        // flat rect and not one that runs backwards. The cursor is pinned to the end of the line
        // as well, so a child pushed past it comes out empty *inside* its parent rather than
        // sitting somewhere off the edge with a positive width.
        let start = cursor.min(limit);
        let main = axis.main(size).clamp(0, (limit - start).max(0));
        // The cross axis is placed rather than divided: every child gets the whole band to sit in,
        // and the alignment decides where in it. `cross` is clamped first so the offsets below are
        // never negative — a child larger than the band is trimmed to it, not centred outside it.
        let cross = axis.cross(size).clamp(0, cross_room);
        // `align-self` beats `align-items`, exactly as in CSS: the child's answer wins where it has
        // one, and the line's applies to everyone else.
        let (cross_at, cross) = match child.align_self().unwrap_or(g.align) {
            CrossAlign::Start => (cross0, cross),
            CrossAlign::Center => (cross0 + (cross_room - cross) / 2, cross),
            CrossAlign::End => (cross0 + cross_room - cross, cross),
            CrossAlign::Stretch => (cross0, cross_room),
        };
        layout_node(child, child_slot, axis.rect(start, cross_at, main, cross), cache, theme);
        cursor = start + main + gap + extra_gap;
        child_slot += child.slot_count();
    }
}

/// A whole frame: begin it, measure, place, paint. The entry point a host should call.
///
/// # `begin_frame` lives here, and only here
///
/// The three passes below are also usable one at a time — a test asserts on sizes without a canvas,
/// a screen lays out a subtree of its own — so none of them may start a frame: `measure_tree`
/// calling `begin_frame` would retire the rects of a tree half-way through laying it out. That
/// leaves exactly one honest owner, the function that runs all three, and it is this one.
///
/// The alternative, asking the caller to remember, is a rule that is invisible when broken. A host
/// that never calls `begin_frame` still draws: the generation never moves, so every rect written
/// last frame keeps answering as though it were written this frame. The screen looks right, and
/// stays right, until a branch is removed from the tree and goes on painting where it used to be.
pub fn draw_frame(
    root: &Node,
    rect: Rect,
    cache: &mut UiCache,
    c: &mut Canvas<'_>,
    theme: &Theme<'_>,
) {
    place_frame(root, rect, cache, theme);
    draw_tree(root, cache, c, theme);
}

/// A frame without the paint: begin it, measure, place.
///
/// Every rect [`dispatch_key`] reads comes from here, and there is one caller that needs them
/// *without* a canvas — the bridge, when a key arrives against a tree that has not been drawn yet.
/// That happens more often than it sounds: an `update` drops the tree, and the next key in the same
/// batch of platform events arrives before any frame has been drawn. Without this, that key would
/// find no rects and be answered by nobody, so holding a direction key would move a list once per
/// frame instead of once per press.
///
/// Split out of [`draw_frame`] rather than reimplemented, so the two cannot disagree about the order
/// of the passes or about who starts the frame.
pub fn place_frame(root: &Node, rect: Rect, cache: &mut UiCache, theme: &Theme<'_>) {
    cache.begin_frame();
    measure_tree(root, Constraints::tight(rect.width(), rect.height()), theme, cache);
    layout_tree(root, rect, cache, theme);
}

/// Paint the tree into the rects [`layout_tree`] worked out.
///
/// Takes the cache by `&`, so it can only read rects: a tree painted before it was laid out finds
/// no rect for its slots and paints nothing rather than something stale. Prefer [`draw_frame`],
/// which cannot be called in the wrong order.
pub fn draw_tree(root: &Node, cache: &UiCache, c: &mut Canvas<'_>, theme: &Theme<'_>) {
    draw_node(root, 0, cache, c, theme);
}

/// [`draw_tree`] for a subtree whose slots start at `slot`.
pub fn draw_node(
    node: &Node,
    slot: usize,
    cache: &UiCache,
    c: &mut Canvas<'_>,
    theme: &Theme<'_>,
) {
    // No rect for this frame means the node was not placed this frame — a branch that has gone
    // away, or a tree drawn before it was laid out. Either way there is nowhere to put it.
    let Some(rect) = cache.rect(slot) else { return };
    if rect.is_empty() {
        return;
    }
    match node {
        Node::Leaf(w) => {
            // Clipped, but not translated: rects come out of the cache in the coordinate space the
            // layout ran in, and moving the origin under them would offset every one of them
            // twice. The clip is what stops a widget that draws a pixel too wide from eating its
            // neighbour — unless the widget declares that its ink is meant to leave its box. See
            // `Widget::overflow_visible`; the ancestors' clips still apply either way.
            let saved = c.save();
            if !w.overflow_visible() {
                c.clip_to(rect);
            }
            w.draw(c, rect, theme);
            c.restore(saved);
        }
        Node::Group(g) => draw_group(g, slot, cache, c, theme),
    }
}

/// Paint a group's background and then everything inside it.
pub fn draw_group(
    g: &Group,
    slot: usize,
    cache: &UiCache,
    c: &mut Canvas<'_>,
    theme: &Theme<'_>,
) {
    let Some(rect) = cache.rect(slot) else { return };
    if rect.is_empty() {
        return;
    }
    let saved = c.save();
    if !g.overflow_visible {
        c.clip_to(rect);
    }
    if let Some(bg) = g.background {
        c.fill_rect(rect, bg);
    }
    // What the children will be standing on. A literal `background` cannot say — a `Color` carries no
    // role — so it leaves the ground alone, which is one more reason `surface` exists beside it.
    let mut ground = theme.ground;
    if let Some(role) = g.surface {
        symbian_ui::paint::band(c, rect, &role.resolve_on(theme, g.has_selection_band()));
        ground = role.ground();
    }
    // Before the children and after any flat background, which is the order `ScrollList` uses: the
    // band goes down first and everything the row draws lands on top of it. Through
    // `chrome::selection` rather than a fill, so a row in a form gets the same band a row in a list
    // gets rather than one that nearly matches.
    if g.has_selection_band() {
        symbian_ui::chrome::selection(c, rect, theme);
        ground = symbian_ui::Ground::Band;
    }
    // Handed down rather than mutated in place, so a sibling that paints no band is unaffected — the
    // ground belongs to a subtree, not to the walk.
    let inner = theme.on(ground);
    let mut child_slot = slot + 1;
    for child in g.children() {
        draw_node(child, child_slot, cache, c, &inner);
        child_slot += child.slot_count();
    }
    // After the children, so a row whose content reaches the bottom edge does not paint over its
    // own separator — the same order the hand-written row draws in, and the reason a border is a
    // property here rather than a child.
    if let Some((ink, inset)) = g.border_bottom {
        let inset = inset.resolve(theme);
        c.hline(rect.y1 - 1, rect.x0 + inset, rect.x1, ink.resolve(theme));
    }
    c.restore(saved);
}

/// Offer a key to the tree, innermost-first, stopping at whoever takes it.
///
/// The fourth pass, and the one that was missing: measure, place and draw all walk the tree, and a
/// key had nowhere to go — [`Widget::handle_key`] was implemented by four widgets and called by
/// nobody. This is that walk, and it is deliberately a transcription of [`draw_node`] rather than a
/// second idea about the tree: same pre-order, same slot arithmetic, same "no rect means not on
/// screen, so not yours".
///
/// # It reuses the last frame's rects, and that is sound
///
/// A key arrives between frames, when no layout has run. [`UiCache::rect`] answers for the current
/// generation, and only [`draw_frame`] advances the generation — so between two draws every rect
/// the last layout wrote is still there to be read. A tree that has never been drawn has no rects
/// at all and takes no keys, which is correct: a widget the user cannot see must not be the one
/// answering.
///
/// # Order
///
/// First `Consumed` wins, and children come before their parent's siblings, so the innermost widget
/// under the key gets it first. This walk is *step three* of the resolution order in
/// [`crate::widgets::OnKey`] — the softkey bar and the app's hatches have already had their say by
/// the time the bridge calls this. Running it earlier would let a text field swallow a key the bar
/// had promised on its label, which is the defect [`crate::keys`] exists to make impossible.
pub fn dispatch_key(
    root: &Node,
    ev: KeyEvent,
    cache: &UiCache,
    cx: &mut KeyCtx<'_>,
) -> Handled {
    dispatch_key_node(root, 0, ev, cache, cx)
}

/// [`dispatch_key`] for a subtree whose slots start at `slot`.
pub fn dispatch_key_node(
    node: &Node,
    slot: usize,
    ev: KeyEvent,
    cache: &UiCache,
    cx: &mut KeyCtx<'_>,
) -> Handled {
    let Some(rect) = cache.rect(slot) else { return Handled::Ignored };
    if rect.is_empty() {
        return Handled::Ignored;
    }
    match node {
        Node::Leaf(w) => w.handle_key(ev, rect, cx),
        Node::Group(g) => dispatch_key_group(g, slot, ev, cache, cx),
    }
}

/// Offer a key to everything inside a group, in the order it was declared.
pub fn dispatch_key_group(
    g: &Group,
    slot: usize,
    ev: KeyEvent,
    cache: &UiCache,
    cx: &mut KeyCtx<'_>,
) -> Handled {
    let Some(rect) = cache.rect(slot) else { return Handled::Ignored };
    if rect.is_empty() {
        return Handled::Ignored;
    }
    let mut child_slot = slot + 1;
    for child in g.children() {
        if dispatch_key_node(child, child_slot, ev, cache, cx) == Handled::Consumed {
            // Stop at the first taker. Carrying on would let two fields on one screen both act on
            // one press — which is exactly what the focus flag on each of them is there to prevent,
            // and this is the second lock on that door.
            return Handled::Consumed;
        }
        child_slot += child.slot_count();
    }
    // A focus scope's own cursor moves only after everything inside it has declined — the ordering
    // `OnKey` already uses, and load-bearing for the same reason: a control must not have to know
    // what encloses it. It is also what makes nesting work. Outer-first, a vertical `RadioGroup`
    // inside a vertical form would never move between its own options, because the form would take
    // every `Down` before the group was asked.
    //
    // A scope that declines here — `EdgePolicy::Escape` at its last stop — leaves the key to bubble
    // to the scope enclosing *it*, which is the whole mechanism a row of buttons inside a form is
    // built on.
    if let Some(hook) = g.focus() {
        if let (Handled::Consumed, _) = hook.handle_key(ev) {
            return Handled::Consumed;
        }
    }
    Handled::Ignored
}

/// A whole frame for a root that is a plain [`Widget`] rather than a [`Node`]: begin, measure,
/// place, draw.
///
/// The bridge's entry point, and a compatibility shim rather than the fast path. A widget handed
/// over as `&dyn Widget` is opaque — the engine cannot see inside it, so it is measured and drawn
/// as one node and any containers within it lay themselves out, uncached, every frame. A root built
/// as a [`Node`] should go through [`measure_tree`]/[`layout_tree`]/[`draw_tree`], which is what
/// makes an idle frame cost nothing.
pub fn draw_widget_tree(
    root: &dyn Widget,
    rect: Rect,
    cache: &mut UiCache,
    c: &mut Canvas<'_>,
    theme: &Theme<'_>,
) {
    cache.begin_frame();
    cache.measure_or_compute(0, root, Constraints::tight(rect.width(), rect.height()), theme);
    cache.set_rect(0, rect);
    let saved = c.save();
    c.clip_to(rect);
    root.draw(c, rect, theme);
    c.restore(saved);
}

/// The extent a declared [`Length`] starts from, given what the parent offered.
///
/// `Fill` and `WrapContent` both begin with everything available — the first because it is claiming
/// a share of it, the second because that is the ceiling it will shrink back from once its children
/// have been measured.
fn resolve(len: Length, available: i32) -> i32 {
    match len {
        Length::Exact(px) => px.clamp(0, available.max(0)),
        _ => available.max(0),
    }
}

fn max_size(c: Constraints) -> Size {
    Size::new(c.max_w, c.max_h)
}

/// A rect that cannot run backwards. `inset_edges` with padding larger than the box produces one,
/// and an inverted rect is invisible rather than wrong-looking, which is how it survives review.
fn sane(r: Rect) -> Rect {
    Rect::new(r.x0, r.y0, r.x1.max(r.x0), r.y1.max(r.y0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::{hash_i32, Widget, WidgetHash};
    use crate::widgets::{Column, Row};
    use alloc::rc::Rc;
    use core::cell::Cell;
    use symbian_ui::{testing, Palette};

    /// A leaf that counts how often it was actually measured.
    ///
    /// The counter is the deliverable of half the tests in this file: "the cache works" is not
    /// something you can see in a size, only in a call that did not happen.
    struct Probe {
        size: Size,
        weight: i32,
        tag: i32,
        calls: Rc<Cell<usize>>,
    }

    fn counter() -> Rc<Cell<usize>> {
        Rc::new(Cell::new(0))
    }

    impl Probe {
        /// A leaf of an exact size, taking no share of the line.
        fn fixed(w: i32, h: i32) -> Self {
            Self { size: Size::new(w, h), weight: 0, tag: 0, calls: counter() }
        }

        /// A leaf that claims a share of the line and asks for nothing of its own, so whatever it
        /// reports back is exactly what the division gave it.
        fn fill(weight: i32) -> Self {
            Self { size: Size::ZERO, weight, tag: 0, calls: counter() }
        }

        /// Report every measure into `c`. Shared so the widget can be rebuilt each frame — as a
        /// real screen does — while the count survives across frames.
        fn counted(mut self, c: &Rc<Cell<usize>>) -> Self {
            self.calls = c.clone();
            self
        }

        /// Change the digest without changing the size, the way re-worded text does.
        fn tag(mut self, t: i32) -> Self {
            self.tag = t;
            self
        }
    }

    impl Widget for Probe {
        fn content_hash(&self) -> WidgetHash {
            let h = hash_i32(hash_i32(0, self.size.w), self.size.h);
            hash_i32(hash_i32(h, self.weight), self.tag)
        }
        fn measure(&self, c: Constraints, _t: &Theme<'_>) -> Size {
            self.calls.set(self.calls.get() + 1);
            c.constrain(self.size)
        }
        fn draw(&self, _c: &mut Canvas<'_>, _r: Rect, _t: &Theme<'_>) {}
        fn flex_weight(&self) -> i32 {
            self.weight
        }
    }

    /// One frame without a canvas: exactly what [`draw_frame`] does, minus the paint.
    fn frame(root: &Node, area: Rect, theme: &Theme<'_>, cache: &mut UiCache) {
        cache.begin_frame();
        measure_tree(root, Constraints::tight(area.width(), area.height()), theme, cache);
        layout_tree(root, area, cache, theme);
    }

    fn run(area: Rect, build: impl Fn() -> Node, frames: usize) -> UiCache {
        testing::with_theme(Palette::DARK, |t| {
            let mut cache = UiCache::new();
            for _ in 0..frames {
                frame(&build(), area, t, &mut cache);
            }
            cache
        })
    }

    fn widths(cache: &UiCache, slots: &[usize]) -> alloc::vec::Vec<i32> {
        slots.iter().map(|&s| cache.rect(s).unwrap().width()).collect()
    }

    fn heights(cache: &UiCache, slots: &[usize]) -> alloc::vec::Vec<i32> {
        slots.iter().map(|&s| cache.rect(s).unwrap().height()).collect()
    }

    const AREA: Rect = Rect { x0: 0, y0: 0, x1: 100, y1: 20 };

    #[test]
    fn two_equal_fills_split_the_line_in_half() {
        let cache = run(
            AREA,
            || Node::Group(Row::new().child(Probe::fill(1)).child(Probe::fill(1))),
            1,
        );
        assert_eq!(widths(&cache, &[1, 2]), [50, 50]);
        assert_eq!(cache.rect(1).unwrap().x0, 0);
        assert_eq!(cache.rect(2).unwrap().x0, 50, "the second child starts where the first ended");
    }

    #[test]
    fn weights_divide_in_proportion() {
        let build = || Node::Group(Row::new().child(Probe::fill(2)).child(Probe::fill(1)));
        let cache = run(Rect::from_xywh(0, 0, 90, 20), build, 1);
        assert_eq!(widths(&cache, &[1, 2]), [60, 30]);
    }

    #[test]
    fn a_line_that_does_not_divide_evenly_still_adds_up() {
        // Three equal children in 100 pixels: 33, 33, 34 — not 33, 33, 33 with a pixel of
        // background showing through at the end, and not 34, 33, 33 either. The running total puts
        // every boundary at the nearest pixel to the exact division, which leaves the remainder
        // with the last child, at the edge nothing is aligned against.
        let cache = run(
            AREA,
            || {
                Node::Group(
                    Row::new()
                        .child(Probe::fill(1))
                        .child(Probe::fill(1))
                        .child(Probe::fill(1)),
                )
            },
            1,
        );
        let w = widths(&cache, &[1, 2, 3]);
        assert_eq!(w, [33, 33, 34]);
        assert_eq!(w.iter().sum::<i32>(), 100, "the row must cover the whole line");
        // And the children must tile it: no overlap, no gutter.
        assert_eq!(cache.rect(1).unwrap().x1, cache.rect(2).unwrap().x0);
        assert_eq!(cache.rect(2).unwrap().x1, cache.rect(3).unwrap().x0);
        assert_eq!(cache.rect(3).unwrap().x1, 100);
    }

    #[test]
    fn two_thirds_and_one_third_of_an_indivisible_line() {
        let cache = run(
            AREA,
            || Node::Group(Row::new().child(Probe::fill(2)).child(Probe::fill(1))),
            1,
        );
        assert_eq!(widths(&cache, &[1, 2]), [66, 34]);
        assert_eq!(66 + 34, 100);
    }

    #[test]
    fn gaps_come_out_before_the_division_not_after_it() {
        // 100 wide, one 10px gap: 90 to divide, not 100. Divide first and each child is 50, the
        // row is 110 wide, and the last one hangs off the edge of a screen that cannot scroll.
        let cache = run(
            AREA,
            || Node::Group(Row::new().gap(10).child(Probe::fill(1)).child(Probe::fill(1))),
            1,
        );
        assert_eq!(widths(&cache, &[1, 2]), [45, 45]);
        assert_eq!(cache.rect(2).unwrap().x0, 55);
        assert_eq!(cache.rect(2).unwrap().x1, 100);
    }

    #[test]
    fn gaps_are_counted_once_per_join_not_once_per_child() {
        // Three children, two gaps. Counting one per child would take ten pixels from a row that
        // has no eleventh edge to spend them on.
        let cache = run(
            AREA,
            || {
                Node::Group(
                    Row::new()
                        .gap(10)
                        .child(Probe::fill(1))
                        .child(Probe::fill(1))
                        .child(Probe::fill(1)),
                )
            },
            1,
        );
        let w = widths(&cache, &[1, 2, 3]);
        assert_eq!(w.iter().sum::<i32>(), 80);
        assert_eq!(cache.rect(3).unwrap().x1, 100, "the last child still ends at the far edge");
    }

    #[test]
    fn padding_is_spent_before_the_division_too() {
        let cache = run(
            AREA,
            || Node::Group(Row::new().pad(5).child(Probe::fill(1)).child(Probe::fill(1))),
            1,
        );
        assert_eq!(widths(&cache, &[1, 2]), [45, 45]);
        assert_eq!(cache.rect(1).unwrap().x0, 5);
        assert_eq!(cache.rect(2).unwrap().x1, 95);
        assert_eq!(cache.rect(1).unwrap().y0, 5, "padding applies across the line as well");
    }

    #[test]
    fn fixed_children_are_paid_before_the_share_is_worked_out() {
        let cache = run(
            AREA,
            || Node::Group(Row::new().child(Probe::fixed(30, 8)).child(Probe::fill(1))),
            1,
        );
        assert_eq!(widths(&cache, &[1, 2]), [30, 70]);
    }

    #[test]
    fn a_second_frame_with_the_same_content_measures_nothing() {
        // The whole reason this layer exists. The tree is rebuilt from scratch each frame, exactly
        // as a real screen does, and the second build must not cost a single call to `measure`.
        testing::with_theme(Palette::DARK, |t| {
            let (a, b, c) = (counter(), counter(), counter());
            let build = || {
                Node::Group(
                    Row::new()
                        .gap(2)
                        .child(Probe::fixed(30, 8).counted(&a))
                        .child(Probe::fill(1).counted(&b))
                        .group(Column::new().child(Probe::fixed(10, 4).counted(&c))),
                )
            };
            let mut cache = UiCache::new();

            frame(&build(), AREA, t, &mut cache);
            assert_eq!(
                (a.get(), b.get(), c.get()),
                (1, 1, 1),
                "every leaf is measured exactly once on the first frame"
            );
            assert_eq!(cache.measure_calls(), 3);
            let first = (cache.rect(1), cache.rect(2), cache.rect(3), cache.rect(4));

            frame(&build(), AREA, t, &mut cache);
            assert_eq!(cache.measure_calls(), 0, "an idle frame must not measure anything");
            assert_eq!((a.get(), b.get(), c.get()), (1, 1, 1));
            assert_eq!(
                (cache.rect(1), cache.rect(2), cache.rect(3), cache.rect(4)),
                first,
                "and it must still produce the same screen"
            );

            // Ten more for good measure: nothing accumulates, nothing drifts.
            for _ in 0..10 {
                frame(&build(), AREA, t, &mut cache);
                assert_eq!(cache.measure_calls(), 0);
            }
        });
    }

    #[test]
    fn a_changed_digest_is_measured_again_and_its_siblings_are_not() {
        testing::with_theme(Palette::DARK, |t| {
            let (a, b) = (counter(), counter());
            let build = |a_w: i32| {
                Node::Group(
                    Row::new()
                        .child(Probe::fixed(a_w, 8).counted(&a))
                        .child(Probe::fixed(10, 4).counted(&b)),
                )
            };
            let mut cache = UiCache::new();
            frame(&build(30), AREA, t, &mut cache);
            assert_eq!((a.get(), b.get()), (1, 1));

            frame(&build(40), AREA, t, &mut cache);
            // The changed child is re-measured; so is its parent, whose digest folds it in. The
            // sibling is not, which is the half of the behaviour that is easy to get wrong — and
            // the reason a fixed child is offered the whole line rather than what its predecessors
            // left, which would have made every sibling after this one a miss.
            assert_eq!(a.get(), 2, "the changed child must be measured again");
            assert_eq!(b.get(), 1, "the unchanged sibling must not be");
            assert_eq!(cache.rect(1).unwrap().width(), 40);
        });
    }

    #[test]
    fn a_volatile_child_is_re_measured_forever() {
        // A widget that keeps the default digest is asking to be measured every frame. An ancestor
        // that cached over it would answer for it, and the screen would stop updating.
        struct Volatile(Rc<Cell<usize>>);
        impl Widget for Volatile {
            fn measure(&self, c: Constraints, _t: &Theme<'_>) -> Size {
                self.0.set(self.0.get() + 1);
                c.constrain(Size::new(5, 5))
            }
            fn draw(&self, _c: &mut Canvas<'_>, _r: Rect, _t: &Theme<'_>) {}
        }
        testing::with_theme(Palette::DARK, |t| {
            let calls = counter();
            let build = || Node::Group(Row::new().child(Volatile(calls.clone())));
            let mut cache = UiCache::new();
            for _ in 0..3 {
                frame(&build(), AREA, t, &mut cache);
            }
            assert_eq!(calls.get(), 3);
        });
    }

    #[test]
    fn too_little_room_never_produces_a_negative_size() {
        // Three children wanting 50 pixels each in a 100-pixel row. The first two fit, the third
        // must come out flat — not negative, which would be an inverted rect: invisible on screen
        // and silent in every assertion that only checks a width.
        let cache = run(
            AREA,
            || {
                Node::Group(
                    Row::new()
                        .child(Probe::fixed(50, 8))
                        .child(Probe::fixed(50, 8))
                        .child(Probe::fixed(50, 8)),
                )
            },
            1,
        );
        let w = widths(&cache, &[1, 2, 3]);
        assert_eq!(w, [50, 50, 0]);
        for slot in 0..4 {
            let r = cache.rect(slot).unwrap();
            assert!(r.x1 >= r.x0 && r.y1 >= r.y0, "slot {slot} came out inverted: {r:?}");
            assert!(r.x1 <= 100, "slot {slot} escaped the row");
        }
    }

    #[test]
    fn padding_wider_than_the_box_leaves_nothing_rather_than_less_than_nothing() {
        let cache = run(
            Rect::from_xywh(0, 0, 10, 10),
            || Node::Group(Row::new().pad(20).child(Probe::fill(1))),
            1,
        );
        let child = cache.rect(1).unwrap();
        assert!(child.is_empty());
        assert!(child.x1 >= child.x0 && child.y1 >= child.y0);
    }

    #[test]
    fn gaps_wider_than_the_line_do_not_go_backwards() {
        let cache = run(
            Rect::from_xywh(0, 0, 10, 10),
            || Node::Group(Row::new().gap(50).child(Probe::fixed(6, 4)).child(Probe::fixed(6, 4))),
            1,
        );
        for slot in [1, 2] {
            let r = cache.rect(slot).unwrap();
            assert!(r.x1 >= r.x0 && r.x1 <= 10, "slot {slot}: {r:?}");
        }
    }

    #[test]
    fn a_row_with_nothing_in_it_is_nothing() {
        let cache = run(AREA, || Node::Group(Row::new()), 1);
        assert_eq!(cache.rect(0), Some(AREA));
        assert_eq!(cache.size(0), Some(Size::new(100, 20)));
    }

    #[test]
    fn nesting_divides_what_the_parent_handed_down() {
        // A column of two rows, the second twice the weight of the first — the shape of every
        // screen this SDK draws.
        let cache = run(
            Rect::from_xywh(0, 0, 100, 90),
            || {
                Node::Group(
                    Column::new()
                        .group(Row::new().fill(1).child(Probe::fill(1)))
                        .group(
                            Row::new()
                                .fill(2)
                                .stretch_height()
                                .child(Probe::fill(1))
                                .child(Probe::fill(1)),
                        ),
                )
            },
            1,
        );
        assert_eq!(heights(&cache, &[1, 3]), [30, 60]);
        assert_eq!(cache.rect(3).unwrap().y0, 30);
        // The grandchildren split their own row, not the screen.
        assert_eq!(widths(&cache, &[4, 5]), [50, 50]);
        // And they are as tall as they asked to be — nothing stretches them across the row. These
        // probes asked for nothing, so nothing is what they get.
        assert_eq!(heights(&cache, &[4, 5]), [0, 0]);
    }

    #[test]
    fn a_child_takes_only_the_cross_extent_it_asked_for() {
        // Cross-axis sizing is the child's answer, not a stretch: a 4px label in a 20px row is 4px
        // tall and sits at the top. A widget that wants the whole row returns the whole row from
        // `measure` — the offer tells it how much there is.
        let cache = run(AREA, || Node::Group(Row::new().child(Probe::fixed(10, 4))), 1);
        assert_eq!(cache.rect(1).unwrap().height(), 4);
        assert_eq!(cache.rect(1).unwrap().y0, 0);
    }

    #[test]
    fn a_wrapping_group_is_as_big_as_what_it_holds() {
        testing::with_theme(Palette::DARK, |t| {
            let mut cache = UiCache::new();
            let root = Node::Group(
                Row::new().gap(3).pad(2).child(Probe::fixed(10, 4)).child(Probe::fixed(20, 6)),
            );
            let size = measure_tree(&root, Constraints::loose(100, 50), t, &mut cache);
            // 10 + 3 + 20 + 2*2 across, tallest child + 2*2 down.
            assert_eq!(size, Size::new(37, 10));
        });
    }

    #[test]
    fn an_exact_size_wins_over_what_the_children_want() {
        testing::with_theme(Palette::DARK, |t| {
            let mut cache = UiCache::new();
            let root = Node::Group(Row::new().width(40).height(12).child(Probe::fixed(10, 4)));
            assert_eq!(
                measure_tree(&root, Constraints::loose(100, 50), t, &mut cache),
                Size::new(40, 12)
            );
        });
    }

    #[test]
    fn an_exact_size_larger_than_the_offer_is_still_the_offer() {
        testing::with_theme(Palette::DARK, |t| {
            let mut cache = UiCache::new();
            let root = Node::Group(Row::new().width(400).child(Probe::fill(1)));
            let size = measure_tree(&root, Constraints::loose(100, 20), t, &mut cache);
            assert_eq!(size.w, 100, "nothing may claim more of the screen than there is");
            // And the child inside it must not be handed the imaginary 400 either.
            layout_tree(&root, Rect::from_xywh(0, 0, 100, 20), &mut cache, t);
            assert_eq!(cache.rect(1).unwrap().width(), 100);
        });
    }

    #[test]
    fn a_group_measured_through_the_widget_trait_agrees_with_the_engine() {
        // The degraded path — a group used as a plain `&dyn Widget` — runs the same arithmetic on
        // a scratch cache. If the two ever disagreed, a container nested by the wrong builder
        // method would lay out differently from one nested by the right one, which is the kind of
        // difference nobody would look for.
        testing::with_theme(Palette::DARK, |t| {
            let offer = Constraints::tight(100, 20);
            let direct =
                Row::new().gap(4).pad(2).child(Probe::fixed(30, 8)).child(Probe::fill(1));
            let through_trait = direct.measure(offer, t);

            let mut cache = UiCache::new();
            let root = Node::Group(
                Row::new().gap(4).pad(2).child(Probe::fixed(30, 8)).child(Probe::fill(1)),
            );
            assert_eq!(measure_tree(&root, offer, t, &mut cache), through_trait);
        });
    }

    #[test]
    fn drawing_paints_where_the_layout_said_and_nowhere_else() {
        testing::with_theme(Palette::DARK, |t| {
            let red = symbian_gfx::Color::hex(0xFF0000);
            let root = Node::Group(
                Row::new()
                    .child(Probe::fill(1))
                    .group(Row::new().fill(1).stretch_height().background(red)),
            );
            let mut cache = UiCache::new();
            let ((), px) = testing::with_canvas(Size::new(100, 20), |c| {
                frame(&root, Rect::from_xywh(0, 0, 100, 20), t, &mut cache);
                draw_tree(&root, &cache, c, t);
            });
            let at = |x: i32, y: i32| px[(y * 100 + x) as usize];
            assert_eq!(at(50, 10), red.to_rgb565().0, "the second half is the group's background");
            assert_eq!(at(49, 10), 0, "the first half belongs to the other child");
            assert_eq!(at(99, 19), red.to_rgb565().0);
        });
    }

    #[test]
    fn a_node_that_left_the_tree_does_not_draw_where_it_used_to_be() {
        testing::with_theme(Palette::DARK, |t| {
            let red = symbian_gfx::Color::hex(0xFF0000);
            let mut cache = UiCache::new();
            let big = Node::Group(
                Row::new()
                    .group(Row::new().fill(1).stretch_height().background(red))
                    .group(Row::new().fill(1).stretch_height().background(red)),
            );
            frame(&big, Rect::from_xywh(0, 0, 100, 20), t, &mut cache);

            let small =
                Node::Group(Row::new().group(Row::new().fill(1).stretch_height().background(red)));
            let ((), px) = testing::with_canvas(Size::new(100, 20), |c| {
                frame(&small, Rect::from_xywh(0, 0, 100, 20), t, &mut cache);
                draw_tree(&small, &cache, c, t);
            });
            // Slot 2 held the second half last frame and holds nothing now. Its rect must not be
            // an answer any more, or the screen keeps a ghost of a branch that was removed.
            assert_eq!(cache.rect(2), None);
            assert_eq!(px[(10 * 100 + 99) as usize], red.to_rgb565().0, "the surviving child grew");
        });
    }

    #[test]
    fn the_engine_never_places_a_child_outside_its_parent() {
        // A blanket invariant over an awkward tree, because the individual assertions above each
        // only cover the case they were written for.
        let cache = run(
            Rect::from_xywh(3, 7, 40, 30),
            || {
                Node::Group(
                    Column::new()
                        .gap(4)
                        .pad(2)
                        .child(Probe::fixed(100, 100))
                        .group(
                            Row::new()
                                .fill(1)
                                .gap(3)
                                .child(Probe::fixed(30, 5))
                                .child(Probe::fill(1)),
                        )
                        .child(Probe::fixed(5, 5)),
                )
            },
            1,
        );
        let root = cache.rect(0).unwrap();
        for slot in 0..6 {
            let r = cache.rect(slot).unwrap();
            assert!(r.x1 >= r.x0 && r.y1 >= r.y0, "slot {slot} inverted: {r:?}");
            assert_eq!(r.intersect(root), r, "slot {slot} escaped the root: {r:?}");
        }
    }

    #[test]
    fn a_resized_screen_is_measured_again() {
        testing::with_theme(Palette::DARK, |t| {
            let calls = counter();
            let build = || Node::Group(Row::new().child(Probe::fill(1).counted(&calls)));
            let mut cache = UiCache::new();
            frame(&build(), Rect::from_xywh(0, 0, 100, 20), t, &mut cache);
            frame(&build(), Rect::from_xywh(0, 0, 60, 20), t, &mut cache);
            // Nothing about the widget changed, but the question did. A digest-only cache would
            // leave a 100px child on a 60px screen.
            assert_eq!(calls.get(), 2);
            assert_eq!(cache.rect(1).unwrap().width(), 60);
        });
    }

    #[test]
    fn a_whole_frame_through_one_call_retires_the_last_one() {
        // `draw_frame` is the only function that starts a frame, and this is why that matters. A
        // host that drove the three passes by hand and forgot `begin_frame` would keep drawing a
        // branch that is no longer in the tree, because its rect would still look current.
        testing::with_theme(Palette::DARK, |t| {
            let red = symbian_gfx::Color::hex(0xFF0000);
            let area = Rect::from_xywh(0, 0, 100, 20);
            let mut cache = UiCache::new();
            let two = Node::Group(
                Row::new()
                    .group(Row::new().fill(1).stretch_height().background(red))
                    .group(Row::new().fill(1).stretch_height().background(red)),
            );
            let one =
                Node::Group(Row::new().group(Row::new().fill(1).stretch_height().background(red)));

            let ((), _) = testing::with_canvas(Size::new(100, 20), |c| {
                draw_frame(&two, area, &mut cache, c, t);
            });
            assert!(cache.rect(2).is_some(), "two branches on the first frame");

            let ((), px) = testing::with_canvas(Size::new(100, 20), |c| {
                draw_frame(&one, area, &mut cache, c, t);
            });
            assert_eq!(cache.rect(2), None, "the branch that left took its rect with it");
            // And the survivor grew into the whole row rather than half of it.
            assert_eq!(px[(10 * 100 + 99) as usize], red.to_rgb565().0);
        });
    }

    #[test]
    fn space_between_pins_the_ends_and_shares_the_middle() {
        // The chat-row case: name against the top of its column, preview against the bottom. Two
        // children, so all the slack goes to the single join between them.
        testing::with_theme(Palette::DARK, |t| {
            let root = Node::Group(
                Column::new()
                    .justify(MainAlign::SpaceBetween)
                    .child(Probe::fixed(20, 10))
                    .child(Probe::fixed(20, 10))
                    .height(50),
            );
            let mut cache = UiCache::new();
            frame(&root, Rect::from_xywh(0, 0, 40, 50), t, &mut cache);
            // First at the top, second at the bottom — not 10 and 20, which is what packing gives.
            assert_eq!(cache.rect(1).unwrap().y0, 0);
            assert_eq!(cache.rect(2).unwrap().y1, 50);
        });
    }

    #[test]
    fn space_between_with_one_child_packs_at_the_start() {
        // There is no second edge to reach, so it degenerates — the same answer CSS gives, and the
        // one that keeps a single-line row from being pushed to the bottom of its band.
        testing::with_theme(Palette::DARK, |t| {
            let root = Node::Group(
                Column::new()
                    .justify(MainAlign::SpaceBetween)
                    .child(Probe::fixed(20, 10))
                    .height(50),
            );
            let mut cache = UiCache::new();
            frame(&root, Rect::from_xywh(0, 0, 40, 50), t, &mut cache);
            assert_eq!(cache.rect(1).unwrap().y0, 0);
        });
    }

    #[test]
    fn end_and_centre_move_the_whole_line_not_the_children() {
        testing::with_theme(Palette::DARK, |t| {
            let build = |j: MainAlign| {
                Node::Group(
                    Column::new()
                        .justify(j)
                        .gap(4)
                        .child(Probe::fixed(20, 10))
                        .child(Probe::fixed(20, 10))
                        .height(50),
                )
            };
            // Content is 10 + 4 + 10 = 24 in a 50-tall box, so 26 of slack.
            let mut end = UiCache::new();
            frame(&build(MainAlign::End), Rect::from_xywh(0, 0, 40, 50), t, &mut end);
            assert_eq!(end.rect(1).unwrap().y0, 26, "the line is pushed down whole");
            assert_eq!(end.rect(2).unwrap().y1, 50);

            let mut mid = UiCache::new();
            frame(&build(MainAlign::Center), Rect::from_xywh(0, 0, 40, 50), t, &mut mid);
            assert_eq!(mid.rect(1).unwrap().y0, 13, "half above, half below");
        });
    }

    #[test]
    fn a_full_line_has_no_slack_to_distribute() {
        // Justification must be a no-op when the children already fill the line, or a row that fits
        // exactly would be spaced differently from one that fits with a pixel to spare.
        testing::with_theme(Palette::DARK, |t| {
            for j in [MainAlign::Start, MainAlign::End, MainAlign::Center, MainAlign::SpaceBetween] {
                let root = Node::Group(
                    Column::new()
                        .justify(j)
                        .child(Probe::fixed(20, 25))
                        .child(Probe::fixed(20, 25))
                        .height(50),
                );
                let mut cache = UiCache::new();
            frame(&root, Rect::from_xywh(0, 0, 40, 50), t, &mut cache);
                assert_eq!(cache.rect(1).unwrap().y0, 0, "{j:?}");
                assert_eq!(cache.rect(2).unwrap().y1, 50, "{j:?}");
            }
        });
    }

    #[test]
    fn justification_never_pushes_a_child_off_the_end() {
        // Overfull is the case that matters: content larger than the box has negative slack, and a
        // lead computed from it would start the line above the top of its own parent.
        testing::with_theme(Palette::DARK, |t| {
            for j in [MainAlign::End, MainAlign::Center, MainAlign::SpaceBetween] {
                let root = Node::Group(
                    Column::new()
                        .justify(j)
                        .child(Probe::fixed(20, 40))
                        .child(Probe::fixed(20, 40))
                        .height(50),
                );
                let mut cache = UiCache::new();
            frame(&root, Rect::from_xywh(0, 0, 40, 50), t, &mut cache);
                assert!(cache.rect(1).unwrap().y0 >= 0, "{j:?} started above the box");
                assert!(cache.rect(1).unwrap().y1 <= 50, "{j:?} ran past the box");
            }
        });
    }

    #[test]
    fn a_centred_child_sits_in_the_middle_of_the_line() {
        // The S60 list row, in numbers: a 17-pixel line of text in a 38-pixel row belongs at 10,
        // not at 0. This is the case the pixel comparison against the hand-written toolkit failed
        // on, and the numbers here are that scene's numbers.
        let cache = run(
            Rect::from_xywh(0, 0, 100, 38),
            || {
                Node::Group(
                    Row::new()
                        .align(CrossAlign::Center)
                        .child(Probe::fixed(30, 17))
                        .child(Probe::fixed(20, 9)),
                )
            },
            1,
        );
        let a = cache.rect(1).unwrap();
        let b = cache.rect(2).unwrap();
        assert_eq!((a.y0, a.y1), (10, 27), "a 17-pixel child centred in 38");
        assert_eq!((b.y0, b.y1), ((38 - 9) / 2, (38 - 9) / 2 + 9), "and each child on its own");
        // The main axis is untouched: alignment moves a child across the line, never along it.
        assert_eq!((a.x0, a.x1), (0, 30));
        assert_eq!((b.x0, b.x1), (30, 50));
    }

    #[test]
    fn the_odd_pixel_of_a_centred_child_goes_below_it() {
        // 38 minus 17 is 21, which does not halve. The truncating division puts 10 above and 11
        // below — the same way `Canvas::draw_text_in` centres a line in its box, which is what
        // makes a centred child and a stretched child holding centred text land on one pixel
        // rather than on two that are nearly the same.
        let cache = run(
            Rect::from_xywh(0, 0, 100, 38),
            || Node::Group(Row::new().align(CrossAlign::Center).child(Probe::fixed(10, 17))),
            1,
        );
        let r = cache.rect(1).unwrap();
        assert_eq!(r.y0, 10, "above");
        assert_eq!(38 - r.y1, 11, "below");
    }

    #[test]
    fn a_stretched_child_fills_the_cross_axis() {
        let cache = run(
            Rect::from_xywh(0, 0, 100, 38),
            || {
                Node::Group(
                    Row::new()
                        .align(CrossAlign::Stretch)
                        .child(Probe::fixed(30, 17))
                        .child(Probe::fill(1)),
                )
            },
            1,
        );
        assert_eq!(heights(&cache, &[1, 2]), [38, 38]);
        assert_eq!(widths(&cache, &[1, 2]), [30, 70], "and the division is unchanged");
    }

    #[test]
    fn a_stretched_child_cannot_escape_the_band_it_was_stretched_into() {
        // The case the clamp exists for. A stretch is imposed on the rect rather than negotiated
        // through `measure`, so a widget that answers `measure` with something absurd changes
        // nothing: it is still handed exactly the band, padding included.
        let cache = run(
            Rect::from_xywh(4, 6, 100, 38),
            || {
                Node::Group(
                    Row::new()
                        .align(CrossAlign::Stretch)
                        .pad(5)
                        .child(Probe::fixed(10, 9999)),
                )
            },
            1,
        );
        let r = cache.rect(1).unwrap();
        assert_eq!((r.y0, r.y1), (11, 39), "inside the padding, not outside the row");
        let root = cache.rect(0).unwrap();
        assert_eq!(r.intersect(root), r);
    }

    #[test]
    fn an_end_aligned_child_sits_against_the_far_edge() {
        let cache = run(
            Rect::from_xywh(0, 0, 100, 38),
            || Node::Group(Row::new().align(CrossAlign::End).child(Probe::fixed(10, 17))),
            1,
        );
        assert_eq!(cache.rect(1).unwrap().y1, 38);
    }

    #[test]
    fn alignment_transposes_with_the_axis() {
        // A column centres across its *width*. If the engine ever grew a second copy of the
        // placement arithmetic, this is where the copy would show.
        let cache = run(
            Rect::from_xywh(0, 0, 100, 38),
            || Node::Group(Column::new().align(CrossAlign::Center).child(Probe::fixed(30, 9))),
            1,
        );
        let r = cache.rect(1).unwrap();
        assert_eq!((r.x0, r.x1), (35, 65));
        assert_eq!((r.y0, r.y1), (0, 9), "and the main axis is still the main axis");
    }

    #[test]
    fn alignment_is_free_because_it_is_not_a_size() {
        // Changing where a child sits must not re-measure anything: a digest says whether a size
        // could have moved, and this cannot move one. If alignment ever joins `content_hash`, this
        // fails and the reason to think again is written down.
        testing::with_theme(Palette::DARK, |t| {
            let calls = counter();
            let build = |align: CrossAlign| {
                Node::Group(Row::new().align(align).child(Probe::fixed(10, 17).counted(&calls)))
            };
            let mut cache = UiCache::new();
            let area = Rect::from_xywh(0, 0, 100, 38);
            frame(&build(CrossAlign::Start), area, t, &mut cache);
            assert_eq!(cache.rect(1).unwrap().y0, 0);

            frame(&build(CrossAlign::Center), area, t, &mut cache);
            assert_eq!(calls.get(), 1, "moving a child is not measuring it");
            assert_eq!(cache.rect(1).unwrap().y0, 10, "and it still moved");
        });
    }

    #[test]
    fn the_default_is_still_the_start_of_the_line() {
        // Said out loud because changing it would move every screen already written against it by
        // a few pixels, which is the sort of thing that is only noticed in a screenshot.
        assert_eq!(CrossAlign::default(), CrossAlign::Start);
        let cache = run(AREA, || Node::Group(Row::new().child(Probe::fixed(10, 4))), 1);
        assert_eq!(cache.rect(1).unwrap().y0, 0);
    }

    #[test]
    fn shares_add_up_to_exactly_what_was_handed_out() {
        // The division on its own, over sizes and weights that do not divide evenly.
        for leftover in [0, 1, 7, 99, 100, 320, 12345] {
            for weights in [&[1, 1, 1][..], &[2, 1][..], &[1, 2, 3, 4][..], &[7][..], &[5, 5][..]] {
                let total: i32 = weights.iter().sum();
                let mut s = Shares::new(leftover, total);
                let given: i32 = weights.iter().map(|&w| s.take(w)).sum();
                assert_eq!(given, leftover, "{leftover} split by {weights:?}");
            }
        }
    }

    #[test]
    fn a_huge_offer_does_not_overflow_the_division() {
        // `Constraints::unbounded` is a quarter of i32::MAX; multiplying that by a weight before
        // dividing is where a 32-bit intermediate would wrap and hand a child a negative width.
        testing::with_theme(Palette::DARK, |t| {
            let mut cache = UiCache::new();
            let root = Node::Group(Row::new().child(Probe::fill(3)).child(Probe::fill(1)));
            let size = measure_tree(&root, Constraints::unbounded(), t, &mut cache);
            assert!(size.w > 0, "an unbounded row wrapped round to {}", size.w);
            assert!(cache.size(1).unwrap().w > 0);
            assert!(cache.size(2).unwrap().w > 0);
        });
    }

    #[test]
    fn a_tagged_change_that_does_not_move_anything_still_re_measures_only_itself() {
        // A label whose text changed but whose width did not: the digest moves, the size does not,
        // and only the widget that changed pays for it.
        testing::with_theme(Palette::DARK, |t| {
            let (a, b) = (counter(), counter());
            let build = |tag: i32| {
                Node::Group(
                    Row::new()
                        .child(Probe::fixed(10, 4).tag(tag).counted(&a))
                        .child(Probe::fixed(10, 4).counted(&b)),
                )
            };
            let mut cache = UiCache::new();
            frame(&build(0), AREA, t, &mut cache);
            frame(&build(1), AREA, t, &mut cache);
            assert_eq!((a.get(), b.get()), (2, 1));
            assert_eq!(cache.rect(1).unwrap().width(), 10, "the size did not have to change");
        });
    }
}

#[cfg(test)]
mod overflow_tests {
    use super::*;
    use crate::widget::Widget;
    use crate::widgets::Row;
    use symbian_gfx::Color;
    use symbian_ui::gfx::Edges;
    use symbian_ui::{testing, Palette};

    /// A leaf that deliberately paints a band *above* the rect it was given.
    ///
    /// Which is what an unread badge does — its ink is taller than its line box — and the only way
    /// to test a clip is with something that would visibly overrun without one.
    struct Overrun(bool);

    impl Widget for Overrun {
        fn measure(&self, c: Constraints, _t: &Theme<'_>) -> Size {
            c.constrain(Size::new(10, 10))
        }
        fn overflow_visible(&self) -> bool {
            self.0
        }
        fn draw(&self, c: &mut Canvas<'_>, rect: Rect, _t: &Theme<'_>) {
            // Four pixels higher than allowed, so a clip is unmistakable in the result.
            c.fill_rect(Rect::from_xywh(rect.x0, rect.y0 - 4, rect.x1 - rect.x0, rect.y1 - rect.y0 + 4), Color::rgb(255, 0, 0));
        }
    }

    /// Paint one `Overrun` at y=10 in a 40x40 buffer and report whether y=8 got ink.
    fn escaped(overflow: bool) -> bool {
        let mut out = false;
        testing::with_theme(Palette::DARK, |t| {
            let node = Node::Group(Row::new().padding(Edges::new(0, 10, 0, 0)).child(Overrun(overflow)).overflow_visible());
            let mut cache = UiCache::new();
            let (_, px) = testing::with_canvas(Size::new(40, 40), |c| {
                draw_frame(&node, Rect::from_xywh(0, 0, 40, 40), &mut cache, c, t);
            });
            out = px[8 * 40 + 2] != 0;
        });
        out
    }

    #[test]
    fn a_leaf_is_clipped_to_its_own_rect_by_default() {
        // The inverse of CSS's initial value, and on purpose: a widget whose draw runs wide would
        // otherwise eat its neighbour with nothing to show for it but a device screenshot.
        assert!(!escaped(false), "the overrun should have been clipped away");
    }

    #[test]
    fn a_leaf_that_declares_overflow_paints_outside_its_box() {
        assert!(escaped(true), "overflow_visible did not reach the draw");
    }
}

#[cfg(test)]
mod align_self_tests {
    use super::*;
    use crate::widget::Widget;
    use crate::widgets::Row;
    use symbian_ui::{testing, Palette};

    /// A fixed box that can carry its own cross-axis answer.
    struct Bubble {
        h: i32,
        mine: Option<CrossAlign>,
    }

    impl Widget for Bubble {
        fn measure(&self, c: Constraints, _t: &Theme<'_>) -> Size {
            c.constrain(Size::new(10, self.h))
        }
        fn align_self(&self) -> Option<CrossAlign> {
            self.mine
        }
        fn draw(&self, _c: &mut Canvas<'_>, _r: Rect, _t: &Theme<'_>) {}
    }

    /// Lay out a row 40 tall with `line` as its align-items, and return each child's rect.
    fn rects(line: CrossAlign, kids: [Option<CrossAlign>; 2]) -> Vec<Rect> {
        let mut out = Vec::new();
        testing::with_theme(Palette::DARK, |t| {
            let node = Node::Group(
                Row::new()
                    .align(line)
                    .child(Bubble { h: 10, mine: kids[0] })
                    .child(Bubble { h: 10, mine: kids[1] }),
            );
            let mut cache = UiCache::new();
            let area = Rect::from_xywh(0, 0, 100, 40);
            cache.begin_frame();
            measure_tree(&node, Constraints::tight(area.width(), area.height()), t, &mut cache);
            layout_tree(&node, area, &mut cache, t);
            out = (1..=2).filter_map(|slot| cache.rect(slot)).collect();
        });
        out
    }

    #[test]
    fn a_child_can_disagree_with_the_line_it_is_on() {
        // The case this exists for: a transcript where every bubble hugs the left except the
        // outgoing ones, which hug the right. One line, two answers.
        let r = rects(CrossAlign::Start, [None, Some(CrossAlign::End)]);
        assert_eq!(r[0].y0, 0, "the default child follows the line");
        assert_eq!(r[1].y1, 40, "the overriding child reaches the far edge");
    }

    #[test]
    fn without_an_override_every_child_still_follows_the_line() {
        // The regression that would matter most: `align_self` reaching the layout as `Some` by
        // accident would silently ignore every `align` in the SDK.
        for line in [CrossAlign::Start, CrossAlign::Center, CrossAlign::End] {
            let with = rects(line, [None, None]);
            // Only the cross axis: they are side by side on a row, so their x differs by design.
            assert_eq!(
                (with[0].y0, with[0].y1),
                (with[1].y0, with[1].y1),
                "{line:?}: both children agree across the line when neither overrides"
            );
        }
        assert_eq!(rects(CrossAlign::Start, [None, None])[0].y0, 0);
        assert_eq!(rects(CrossAlign::End, [None, None])[0].y1, 40);
    }

    #[test]
    fn an_override_beats_the_line_rather_than_blending_with_it() {
        // Stretch is the one that would look like it worked while doing something else: it changes
        // the child's *size*, not only its position, so a child that overrode a stretching line
        // must come back to its measured height.
        let r = rects(CrossAlign::Stretch, [None, Some(CrossAlign::Start)]);
        assert_eq!(r[0].y1 - r[0].y0, 40, "the stretched child fills the band");
        assert_eq!(r[1].y1 - r[1].y0, 10, "the overriding one keeps its own height");
        assert_eq!(r[1].y0, 0);
    }

    #[test]
    fn a_group_can_override_for_itself_too() {
        // Same property, reached through `Group::align_self` rather than the trait — the two must
        // not be two mechanisms.
        let mut out = None;
        testing::with_theme(Palette::DARK, |t| {
            let node = Node::Group(
                Row::new()
                    .align(CrossAlign::Start)
                    .group(Row::new().align_self(CrossAlign::End).child(Bubble { h: 10, mine: None })),
            );
            let mut cache = UiCache::new();
            let area = Rect::from_xywh(0, 0, 100, 40);
            cache.begin_frame();
            measure_tree(&node, Constraints::tight(area.width(), area.height()), t, &mut cache);
            layout_tree(&node, area, &mut cache, t);
            out = cache.rect(1);
        });
        assert_eq!(out.expect("the group was placed").y1, 40);
    }
}
