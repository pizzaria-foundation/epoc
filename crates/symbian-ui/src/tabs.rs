//! Horizontal tabs — the strip S60 puts across the top of a settings dialog, switched with
//! Left/Right. A plain struct that owns only the active index; the labels are the caller's (this
//! crate ships no text), and the active tab is drawn with the same rounded-top band S60 used.
//!
//! It is a *strip*, not a container: it does not own the tabs' contents. A screen holds a `Tabs`
//! plus one piece of state per tab, routes Left/Right to `handle_key`, and draws the content of
//! `active()` itself. That keeps the tab set open — a tab can hold a list, a form, anything — and
//! keeps `Tabs` testable in isolation.

use symbian_gfx::{Align, Canvas, Rect};

use crate::input::{Handled, Key, KeyEvent};
use crate::paint;
use crate::theme::Theme;

/// A row of tabs. Owns the active index; everything else is passed in per call.
#[derive(Copy, Clone, Debug, Default)]
pub struct Tabs {
    active: usize,
}

impl Tabs {
    pub const fn new() -> Self {
        Self { active: 0 }
    }

    /// Which tab is active.
    pub fn active(&self) -> usize {
        self.active
    }

    /// Force the active tab, clamped to `count`.
    pub fn set_active(&mut self, index: usize, count: usize) {
        if count > 0 {
            self.active = index.min(count - 1);
        }
    }

    /// Left/Right move between tabs (clamped, no wraparound — the edges hold, matching every other
    /// cursor in the toolkit). `count` is how many tabs there are right now. Everything else is
    /// `Ignored` so the screen can route Up/Down/Select to the active tab's content.
    pub fn handle_key(&mut self, ev: KeyEvent, count: usize) -> Handled {
        if count == 0 {
            return Handled::Ignored;
        }
        match ev.key {
            Key::Left => {
                self.active = self.active.saturating_sub(1);
                Handled::Consumed
            }
            Key::Right => {
                if self.active + 1 < count {
                    self.active += 1;
                }
                Handled::Consumed
            }
            _ => Handled::Ignored,
        }
    }

    /// Draw the strip across `r`, one equal-width cell per label. The active tab gets the rounded-top
    /// band ([`paint::band_top_rounded`], the S60 active-tab shape) in the selection surface; the
    /// rest are flat chrome. `r` is usually the strip just below the title bar.
    pub fn draw(&self, c: &mut Canvas<'_>, r: Rect, theme: &Theme<'_>, labels: &[&str]) {
        if r.is_empty() || labels.is_empty() {
            return;
        }
        let p = &theme.palette;
        let bg = p.bg.mid();
        let cells = labels.len() as i32;
        let step = r.width() / cells;
        let mut x = r.x0;
        for (i, label) in labels.iter().enumerate() {
            // The last cell takes the rounding remainder so the strip fills `r` exactly.
            let cell_w = if i + 1 == labels.len() { r.x1 - x } else { step };
            let cell = Rect::new(x, r.y0, x + cell_w, r.y1);
            x += cell_w;

            let active = i == self.active;
            if active {
                paint::band_top_rounded(c, cell, &p.selection, theme.metrics.radius, bg);
            } else {
                paint::band(c, cell, &p.chrome);
            }
            let color = if active { p.selection_text } else { p.chrome_text };
            c.draw_text_in(cell, label, theme.fonts.small, color, Align::Center);
        }
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
    fn left_right_move_and_hold_at_the_edges() {
        let mut t = Tabs::new();
        assert_eq!(t.active(), 0);
        assert_eq!(t.handle_key(ev(Key::Right), 3), Handled::Consumed);
        assert_eq!(t.active(), 1);
        t.handle_key(ev(Key::Right), 3);
        t.handle_key(ev(Key::Right), 3); // at 2, last
        assert_eq!(t.active(), 2);
        t.handle_key(ev(Key::Right), 3); // holds
        assert_eq!(t.active(), 2);
        t.handle_key(ev(Key::Left), 3);
        assert_eq!(t.active(), 1);
    }

    #[test]
    fn non_horizontal_keys_are_ignored() {
        let mut t = Tabs::new();
        assert_eq!(t.handle_key(ev(Key::Up), 3), Handled::Ignored);
        assert_eq!(t.handle_key(ev(Key::Select), 3), Handled::Ignored);
    }

    #[test]
    fn draws_in_every_palette() {
        for (_, palette) in Palette::ALL {
            let mut t = Tabs::new();
            t.set_active(1, 3);
            let (_, px) = testing::with_canvas(symbian_gfx::Size::new(320, 20), |c| {
                testing::with_theme(palette, |th| {
                    t.draw(c, symbian_gfx::Rect::from_xywh(0, 0, 320, 18), th, &["General", "Apps", "Home"]);
                });
            });
            assert!(px.iter().any(|&p| p != 0));
        }
    }
}
