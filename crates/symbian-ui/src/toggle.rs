//! A labelled on/off switch — the affordance a settings row needs for a boolean. Select flips it;
//! it draws a caption on the left and a pill-and-knob switch on the right, filled in the accent
//! colour when on. The caller supplies the label (the crate ships no text) and whether the row is
//! focused, so the row can carry the selection highlight like any other list row.

use symbian_gfx::{Align, Canvas, Rect};

use crate::input::{Handled, Key, KeyEvent};
use crate::theme::Theme;

/// An on/off switch. Owns only the boolean.
#[derive(Copy, Clone, Debug, Default)]
pub struct Toggle {
    on: bool,
}

impl Toggle {
    pub const fn new(on: bool) -> Self {
        Self { on }
    }

    pub fn on(&self) -> bool {
        self.on
    }

    pub fn set(&mut self, on: bool) {
        self.on = on;
    }

    /// Select flips it. Everything else is `Ignored` so the screen keeps its navigation.
    pub fn handle_key(&mut self, ev: KeyEvent) -> Handled {
        match ev.key {
            Key::Select => {
                self.on = !self.on;
                Handled::Consumed
            }
            _ => Handled::Ignored,
        }
    }

    /// Draw the row: `label` on the left, the switch on the right. `focused` carries the full-bleed
    /// selection band so the row reads as the cursor's position.
    pub fn draw(&self, c: &mut Canvas<'_>, r: Rect, theme: &Theme<'_>, label: &str, focused: bool) {
        let p = &theme.palette;
        if focused {
            crate::chrome::selection(c, r, theme);
        }
        let inner = r.inset_xy(theme.metrics.pad, 0);

        // The switch: a rounded track with a round knob. Sized off the row so it scales with the
        // metrics rather than being a magic pixel count.
        let sw_w = 30;
        let sw_h = (inner.height() - theme.metrics.pad * 2).clamp(10, 18);
        let track = Rect::from_xywh(
            inner.x1 - sw_w,
            inner.y0 + (inner.height() - sw_h) / 2,
            sw_w,
            sw_h,
        );
        let text_color = if focused { p.selection_text } else { p.text };
        // Label gets everything to the left of the switch, with a gap.
        let label_area = Rect { x1: track.x0 - theme.metrics.pad, ..inner };
        c.draw_text_in(label_area, label, theme.fonts.body, text_color, Align::Start);

        let track_color = if self.on { p.accent } else { p.dim };
        c.fill_round_rect(track, sw_h / 2, track_color);
        let knob_d = (sw_h - 4).max(2);
        let knob_x = if self.on { track.x1 - knob_d - 2 } else { track.x0 + 2 };
        let knob = Rect::from_xywh(knob_x, track.y0 + 2, knob_d, knob_d);
        c.fill_round_rect(knob, knob_d / 2, p.bg.mid());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;
    use crate::theme::Palette;

    fn ev(key: Key) -> KeyEvent {
        KeyEvent::new(key)
    }

    #[test]
    fn select_flips_and_reports_consumed() {
        let mut t = Toggle::new(false);
        assert_eq!(t.handle_key(ev(Key::Select)), Handled::Consumed);
        assert!(t.on());
        t.handle_key(ev(Key::Select));
        assert!(!t.on());
    }

    #[test]
    fn other_keys_pass_through() {
        let mut t = Toggle::new(false);
        assert_eq!(t.handle_key(ev(Key::Up)), Handled::Ignored);
        assert!(!t.on());
    }

    #[test]
    fn draws_on_and_off_in_every_palette() {
        for (_, palette) in Palette::ALL {
            for on in [false, true] {
                let t = Toggle::new(on);
                let (_, px) = testing::with_canvas(symbian_gfx::Size::new(320, 40), |c| {
                    testing::with_theme(palette, |th| {
                        t.draw(c, symbian_gfx::Rect::from_xywh(0, 0, 320, 38), th, "Replace Main", on);
                    });
                });
                assert!(px.iter().any(|&p| p != 0));
            }
        }
    }
}
