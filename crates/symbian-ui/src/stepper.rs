//! A labelled numeric stepper for a settings row — the integer counterpart of [`crate::Toggle`].
//! It owns a value bounded to `[min, max]`. Select cycles it up by one and wraps past the top back
//! to the bottom (so it works where Left/Right are unavailable — e.g. a tabbed screen whose tab
//! strip already consumes Left/Right); Left/Right also step it down/up when they do reach the
//! widget. Draws the caption on the left and `‹ N ›` on the right.

use symbian_gfx::{Align, Canvas, Rect};

use crate::input::{Handled, Key, KeyEvent};
use crate::theme::Theme;

/// A bounded integer picker. Owns the value and its inclusive bounds.
#[derive(Copy, Clone, Debug)]
pub struct Stepper {
    value: i32,
    min: i32,
    max: i32,
}

impl Stepper {
    pub fn new(value: i32, min: i32, max: i32) -> Self {
        let (min, max) = if min <= max { (min, max) } else { (max, min) };
        Self { value: value.clamp(min, max), min, max }
    }

    pub fn value(&self) -> i32 {
        self.value
    }

    pub fn set(&mut self, value: i32) {
        self.value = value.clamp(self.min, self.max);
    }

    /// Select cycles up with wrap; Left/Right step down/up with clamping. Anything else is
    /// `Ignored` so the surrounding screen keeps its navigation.
    pub fn handle_key(&mut self, ev: KeyEvent) -> Handled {
        match ev.key {
            Key::Select => {
                self.value = if self.value >= self.max { self.min } else { self.value + 1 };
                Handled::Consumed
            }
            Key::Right => {
                if self.value < self.max {
                    self.value += 1;
                }
                Handled::Consumed
            }
            Key::Left => {
                if self.value > self.min {
                    self.value -= 1;
                }
                Handled::Consumed
            }
            _ => Handled::Ignored,
        }
    }

    /// Draw the row: `label` left, `‹ N ›` right; the value takes the accent colour when focused.
    pub fn draw(&self, c: &mut Canvas<'_>, r: Rect, theme: &Theme<'_>, label: &str, focused: bool) {
        let p = &theme.palette;
        if focused {
            crate::chrome::selection(c, r, theme);
        }
        let inner = r.inset_xy(theme.metrics.pad, 0);
        let text_color = if focused { p.selection_text } else { p.text };

        // Right-aligned value block "‹ N ›". Fixed width so the label area is stable.
        let val_w = 46;
        let val_area = Rect { x0: inner.x1 - val_w, ..inner };
        let mut buf = [0u8; 16];
        let s = fmt_stepper(self.value, &mut buf);
        c.draw_text_in(val_area, s, theme.fonts.body, if focused { p.selection_text } else { p.accent }, Align::Center);

        let label_area = Rect { x1: val_area.x0 - theme.metrics.pad, ..inner };
        c.draw_text_in(label_area, label, theme.fonts.body, text_color, Align::Start);
    }
}

/// Format `‹ N ›` into a small stack buffer (no alloc). N is a small non-negative count here.
fn fmt_stepper(n: i32, buf: &mut [u8; 16]) -> &str {
    // "< " + digits + " >"
    let mut tmp = [0u8; 8];
    let mut i = tmp.len();
    let mut v = n.max(0) as u32;
    loop {
        i -= 1;
        tmp[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    let digits = &tmp[i..];
    let mut w = 0;
    for &b in b"< " {
        buf[w] = b;
        w += 1;
    }
    for &b in digits {
        buf[w] = b;
        w += 1;
    }
    for &b in b" >" {
        buf[w] = b;
        w += 1;
    }
    core::str::from_utf8(&buf[..w]).unwrap_or("?")
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
    fn select_cycles_with_wrap() {
        let mut s = Stepper::new(2, 2, 4);
        s.handle_key(ev(Key::Select)); // 3
        assert_eq!(s.value(), 3);
        s.handle_key(ev(Key::Select)); // 4
        s.handle_key(ev(Key::Select)); // wrap to 2
        assert_eq!(s.value(), 2);
    }

    #[test]
    fn left_right_clamp() {
        let mut s = Stepper::new(2, 2, 4);
        assert_eq!(s.handle_key(ev(Key::Left)), Handled::Consumed);
        assert_eq!(s.value(), 2, "clamped at min");
        s.handle_key(ev(Key::Right));
        s.handle_key(ev(Key::Right));
        s.handle_key(ev(Key::Right));
        assert_eq!(s.value(), 4, "clamped at max");
    }

    #[test]
    fn new_clamps_and_orders_bounds() {
        assert_eq!(Stepper::new(9, 2, 4).value(), 4);
        assert_eq!(Stepper::new(0, 2, 4).value(), 2);
        // reversed bounds are tolerated
        assert_eq!(Stepper::new(3, 4, 2).value(), 3);
    }

    #[test]
    fn draws_in_every_palette() {
        for (_, palette) in Palette::ALL {
            let s = Stepper::new(3, 1, 5);
            let (_, px) = testing::with_canvas(symbian_gfx::Size::new(320, 40), |c| {
                testing::with_theme(palette, |th| {
                    s.draw(c, symbian_gfx::Rect::from_xywh(0, 0, 320, 38), th, "Columns", true);
                });
            });
            assert!(px.iter().any(|&p| p != 0));
        }
    }
}
