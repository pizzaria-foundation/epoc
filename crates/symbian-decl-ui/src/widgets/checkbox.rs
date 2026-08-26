//! A box or a circle that says "chosen".
//!
//! # One widget for both, because the difference is not in the widget
//!
//! A checkbox and a radio button differ in *meaning* — one of many against one of few — and a widget
//! cannot see its siblings, so neither of them can enforce its own meaning. What they differ in
//! visually is the outline, which is one parameter.
//!
//! Two named constructors rather than one with a flag, so a call site reads as what it means:
//!
//! ```ignore
//! ListItem::new("Notify me")
//!     .leading(Checkbox::checked(model.notify).focused(sel).out(out.clone(), Msg::ToggleNotify))
//! ListItem::new("Every day")
//!     .leading(Checkbox::radio(model.freq == Daily).focused(sel).out(out.clone(), Msg::SetDaily))
//! ```
//!
//! # Single selection is the caller's job
//!
//! A radio button reports that it was pressed. Turning that into "this one on, the others off" is a
//! line in `update`, and it has to be: only the model knows what the group is. A widget that tried
//! would need to know its siblings, which is the one thing a tree of values does not hand it.
//!
//! It follows that a radio button never reports being turned *off*. Pressing the chosen one sends its
//! message again and `update` sets the same value — which is the right no-op, and the reason the
//! message is "set this" rather than "toggle this".
//!
//! # Where it sits
//!
//! On the **left**, through [`ListItem::leading`](super::ListItem::leading), where
//! [`Switch`](super::Switch) goes on the right. That is convention rather than accident: a box
//! precedes what it labels because the eye reads the state before the text, and a switch follows
//! because the text is the question. `symbian_ui::tick::mark_box` puts it there.

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::tick::{self, Mark};
use symbian_ui::{Handled, Key, KeyEvent, Theme};

use crate::constraints::Constraints;
use crate::outbox::Outbox;
use crate::widget::{hash_i32, KeyCtx, Widget, WidgetHash};

/// A mark showing whether something is chosen.
pub struct Checkbox<M> {
    mark: Mark,
    checked: bool,
    focused: bool,
    out: Option<(Outbox<M>, M)>,
}

impl<M: Clone> Checkbox<M> {
    /// A square: one of many.
    pub fn checked(checked: bool) -> Self {
        Self { mark: Mark::Check, checked, focused: false, out: None }
    }

    /// A circle: one of few. See the module docs on why single selection is not enforced here.
    pub fn radio(chosen: bool) -> Self {
        Self { mark: Mark::Radio, checked: chosen, focused: false, out: None }
    }

    /// Whether this mark has the cursor. Only a focused one answers a key.
    pub fn focused(mut self, on: bool) -> Self {
        self.focused = on;
        self
    }

    /// What a press means, and where the message goes. Both together — see
    /// [`Switch::out`](super::Switch::out).
    pub fn out(mut self, out: Outbox<M>, msg: M) -> Self {
        self.out = Some((out, msg));
        self
    }

    pub fn is_checked(&self) -> bool {
        self.checked
    }
}

impl<M: Clone + 'static> Widget for Checkbox<M> {
    fn focus_state(&self) -> Option<bool> {
        Some(self.focused)
    }

    fn content_hash(&self) -> WidgetHash {
        // The shape, because a square and a circle could size differently under a future theme. Not
        // `checked` and not `focused`: a mark is the same box in every state, and folding them in
        // would re-measure the row on every press to produce the same number.
        hash_i32(hash_i32(0, 0x6D), self.mark as u8 as i32)
    }

    fn measure(&self, constraints: Constraints, theme: &Theme<'_>) -> Size {
        let s = tick::mark_size(constraints.max_h, theme);
        constraints.constrain(Size::new(s, s))
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        // Through `mark_box` and not into `rect` directly: `CrossAlign::Stretch` on a list row hands
        // this the whole 38-pixel band, and a mark drawn into that would be a tall rounded rectangle.
        // `self.focused` doubles as "on the selection band" — a control has the focus exactly when
        // its row is the selected one. See `chrome::control_colors`.
        tick::draw_mark(c, tick::mark_box(rect, theme), theme, self.mark, self.checked, self.focused);
    }

    fn handle_key(&self, ev: KeyEvent, _rect: Rect, _cx: &mut KeyCtx<'_>) -> Handled {
        if !self.focused || ev.key != Key::Select {
            return Handled::Ignored;
        }
        if let Some((out, msg)) = &self.out {
            out.push(msg.clone());
        }
        Handled::Consumed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::with_key_ctx;
    use symbian_ui::{testing, Palette};

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Msg {
        Pick,
    }

    const ROW: Rect = Rect { x0: 0, y0: 0, x1: 40, y1: 38 };

    fn press(b: &Checkbox<Msg>, key: Key) -> Handled {
        testing::with_theme(Palette::DARK, |_t| {
            with_key_ctx(|cx| b.handle_key(KeyEvent::new(key), ROW, cx))
        })
    }

    fn paint(b: &Checkbox<Msg>) -> Vec<u16> {
        let (_, buf) = testing::with_canvas(Size::new(40, 38), |c| {
            testing::with_theme(Palette::DARK, |t| {
                c.clear(Palette::DARK.bg.mid());
                b.draw(c, ROW, t);
            });
        });
        buf
    }

    #[test]
    fn a_focused_mark_reports_a_press_and_does_not_change_itself() {
        let out = Outbox::new();
        let b = Checkbox::checked(false).focused(true).out(out.clone(), Msg::Pick);
        assert_eq!(press(&b, Key::Select), Handled::Consumed);
        assert_eq!(out.take(), alloc::vec![Msg::Pick]);
        assert!(!b.is_checked(), "it still shows what the model said");
    }

    #[test]
    fn an_unfocused_mark_answers_nothing() {
        let out = Outbox::new();
        let b = Checkbox::checked(false).out(out.clone(), Msg::Pick);
        assert_eq!(press(&b, Key::Select), Handled::Ignored);
        assert!(out.is_empty());
    }

    #[test]
    fn a_mark_never_takes_a_navigation_key() {
        let out = Outbox::new();
        let b = Checkbox::radio(true).focused(true).out(out.clone(), Msg::Pick);
        for key in [Key::Up, Key::Down, Key::Left, Key::Right] {
            assert_eq!(press(&b, key), Handled::Ignored, "{key:?}");
        }
        assert!(out.is_empty());
    }

    #[test]
    fn pressing_a_chosen_radio_reports_it_again_rather_than_unchoosing_it() {
        // The consequence of the message being "set this" rather than "toggle this", and worth an
        // assertion: a radio group where the chosen option could be pressed off would leave the model
        // with no value at all, and no way for the user to get back to one.
        let out = Outbox::new();
        let b = Checkbox::radio(true).focused(true).out(out.clone(), Msg::Pick);
        assert_eq!(press(&b, Key::Select), Handled::Consumed);
        assert_eq!(out.take(), alloc::vec![Msg::Pick]);
    }

    #[test]
    fn it_measures_the_mark_the_toolkit_would_draw() {
        testing::with_theme(Palette::DARK, |t| {
            let s = tick::mark_size(38, t);
            assert_eq!(
                Checkbox::<Msg>::checked(true).measure(Constraints::loose(320, 38), t),
                Size::new(s, s)
            );
        });
    }

    #[test]
    fn a_square_and_a_circle_are_different_widgets_on_screen_and_in_the_digest() {
        assert_ne!(paint(&Checkbox::checked(true)), paint(&Checkbox::radio(true)));
        assert_ne!(
            Checkbox::<Msg>::checked(true).content_hash(),
            Checkbox::<Msg>::radio(true).content_hash()
        );
    }

    #[test]
    fn checked_and_unchecked_are_different_pixels() {
        assert_ne!(paint(&Checkbox::checked(false)), paint(&Checkbox::checked(true)));
        assert_ne!(paint(&Checkbox::radio(false)), paint(&Checkbox::radio(true)));
    }

    #[test]
    fn the_stretch_a_list_row_applies_does_not_stretch_the_box() {
        // The same trap `Switch` has: the row hands over 38 pixels and the mark is square.
        let buf = paint(&Checkbox::checked(true));
        let bg = Palette::DARK.bg.mid().to_rgb565().0;
        let rows: Vec<i32> =
            (0..38).filter(|&y| (0..40).any(|x| buf[(y * 40 + x) as usize] != bg)).collect();
        let s = testing::with_theme(Palette::DARK, |t| tick::mark_size(38, t));
        assert_eq!(rows.len() as i32, s, "the box is square, not the band's height");
        assert_eq!(rows[0], (38 - s) / 2, "and centred in it");
    }

    #[test]
    fn the_digest_ignores_the_state_and_is_never_zero() {
        let a = Checkbox::<Msg>::checked(false);
        let b = Checkbox::<Msg>::checked(true).focused(true);
        assert_eq!(a.content_hash(), b.content_hash(), "state moves no pixel of the box");
        assert_ne!(a.content_hash(), 0);
    }
}
