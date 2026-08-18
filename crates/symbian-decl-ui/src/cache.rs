//! What the last frame worked out, kept for this one.
//!
//! Drawing a screen is cheap on this hardware; *measuring* one is not. A line of text costs a walk
//! over its bytes through the font's advance table, and a screen is thirty of those — every frame,
//! for a list that has not changed since the phone was switched on. The cache is the answer: a
//! widget's size is recomputed only when the widget says something about it changed, which it does
//! through [`Widget::content_hash`].
//!
//! # Slots are positions, not identities
//!
//! Every entry is addressed by its index in a pre-order walk of the tree — root 0, first child 1,
//! and so on. Nothing here checks that slot 4 still holds the widget it held last frame, because
//! nothing can: a widget is a description rebuilt from scratch each frame, with no identity to
//! compare. What makes that safe is a rule the layout pass keeps: a container folds its children's
//! digests into its own, so a subtree that changed *shape* changes its parent's digest, and the
//! parent's miss forces the whole subtree to be measured again. Break that rule and the failure is
//! the nastiest kind — a screen showing last frame's arithmetic with this frame's content.
//!
//! # A digest of zero means "ask me every time"
//!
//! [`Widget::content_hash`] returns `0` by default, and this cache never answers a lookup for it.
//! That is the safe direction: a widget that has not been taught to describe itself is slow, not
//! wrong.

use alloc::vec::Vec;

use symbian_gfx::{Rect, Size};
use symbian_ui::Theme;

use crate::constraints::Constraints;
use crate::widget::{Widget, WidgetHash};

/// One slot's memory: what it measured to, and where it was put.
#[derive(Clone, Copy)]
struct Entry {
    hash: WidgetHash,
    /// The offer the size was computed against. A cached size is only an answer to the question it
    /// was asked: the same row measured inside a 320px screen and inside a 120px column are
    /// different sizes, and matching on the digest alone would hand back the wrong one the first
    /// time a screen changes width.
    offer: Constraints,
    size: Size,
    /// Whether `size` has ever been computed. A fresh slot is not a zero-sized widget.
    sized: bool,
    rect: Rect,
    /// The frame `rect` was written in. Rects are answered only for the current frame, so a tree
    /// that lost a branch cannot draw the branch's stale rectangle.
    rect_gen: u32,
}

impl Default for Entry {
    fn default() -> Self {
        Self {
            hash: 0,
            offer: Constraints::default(),
            size: Size::ZERO,
            sized: false,
            rect: Rect::EMPTY,
            rect_gen: 0,
        }
    }
}

/// Measured sizes and laid-out rects, kept between frames.
pub struct UiCache {
    entries: Vec<Entry>,
    generation: u32,
    measures: u32,
}

impl UiCache {
    pub fn new() -> Self {
        Self::with_capacity(64)
    }

    /// Room for `cap` slots up front. A screen's node count is known when it is written, and one
    /// allocation at startup is one fewer thing happening during a frame.
    pub fn with_capacity(cap: usize) -> Self {
        // Generation starts at 1 so that a never-written rect (`rect_gen == 0`) can never be
        // mistaken for one laid out this frame.
        Self { entries: Vec::with_capacity(cap), generation: 1, measures: 0 }
    }

    /// Start a frame: rects from the previous one stop being answers.
    ///
    /// Sizes deliberately survive — they are the whole point of the cache. What does not survive is
    /// *placement*, because a rect that is no longer written this frame belongs to a widget that is
    /// no longer on screen.
    pub fn begin_frame(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        // Wrapping past zero would make a stale rect look current for exactly one frame; skipping
        // the value costs nothing and removes the case.
        if self.generation == 0 {
            self.generation = 1;
        }
        self.measures = 0;
    }

    /// The cached size for `slot`, if this widget is asking the same question as last time.
    ///
    /// `None` is returned for a digest of zero: that is a widget asking to be measured every frame.
    pub fn lookup(&self, slot: usize, hash: WidgetHash, offer: Constraints) -> Option<Size> {
        if hash == 0 {
            return None;
        }
        let e = self.entries.get(slot)?;
        (e.sized && e.hash == hash && e.offer == offer).then_some(e.size)
    }

    /// Record what `slot` measured to.
    pub fn store(&mut self, slot: usize, hash: WidgetHash, offer: Constraints, size: Size) {
        self.ensure(slot);
        let e = &mut self.entries[slot];
        e.hash = hash;
        e.offer = offer;
        // A negative size becomes a rect with `x1 < x0`, which draws as nothing and reports
        // nothing. Widgets are supposed to constrain their answer; this is the line that means a
        // widget that forgets cannot blank a screen.
        e.size = Size::new(size.w.max(0), size.h.max(0));
        e.sized = true;
    }

    /// The cached size, or [`Widget::measure`] if there is nothing to reuse.
    ///
    /// This is the only place in the crate that calls `measure`, which is what makes
    /// [`measure_calls`](Self::measure_calls) an honest count.
    pub fn measure_or_compute(
        &mut self,
        slot: usize,
        widget: &dyn Widget,
        offer: Constraints,
        theme: &Theme<'_>,
    ) -> Size {
        let hash = widget.content_hash();
        if let Some(size) = self.lookup(slot, hash, offer) {
            return size;
        }
        self.measures += 1;
        let size = offer.constrain(widget.measure(offer, theme));
        self.store(slot, hash, offer, size);
        size
    }

    /// What `slot` measured to, from this frame or any earlier one.
    ///
    /// The layout pass reads sizes rather than recomputing them, which is why it takes no theme and
    /// cannot call `measure`: everything a placement needs was decided while measuring.
    pub fn size(&self, slot: usize) -> Option<Size> {
        self.entries.get(slot).filter(|e| e.sized).map(|e| e.size)
    }

    /// Place `slot` at `rect` for this frame.
    pub fn set_rect(&mut self, slot: usize, rect: Rect) {
        self.ensure(slot);
        let e = &mut self.entries[slot];
        // Last line of defence, and the reason it is here rather than trusted to the caller: an
        // inverted rect is invisible *and* silent. Every rect that reaches a canvas comes through
        // this function, so one clamp covers every path.
        e.rect = Rect::new(rect.x0, rect.y0, rect.x1.max(rect.x0), rect.y1.max(rect.y0));
        e.rect_gen = self.generation;
    }

    /// Where `slot` was put *this frame*, if it was.
    pub fn rect(&self, slot: usize) -> Option<Rect> {
        self.entries.get(slot).filter(|e| e.rect_gen == self.generation).map(|e| e.rect)
    }

    /// How many times [`Widget::measure`] ran since [`begin_frame`](Self::begin_frame).
    ///
    /// The number this layer exists to keep at zero on an idle frame.
    pub fn measure_calls(&self) -> u32 {
        self.measures
    }

    /// How many slots the cache has memory for. Grows to fit the tree and never shrinks: a screen
    /// that alternates between two shapes would otherwise pay an allocation on every switch.
    pub fn slots(&self) -> usize {
        self.entries.len()
    }

    fn ensure(&mut self, slot: usize) {
        if slot >= self.entries.len() {
            self.entries.resize(slot + 1, Entry::default());
        }
    }
}

impl Default for UiCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::hash_i32;
    use core::cell::Cell;
    use symbian_gfx::Canvas;
    use symbian_ui::testing;

    /// A widget that counts how often it was actually asked to measure.
    struct Probe {
        size: Size,
        hash: WidgetHash,
        calls: Cell<usize>,
    }

    impl Probe {
        fn new(w: i32, h: i32) -> Self {
            Self { size: Size::new(w, h), hash: hash_i32(hash_i32(0, w), h), calls: Cell::new(0) }
        }
    }

    impl Widget for Probe {
        fn content_hash(&self) -> WidgetHash {
            self.hash
        }
        fn measure(&self, c: Constraints, _t: &Theme<'_>) -> Size {
            self.calls.set(self.calls.get() + 1);
            c.constrain(self.size)
        }
        fn draw(&self, _c: &mut Canvas<'_>, _r: Rect, _t: &Theme<'_>) {}
    }

    #[test]
    fn the_second_ask_does_not_reach_the_widget() {
        testing::with_theme(symbian_ui::Palette::DARK, |t| {
            let mut cache = UiCache::new();
            let p = Probe::new(10, 4);
            let offer = Constraints::loose(100, 50);

            assert_eq!(cache.measure_or_compute(0, &p, offer, t), Size::new(10, 4));
            assert_eq!(cache.measure_or_compute(0, &p, offer, t), Size::new(10, 4));
            assert_eq!(p.calls.get(), 1, "the second ask was answered from the cache");
        });
    }

    #[test]
    fn a_different_offer_is_a_different_question() {
        testing::with_theme(symbian_ui::Palette::DARK, |t| {
            let mut cache = UiCache::new();
            let p = Probe::new(10, 4);
            cache.measure_or_compute(0, &p, Constraints::loose(100, 50), t);
            // Same widget, narrower parent: the cached answer was to another question. A cache
            // that ignored the offer would keep a 10px row at 10px inside a 4px column.
            let narrow = cache.measure_or_compute(0, &p, Constraints::loose(4, 50), t);
            assert_eq!(narrow, Size::new(4, 4));
            assert_eq!(p.calls.get(), 2);
        });
    }

    #[test]
    fn a_digest_of_zero_is_never_cached() {
        struct Volatile;
        impl Widget for Volatile {
            fn measure(&self, c: Constraints, _t: &Theme<'_>) -> Size {
                c.constrain(Size::new(3, 3))
            }
            fn draw(&self, _c: &mut Canvas<'_>, _r: Rect, _t: &Theme<'_>) {}
        }
        testing::with_theme(symbian_ui::Palette::DARK, |t| {
            let mut cache = UiCache::new();
            let offer = Constraints::loose(50, 50);
            for _ in 0..3 {
                cache.measure_or_compute(0, &Volatile, offer, t);
            }
            // Three asks, three measures: the default digest means "I might have changed".
            assert_eq!(cache.measure_calls(), 3);
        });
    }

    #[test]
    fn slots_are_independent() {
        testing::with_theme(symbian_ui::Palette::DARK, |t| {
            let mut cache = UiCache::new();
            let (a, b) = (Probe::new(10, 4), Probe::new(20, 4));
            let offer = Constraints::loose(100, 50);
            assert_eq!(cache.measure_or_compute(0, &a, offer, t), Size::new(10, 4));
            assert_eq!(cache.measure_or_compute(1, &b, offer, t), Size::new(20, 4));
            assert_eq!(cache.measure_or_compute(0, &a, offer, t), Size::new(10, 4));
            assert_eq!(cache.measure_or_compute(1, &b, offer, t), Size::new(20, 4));
            assert_eq!(cache.measure_calls(), 2);
        });
    }

    #[test]
    fn sizes_survive_a_frame_boundary_and_rects_do_not() {
        testing::with_theme(symbian_ui::Palette::DARK, |t| {
            let mut cache = UiCache::new();
            let p = Probe::new(10, 4);
            let offer = Constraints::loose(100, 50);
            cache.measure_or_compute(0, &p, offer, t);
            cache.set_rect(0, Rect::from_xywh(1, 2, 10, 4));
            assert_eq!(cache.rect(0), Some(Rect::from_xywh(1, 2, 10, 4)));

            cache.begin_frame();
            // The size is still an answer; the placement is not, because nothing has placed it yet
            // this frame. That is what stops a widget dropped from the tree from drawing where it
            // used to be.
            assert_eq!(cache.size(0), Some(Size::new(10, 4)));
            assert_eq!(cache.rect(0), None);
            assert_eq!(cache.measure_calls(), 0);
            cache.measure_or_compute(0, &p, offer, t);
            assert_eq!(p.calls.get(), 1);
        });
    }

    #[test]
    fn an_inverted_rect_cannot_get_in() {
        let mut cache = UiCache::new();
        cache.set_rect(0, Rect::new(50, 50, 10, 10));
        let r = cache.rect(0).unwrap();
        assert!(r.x1 >= r.x0 && r.y1 >= r.y0, "an inverted rect draws nothing and says nothing");
        assert!(r.is_empty());
    }

    #[test]
    fn a_negative_size_is_stored_as_nothing() {
        let mut cache = UiCache::new();
        cache.store(0, 7, Constraints::loose(10, 10), Size::new(-5, -5));
        assert_eq!(cache.size(0), Some(Size::ZERO));
    }

    #[test]
    fn an_unwritten_slot_has_nothing_to_say() {
        let cache = UiCache::new();
        assert_eq!(cache.size(9), None);
        assert_eq!(cache.rect(9), None);
        assert_eq!(cache.lookup(9, 1234, Constraints::loose(10, 10)), None);
    }

    #[test]
    fn the_generation_never_lands_on_the_never_written_value() {
        let mut cache = UiCache::new();
        cache.generation = u32::MAX;
        cache.set_rect(0, Rect::from_xywh(0, 0, 1, 1));
        cache.begin_frame();
        // Wrapping to 0 would make last frame's rect (`rect_gen == 0` for a fresh slot) look
        // current, so the wrap skips it.
        assert_ne!(cache.generation, 0);
        assert_eq!(cache.rect(0), None);
    }
}
