//! A boolean, as the pill-and-knob a settings row shows.
//!
//! # It is the switch, not the row
//!
//! `symbian_ui::Toggle` draws a whole settings row: the selection band, the label, and the switch
//! against the right edge. That is the right shape for a hand-written screen and the wrong one here,
//! because [`ListItem`](super::ListItem) already owns the band, the label and the margins — and owns
//! them with a parity test behind them.
//!
//! So this widget is only the pill:
//!
//! ```ignore
//! ListItem::new("Wi-Fi")
//!     .selected(sel)
//!     .trailing(Switch::new(model.wifi).focused(sel).out(model.out.clone(), Msg::ToggleWifi))
//!     .build()
//! ```
//!
//! The geometry and the ink both come from [`symbian_ui::toggle`]'s free functions —
//! [`switch_track`](symbian_ui::switch_track) and [`draw_switch`](symbian_ui::draw_switch) — which
//! were extracted from `Toggle::draw` for this. Reimplementing the rounding here would have been a
//! second switch, agreeing with the first on the day it was written.
//!
//! # The boolean is the model's
//!
//! This widget does not own the value and cannot flip it. `Select` on a focused switch pushes a
//! message and `update` changes the model, which is the same rule every other widget here follows:
//! there is exactly one place the model changes.
//!
//! That is a real difference from `symbian_ui::Toggle`, which owns its `bool` and mutates it on
//! `Select`. Both are right for their layer — an imperative screen *is* its own state — and the
//! distinction is worth knowing before reaching for one expecting the other.

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::{toggle, Handled, Key, KeyEvent, Theme};

use crate::constraints::Constraints;
use crate::outbox::Outbox;
use crate::widget::{hash_str, KeyCtx, Widget, WidgetHash};

/// An on/off switch that reports presses and owns nothing.
pub struct Switch<M> {
    on: bool,
    focused: bool,
    /// The message a press means, and where to send it. Both or neither: a message with nowhere to go
    /// is the defect [`Button`](super::Button) had, so they arrive together.
    out: Option<(Outbox<M>, M)>,
}

impl<M: Clone> Switch<M> {
    /// A switch showing `on`.
    pub fn new(on: bool) -> Self {
        Self { on, focused: false, out: None }
    }

    /// Whether this switch has the cursor. Only a focused switch answers a key.
    pub fn focused(mut self, on: bool) -> Self {
        self.focused = on;
        self
    }

    /// What a press means, and where the message goes.
    ///
    /// One method for both, deliberately: a `Button` that could be given a message without a channel
    /// spent a while consuming keys and firing nothing, and there is no reason to make that shape
    /// expressible twice.
    pub fn out(mut self, out: Outbox<M>, msg: M) -> Self {
        self.out = Some((out, msg));
        self
    }

    pub fn is_on(&self) -> bool {
        self.on
    }
}

impl<M: Clone + 'static> Widget for Switch<M> {
    fn focus_state(&self) -> Option<bool> {
        Some(self.focused)
    }

    fn content_hash(&self) -> WidgetHash {
        // Neither `on` nor `focused` is in here, and that is not an oversight: a switch is the same
        // size in all four combinations. What *would* change it is the band it is measured in, and
        // that arrives as the offer, which the cache already keys on. A digest that folded in `on`
        // would re-measure the row on every flip to produce the same number.
        //
        // A constant, not zero — zero means "always re-measure", which would put a switch's row on
        // the slow path for ever.
        hash_str(0, "switch")
    }

    fn measure(&self, constraints: Constraints, theme: &Theme<'_>) -> Size {
        // Height from the band it was offered, so a switch in a 38-pixel row and one in a short
        // dialog line are both proportionate. `switch_height` clamps at both ends.
        let h = toggle::switch_height(constraints.max_h, theme);
        constraints.constrain(Size::new(toggle::SWITCH_W, h))
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        // `switch_track` against its own rect: the rect is already switch-width, so this is only the
        // vertical centring — which matters because `CrossAlign::Stretch` on a list row hands this
        // widget the whole 38-pixel band and not the 18 pixels it measured.
        // `self.focused` is also "this row is selected": a control only takes the focus when the
        // cursor is on its row, so one flag answers both questions. See `chrome::control_colors`.
        toggle::draw_switch(c, toggle::switch_track(rect, theme), theme, self.on, self.focused);
    }

    fn handle_key(&self, ev: KeyEvent, _rect: Rect, _cx: &mut KeyCtx<'_>) -> Handled {
        if !self.focused || ev.key != Key::Select {
            // Everything else falls through, and that is what lets the enclosing list keep its
            // navigation: a switch that consumed `Down` would trap the cursor on itself.
            return Handled::Ignored;
        }
        if let Some((out, msg)) = &self.out {
            out.push(msg.clone());
        }
        // Consumed either way — see `Button`'s note on why a missing channel must not hand the press
        // back to whatever encloses this.
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
        Toggle,
    }

    const ROW: Rect = Rect { x0: 0, y0: 0, x1: 60, y1: 38 };

    fn press(sw: &Switch<Msg>, key: Key) -> Handled {
        testing::with_theme(Palette::DARK, |_t| {
            with_key_ctx(|cx| sw.handle_key(KeyEvent::new(key), ROW, cx))
        })
    }

    #[test]
    fn a_focused_switch_reports_a_press_and_does_not_flip_itself() {
        // The rule the whole crate runs on: the model changes in `update` and nowhere else. A widget
        // that flipped its own copy would show the new state for one frame and then be overwritten by
        // a `view` built from the old model — a switch that bounces back.
        let out = Outbox::new();
        let sw = Switch::new(false).focused(true).out(out.clone(), Msg::Toggle);
        assert_eq!(press(&sw, Key::Select), Handled::Consumed);
        assert_eq!(out.take(), alloc::vec![Msg::Toggle]);
        assert!(!sw.is_on(), "it still shows what the model said");
    }

    #[test]
    fn an_unfocused_switch_answers_nothing() {
        // Two switches on one screen and one press: without the flag both would fire.
        let out = Outbox::new();
        let sw = Switch::new(false).out(out.clone(), Msg::Toggle);
        assert_eq!(press(&sw, Key::Select), Handled::Ignored);
        assert!(out.is_empty());
    }

    #[test]
    fn a_switch_never_takes_a_navigation_key() {
        // What keeps the cursor able to leave it. A switch that consumed `Down` would trap the focus
        // on the one row nobody can get past.
        let out = Outbox::new();
        let sw = Switch::new(true).focused(true).out(out.clone(), Msg::Toggle);
        for key in [Key::Up, Key::Down, Key::Left, Key::Right, Key::Backspace] {
            assert_eq!(press(&sw, key), Handled::Ignored, "{key:?}");
        }
        assert!(out.is_empty());
    }

    #[test]
    fn it_measures_the_switch_the_toolkit_would_draw() {
        // Pinned to `symbian_ui`'s own functions rather than to numbers, so the two cannot drift: the
        // declarative switch and the imperative row draw the same pill or this fails.
        testing::with_theme(Palette::DARK, |t| {
            let got = Switch::<Msg>::new(true).measure(Constraints::loose(320, 38), t);
            assert_eq!(got, Size::new(toggle::SWITCH_W, toggle::switch_height(38, t)));
        });
    }

    #[test]
    fn on_and_off_are_different_pixels_in_every_palette() {
        for (name, palette) in Palette::ALL {
            let paint = |on: bool| {
                let (_, buf) = testing::with_canvas(Size::new(60, 38), |c| {
                    testing::with_theme(palette, |t| {
                        c.clear(palette.bg.mid());
                        Switch::<Msg>::new(on).draw(c, ROW, t);
                    });
                });
                buf
            };
            assert_ne!(paint(false), paint(true), "{name}: the knob did not move");
        }
    }

    #[test]
    fn it_draws_the_same_pill_the_imperative_row_draws() {
        // Parity, and cheap because both go through `toggle::draw_switch`. Worth asserting anyway:
        // the point of extracting the primitive was that these two can never be two switches, and a
        // test is what keeps a future edit from putting the geometry back in one of them.
        testing::with_theme(Palette::DARK, |t| {
            let track = toggle::switch_track(ROW, t);
            let (_, mine) = testing::with_canvas(Size::new(60, 38), |c| {
                testing::with_theme(Palette::DARK, |t| {
                    c.clear(Palette::DARK.bg.mid());
                    Switch::<Msg>::new(true).draw(c, ROW, t);
                });
            });
            let (_, theirs) = testing::with_canvas(Size::new(60, 38), |c| {
                testing::with_theme(Palette::DARK, |t2| {
                    c.clear(Palette::DARK.bg.mid());
                    // `false`: the declared switch under test is unfocused, so the imperative
                    // one it is compared against must be told the same. Passing `true` here would be
                    // comparing a switch on the selection band with one on the page.
                    toggle::draw_switch(c, track, t2, true, false);
                });
            });
            let _ = t;
            assert_eq!(mine, theirs);
        });
    }

    #[test]
    fn the_stretch_a_list_row_applies_does_not_stretch_the_pill() {
        // `CrossAlign::Stretch` hands this widget the whole 38-pixel band, not the 18 it measured. If
        // `draw` used its rect directly the pill would be a 30x38 lozenge; `switch_track` centres it.
        let (_, buf) = testing::with_canvas(Size::new(60, 38), |c| {
            testing::with_theme(Palette::DARK, |t| {
                c.clear(Palette::DARK.bg.mid());
                Switch::<Msg>::new(true).draw(c, ROW, t);
            });
        });
        let bg = Palette::DARK.bg.mid().to_rgb565().0;
        let painted: Vec<i32> =
            (0..38).filter(|&y| (0..60).any(|x| buf[(y * 60 + x) as usize] != bg)).collect();
        let h = testing::with_theme(Palette::DARK, |t| toggle::switch_height(38, t));
        assert_eq!(painted.len() as i32, h, "the pill is its own height, not the band's");
        assert_eq!(painted[0], (38 - h) / 2, "and centred in it");
    }

    #[test]
    fn the_digest_is_constant_and_not_zero() {
        // Constant because a switch is the same size in all four states; not zero because zero means
        // "re-measure me every frame", which would put every switch's row on the slow path.
        let a = Switch::<Msg>::new(false);
        let b = Switch::<Msg>::new(true).focused(true);
        assert_eq!(a.content_hash(), b.content_hash());
        assert_ne!(a.content_hash(), 0);
    }
}
