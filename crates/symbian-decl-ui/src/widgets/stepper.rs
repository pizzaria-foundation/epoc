//! A bounded integer, as the `‹ N ›` block a settings row shows.
//!
//! # It is the block, not the row
//!
//! `symbian_ui::Stepper` draws a whole settings row: the selection band, the caption, and the value
//! against the right edge. That is the right shape for a hand-written screen and the wrong one here,
//! for the reason [`Switch`](super::Switch) gives — [`ListItem`](super::ListItem) already owns the
//! band, the label and the margins, and owns them with a parity test behind them.
//!
//! So this widget is only the block:
//!
//! ```ignore
//! ListItem::new("Retries")
//!     .selected(sel)
//!     .trailing(Stepper::new(model.retries, 0, 9).focused(sel).out(out.clone(), Msg::SetRetries))
//!     .build()
//! ```
//!
//! The geometry and the ink both come from [`symbian_ui::stepper`]'s free functions —
//! [`stepper_box`](symbian_ui::stepper::stepper_box) and
//! [`draw_stepper`](symbian_ui::stepper::draw_stepper) — extracted from `Stepper::draw` for this.
//! Reimplementing the `‹ N ›` formatting and the 46-pixel reservation here would have been a second
//! stepper, agreeing with the first on the day it was written.
//!
//! # The arithmetic is borrowed, not copied
//!
//! Clamping to `[min, max]`, tolerating reversed bounds, the wrap past the top: none of it is
//! written again below. [`handle_key`](Stepper::handle_key) builds a throwaway `symbian_ui::Stepper`
//! from the model's numbers, offers it the key, and reads the value back out. The copy is dropped
//! before the function returns, so nothing is mutated and there is exactly one implementation of
//! "what does Right do at the maximum" in the tree.
//!
//! That is worth more than it looks. The three interesting cases — `Left` at the floor, `Select`
//! wrapping, `min > max` — are each one line of arithmetic and each one a place where a second copy
//! would have chosen differently.
//!
//! # The value is the model's
//!
//! This widget cannot change it. `Left`/`Right`/`Select` push a message carrying the *new* value and
//! `update` puts it in the model, which is the rule the whole crate runs on. A widget that stepped
//! its own copy would show the new number for one frame and then be overwritten by a `view` built
//! from the old model — a stepper that springs back, which is the exact failure
//! [`Switch`](super::Switch) documents for booleans.
//!
//! That is a real difference from `symbian_ui::Stepper`, which owns its `i32` and steps it in place.
//! Both are right for their layer, and it is worth knowing before reaching for one expecting the
//! other.
//!
//! # Why the message is a function pointer and not a message
//!
//! [`Switch`](super::Switch) and [`Checkbox`](super::Checkbox) take `(Outbox<M>, M)`: a press means
//! one fixed thing. A stepper reports a *number*, so there is no one message it can hold — the
//! message depends on the key that arrived.
//!
//! The alternative the crate already has is [`Outbox::wrapped`]: `out.wrapped(Msg::SetRetries)`
//! yields an `Outbox<i32>`, and this widget could have taken one and pushed a bare `i32` into it.
//! That was rejected on cost, not on taste. `wrapped` boxes a closure and allocates an `Rc` per
//! call, and a `view` is rebuilt on every frame — so a settings screen with four steppers would
//! make eight heap allocations per frame, on a phone with 128 MB and a shim allocator we already
//! measure. `wrapped` is right for the thing it was built for, which is handing a whole *screen* a
//! channel once.
//!
//! A tuple-variant constructor coerces to `fn(i32) -> M`, so the call site reads exactly like
//! `Switch`'s — `.out(out.clone(), Msg::SetRetries)` — and what the widget holds is a `Copy`
//! function pointer, no allocation and no closure. The rule this crate states as "widgets hold
//! values, not closures" is kept: a `fn` item *is* a value.
//!
//! # No `Gap` appears below
//!
//! Deliberately, and worth saying because the crate's rule is that spacing is named. This widget has
//! no spacing of its own: [`STEPPER_W`](symbian_ui::stepper::STEPPER_W) is a *shape* — the width
//! reserved so a caption does not shuffle sideways when a count crosses ten, exactly as `SWITCH_W`
//! is a shape — and the distance between the block and the label belongs to the row that holds both.

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::{stepper as ui, Handled, KeyEvent, Theme};

use crate::constraints::Constraints;
use crate::outbox::Outbox;
use crate::widget::{hash_str, KeyCtx, Widget, WidgetHash};

/// Where a new value goes, and how to name it once it gets there.
///
/// An alias because the pair is spelled twice — the field and [`Stepper::out`] — and because
/// `Option<(Outbox<M>, fn(i32) -> M)>` in a struct definition reads as machinery rather than as the
/// one thing it is: a channel and the message it carries.
type Report<M> = (Outbox<M>, fn(i32) -> M);

/// A bounded integer picker that reports new values and owns nothing.
pub struct Stepper<M> {
    /// The model's value and bounds, already normalised by `symbian_ui::Stepper::new` — so an
    /// out-of-range number from the model is clamped once, here, rather than at every read.
    inner: ui::Stepper,
    focused: bool,
    /// Where a new value goes, and how to say it. Both or neither, for the reason
    /// [`Switch::out`](super::Switch::out) gives: a message with nowhere to go was a real defect.
    out: Option<Report<M>>,
}

impl<M> Stepper<M> {
    /// A stepper showing `value`, bounded to `[min, max]` inclusive.
    ///
    /// Reversed bounds are tolerated rather than panicked on, because that is what
    /// `symbian_ui::Stepper::new` does and this delegates to it — and because a `view` runs on a
    /// phone whose entire failure report is a dialog with a number in it.
    pub fn new(value: i32, min: i32, max: i32) -> Self {
        Self { inner: ui::Stepper::new(value, min, max), focused: false, out: None }
    }

    /// Whether this stepper has the cursor. Only a focused stepper answers a key.
    pub fn focused(mut self, on: bool) -> Self {
        self.focused = on;
        self
    }

    /// What a step means, and where the message goes.
    ///
    /// `msg` receives the *new* value, already clamped. See the module docs on why this is a
    /// function pointer rather than a message or an `Outbox<i32>`.
    pub fn out(mut self, out: Outbox<M>, msg: fn(i32) -> M) -> Self {
        self.out = Some((out, msg));
        self
    }

    /// What it is showing — the model's value after clamping, never a value it stepped itself.
    pub fn value(&self) -> i32 {
        self.inner.value()
    }
}

impl<M: 'static> Widget for Stepper<M> {
    fn focus_state(&self) -> Option<bool> {
        Some(self.focused)
    }

    fn content_hash(&self) -> WidgetHash {
        // A constant, and every field left out on purpose.
        //
        // `value` is out because the block does not resize with it: the whole point of
        // `STEPPER_W` being fixed is that "9" and "10" occupy the same 46 pixels, so a caption
        // never shuffles sideways. Folding the value in would re-measure the row on every press to
        // produce the same number — which is the cost this digest exists to avoid.
        //
        // `min`/`max` are out because all they do is clamp `value`, and `focused` is out because it
        // chooses a colour. What *would* change the size is the band this is measured in, and that
        // arrives as the offer, which the cache already keys on.
        //
        // Not zero: zero means "re-measure me every frame", and a zero here would put every settings
        // row containing a stepper — and, through `Group::content_hash`, the whole screen above it —
        // on the slow path for ever.
        hash_str(0, "stepper")
    }

    fn measure(&self, constraints: Constraints, theme: &Theme<'_>) -> Size {
        // One line tall, not the band. `draw_text_in` centres in whatever rect it is handed, so a
        // stepper that reported the band's height would still *look* right and would have lied to
        // every row that asked it how big it is — and to every alignment computed from that answer.
        let h = ui::stepper_height(constraints.max_h, theme);
        constraints.constrain(Size::new(ui::STEPPER_W, h))
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        // `stepper_box` against its own rect: the rect is already block-width, so this is the
        // vertical centring — which matters because `CrossAlign::Stretch` on a list row hands this
        // widget the whole 38-pixel band and not the 17 pixels it measured.
        ui::draw_stepper(c, ui::stepper_box(rect, theme), theme, self.inner.value(), self.focused);
    }

    fn handle_key(&self, ev: KeyEvent, _rect: Rect, _cx: &mut KeyCtx<'_>) -> Handled {
        if !self.focused {
            // Two steppers on one screen and one press: without the flag both would fire.
            return Handled::Ignored;
        }
        // A throwaway copy of the imperative widget does the arithmetic and is dropped here. `Up`,
        // `Down` and everything else come back `Ignored` from it, which is what lets the enclosing
        // `FocusScope` keep its navigation — a control that consumed `Down` would trap the cursor on
        // the one row nobody can get past.
        let mut probe = self.inner;
        let handled = probe.handle_key(ev);
        if handled == Handled::Consumed && probe.value() != self.inner.value() {
            // Only on an actual change. `Left` at the floor is still `Consumed` — that is the
            // imperative widget's answer and the cursor must not slide sideways off a stepper that
            // is merely at its limit — but a message saying "set it to what it already is" would be
            // a redraw and an `update` per keypress for nothing.
            if let Some((out, msg)) = &self.out {
                out.push(msg(probe.value()));
            }
        }
        handled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::with_key_ctx;
    use symbian_ui::{testing, Key, Palette};

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Msg {
        SetRetries(i32),
    }

    /// A list row's band: full block width, and 38 pixels tall — the `Stretch` a row applies.
    const ROW: Rect = Rect { x0: 0, y0: 0, x1: ui::STEPPER_W, y1: 38 };

    fn press(s: &Stepper<Msg>, key: Key) -> Handled {
        testing::with_theme(Palette::DARK, |_t| {
            with_key_ctx(|cx| s.handle_key(KeyEvent::new(key), ROW, cx))
        })
    }

    fn wired(value: i32, min: i32, max: i32) -> (Outbox<Msg>, Stepper<Msg>) {
        let out = Outbox::new();
        let s = Stepper::new(value, min, max).focused(true).out(out.clone(), Msg::SetRetries);
        (out, s)
    }

    /// The *real* device atlases, not the one-glyph test atlas.
    ///
    /// `testing::with_theme` loads an atlas containing exactly one glyph — lowercase 'a' — so
    /// `‹ N ›` paints nothing at all under it and every pixel assertion about this widget would pass
    /// whatever `draw` did. A stepper is made entirely of characters that atlas has not got, which
    /// makes it the one widget in the catalogue whose pixels cannot be tested the cheap way.
    fn with_real_theme<R>(f: impl FnOnce(&Theme<'_>) -> R) -> R {
        let atlases = symbian_preview::Atlases::load();
        atlases.with_fonts(|fonts| f(&symbian_ui::Theme::dark(fonts)))
    }

    /// Paint one stepper over the row band and hand back the buffer.
    fn paint(t: &Theme<'_>, value: i32, focused: bool) -> alloc::vec::Vec<u16> {
        let (_, buf) = testing::with_canvas(Size::new(ui::STEPPER_W, 38), |c| {
            c.clear(t.palette.bg.mid());
            Stepper::<Msg>::new(value, 0, 99).focused(focused).draw(c, ROW, t);
        });
        buf
    }

    /// Which rows of `buf` have any ink in them.
    fn inked_rows(t: &Theme<'_>, buf: &[u16]) -> alloc::vec::Vec<i32> {
        let bg = t.palette.bg.mid().to_rgb565().0;
        let w = ui::STEPPER_W;
        (0..38).filter(|&y| (0..w).any(|x| buf[(y * w + x) as usize] != bg)).collect()
    }

    #[test]
    fn a_focused_stepper_reports_the_new_value_and_does_not_step_its_own() {
        // The rule the whole crate runs on. A widget that stepped its own copy would show 4 for one
        // frame and then be overwritten by a `view` built from a model that still says 3.
        let (out, s) = wired(3, 0, 9);
        assert_eq!(press(&s, Key::Right), Handled::Consumed);
        assert_eq!(out.take(), alloc::vec![Msg::SetRetries(4)]);
        assert_eq!(s.value(), 3, "it still shows what the model said");
    }

    #[test]
    fn left_reports_one_less_and_right_one_more() {
        let (out, s) = wired(5, 0, 9);
        press(&s, Key::Left);
        assert_eq!(out.take(), alloc::vec![Msg::SetRetries(4)]);
        press(&s, Key::Right);
        assert_eq!(out.take(), alloc::vec![Msg::SetRetries(6)]);
    }

    #[test]
    fn an_unfocused_stepper_answers_nothing() {
        // Two steppers on one settings screen and one press: without the flag both would fire, and
        // the model would end up with the second one's value.
        let out = Outbox::new();
        let s = Stepper::new(3, 0, 9).out(out.clone(), Msg::SetRetries);
        for key in [Key::Left, Key::Right, Key::Select] {
            assert_eq!(press(&s, key), Handled::Ignored, "{key:?}");
        }
        assert!(out.is_empty());
    }

    #[test]
    fn a_stepper_never_takes_a_navigation_key() {
        // What keeps the cursor able to leave it. A stepper that consumed `Down` would trap the
        // focus on the one row nobody can get past — and `Up`/`Down` are precisely the keys the
        // enclosing `FocusScope` needs, because Left/Right are the ones it gives away to controls
        // like this one.
        let (out, s) = wired(3, 0, 9);
        for key in [Key::Up, Key::Down, Key::Backspace] {
            assert_eq!(press(&s, key), Handled::Ignored, "{key:?}");
        }
        assert!(out.is_empty());
    }

    #[test]
    fn a_step_past_a_bound_is_swallowed_rather_than_reported_or_handed_back() {
        // Two properties in one, and both matter. `Consumed`, because a stepper sitting at its
        // minimum must not let `Left` slide the cursor onto the neighbouring control — that is the
        // imperative widget's answer and the row's navigation is built on it. And *no message*,
        // because "set it to 0" when it is already 0 is an `update` and a repaint per keypress for a
        // value that did not move.
        let (out, s) = wired(0, 0, 9);
        assert_eq!(press(&s, Key::Left), Handled::Consumed);
        assert!(out.is_empty(), "nothing changed, so nothing was reported");

        let (out, s) = wired(9, 0, 9);
        assert_eq!(press(&s, Key::Right), Handled::Consumed);
        assert!(out.is_empty());
    }

    #[test]
    fn select_wraps_past_the_top_because_a_tab_strip_may_own_left_and_right() {
        // Inherited from `symbian_ui::Stepper` rather than chosen again: a stepper on a tabbed
        // screen never sees Left or Right, because the tab strip consumed them, and Select is the
        // only key it can be driven with. Wrapping is what makes that usable.
        let (out, s) = wired(9, 0, 9);
        assert_eq!(press(&s, Key::Select), Handled::Consumed);
        assert_eq!(out.take(), alloc::vec![Msg::SetRetries(0)]);

        let (out, s) = wired(3, 0, 9);
        press(&s, Key::Select);
        assert_eq!(out.take(), alloc::vec![Msg::SetRetries(4)]);
    }

    #[test]
    fn a_single_valued_stepper_reports_nothing_at_all() {
        // `min == max` is reachable from a model — a retry count whose only legal value is one — and
        // the wrap arithmetic maps max back to min, which is the same number. Without the
        // did-it-change guard this would push a message on every Select for ever.
        let (out, s) = wired(4, 4, 4);
        for key in [Key::Left, Key::Right, Key::Select] {
            assert_eq!(press(&s, key), Handled::Consumed, "{key:?}");
        }
        assert!(out.is_empty());
    }

    #[test]
    fn a_value_the_model_cannot_justify_is_clamped_before_it_is_ever_drawn() {
        // Clamped once, in the constructor, by the imperative widget's own `new` — so a model that
        // says 99 out of a range of 0..9 draws 9 rather than painting a number the user cannot
        // reach with any key. Reversed bounds are tolerated for the same reason: a panic in `view`
        // is a dead application on a phone with no console.
        assert_eq!(Stepper::<Msg>::new(99, 0, 9).value(), 9);
        assert_eq!(Stepper::<Msg>::new(-4, 0, 9).value(), 0);
        assert_eq!(Stepper::<Msg>::new(3, 9, 0).value(), 3, "reversed bounds are tolerated");
        assert_eq!(Stepper::<Msg>::new(99, 9, 0).value(), 9);
    }

    #[test]
    fn a_clamped_value_still_steps_from_where_it_is_drawn_and_not_from_where_it_came() {
        // The consequence of clamping in the constructor: a model holding 99 shows 9, and Right on
        // it must report 9 — not 100. Stepping from the raw number would push a value that fails
        // the model's own invariant on the very next frame.
        let (out, s) = wired(99, 0, 9);
        assert_eq!(press(&s, Key::Right), Handled::Consumed);
        assert!(out.is_empty(), "it is already at the top it was clamped to");
        let (out, s) = wired(99, 0, 9);
        press(&s, Key::Left);
        assert_eq!(out.take(), alloc::vec![Msg::SetRetries(8)]);
    }

    #[test]
    fn a_stepper_with_nowhere_to_send_still_consumes_the_key() {
        // `Switch`'s note applies unchanged: handing the press back because the caller forgot the
        // channel would move the cursor instead, which reads as a stepper that navigates.
        let s = Stepper::<Msg>::new(3, 0, 9).focused(true);
        assert_eq!(press(&s, Key::Right), Handled::Consumed);
    }

    #[test]
    fn it_measures_the_block_the_imperative_row_draws() {
        // Pinned to `symbian_ui`'s own functions rather than to numbers, so the two cannot drift.
        testing::with_theme(Palette::DARK, |t| {
            let got = Stepper::<Msg>::new(3, 0, 9).measure(Constraints::loose(320, 38), t);
            assert_eq!(got, Size::new(ui::STEPPER_W, ui::stepper_height(38, t)));
            // And it is a line, not the band it was offered — the assertion the `Stretch` trap
            // turns on. A block that returned 38 would have been believed.
            assert_eq!(got.h, t.fonts.body.line_height());
            assert!(got.h < 38);
        });
    }

    #[test]
    fn the_digest_is_constant_and_not_zero() {
        // Constant because the block is the same 46 pixels whatever number is in it — that is what
        // `STEPPER_W` being fixed buys. Not zero, because zero means "re-measure me every frame",
        // which would put every settings row holding a stepper on the slow path.
        let a = Stepper::<Msg>::new(3, 0, 9);
        let b = Stepper::<Msg>::new(9, -100, 100).focused(true);
        assert_eq!(a.content_hash(), b.content_hash());
        assert_ne!(a.content_hash(), 0);
    }

    #[test]
    fn the_real_atlas_paints_the_value_so_the_pixel_tests_below_can_fail() {
        // The negative control, and this widget needs one more than most: under the one-glyph test
        // atlas `‹ N ›` has no renderable characters at all, so every pixel assertion here would be
        // vacuously true. This is the test that says the buffers are real ink and that the value
        // reaches the canvas.
        with_real_theme(|t| {
            let bg = t.palette.bg.mid().to_rgb565().0;
            let three = paint(t, 3, false);
            assert!(three.iter().any(|&p| p != bg), "nothing was painted at all");
            assert_ne!(three, paint(t, 8, false), "the digit does not reach the canvas");
            assert_ne!(three, paint(t, 3, true), "focus does not change the ink");
        });
    }

    #[test]
    fn the_stretch_a_list_row_applies_does_not_stretch_the_block() {
        // `CrossAlign::Stretch` hands this widget the whole 38-pixel band, not the 17 it measured.
        // `stepper_box` is what keeps the ink inside the line it claimed — and the reason this is
        // asserted as containment rather than as "the ink is centred" is that `draw_text_in` centres
        // in whatever rect it is given, so a stepper drawn straight into the band would land in the
        // same place while still reporting the wrong size. Containment is the property that survives
        // both.
        with_real_theme(|t| {
            let rows = inked_rows(t, &paint(t, 3, false));
            let slot = ui::stepper_box(ROW, t);
            assert!(!rows.is_empty());
            assert!(
                rows.iter().all(|&y| y >= slot.y0 && y < slot.y1),
                "ink at rows {rows:?} escaped the slot {slot:?}"
            );
            // And the slot really is smaller than the band, or the containment above proves nothing.
            assert!(slot.height() < ROW.height());

            // The other half of the control: that `inked_rows` tracks the geometry at all. Hand the
            // widget a band six pixels lower and every inked row moves six pixels with it — so the
            // containment above is a measurement and not a coincidence of a full canvas.
            let lower = Rect { y0: ROW.y0 + 6, y1: ROW.y1, ..ROW };
            let (_, buf) = testing::with_canvas(Size::new(ui::STEPPER_W, 38), |c| {
                c.clear(t.palette.bg.mid());
                Stepper::<Msg>::new(3, 0, 99).draw(c, lower, t);
            });
            let moved = inked_rows(t, &buf);
            assert_eq!(moved, rows.iter().map(|y| y + 3).collect::<alloc::vec::Vec<_>>());
        });
    }

    #[test]
    fn it_draws_the_same_block_the_imperative_row_draws() {
        // Parity, and cheap because both go through `draw_stepper`. Worth asserting anyway: the
        // point of extracting the primitive was that these two can never be two steppers, and a
        // test is what stops a future edit putting the formatting back into one of them.
        with_real_theme(|t| {
            let (_, theirs) = testing::with_canvas(Size::new(ui::STEPPER_W, 38), |c| {
                c.clear(t.palette.bg.mid());
                ui::draw_stepper(c, ui::stepper_box(ROW, t), t, 7, true);
            });
            assert_eq!(paint(t, 7, true), theirs);
        });
    }

    #[test]
    fn the_focused_colour_is_the_one_a_selection_band_needs() {
        // Not a colour assertion — the palette owns that — but a "the flag is wired to the ink"
        // assertion, in both palettes, because a stepper whose focused ink equalled its resting ink
        // would be invisible on top of the selection band that `ListItem` paints under it.
        for (name, palette) in Palette::ALL {
            let atlases = symbian_preview::Atlases::load();
            atlases.with_fonts(|fonts| {
                let t = symbian_ui::Theme::new(palette, fonts);
                assert_ne!(paint(&t, 3, false), paint(&t, 3, true), "{name}");
            });
        }
    }
}
