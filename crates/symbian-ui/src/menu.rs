//! The Options menu: a panel that rises from the left softkey.
//!
//! S60's own shape, and the one this toolkit was missing. A dialog ([`crate::modal`]) is centred,
//! dims everything behind it and asks a question. A menu is not a question — it is the verbs of the
//! screen you are already on, and it belongs *at* the key that opened it so the eye travels from
//! the word "Options" to the list without crossing the screen.
//!
//! # Why it grows upward from the corner
//!
//! Because that is where it was summoned from. A centred panel makes the reader look away from the
//! key they pressed and back again, and on a 320×240 screen a centred list of three verbs covers
//! the very content those verbs act on. Anchored bottom-left, the page stays readable beside it —
//! which matters when the menu item you are choosing between depends on what is on the page.
//!
//! # No scrim
//!
//! Deliberately different from [`crate::modal`]. A dialog dims the screen because the screen is
//! *not available* until you answer; a menu can be dismissed with one key and the page underneath is
//! still the subject. Dimming it would say otherwise. The panel earns its separation with an edge
//! and a shadow instead.

use alloc::string::String;
use alloc::vec::Vec;

use symbian_gfx::{Canvas, Point, Rect};

use crate::input::{Handled, Key, KeyEvent, Softkey};
use crate::theme::Theme;

/// How far the shadow falls. Two, matching [`crate::prompt`]: enough to lift the panel off the page
/// at this size, small enough not to read as a second panel behind the first.
const SHADOW: i32 = 2;

/// What a key did to the menu.
///
/// `Chosen` carries the caller's own value rather than an index, and that is not a nicety: the first
/// version returned an index into the list the caller *thought* it had built, and the first caller
/// hid an entry that did not apply — so every index after it pointed at the wrong verb. The widget
/// hands back what was registered, and the mistake is unavailable.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum MenuAction<T> {
    Chosen(T),
    /// Dismissed without choosing.
    Cancelled,
    None,
}

/// A menu of verbs, anchored to the left softkey.
pub struct Menu<T> {
    items: Vec<(String, T)>,
    sel: usize,
}

impl<T: Clone> Menu<T> {
    pub fn new() -> Self {
        Self { items: Vec::new(), sel: 0 }
    }

    pub fn item(mut self, label: impl Into<String>, value: T) -> Self {
        self.items.push((label.into(), value));
        self
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Which entry is highlighted.
    pub fn selected(&self) -> usize {
        self.sel
    }

    /// Move the highlight, wrapping at both ends.
    ///
    /// Wrapping because a menu is short: with three entries, "down" from the last one meaning
    /// nothing is a key press that does nothing, and the reader cannot tell that from a menu that
    /// stopped responding.
    pub fn step(&mut self, delta: i32) {
        if self.items.is_empty() {
            return;
        }
        let n = self.items.len() as i32;
        let next = (self.sel as i32 + delta).rem_euclid(n);
        self.sel = next as usize;
    }

    pub fn handle_key(&mut self, ev: KeyEvent) -> MenuAction<T> {
        match ev.key {
            Key::Up => {
                self.step(-1);
                MenuAction::None
            }
            Key::Down => {
                self.step(1);
                MenuAction::None
            }
            Key::Select | Key::Enter | Key::Softkey(Softkey::Left) => {
                match self.items.get(self.sel) {
                    Some((_, v)) => MenuAction::Chosen(v.clone()),
                    None => MenuAction::Cancelled,
                }
            }
            // The right softkey, and also either horizontal arrow: a menu that rose from the corner
            // is dismissed by moving away from it, and left/right have nothing else to do here.
            Key::Softkey(Softkey::Right) | Key::Left | Key::Right => MenuAction::Cancelled,
            _ => MenuAction::None,
        }
    }

    /// The value behind an index, for the caller's match.
    pub fn value(&self, i: usize) -> Option<T> {
        self.items.get(i).map(|(_, v)| v.clone())
    }

    /// Where the panel sits, given the whole screen.
    ///
    /// Separated from [`Menu::draw`] so it can be asserted on without a canvas — the anchoring is
    /// the whole point of this widget, and "it looked right on the phone" is not a test.
    ///
    /// Bottom edge at the top of the softkey bar, left edge at the screen's left. It grows upward,
    /// and is clamped to the screen when there are more entries than fit.
    pub fn panel(&self, screen: Rect, theme: &Theme<'_>) -> Rect {
        let m = &theme.metrics;
        let font = theme.fonts.body;
        let row_h = font.line_height() + m.pad;

        let widest = self.items.iter().map(|(l, _)| font.measure(l)).max().unwrap_or(0);
        let w = (widest + m.pad * 3).clamp(60, screen.width() - m.pad);

        let wanted = row_h * self.items.len() as i32 + 2;
        let bottom = screen.y1 - m.softkey_h;
        let h = wanted.min(bottom - screen.y0);

        Rect { x0: screen.x0, y0: bottom - h, x1: screen.x0 + w, y1: bottom }
    }

    /// The row height this menu draws with.
    pub fn row_h(&self, theme: &Theme<'_>) -> i32 {
        theme.fonts.body.line_height() + theme.metrics.pad
    }

    pub fn draw(&self, c: &mut Canvas<'_>, theme: &Theme<'_>) {
        let screen = Rect::from_size(c.size());
        let panel = self.panel(screen, theme);
        if panel.is_empty() {
            return;
        }
        let p = &theme.palette;

        // Shadow first, offset down-right, then the panel over it.
        c.fill_rect(
            Rect {
                x0: panel.x0 + SHADOW,
                y0: panel.y0 + SHADOW,
                x1: panel.x1 + SHADOW,
                y1: panel.y1 + SHADOW,
            },
            symbian_gfx::Color::rgb(0, 0, 0).with_alpha(0x60),
        );
        crate::paint::band(c, panel, &p.chrome);
        c.stroke_rect(panel, p.divider);

        let row_h = self.row_h(theme);
        let font = theme.fonts.body;
        for (i, (label, _)) in self.items.iter().enumerate() {
            let y0 = panel.y0 + 1 + row_h * i as i32;
            if y0 + row_h > panel.y1 {
                break;
            }
            let row = Rect { x0: panel.x0 + 1, y0, x1: panel.x1 - 1, y1: y0 + row_h };
            let fg = if i == self.sel {
                crate::paint::band(c, row, &p.selection);
                p.selection_text
            } else {
                p.chrome_text
            };
            c.draw_text(
                Point::new(row.x0 + theme.metrics.pad, row.y0 + (row_h - font.line_height()) / 2 + font.ascent()),
                label,
                font,
                fg,
            );
        }
    }
}

impl<T: Clone> Default for Menu<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Route a key to an optional menu, clearing it when it is answered or dismissed.
///
/// The same shape as [`crate::modal::route`], and for the same reason: the order — menu before the
/// screen behind it — is the part every caller would otherwise get to re-derive.
pub fn route<T: Clone>(slot: &mut Option<Menu<T>>, ev: KeyEvent) -> Option<MenuAction<T>> {
    let m = slot.as_mut()?;
    let answer = m.handle_key(ev);
    if !matches!(answer, MenuAction::None) {
        *slot = None;
    }
    Some(answer)
}

/// Whether a menu is up and therefore owns the keyboard.
pub fn owns_keys<T>(slot: &Option<Menu<T>>) -> Handled {
    if slot.is_some() {
        Handled::Consumed
    } else {
        Handled::Ignored
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{testing, Palette};
    use symbian_gfx::Size;

    fn three() -> Menu<u8> {
        Menu::new().item("Go to address", 1u8).item("Reload", 2u8).item("Home", 3u8)
    }

    #[test]
    fn it_sits_in_the_bottom_left_corner() {
        testing::with_theme(Palette::DARK, |t| {
            let screen = Rect::from_size(Size::new(320, 240));
            let p = three().panel(screen, t);
            // The two edges that make it a menu and not a dialog.
            assert_eq!(p.x0, screen.x0, "flush with the left edge");
            assert_eq!(p.y1, screen.y1 - t.metrics.softkey_h, "sitting on the softkey bar");
            // And it grew upward rather than filling the screen.
            assert!(p.y0 > screen.y0, "top edge is above the bar but below the screen top");
            assert!(p.x1 < screen.x1, "narrower than the screen, so the page shows beside it");
        });
    }

    #[test]
    fn it_hugs_its_widest_label() {
        testing::with_theme(Palette::DARK, |t| {
            let screen = Rect::from_size(Size::new(320, 240));
            let narrow = Menu::new().item("Go", 1u8).panel(screen, t);
            let wide = Menu::new().item("Go to address really far", 1u8).panel(screen, t);
            assert!(wide.width() > narrow.width());
        });
    }

    #[test]
    fn more_entries_make_it_taller_upward() {
        testing::with_theme(Palette::DARK, |t| {
            let screen = Rect::from_size(Size::new(320, 240));
            let one = Menu::new().item("A", 1u8).panel(screen, t);
            let three_ = three().panel(screen, t);
            assert!(three_.height() > one.height());
            // Grows from the anchor, so the bottom edge never moves.
            assert_eq!(one.y1, three_.y1);
        });
    }

    #[test]
    fn a_menu_longer_than_the_screen_is_clamped() {
        testing::with_theme(Palette::DARK, |t| {
            let screen = Rect::from_size(Size::new(320, 240));
            let mut m = Menu::new();
            for _ in 0..40 {
                m = m.item("An entry", 1u8);
            }
            let p = m.panel(screen, t);
            assert!(p.y0 >= screen.y0, "never off the top of the screen");
            assert!(p.height() <= screen.height());
        });
    }

    #[test]
    fn the_highlight_wraps_at_both_ends() {
        let mut m = three();
        assert_eq!(m.selected(), 0);
        m.step(-1);
        assert_eq!(m.selected(), 2, "up from the first goes to the last");
        m.step(1);
        assert_eq!(m.selected(), 0, "and down from the last comes back");
    }

    #[test]
    fn either_horizontal_arrow_dismisses() {
        // A menu anchored to a corner is left by moving away from it.
        for key in [Key::Left, Key::Right, Key::Softkey(Softkey::Right)] {
            let mut slot = Some(three());
            let a = route(&mut slot, KeyEvent::new(key));
            assert_eq!(a, Some(MenuAction::<u8>::Cancelled));
            assert!(slot.is_none(), "and the menu is gone");
        }
    }

    #[test]
    fn choosing_reports_the_index_and_closes() {
        let mut slot = Some(three());
        slot.as_mut().unwrap().step(1);
        let a = route(&mut slot, KeyEvent::new(Key::Select));
        assert_eq!(a, Some(MenuAction::Chosen(2u8)), "the value, not the row it sat in");
        assert!(slot.is_none());
    }

    #[test]
    fn a_hidden_entry_cannot_shift_what_the_others_mean() {
        // The bug this widget's API exists to prevent: a caller that omits an entry which does not
        // apply. With indices, everything after the gap answered as its neighbour.
        let mut slot = Some(Menu::new().item("Go", 1u8).item("Home", 3u8));
        slot.as_mut().unwrap().step(1);
        assert_eq!(route(&mut slot, KeyEvent::new(Key::Select)), Some(MenuAction::Chosen(3u8)));
    }

    #[test]
    fn moving_keeps_it_open_and_eats_the_key() {
        let mut slot = Some(three());
        assert_eq!(route(&mut slot, KeyEvent::new(Key::Down)), Some(MenuAction::<u8>::None));
        assert!(slot.is_some(), "still up");
        assert_eq!(owns_keys(&slot), Handled::Consumed);
    }
}
