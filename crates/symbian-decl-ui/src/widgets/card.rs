//! A band with a ground of its own, and something inside it.
//!
//! ```ignore
//! Card::new(slots)
//!     .node(Column::new()
//!         .gap(Gap::Snug)
//!         .child(Text::new("Vivo Internet").font(FontRole::Strong))
//!         .child(Text::new("Connected").dim())
//!         .stretch_width())
//!     .stretch_width()
//! ```
//!
//! # It had to be a widget, and that is the interesting part
//!
//! The first draft was pure composition: a [`Group`](super::Group) with padding and a background,
//! no new type, nothing to test. It does not work, and the reason is worth writing down because it
//! is a hole in `Group` rather than a fact about cards.
//!
//! `Group` can paint exactly two things behind its children. One is
//! [`selection_band`](super::Group::selection_band), which is the cursor and belongs to the list or
//! the focused row — a card that borrowed it would put a second cursor on the screen. The other is
//! [`background`](super::Group::background), which takes a **`Color`**, and a `view` is built
//! *without a theme*: `DeclarativeApp::view` has no palette in hand, deliberately, which is the
//! whole reason [`Ink`](super::Ink) and [`Gap`](crate::spacing::Gap) exist. So the only colour a
//! view can pass to `background` is a literal — a card that stays the same colour when the theme
//! changes, which on `HIGH_CONTRAST` is a dark panel on a white page.
//!
//! A ground named by its role and resolved at draw time therefore needs *something* that draws, and
//! `Group` has no property for one. This widget is that something. If `Group` ever grows a
//! `surface(role)` to sit beside `border_bottom(Ink, ..)` — which is the same idea for a hairline,
//! already there — this type collapses back into a two-line builder, and that is the right outcome
//! rather than a threat.
//!
//! # It owns a cache, which is what keeps it from being the `Group: Widget` trap
//!
//! A leaf is opaque to the layout pass, so everything under this node is measured by this node. Done
//! naively that is the trap `mod.rs` warns about: a container acting as a leaf measures its whole
//! subtree from a throwaway cache, every frame, for ever.
//!
//! [`Stack`](super::Stack) already solved it and this is the same solution — a [`UiCache`] taken
//! from the slot table, so the measurements survive between frames exactly as they would inside the
//! engine. That is why [`Card::new`] wants a [`SlotTable`] for what looks like a purely visual
//! wrapper: the alternative is a card whose contents re-measure on every frame the screen paints.
//!
//! # Rounded, and no frame on top of it
//!
//! [`paint::band_round`] already draws the lit top edge and the dark bottom one out of the
//! [`Surface`](symbian_ui::Surface) it is given, so [`paint::frame_raised`] over it would be a
//! second bevel on the same rectangle — two highlights a pixel apart, which reads as a rendering
//! fault rather than as depth. The rounding is [`Metrics::radius`](symbian_ui::Metrics), the same
//! number bubbles and buttons use, and [`square`](Card::square) turns it off for a card that runs to
//! the edges of the screen, where a corner would be a notch cut out of the page.

use super::surface_role::SurfaceRole;
use alloc::rc::Rc;
use core::cell::RefCell;

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::{paint, Ground, Handled, KeyEvent, Theme};

use crate::cache::UiCache;
use crate::constraints::Constraints;
use crate::layout;
use crate::slot::SlotTable;
use crate::spacing::{Gap, Pad};
use crate::widget::{hash_bytes, hash_i32, KeyCtx, Widget, WidgetHash};
use crate::widgets::{Group, Node};

/// A grouping band: a ground, a rounding, a padding, and one child.
///
/// One child rather than a list of them, because a card that arranged its own children would be a
/// second [`Group`](super::Group) — and a worse one, since it would be arranging them behind a leaf
/// where the engine cannot see. The child is normally a `Column`, built with the same `gap` and
/// `align` a caller would use anywhere else.
pub struct Card {
    child: Option<Node>,
    /// Kept across frames for the same reason [`Stack`](super::Stack) keeps one: a cache born and
    /// buried inside `draw` misses on every lookup, which is the whole cost this widget was
    /// otherwise going to pay.
    cache: Rc<RefCell<UiCache>>,
    surface: SurfaceRole,
    pad: Pad,
    rounded: bool,
    stretch_w: bool,
    weight: i32,
    /// Whether the row this card sits in is the selected one. See [`SurfaceRole::resolve_on`].
    selected: bool,
}

impl Card {
    /// An empty card. Takes the slot table for its cache — see the module docs.
    pub fn new(slots: &mut SlotTable) -> Self {
        let cache = slots.use_state_with(|| Rc::new(RefCell::new(UiCache::new()))).clone();
        Self {
            child: None,
            cache,
            surface: SurfaceRole::Chrome,
            // `Wide` on every side: a card is the "between groups" distance made visible, and its
            // inside wants the same air its outside does. `Base` — the list row's margin — reads as
            // a row that happens to have a colour.
            pad: Pad::all(Gap::Wide),
            rounded: true,
            stretch_w: false,
            weight: 0,
            selected: false,
        }
    }

    /// Whether the row this card is in has the cursor.
    ///
    /// Not `focused`, because a card takes no keys — it is asking about the *ground*, exactly like
    /// [`Chip::selected`](super::Chip::selected). See [`SurfaceRole::resolve_on`].
    pub fn selected(mut self, on: bool) -> Self {
        self.selected = on;
        self
    }

    /// What goes inside.
    pub fn node(mut self, n: Node) -> Self {
        self.child = Some(n);
        self
    }

    /// A single widget inside.
    pub fn child(self, w: impl Widget + 'static) -> Self {
        self.node(Node::leaf(w))
    }

    /// A row or a column inside, kept a container rather than flattened to a leaf.
    pub fn group(self, g: Group) -> Self {
        self.node(Node::Group(g))
    }

    /// Which ground the card paints. Defaults to [`SurfaceRole::Chrome`].
    pub fn surface(mut self, s: SurfaceRole) -> Self {
        self.surface = s;
        self
    }

    /// Inset between the band and its contents. Defaults to [`Gap::Wide`] on every side.
    pub fn pad(mut self, p: impl Into<Pad>) -> Self {
        self.pad = p.into();
        self
    }

    /// Square corners, for a card that runs to the edges of the screen.
    ///
    /// A rounded corner against the edge of the display is a notch cut out of the page: there is
    /// nothing outside it for the background to be, so the corner shows whatever was painted before.
    pub fn square(mut self) -> Self {
        self.rounded = false;
        self
    }

    /// Be as wide as the parent offered rather than as wide as the contents.
    ///
    /// The transpose of [`Group::stretch_width`](super::Group::stretch_width) and needed for the
    /// same reason: a band that shrank to fit its label is a stripe ending mid-screen. Not needed
    /// under a parent that already says [`CrossAlign::Stretch`](crate::layout::CrossAlign::Stretch),
    /// which hands out the full width whatever the child measured.
    pub fn stretch_width(mut self) -> Self {
        self.stretch_w = true;
        self
    }

    /// Take a share of the parent's leftover space along *its* axis.
    pub fn fill(mut self, weight: i32) -> Self {
        self.weight = weight;
        self
    }

    /// The rect the contents get: the band, less the padding.
    ///
    /// One function, called by `measure`, `draw` and `handle_key`, because the three must agree
    /// about where the child is. A key answered at a rect the paint pass did not use is the defect
    /// the whole crate's dispatch order is written to avoid.
    fn inner(&self, rect: Rect, theme: &Theme<'_>) -> Rect {
        let p = self.pad.resolve(theme);
        let r = Rect {
            x0: rect.x0 + p.left,
            y0: rect.y0 + p.top,
            x1: rect.x1 - p.right,
            y1: rect.y1 - p.bottom,
        };
        // A card smaller than its own padding produces `x1 < x0`, and an inverted rect draws as
        // nothing at all with no error anywhere — the failure `Constraints` names as the worst this
        // layer can produce. Collapsed to empty at the card's own origin instead.
        if r.x1 < r.x0 || r.y1 < r.y0 {
            Rect { x1: rect.x0, y1: rect.y0, ..rect }
        } else {
            r
        }
    }

    /// Measure and place the child over this card's own cache, then hand it to `f`.
    ///
    /// [`Stack::each`](super::Stack)'s shape, minus the loop: the walk that `draw` and `handle_key`
    /// share so that neither can place the child somewhere the other did not.
    fn placed(&self, rect: Rect, theme: &Theme<'_>, f: impl FnOnce(&Node, &UiCache)) {
        let Some(child) = &self.child else { return };
        let inner = self.inner(rect, theme);
        let mut cache = self.cache.borrow_mut();
        cache.begin_frame();
        layout::measure_node(
            child,
            0,
            Constraints::tight(inner.width(), inner.height()),
            theme,
            &mut cache,
        );
        layout::layout_node(child, 0, inner, &mut cache, theme);
        drop(cache);
        f(child, &self.cache.borrow());
    }
}

impl Widget for Card {
    /// Everything that could move the band's edges, plus the child's own digest.
    ///
    /// Zero when the child is volatile, for the reason `Group::content_hash` gives: a hit here
    /// returns a cached size *and skips the subtree entirely*, which is the child's request to be
    /// re-measured being silently dropped. `rounded` is in although a corner is not a size, because
    /// it is one bit beside the six things that are and telling the two apart at a glance is worth
    /// more than the fold it saves.
    fn content_hash(&self) -> WidgetHash {
        let child = match &self.child {
            None => hash_i32(0, -1),
            Some(n) => match n.content_hash() {
                0 => return 0,
                h => hash_bytes(0, &h.to_le_bytes()),
            },
        };
        let h = self.pad.hash(hash_i32(child, self.surface.tag()));
        let h = hash_i32(h, self.rounded as i32);
        hash_i32(hash_i32(h, self.stretch_w as i32), self.weight)
    }

    /// The child's size plus the padding — clamped, always.
    ///
    /// The clamp is not ceremony. [`Avatar`](super::Avatar) measured from its offer without one and
    /// came back 180 pixels square inside a form; [`Slider`](super::Slider) answered with the whole
    /// offered width and ate the label off its row. A card wraps its contents by default precisely
    /// so that it cannot do either — a full-width card says [`stretch_width`](Self::stretch_width)
    /// and means it.
    fn measure(&self, constraints: Constraints, theme: &Theme<'_>) -> Size {
        let p = self.pad.resolve(theme);
        let (pw, ph) = (p.left + p.right, p.top + p.bottom);
        let inner = constraints.loosen().shrink(pw, ph);
        let child = match &self.child {
            None => Size::new(0, 0),
            Some(n) => {
                let mut cache = self.cache.borrow_mut();
                cache.begin_frame();
                layout::measure_node(n, 0, inner, theme, &mut cache)
            }
        };
        let w = if self.stretch_w { constraints.max_w } else { child.w + pw };
        constraints.constrain(Size::new(w, child.h + ph))
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        if rect.is_empty() {
            return;
        }
        let s = self.surface.resolve_on(theme, self.selected);
        if self.rounded {
            paint::band_round(c, rect, &s, theme.metrics.radius);
        } else {
            paint::band(c, rect, &s);
        }
        // The children are drawn onto the band this card just painted, not onto the page — so they
        // are told. Without it a card's label resolves `Ink::Text` against the page and comes out
        // white on white on `HIGH_CONTRAST`, where `text` and `chrome` are the same colour. That was
        // true before `Ground` existed and invisible only because the card itself was invisible.
        let inner = theme.on(if self.selected { Ground::Band } else { self.surface.ground() });
        self.placed(rect, theme, |child, cache| {
            layout::draw_node(child, 0, cache, c, &inner);
        });
    }

    /// Hand the key to the contents, at the rect they were drawn at.
    ///
    /// Without this a card would be a wall: `Widget::handle_key` defaults to `Ignored`, so every
    /// control inside one would be unreachable by any key on the phone. That is not hypothetical —
    /// it is the same gap `Group`'s own `Widget` impl was found to have, on the launcher's settings
    /// screen, where the first frame drew correctly and answered nothing at all.
    fn handle_key(&self, ev: KeyEvent, rect: Rect, cx: &mut KeyCtx<'_>) -> Handled {
        let mut out = Handled::Ignored;
        self.placed(rect, cx.theme, |child, cache| {
            out = layout::dispatch_key_node(child, 0, ev, cache, cx);
        });
        out
    }

    fn flex_weight(&self) -> i32 {
        self.weight
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::CrossAlign;
    use crate::widgets::{Column, Spacer, Text};
    use symbian_gfx::{Color, Size as GSize};
    use symbian_ui::{testing, Handled, Key, Palette};

    const W: i32 = 200;
    const H: i32 = 120;
    const BAND: Rect = Rect { x0: 0, y0: 0, x1: W, y1: H };

    /// A widget that fills its rect with one colour, so a test can see exactly which pixels the
    /// contents were given. `Fill`'s trick, borrowed from `stack.rs`.
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

    /// A fill with a size of its own, so a wrapping parent has something to wrap *to*.
    ///
    /// `Fill` cannot serve here: it answers with whatever it is offered, so a column that wrapped
    /// around it would still come out full width and the difference this file is asserting would be
    /// invisible. That is not a hypothetical — it is how the test below first passed.
    struct Block(Color, i32, i32);

    impl Widget for Block {
        fn content_hash(&self) -> WidgetHash {
            hash_i32(hash_i32(0, self.1), self.2)
        }
        fn measure(&self, c: Constraints, _t: &Theme<'_>) -> Size {
            c.constrain(Size::new(self.1, self.2))
        }
        fn draw(&self, c: &mut Canvas<'_>, rect: Rect, _t: &Theme<'_>) {
            c.fill_rect(rect, self.0);
        }
    }

    /// A widget that records the rect it was offered a key at, and takes it.
    struct Taker(Rc<core::cell::Cell<Option<Rect>>>);

    impl Widget for Taker {
        fn content_hash(&self) -> WidgetHash {
            hash_i32(0, 11)
        }
        fn measure(&self, c: Constraints, _t: &Theme<'_>) -> Size {
            c.constrain(Size::new(20, 10))
        }
        fn draw(&self, _c: &mut Canvas<'_>, _r: Rect, _t: &Theme<'_>) {}
        fn handle_key(&self, _ev: KeyEvent, rect: Rect, _cx: &mut KeyCtx<'_>) -> Handled {
            self.0.set(Some(rect));
            Handled::Consumed
        }
    }

    fn slots() -> SlotTable {
        let mut s = SlotTable::new();
        s.begin_frame();
        s
    }

    /// Draw `build`'s card over the whole band and hand back the framebuffer.
    fn painted(build: impl FnOnce(&mut SlotTable) -> Card) -> Vec<u16> {
        let mut table = slots();
        let card = build(&mut table);
        let (_, buf) = testing::with_canvas(GSize::new(W, H), |c| {
            testing::with_theme(Palette::DARK, |t| {
                c.clear(Palette::DARK.bg.mid());
                card.draw(c, BAND, t);
            });
        });
        buf
    }

    fn at(buf: &[u16], x: i32, y: i32) -> u16 {
        buf[(y * W + x) as usize]
    }

    #[test]
    fn the_contents_are_inset_by_the_padding_and_nothing_of_them_lands_outside_it() {
        // The one property a card has to have: a band around a child. A `Fill` takes everything it
        // is offered, so every pixel it painted is a pixel the card handed over — which makes the
        // inset measurable rather than eyeballed.
        let red = Color::hex(0xFF0000).to_rgb565().0;
        let buf = painted(|s| Card::new(s).child(Fill(Color::hex(0xFF0000))).stretch_width());
        let pad = testing::with_theme(Palette::DARK, |t| Pad::all(Gap::Wide).resolve(t));

        assert_eq!(at(&buf, pad.left, pad.top), red, "the contents did not reach their corner");
        assert_eq!(at(&buf, W - pad.right - 1, H - pad.bottom - 1), red);
        // And the band itself is still there on all four sides.
        for (x, y) in [(pad.left, pad.top - 1), (pad.left - 1, pad.top), (W - 1, H / 2), (W / 2, H - 1)] {
            assert_ne!(at(&buf, x, y), red, "the contents escaped at {x},{y}");
        }
    }

    #[test]
    fn the_test_above_would_notice_a_card_that_did_not_inset_at_all() {
        // The negative control, and it is needed: "red at (6,6)" is also true of a card that handed
        // the child the whole band, which is the exact bug being ruled out. A card with no padding
        // paints red into the corner the padded one must not.
        let red = Color::hex(0xFF0000).to_rgb565().0;
        let buf = painted(|s| Card::new(s).child(Fill(Color::hex(0xFF0000))).pad(0).stretch_width());
        assert_eq!(at(&buf, 0, 0), red, "with no padding the child does reach the corner");
        let padded = painted(|s| Card::new(s).child(Fill(Color::hex(0xFF0000))).stretch_width());
        assert_ne!(at(&padded, 0, 0), red);
    }

    #[test]
    fn a_card_paints_a_ground_that_is_not_the_page() {
        // A card whose band equalled the background would be padding with extra steps, and the
        // failure is invisible — the layout is right and the grouping simply is not there.
        let buf = painted(|s| Card::new(s).stretch_width());
        let bg = Palette::DARK.bg.mid().to_rgb565().0;
        assert!(buf.iter().any(|&p| p != bg), "the band is the page");
    }

    #[test]
    fn every_ground_reads_against_the_page_in_every_palette() {
        // Swept across all five for the reason `chrome::control_colors` was: a ground derived from
        // the page can be arithmetically distinct and practically invisible, and it is only
        // *visible* in one palette at a time. `HIGH_CONTRAST` is where a derivation from a
        // near-black page stops being a colour at all, which is why `Chrome` is the default.
        for (name, palette) in Palette::ALL {
            for surface in [SurfaceRole::Chrome, SurfaceRole::Raised, SurfaceRole::Sunken] {
                testing::with_theme(palette, |t| {
                    let s = surface.resolve(t);
                    assert_ne!(s.mid(), palette.bg.mid(), "{name} {surface:?}: the card is the page");
                });
            }
        }
    }

    #[test]
    fn a_square_card_fills_its_corners_and_a_rounded_one_does_not() {
        // The one visible difference between the two, asserted where it happens. Anywhere else on
        // the band they are the same pixels.
        let round = painted(|s| Card::new(s).stretch_width());
        let square = painted(|s| Card::new(s).square().stretch_width());
        let bg = Palette::DARK.bg.mid().to_rgb565().0;
        assert_eq!(at(&square, 0, 0), at(&square, W / 2, 0), "a square card has a straight edge");
        assert_ne!(at(&round, 0, 0), at(&square, 0, 0), "the corner was not cut");
        assert_eq!(at(&round, 0, 0), bg, "and what shows through a cut corner is the page");
    }

    #[test]
    fn a_card_wraps_its_contents_rather_than_claiming_the_band_it_was_offered() {
        // The `Avatar` defect, met here: a widget that measures from its offer without a reason is
        // a widget that eats the screen. A card is as big as what is in it plus its padding, and a
        // card that wants the whole width has to say so.
        testing::with_theme(Palette::DARK, |t| {
            let pad = Pad::all(Gap::Wide).resolve(t);
            let mut table = slots();
            let card = Card::new(&mut table).child(Spacer::new().width(40).height(10));
            let got = card.measure(Constraints::loose(W, H), t);
            assert_eq!(got, Size::new(40 + pad.left + pad.right, 10 + pad.top + pad.bottom));
            assert!(got.w < W && got.h < H, "it took the offer");

            let mut table = slots();
            let wide = Card::new(&mut table).child(Spacer::new().width(40).height(10)).stretch_width();
            assert_eq!(wide.measure(Constraints::loose(W, H), t).w, W);
        });
    }

    #[test]
    fn a_card_smaller_than_its_own_padding_is_empty_rather_than_inside_out() {
        // Reachable: a card in a column that ran out of room. An inverted rect draws as nothing at
        // all and reports nothing, which is the worst failure this layer can produce — so the
        // contents collapse to an empty rect at the card's origin instead of to `x1 < x0`.
        testing::with_theme(Palette::DARK, |t| {
            let mut table = slots();
            let card = Card::new(&mut table).child(Spacer::new().width(10).height(10));
            let inner = card.inner(Rect::from_xywh(0, 0, 4, 4), t);
            assert!(inner.x1 >= inner.x0 && inner.y1 >= inner.y0, "{inner:?} runs backwards");
            assert!(inner.is_empty());
        });
    }

    #[test]
    fn a_key_reaches_the_contents_at_the_rect_they_were_drawn_at() {
        // Without this a card is a wall, and it is the wall `Group`'s own `Widget` impl was found to
        // be on the launcher's settings screen: the frame draws correctly and answers nothing.
        // Asserted at the *inset* rect, because a key answered where the paint pass did not put the
        // child is the defect the crate's whole dispatch order exists to avoid.
        let seen = Rc::new(core::cell::Cell::new(None));
        let mut table = slots();
        let card = Card::new(&mut table).child(Taker(seen.clone())).stretch_width();
        let handled = testing::with_theme(Palette::DARK, |t| {
            let mut clip = symbian_ui::NoClipboard;
            let mut cx = KeyCtx::new(t, &mut clip);
            card.handle_key(KeyEvent::new(Key::Select), BAND, &mut cx)
        });
        assert_eq!(handled, Handled::Consumed);
        let pad = testing::with_theme(Palette::DARK, |t| Pad::all(Gap::Wide).resolve(t));
        let got = seen.get().expect("the key never reached the child");
        assert_eq!((got.x0, got.y0), (pad.left, pad.top));
    }

    #[test]
    fn an_empty_card_answers_no_keys_rather_than_claiming_them() {
        // A card with nothing in it is decoration, and decoration that swallowed an arrow would
        // stop the screen's cursor dead at whatever row the card sits on.
        let mut table = slots();
        let card = Card::new(&mut table).stretch_width();
        let handled = testing::with_theme(Palette::DARK, |t| {
            let mut clip = symbian_ui::NoClipboard;
            let mut cx = KeyCtx::new(t, &mut clip);
            card.handle_key(KeyEvent::new(Key::Down), BAND, &mut cx)
        });
        assert_eq!(handled, Handled::Ignored);
    }

    #[test]
    fn the_stretch_a_parent_applies_stops_at_the_card_and_the_child_says_so_itself() {
        // The trap `list_item.rs` paid for, on the other side of the fence. A card is a box, and a
        // box is where `CrossAlign::Stretch` stops — the parent's alignment does not reach through
        // it, so a column *inside* a card that wants its children full width has to say
        // `stretch_width` on its own account. Asserted rather than assumed, because the symptom is
        // a label hugging the left of a card it was supposed to fill and nothing else looking wrong.
        let red = Color::hex(0xFF0000).to_rgb565().0;
        let block = || Block(Color::hex(0xFF0000), 20, 10);
        let hugging = painted(|s| Card::new(s).group(Column::new().child(block())).stretch_width());
        let stretched = painted(|s| {
            Card::new(s)
                .group(Column::new().align(CrossAlign::Stretch).stretch_width().child(block()))
                .stretch_width()
        });
        let pad = testing::with_theme(Palette::DARK, |t| Pad::all(Gap::Wide).resolve(t));
        // The row the block occupies, which is the top of the card's contents in both cases.
        let y = pad.top + 2;
        assert_eq!(at(&stretched, W - pad.right - 1, y), red, "the stretched column did not fill it");
        assert_ne!(
            at(&hugging, W - pad.right - 1, y),
            red,
            "a wrapping column reached the far edge anyway"
        );
        // And the control that says both cards drew at all: the block is at the left edge of the
        // contents either way, so a blank canvas cannot pass this test.
        assert_eq!(at(&hugging, pad.left, y), red);
        assert_eq!(at(&stretched, pad.left, y), red);
    }

    #[test]
    fn the_digest_moves_with_everything_that_moves_an_edge_and_is_never_zero() {
        let base = || {
            let mut table = slots();
            Card::new(&mut table).child(Text::new("Ana")).content_hash()
        };
        assert_eq!(base(), base());
        assert_ne!(base(), 0, "a card that always re-measures re-measures its whole subtree");

        let mut table = slots();
        assert_ne!(base(), Card::new(&mut table).child(Text::new("Bea")).content_hash());
        let mut table = slots();
        assert_ne!(base(), Card::new(&mut table).child(Text::new("Ana")).pad(0).content_hash());
        let mut table = slots();
        assert_ne!(
            base(),
            Card::new(&mut table).child(Text::new("Ana")).surface(SurfaceRole::Sunken).content_hash()
        );
        let mut table = slots();
        assert_ne!(
            base(),
            Card::new(&mut table).child(Text::new("Ana")).stretch_width().content_hash()
        );
        let mut table = slots();
        assert_ne!(base(), Card::new(&mut table).content_hash(), "an empty card is a different box");
    }

    #[test]
    fn a_volatile_child_makes_the_card_volatile_too() {
        // `Group`'s rule, and it has to hold through a leaf as well: a hit on this card's digest
        // returns a cached size *and skips the subtree*, so a child asking to be re-measured every
        // frame would be silently overruled by the wrapper around it.
        struct Volatile;
        impl Widget for Volatile {
            fn measure(&self, c: Constraints, _t: &Theme<'_>) -> Size {
                c.constrain(Size::new(1, 1))
            }
            fn draw(&self, _c: &mut Canvas<'_>, _r: Rect, _t: &Theme<'_>) {}
        }
        let mut table = slots();
        assert_eq!(Card::new(&mut table).child(Volatile).content_hash(), 0);
    }

    #[test]
    fn a_card_is_visible_on_the_selection_band() {
        // The defect, measured before it was written down: on `HIGH_CONTRAST` a default card drew
        // **zero** pixels differing from the band, because `chrome` and `selection` are both white
        // there. Counting differing pixels rather than comparing to a named colour, for the reason
        // the meter's tests give — a fill that changes for a good reason should not turn this red.
        for (name, palette) in Palette::ALL {
            for role in [SurfaceRole::Chrome, SurfaceRole::Raised, SurfaceRole::Sunken] {
                let ground = palette.selection.mid();
                let mut slots = SlotTable::new();
                let card = Card::new(&mut slots).surface(role).selected(true).stretch_width();
                let (_, buf) = testing::with_canvas(Size::new(160, 60), |c| {
                    testing::with_theme(palette, |t| {
                        c.clear(ground);
                        card.draw(c, Rect { x0: 0, y0: 0, x1: 160, y1: 60 }, t);
                    });
                });
                let g = ground.to_rgb565().0;
                let differing = buf.iter().filter(|&&p| p != g).count();
                assert!(differing > 0, "{name} / {role:?}: the card vanished into the band");
            }
        }
    }
}
