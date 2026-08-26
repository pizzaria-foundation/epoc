//! A bounded quantity as a track you slide along.
//!
//! [`Stepper`](super::Stepper) is the other answer and the right one for a *count* — four retries,
//! nine days, where the number is the content. A slider is for a quantity nobody reads exactly:
//! volume, brightness, a timeout. The useful information is "about a third along", and the number, if
//! it is shown at all, is confirmation.
//!
//! ```ignore
//! ListItem::new("Volume")
//!     .selected(sel)
//!     .trailing(Text::new(format!("{}%", m.volume)).font(FontRole::Small))
//!     .build()
//! // and the track on the line below, or as the row's own control:
//! Slider::new(m.volume, 0, 100).step(5).focused(sel).out(m.out.clone(), Msg::SetVolume)
//! ```
//!
//! # This file contains no arithmetic
//!
//! All of it is in [`symbian_ui::slider`], pure and with eighteen tests: the stepping, the clamping,
//! the rounding of the fill, the collapse of a backwards range, the 64-bit multiply that a wide range
//! would otherwise overflow. What lives here is the shell — where the value comes from, where the
//! message goes, and how a fixed-height track survives being handed a 38-pixel band.
//!
//! # An arrow at the end is consumed
//!
//! `Left` at the minimum moves nothing and **is still taken**. That is the decision
//! [`symbian_ui::slider`]'s own docs argue, matching `ListState`, `GridState` and `EdgePolicy::Stop`:
//! an arrow that falls through only at the ends is an arrow whose meaning depends on the value, and a
//! user experiences that as the phone being broken rather than as a boundary.
//!
//! It costs one thing, and the cost is worth knowing: inside a **horizontal**
//! [`FocusScope`](super::FocusScope) a slider holds the cursor, because the scope never sees the
//! arrow that would have moved past it. The fix is the scope's `EdgePolicy`, not the slider's — a
//! slider in a row of controls is a layout that has to say what its edges mean.
//!
//! # The value is the model's, and the message is a function pointer
//!
//! The widget cannot change the value. `Left`/`Right` push the *new value* and `update` writes it.
//!
//! Which raises the question of how a widget that reports a number reaches a `Msg` that carries one.
//! The first draft here took an `Outbox<i32>` and expected the caller to say
//! `m.out.wrapped(Msg::SetVolume)`. [`Outbox::wrapped`] does exist for this, and it is the wrong tool
//! here: it allocates an `Rc` **and** boxes a closure on every call, and the call would be in `view`,
//! which is rebuilt every frame. A settings screen with four sliders and steppers would make eight
//! heap allocations a frame on a phone with 128 MB.
//!
//! So it takes `fn(i32) -> M` — a tuple-variant constructor coerces to one, and a `fn` pointer is
//! `Copy` and allocation-free. `.out(out.clone(), Msg::SetVolume)` reads the same as
//! [`Switch`](super::Switch)'s and keeps the type honestly `Slider<M>` instead of collapsing it to a
//! slider that only speaks in integers. The crate's rule — widgets hold values, not closures — is
//! kept: a `fn` item is a value.
//!
//! [`Stepper`](super::Stepper) made the same call for the same reason. Two shapes for one idea is the
//! thing this catalogue exists to avoid, so there is one.

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::slider::{self, Slid};
use symbian_ui::{Handled, KeyEvent, Theme};

use crate::constraints::Constraints;
use crate::outbox::Outbox;
use crate::widget::{hash_i32, hash_str, KeyCtx, Widget, WidgetHash};

/// How much of the track is filled, and by how much an arrow moves it.
pub struct Slider<M> {
    value: i32,
    min: i32,
    max: i32,
    by: i32,
    focused: bool,
    /// This slider's share of its parent's leftover space. `0` means it takes
    /// [`SLIDER_W`](symbian_ui::slider::SLIDER_W) and leaves the rest of the row alone.
    flex: i32,
    /// Where the new value goes, and how a number becomes a message. See the module docs on why this
    /// is a `fn` pointer rather than an `Outbox<i32>`.
    out: Report<M>,
}

/// Where a new value goes, and how a number becomes a message.
///
/// Named because the tuple appears in the struct, in the builder and in the dispatch, and a reader
/// meeting `Option<(Outbox<M>, fn(i32) -> M)>` three times has to re-read it three times.
type Report<M> = Option<(Outbox<M>, fn(i32) -> M)>;

impl<M: Clone> Slider<M> {
    /// A slider showing `value` within `min..=max`.
    ///
    /// A backwards range does not panic — see [`symbian_ui::slider::step`]. Two model fields in the
    /// wrong order should show *a* value, because a panic in a key handler on this device reports as a
    /// dialog with a number in it.
    pub fn new(value: i32, min: i32, max: i32) -> Self {
        Self { value, min, max, by: 1, focused: false, flex: 0, out: None }
    }

    /// How far one arrow moves it. `1` by default; `0` and negatives are read as `1`, because a
    /// slider that cannot move is a label with extra machinery.
    pub fn step(mut self, by: i32) -> Self {
        self.by = by;
        self
    }

    /// Whether this slider has the cursor. Only a focused one answers a key or draws its accent fill.
    pub fn focused(mut self, on: bool) -> Self {
        self.focused = on;
        self
    }

    /// Take a share of the parent's leftover space instead of a fixed width.
    ///
    /// A slider on its own line wants `flex(1)`; a slider at the end of a labelled row wants the
    /// default. Getting that backwards is what the first version did *by default*, and the symptom
    /// was a row whose label had disappeared — see [`SLIDER_W`](symbian_ui::slider::SLIDER_W).
    pub fn flex(mut self, weight: i32) -> Self {
        self.flex = weight.max(0);
        self
    }

    /// Where the new value goes, and which message carries it.
    ///
    /// `msg` is usually a tuple-variant constructor — `.out(out.clone(), Msg::SetVolume)` — which
    /// coerces to a `fn` pointer.
    pub fn out(mut self, out: Outbox<M>, msg: fn(i32) -> M) -> Self {
        self.out = Some((out, msg));
        self
    }

    pub fn value(&self) -> i32 {
        self.value
    }
}

impl<M: Clone + 'static> Widget for Slider<M> {
    fn focus_state(&self) -> Option<bool> {
        Some(self.focused)
    }

    fn content_hash(&self) -> WidgetHash {
        // Constant, and not zero. A slider is the same box at every value — what changes is which
        // part of it is filled, and the box is a function of the offer, which the cache already keys
        // on. Folding the value in would re-measure the row on every press of Right to produce the
        // same number; returning zero would put the row on the slow path for ever.
        hash_i32(hash_str(0, "slider"), 0)
    }

    fn measure(&self, constraints: Constraints, theme: &Theme<'_>) -> Size {
        let h = slider::track_height(constraints.max_h, theme);
        // `SLIDER_W` unless it was told to fill. Returning the whole offer unconditionally is what the
        // first version did, and it ate the label off every row it sat in: the layout's first pass
        // offers every fixed child the whole line, so a greedy fixed child leaves nothing for the
        // flexible ones.
        let w = if self.flex > 0 { constraints.max_w } else { slider::SLIDER_W };
        constraints.constrain(Size::new(w, h))
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        // Through `track` and not into `rect`: `CrossAlign::Stretch` on a list row hands this the
        // whole 38-pixel band, and a track drawn into that is a progress bar.
        slider::draw(
            c,
            slider::track(rect, theme),
            theme,
            self.value,
            self.min,
            self.max,
            self.focused,
        );
    }

    fn flex_weight(&self) -> i32 {
        self.flex
    }

    fn handle_key(&self, ev: KeyEvent, _rect: Rect, _cx: &mut KeyCtx<'_>) -> Handled {
        if !self.focused {
            return Handled::Ignored;
        }
        let slid = slider::handle_key(ev, self.value, self.min, self.max, self.by);
        if let (Slid::To(next), Some((out, msg))) = (slid, &self.out) {
            // Only on an actual move. Pushing on `Clamped` would send `update` the value it already
            // has on every press against the end — a stream of no-op writes that a screen watching
            // for changes would read as changes.
            out.push(msg(next));
        }
        // `Clamped` is still consumed. See the module docs.
        slider::consumed(slid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::with_key_ctx;
    use symbian_ui::{testing, Key, Palette};

    /// What the test screen does with a new value. A tuple variant, so it coerces to `fn(i32) -> Msg`.
    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Msg {
        SetVolume(i32),
    }

    const ROW: Rect = Rect { x0: 0, y0: 0, x1: 120, y1: 38 };

    fn press(s: &Slider<Msg>, key: Key) -> Handled {
        testing::with_theme(Palette::DARK, |_t| {
            with_key_ctx(|cx| s.handle_key(KeyEvent::new(key), ROW, cx))
        })
    }

    fn paint(s: &Slider<Msg>) -> Vec<u16> {
        let (_, buf) = testing::with_canvas(Size::new(120, 38), |c| {
            testing::with_theme(Palette::DARK, |t| {
                c.clear(Palette::DARK.bg.mid());
                s.draw(c, ROW, t);
            });
        });
        buf
    }

    #[test]
    fn a_focused_slider_reports_the_new_value_and_does_not_change_its_own() {
        let out: Outbox<Msg> = Outbox::new();
        let s = Slider::new(40, 0, 100).step(5).focused(true).out(out.clone(), Msg::SetVolume);
        assert_eq!(press(&s, Key::Right), Handled::Consumed);
        assert_eq!(out.take(), alloc::vec![Msg::SetVolume(45)]);
        assert_eq!(s.value(), 40, "it still shows what the model said");
    }

    #[test]
    fn it_reports_the_value_and_not_the_delta() {
        // A delta would make two presses arriving in one frame add up wrong, and would make `update`
        // responsible for the clamping this widget already did.
        let out: Outbox<Msg> = Outbox::new();
        let s = Slider::new(98, 0, 100).step(5).focused(true).out(out.clone(), Msg::SetVolume);
        press(&s, Key::Right);
        assert_eq!(out.take(), alloc::vec![Msg::SetVolume(100)], "the end, not 103");
    }

    #[test]
    fn an_arrow_at_the_end_is_consumed_and_reports_nothing() {
        // Two halves, both deliberate. Consumed, because an arrow that falls through only at the ends
        // has a meaning that depends on the value. Nothing reported, because pushing the value it
        // already has would be a no-op write that a screen watching for changes reads as a change.
        let out: Outbox<Msg> = Outbox::new();
        let s = Slider::new(100, 0, 100).step(5).focused(true).out(out.clone(), Msg::SetVolume);
        assert_eq!(press(&s, Key::Right), Handled::Consumed);
        assert!(out.is_empty());

        let s = Slider::new(0, 0, 100).step(5).focused(true).out(out.clone(), Msg::SetVolume);
        assert_eq!(press(&s, Key::Left), Handled::Consumed);
        assert!(out.is_empty());
    }

    #[test]
    fn an_unfocused_slider_answers_nothing() {
        let out: Outbox<Msg> = Outbox::new();
        let s = Slider::new(40, 0, 100).out(out.clone(), Msg::SetVolume);
        assert_eq!(press(&s, Key::Right), Handled::Ignored);
        assert!(out.is_empty());
    }

    #[test]
    fn a_slider_never_takes_a_vertical_key() {
        // What keeps a form navigable: a slider that consumed `Down` would be the one field the
        // cursor cannot get past.
        //
        // `Select` is **not** in this list any more, and that is the change rather than an oversight.
        // A tab strip takes Left and Right before the panel under it sees them, so on a tabbed screen
        // a slider with no centre-key fallback was drivable by no key at all — `Stepper` had had that
        // fallback since it was written. See `symbian_ui::slider::handle_key`.
        let out: Outbox<Msg> = Outbox::new();
        let s = Slider::new(40, 0, 100).focused(true).out(out.clone(), Msg::SetVolume);
        for key in [Key::Up, Key::Down, Key::Backspace] {
            assert_eq!(press(&s, key), Handled::Ignored, "{key:?}");
        }
        assert!(out.is_empty());
    }

    #[test]
    fn the_centre_key_steps_it_when_the_arrows_are_taken() {
        let out: Outbox<Msg> = Outbox::new();
        let s = Slider::new(40, 0, 100).step(5).focused(true).out(out.clone(), Msg::SetVolume);
        assert_eq!(press(&s, Key::Select), Handled::Consumed);
        assert_eq!(out.take(), alloc::vec![Msg::SetVolume(45)]);

        // And at the top it wraps, which is the only way one key reaches a lower value.
        let s = Slider::new(100, 0, 100).step(5).focused(true).out(out.clone(), Msg::SetVolume);
        press(&s, Key::Select);
        assert_eq!(out.take(), alloc::vec![Msg::SetVolume(0)]);
    }

    #[test]
    fn a_plain_slider_takes_its_own_width_and_leaves_the_rest_of_the_row_alone() {
        // The defect the gallery found. The layout's first pass offers *every* fixed child the whole
        // line, so a slider that answered with the offer took all of it and the label beside it —
        // flexing for the leftover — got nothing. The row rendered as a bare track with no idea what
        // it was for.
        testing::with_theme(Palette::DARK, |t| {
            let got = Slider::<Msg>::new(40, 0, 100).measure(Constraints::loose(120, 38), t);
            assert_eq!(got, Size::new(slider::SLIDER_W, slider::track_height(38, t)));
            // And never wider than the offer, however small it is.
            assert_eq!(Slider::<Msg>::new(40, 0, 100).measure(Constraints::loose(40, 38), t).w, 40);
        });
    }

    #[test]
    fn a_flexed_slider_takes_the_line() {
        // What a slider on its own line wants, and what it now has to ask for.
        testing::with_theme(Palette::DARK, |t| {
            let got = Slider::<Msg>::new(40, 0, 100).flex(1).measure(Constraints::loose(120, 38), t);
            assert_eq!(got.w, 120);
            assert_eq!(Slider::<Msg>::new(40, 0, 100).flex(1).flex_weight(), 1);
            assert_eq!(Slider::<Msg>::new(40, 0, 100).flex_weight(), 0);
        });
    }

    #[test]
    fn a_label_beside_a_slider_keeps_its_room() {
        // The row from the gallery, asserted end to end: the label flexes, the slider does not, and
        // the label's rect is most of the line rather than nothing. This is the regression test for
        // the defect, at the level the defect appeared at.
        use crate::widgets::{ListItem, Node};
        let root = ListItem::new("Volume")
            .trailing_node(Node::leaf(Slider::<Msg>::new(40, 0, 100)))
            .build();
        let label = testing::with_theme(Palette::DARK, |theme| {
            let mut cache = crate::UiCache::with_capacity(root.slot_count());
            crate::layout::place_frame(&root, Rect { x0: 0, y0: 0, x1: 320, y1: 38 }, &mut cache, theme);
            cache.rect(2).expect("the label was placed")
        });
        assert!(label.width() > 200, "the label got {}px of 320", label.width());
    }

    #[test]
    fn the_stretch_a_list_row_applies_does_not_fatten_the_track() {
        // The trap every control in this catalogue shares: the row hands over 38 pixels.
        let buf = paint(&Slider::<Msg>::new(50, 0, 100));
        let bg = Palette::DARK.bg.mid().to_rgb565().0;
        let rows: Vec<i32> =
            (0..38).filter(|&y| (0..120).any(|x| buf[(y * 120 + x) as usize] != bg)).collect();
        let h = testing::with_theme(Palette::DARK, |t| slider::track_height(38, t));
        assert_eq!(rows.len() as i32, h, "the track is its own height, not the band's");
        assert_eq!(rows[0], (38 - h) / 2, "and centred in it");
    }

    #[test]
    fn more_value_is_more_fill() {
        // Counting the *fill colour*, not ink: the track is painted end to end whatever the value, so
        // a total-ink count is identical at 0 and at 100 and only the colour of part of it changes.
        // The first version of the equivalent test in `symbian_ui::slider` counted ink and failed for
        // exactly that reason.
        // Counted as "pixels that are not the track", not as "pixels of the accent". The fill's colour
        // is the band-aware ink now — `chrome::control_colors` — so a test naming `accent` was
        // measuring one particular answer rather than the property, and it went red the moment the
        // answer changed for a good reason.
        let filled = |v: i32| {
            let buf = paint(&Slider::<Msg>::new(v, 0, 100).focused(true));
            let empty = paint(&Slider::<Msg>::new(0, 0, 100).focused(true));
            buf.iter().zip(empty.iter()).filter(|(a, b)| a != b).count()
        };
        assert_eq!(filled(0), 0, "an empty slider differs from itself nowhere");
        assert!(filled(50) > filled(0));
        assert!(filled(100) > filled(50));
    }

    #[test]
    fn focus_changes_the_fill_colour_and_not_the_geometry() {
        let a = paint(&Slider::<Msg>::new(50, 0, 100));
        let b = paint(&Slider::<Msg>::new(50, 0, 100).focused(true));
        assert_ne!(a, b);
        let bg = Palette::DARK.bg.mid().to_rgb565().0;
        assert_eq!(
            a.iter().filter(|&&p| p != bg).count(),
            b.iter().filter(|&&p| p != bg).count(),
            "the same pixels, in a different colour — a ring would change the count"
        );
    }

    #[test]
    fn a_slider_with_no_channel_still_consumes_its_arrow() {
        // Same rule as `Button` and `Switch`: reporting `Ignored` for want of a channel would hand
        // the press to whatever encloses this, which is a worse failure than a lost value.
        let s: Slider<Msg> = Slider::new(40, 0, 100).focused(true);
        assert_eq!(press(&s, Key::Right), Handled::Consumed);
    }

    #[test]
    fn the_digest_is_constant_and_never_zero() {
        let a: Slider<Msg> = Slider::new(0, 0, 100);
        let b: Slider<Msg> = Slider::new(73, 0, 100).focused(true).step(9);
        assert_eq!(a.content_hash(), b.content_hash(), "the value moves no pixel of the box");
        assert_ne!(a.content_hash(), 0);
    }

    #[test]
    fn a_backwards_range_draws_and_answers_instead_of_panicking() {
        // Two model fields in the wrong order. The arithmetic collapses the range; what matters here
        // is that nothing on the drawing path divides by the span it just collapsed.
        let out: Outbox<Msg> = Outbox::new();
        let s = Slider::new(50, 100, 0).focused(true).out(out.clone(), Msg::SetVolume);
        assert_eq!(press(&s, Key::Right), Handled::Consumed);
        assert!(out.is_empty());
        let _ = paint(&s);
    }
}
