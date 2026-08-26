//! Children in one place, drawn back to front.
//!
//! # Why a layer and not a row or a column
//!
//! Because a real screen has content that does not queue. `tg`'s login panel centres a title, a
//! field and an error line *in the whole content band*, and then writes a connection status along
//! the bottom of that same band — and the status must not push the block upwards or the block is no
//! longer centred in what it was centred in. Modelled as a column, the two compete for the axis and
//! the centring moves by half the status line; modelled as layers, it is exactly what the
//! hand-written screen does.
//!
//! That is the only reason this exists. It is not a positioning system: there is no z-index, no
//! offset, no anchor. Every child gets the same rect and decides for itself where in it to sit,
//! with `justify`/`align` on the group it is — which is enough for an overlay, a watermark behind a
//! panel, and a note pinned to the bottom of a transcript.
//!
//! # It is a leaf to the engine, like [`ScrollList`](crate::widgets::ScrollList)
//!
//! The layout pass divides a box along an axis; a stack does not divide anything. Rather than teach
//! `layout` a third axis that means "do not divide", this widget owns its subtree the way the list
//! owns its rows: its own cache in a slot, its own three passes, one node as far as anything above
//! it can see. The cost is that an ancestor cannot see inside it, which for two overlaid panels is
//! not a cost at all.
//!
//! # Keys go to the top layer first
//!
//! The child declared *last* is the one drawn on top, so it is the one a key is offered to first.
//! Anything else would let a panel underneath answer for a press that visibly landed on the one in
//! front of it.

use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::{Handled, KeyEvent, Theme};

use crate::cache::UiCache;
use crate::constraints::Constraints;
use crate::layout;
use crate::slot::SlotTable;
use crate::widget::{hash_bytes, hash_i32, KeyCtx, Widget, WidgetHash};
use crate::widgets::{Group, Node};

/// Overlaid children: same rect, painted in the order they were added.
pub struct Stack {
    children: Vec<Node>,
    /// Measured sizes for the layers, kept across frames for the same reason a list keeps one: a
    /// cache born and buried inside `draw` would miss on every lookup.
    cache: Rc<RefCell<UiCache>>,
    child_hash: WidgetHash,
    volatile: bool,
    slots: usize,
    weight: i32,
}

impl Stack {
    /// Takes the slot table for its cache, exactly as [`ScrollList`](crate::widgets::ScrollList)
    /// does — the widget is rebuilt every frame and the measurements must not be.
    pub fn new(slots: &mut SlotTable) -> Self {
        let cache = slots.use_state_with(|| Rc::new(RefCell::new(UiCache::new()))).clone();
        Self { children: Vec::new(), cache, child_hash: 0, volatile: false, slots: 0, weight: 0 }
    }

    /// Add a layer on top of whatever is already there.
    pub fn layer(mut self, n: Node) -> Self {
        let h = n.content_hash();
        // Same rule as `Group::node`: a child that wants re-measuring every frame makes this
        // widget's digest useless, because a hit here would skip the subtree entirely.
        if h == 0 {
            self.volatile = true;
        } else {
            self.child_hash = hash_bytes(self.child_hash, &h.to_le_bytes());
        }
        self.slots += n.slot_count();
        self.children.push(n);
        self
    }

    /// A layer that is a row or a column, which is what most of them are.
    pub fn group(self, g: Group) -> Self {
        self.layer(Node::Group(g))
    }

    /// A layer that is a single widget.
    pub fn child(self, w: impl Widget + 'static) -> Self {
        self.layer(Node::leaf(w))
    }

    /// Take a share of the parent's leftover space.
    pub fn fill(mut self, weight: i32) -> Self {
        self.weight = weight;
        self
    }

    pub fn layers(&self) -> usize {
        self.children.len()
    }

    /// Measure, place and then `f` for every layer, over this widget's own cache.
    ///
    /// One walk shared by `draw` and `handle_key`, because the two must agree about where a layer
    /// is: a key answered at a rect the paint pass did not use is the defect the whole crate's
    /// dispatch order is written to avoid.
    fn each(&self, rect: Rect, theme: &Theme<'_>, mut f: impl FnMut(&Node, usize, &UiCache)) {
        let mut cache = self.cache.borrow_mut();
        cache.begin_frame();
        let mut slot = 0usize;
        for child in &self.children {
            let offer = Constraints::tight(rect.width(), rect.height());
            layout::measure_node(child, slot, offer, theme, &mut cache);
            layout::layout_node(child, slot, rect, &mut cache, theme);
            slot += child.slot_count();
        }
        let mut slot = 0usize;
        for child in &self.children {
            f(child, slot, &cache);
            slot += child.slot_count();
        }
    }
}

impl Widget for Stack {
    fn content_hash(&self) -> WidgetHash {
        if self.volatile {
            return 0;
        }
        let h = hash_i32(0, self.children.len() as i32);
        hash_bytes(hash_i32(h, self.weight), &self.child_hash.to_le_bytes())
    }

    /// Everything it is offered.
    ///
    /// Not "the largest layer": a stack exists to put one thing over another *inside a band*, and a
    /// stack that shrank to its tallest layer would move the band, which is the one thing its
    /// callers are trying to stop happening.
    fn measure(&self, constraints: Constraints, _theme: &Theme<'_>) -> Size {
        constraints.constrain(Size::new(constraints.max_w, constraints.max_h))
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        self.each(rect, theme, |child, slot, cache| {
            layout::draw_node(child, slot, cache, c, theme);
        });
    }

    fn handle_key(&self, ev: KeyEvent, rect: Rect, cx: &mut KeyCtx<'_>) -> Handled {
        // Top layer first, so the panel in front answers for a press that landed on it. The rects
        // come from the same walk `draw` uses.
        let mut order: Vec<(usize, usize)> = Vec::with_capacity(self.children.len());
        self.each(rect, cx.theme, |_child, slot, _cache| {
            order.push((order.len(), slot));
        });
        let cache = self.cache.borrow();
        for (index, slot) in order.into_iter().rev() {
            if layout::dispatch_key_node(&self.children[index], slot, ev, &cache, cx)
                == Handled::Consumed
            {
                return Handled::Consumed;
            }
        }
        Handled::Ignored
    }

    fn flex_weight(&self) -> i32 {
        self.weight
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::{Column, Spacer, Text};
    use symbian_gfx::{Color, Size as GSize};
    use symbian_ui::{testing, Align, Key, Palette};

    const W: i32 = 200;
    const H: i32 = 100;

    /// A widget that fills its rect with one colour, so a test can see which pixels it got.
    struct Fill(Color);

    impl Widget for Fill {
        fn content_hash(&self) -> WidgetHash {
            hash_i32(0, self.0.to_rgb565().0 as i32)
        }
        fn measure(&self, c: Constraints, _t: &Theme<'_>) -> Size {
            c.constrain(Size::new(c.max_w, c.max_h))
        }
        fn draw(&self, c: &mut Canvas<'_>, rect: Rect, _t: &Theme<'_>) {
            c.fill_rect(rect, self.0);
        }
    }

    fn pixels(build: impl FnOnce(&mut SlotTable) -> Stack) -> alloc::vec::Vec<u16> {
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let stack = build(&mut slots);
        let mut buf = alloc::vec![0u16; (W * H) as usize];
        testing::with_theme(Palette::DARK, |t| {
            let mut c = Canvas::from_slice(&mut buf, GSize::new(W, H));
            stack.draw(&mut c, Rect::from_xywh(0, 0, W, H), t);
        });
        buf
    }

    #[test]
    fn every_layer_gets_the_whole_rect() {
        // The property the login panel needs: the second layer does not start below the first.
        let red = Color::hex(0xFF0000);
        let buf = pixels(|slots| Stack::new(slots).child(Fill(Color::hex(0x00FF00))).child(Fill(red)));
        assert!(buf.iter().all(|&px| px == red.to_rgb565().0), "the top layer did not cover the rect");
    }

    #[test]
    fn the_last_layer_is_the_one_on_top() {
        let green = Color::hex(0x00FF00);
        // Half-height top layer, so the layer underneath shows through where it does not reach.
        let buf = pixels(|slots| {
            Stack::new(slots).child(Fill(green)).group(
                Column::new()
                    .justify(crate::MainAlign::Start)
                    .stretch_width()
                    .child(Spacer::new().height(H / 2))
                    .child(Fill(Color::hex(0x0000FF))),
            )
        });
        assert_eq!(buf[0], green.to_rgb565().0, "the top half should still be the layer underneath");
        assert_eq!(buf[(W * (H - 1)) as usize], Color::hex(0x0000FF).to_rgb565().0);
    }

    #[test]
    fn a_stack_takes_the_band_rather_than_its_tallest_layer() {
        // Shrinking to fit would move the band, which is exactly what its callers use it to prevent.
        testing::with_theme(Palette::DARK, |t| {
            let mut slots = SlotTable::new();
            slots.begin_frame();
            let s = Stack::new(&mut slots).child(Text::new("one line").align(Align::Center));
            assert_eq!(s.measure(Constraints::loose(W, H), t), Size::new(W, H));
        });
    }

    #[test]
    fn keys_reach_the_top_layer_first() {
        // Two layers that both answer. The one in front wins, or a press would be handled by
        // something the user cannot see.
        use core::cell::Cell;
        struct Taker(Rc<Cell<u32>>, u32);
        impl Widget for Taker {
            fn content_hash(&self) -> WidgetHash {
                hash_i32(0, self.1 as i32)
            }
            fn measure(&self, c: Constraints, _t: &Theme<'_>) -> Size {
                c.constrain(Size::new(c.max_w, c.max_h))
            }
            fn draw(&self, _c: &mut Canvas<'_>, _r: Rect, _t: &Theme<'_>) {}
            fn handle_key(&self, _ev: KeyEvent, _r: Rect, _cx: &mut KeyCtx<'_>) -> Handled {
                self.0.set(self.1);
                Handled::Consumed
            }
        }
        let who = Rc::new(Cell::new(0));
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let s = Stack::new(&mut slots).child(Taker(who.clone(), 1)).child(Taker(who.clone(), 2));
        crate::widget::with_key_ctx(|cx| {
            assert_eq!(s.handle_key(KeyEvent::new(Key::Down), Rect::from_xywh(0, 0, W, H), cx), Handled::Consumed);
        });
        assert_eq!(who.get(), 2, "the layer underneath answered for the one in front");
    }

    #[test]
    fn a_key_nobody_answered_falls_through() {
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let s = Stack::new(&mut slots).child(Text::new("nothing to press"));
        crate::widget::with_key_ctx(|cx| {
            assert_eq!(s.handle_key(KeyEvent::new(Key::Down), Rect::from_xywh(0, 0, W, H), cx), Handled::Ignored);
        });
    }

    #[test]
    fn the_digest_moves_with_the_layers_and_stops_at_a_volatile_one() {
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let one = Stack::new(&mut slots).child(Fill(Color::hex(0x111111)));
        let two = Stack::new(&mut slots).child(Fill(Color::hex(0x111111))).child(Fill(Color::hex(0x222222)));
        assert_ne!(one.content_hash(), two.content_hash());
        assert_ne!(one.content_hash(), 0);
        // A layer that measures every frame must not be cached over.
        struct Volatile;
        impl Widget for Volatile {
            fn measure(&self, c: Constraints, _t: &Theme<'_>) -> Size {
                c.constrain(Size::new(1, 1))
            }
            fn draw(&self, _c: &mut Canvas<'_>, _r: Rect, _t: &Theme<'_>) {}
        }
        let v = Stack::new(&mut slots).child(Volatile);
        assert_eq!(v.content_hash(), 0);
    }
}
