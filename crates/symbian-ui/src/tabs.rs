//! Horizontal tabs — the strip S60 puts across the top of a settings dialog, switched with
//! Left/Right. A plain struct that owns only the active index; the labels are the caller's (this
//! crate ships no text), and the active tab is drawn as the page rising into the strip — see
//! [`Tabs::draw`] for why that is not the same as highlighting it.
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
    /// How tall a strip wants to be.
    ///
    /// One function because two callers were each computing `title_h + pad`, which is the height of
    /// a *title bar* and not of a row of labels. A strip is small text and a little air: on a
    /// 240-pixel screen the old number spent 29 of them on seven words, and the ten it gives back
    /// are a whole extra row of content.
    pub fn height(theme: &Theme<'_>) -> i32 {
        theme.fonts.small.line_height() + theme.metrics.pad + 2
    }

    pub fn draw(&self, c: &mut Canvas<'_>, r: Rect, theme: &Theme<'_>, labels: &[&str]) {
        if r.is_empty() || labels.is_empty() {
            return;
        }
        let p = &theme.palette;
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
                // **The page, not the selection colour.** A tab is the content coming up to meet
                // the strip, and drawing it that way is what makes it read as a tab rather than as
                // a highlighted button.
                //
                // It used to use `p.selection`, which works in the built-in palettes and works *by
                // accident*: their accent is a different hue from their chrome, so the two read
                // apart. A palette derived from a phone's own theme has no such luck — measured on
                // this handset's pink theme, chrome is `#cfb0b7` and selection `#98767e`: the same
                // mauve, one darker. Fifty-eight of luma and nothing else, which a person reads as
                // "slightly different" rather than as "this one".
                //
                // Against the page it is a different *surface*, and that survives any palette.
                paint::band_top_rounded(c, cell, &p.bg, theme.metrics.radius, p.chrome.bottom);
            } else {
                paint::band(c, cell, &p.chrome);
                // A hairline along the bottom of every tab that is *not* active. It is what the
                // active one is missing, and the gap in that line is the second signal — the one
                // that still works when somebody's palette makes two surfaces nearly the same
                // colour.
                c.fill_rect(Rect::new(cell.x0, cell.y1 - 1, cell.x1, cell.y1), p.divider);
            }
            let color = if active { p.text } else { p.chrome_text };
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
