//! A labelled control that fires one message.
//!
//! # Why a button does not choose its own key
//!
//! There is no pointer on this device. A button is not something you press, it is something the
//! *action key* presses while it is focused — and which key that is, is a piece of platform trivia
//! that has already been got wrong once here. The launcher's task manager bound a middle-slot
//! action to `Softkey::Middle`, an event S60 never sends; the label said one thing and the key did
//! another, and both halves were perfectly consistent with themselves.
//!
//! So this widget does not match on `Key::Select`. It holds a [`Softkeys`] with its label in the
//! action slot and asks [`Softkeys::dispatch`] what a key press means, which is the same call the
//! softkey bar makes. If the platform's idea of the action key ever changes, it changes in
//! [`crate::keys`] and this button changes with it, because there is only one place that knows.
//!
//! # Why the message comes back rather than being called
//!
//! A button holds a value, not a closure — the same decision [`SoftkeyDef`](crate::SoftkeyDef)
//! makes. A screen stays a plain description that can be built, compared and tested without
//! running anything, and the message goes to `update` like every other message, so there is still
//! exactly one place where the model changes.

use alloc::string::String;

use symbian_gfx::{Align, Canvas, Rect, Size};
use symbian_ui::{Handled, KeyEvent, Theme};

use crate::constraints::Constraints;
use crate::keys::Softkeys;
use crate::widget::{hash_i32, hash_str, KeyCtx, Widget, WidgetHash};

/// A focusable button carrying the message it sends.
pub struct Button<M> {
    /// The label and its message, held in the action slot so that what fires this button is
    /// decided by [`Softkeys::dispatch`] and not by a match written here.
    keys: Softkeys<M>,
    focused: bool,
}

impl<M: Clone> Button<M> {
    pub fn new(label: impl Into<String>, msg: M) -> Self {
        Self { keys: Softkeys::new().action(label, msg), focused: false }
    }

    /// Whether this button has the focus. Only a focused button fires.
    pub fn focused(mut self, on: bool) -> Self {
        self.focused = on;
        self
    }

    pub fn label(&self) -> &str {
        self.keys.action.as_ref().map_or("", |d| d.label.as_str())
    }

    /// The message this key press means for this button, if it means one.
    ///
    /// `None` for every key that is not the action key, and for every key at all when the button
    /// is not focused — which is what lets a screen offer a row of buttons and hand the same key
    /// to all of them.
    pub fn press(&self, ev: KeyEvent) -> Option<M> {
        if !self.focused {
            return None;
        }
        self.keys.dispatch(ev)
    }
}

impl<M: Clone + 'static> Widget for Button<M> {
    fn content_hash(&self) -> WidgetHash {
        // Focus is in the digest because the ring is drawn inside the button's own box on this
        // theme; if a future theme drew it outside, the size would change with focus and a digest
        // that ignored it would keep the old one.
        hash_i32(hash_str(0, self.label()), self.focused as i32)
    }

    fn measure(&self, constraints: Constraints, theme: &Theme<'_>) -> Size {
        let w = theme.fonts.strong.measure(self.label()) + theme.metrics.space.base * 2;
        let h = theme.fonts.strong.line_height() + theme.metrics.space.snug * 2;
        constraints.constrain(Size::new(w, h))
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        // Focused and unfocused differ in fill, not in size or position. A button that grew when
        // focused would shift its neighbours as the D-pad moved along a row, and the row would
        // appear to wobble under the thumb.
        let (fill, ink) = if self.focused {
            (theme.palette.accent, theme.palette.accent_text)
        } else {
            (theme.palette.chrome.mid(), theme.palette.text)
        };
        c.fill_rect(rect, fill);
        c.stroke_rect(rect, theme.palette.divider);
        c.draw_text_in(rect, self.label(), theme.fonts.strong, ink, Align::Center);
    }

    fn handle_key(&self, ev: KeyEvent, _rect: Rect, _cx: &mut KeyCtx<'_>) -> Handled {
        // Consumed only when it would actually fire. Returning `Consumed` for every key would eat
        // the D-pad and trap the focus on this button.
        if self.press(ev).is_some() {
            Handled::Consumed
        } else {
            Handled::Ignored
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_gfx::Size as GSize;
    use symbian_ui::{testing, Key, Palette, Softkey};

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Msg {
        Send,
        Cancel,
    }

    fn press(k: Key) -> KeyEvent {
        KeyEvent::new(k)
    }

    fn button() -> Button<Msg> {
        Button::new("Send", Msg::Send).focused(true)
    }

    #[test]
    fn a_button_fires_on_the_action_key_the_platform_actually_sends() {
        // The bug this widget is shaped to prevent: S60 delivers the centre of the bar as
        // `Select`, never as `Softkey::Middle`, and a button that matched on the latter would be
        // a label that promises something the key does not do.
        assert_eq!(button().press(press(Key::Select)), Some(Msg::Send));
        assert_eq!(button().press(press(Key::Enter)), Some(Msg::Send));
        assert_eq!(button().press(press(Key::Softkey(Softkey::Middle))), Some(Msg::Send));
    }

    #[test]
    fn a_button_does_not_answer_to_keys_that_are_not_its_own() {
        // Up/Down must reach whatever moves the focus, and the outer softkeys belong to the
        // screen's bar — a button that took them would be a second, invisible bar.
        let b = button();
        assert_eq!(b.press(press(Key::Up)), None);
        assert_eq!(b.press(press(Key::Down)), None);
        assert_eq!(b.press(press(Key::Char('s'))), None, "not the first letter of its label either");
        assert_eq!(b.press(press(Key::Softkey(Softkey::Left))), None);
        assert_eq!(b.press(press(Key::Softkey(Softkey::Right))), None);
    }

    #[test]
    fn an_unfocused_button_fires_at_nothing() {
        // Two buttons on a screen are both handed the key; only the focused one may act.
        let send = Button::new("Send", Msg::Send).focused(true);
        let cancel = Button::new("Cancel", Msg::Cancel).focused(false);
        assert_eq!(send.press(press(Key::Select)), Some(Msg::Send));
        assert_eq!(cancel.press(press(Key::Select)), None);
    }

    #[test]
    fn handled_says_consumed_only_when_something_happened() {
        let b = button();
        let r = Rect::from_xywh(0, 0, 80, 24);
        crate::widget::with_key_ctx(|cx| {
            assert_eq!(b.handle_key(press(Key::Select), r, cx), Handled::Consumed);
            // Anything else must fall through, or the D-pad cannot leave the button.
            assert_eq!(b.handle_key(press(Key::Down), r, cx), Handled::Ignored);
            let idle = Button::new("Send", Msg::Send);
            assert_eq!(idle.handle_key(press(Key::Select), r, cx), Handled::Ignored);
        });
    }

    #[test]
    fn the_label_and_the_message_are_one_declaration() {
        // You cannot label a key you do not handle: both come from the same `Softkeys` slot, so
        // there is nowhere for them to disagree.
        let b = Button::new("Open", Msg::Send);
        assert_eq!(b.label(), "Open");
        assert_eq!(b.focused(true).press(press(Key::Select)), Some(Msg::Send));
    }

    #[test]
    fn focus_changes_the_colours_and_not_the_size() {
        // A button that grew when focused would shove its neighbours sideways as the D-pad walked
        // along a row of them.
        testing::with_theme(Palette::DARK, |t| {
            let c = Constraints::loose(200, 100);
            let idle = Button::new("Send", Msg::Send).measure(c, t);
            let hot = Button::new("Send", Msg::Send).focused(true).measure(c, t);
            assert_eq!(idle, hot);
            assert!(hot.w > 0 && hot.h > 0);
        });
    }

    #[test]
    fn a_button_fits_the_offer_it_is_given() {
        testing::with_theme(Palette::DARK, |t| {
            let long = Button::new("A very long button label indeed", Msg::Send);
            let got = long.measure(Constraints::loose(60, 20), t);
            assert!(got.w <= 60 && got.h <= 20, "measure must stay inside the offer");
        });
    }

    #[test]
    fn drawing_a_button_fills_its_box_either_way() {
        testing::with_theme(Palette::DARK, |t| {
            for focused in [false, true] {
                let mut buf = alloc::vec![0u16; 80 * 24];
                {
                    let mut c = Canvas::from_slice(&mut buf, GSize::new(80, 24));
                    Button::new("Send", Msg::Send)
                        .focused(focused)
                        .draw(&mut c, Rect::from_xywh(0, 0, 80, 24), t);
                }
                assert!(buf.iter().any(|&p| p != 0), "focused={focused} drew nothing at all");
            }
        });
    }
}
