//! A drop-down: a closed field showing the current option, that opens into a popup list. Select
//! opens it and, when open, commits the highlighted row; the red/Backspace key cancels. Options are
//! passed in by the caller each call (the crate ships no text), so a `Select` stores only the
//! chosen index and the open/popup state — nothing about the choices themselves.
//!
//! It reuses [`crate::list::ListState`] for the open popup, so scrolling a long option list is the
//! same tested code every other list uses. It reports back with the same `(Handled, Action)` shape
//! as [`crate::viewer::Viewer`], so a screen can react to a change without polling.

use symbian_gfx::{Align, Canvas, Rect};

use crate::input::{Handled, Key, KeyEvent, Softkey};
use crate::list::{ListState, Uniform};
use crate::theme::Theme;

/// What a key did to the selection.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum SelectAction {
    /// The chosen index changed to this value (the popup was committed on a new row).
    Changed(usize),
    None,
}

/// A drop-down over caller-supplied options.
#[derive(Copy, Clone, Debug, Default)]
pub struct Select {
    index: usize,
    open: bool,
    list: ListState,
    /// The popup viewport height and row height from the last draw, so key handling can scroll the
    /// open list without being handed a rect. Zero until first drawn open.
    view_h: i32,
    row_h: i32,
}

impl Select {
    pub const fn new(index: usize) -> Self {
        Self { index, open: false, list: ListState::new(), view_h: 0, row_h: 0 }
    }

    /// The chosen option index.
    pub fn selected(&self) -> usize {
        self.index
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Force the chosen index (clamped by the caller against its own option count).
    pub fn set(&mut self, index: usize) {
        self.index = index;
    }

    /// Closed: Select opens the popup (highlighting the current option). Open: Up/Down move the
    /// highlight, Select commits it, End/Backspace cancels. Returns `Ignored` when closed for keys
    /// other than Select, so the screen keeps its navigation.
    pub fn handle_key(&mut self, ev: KeyEvent, options: &[&str]) -> (Handled, SelectAction) {
        let n = options.len();
        if !self.open {
            return match ev.key {
                Key::Select if n > 0 => {
                    self.open = true;
                    self.list.selected = self.index.min(n - 1);
                    self.list.scroll = 0;
                    (Handled::Consumed, SelectAction::None)
                }
                _ => (Handled::Ignored, SelectAction::None),
            };
        }
        // Open: trap navigation inside the popup.
        match ev.key {
            Key::Up | Key::Down => {
                let rows = Uniform { count: n, height: self.row_h.max(1) };
                let delta = if matches!(ev.key, Key::Up) { -1 } else { 1 };
                self.list.move_selection(delta, &rows, self.view_h.max(1));
                (Handled::Consumed, SelectAction::None)
            }
            Key::Select => {
                self.open = false;
                let i = self.list.selected.min(n.saturating_sub(1));
                if i != self.index {
                    self.index = i;
                    (Handled::Consumed, SelectAction::Changed(i))
                } else {
                    (Handled::Consumed, SelectAction::None)
                }
            }
            // Back cancels, same as the red key and Backspace. The popup is modal and its
            // catch-all eats everything else, so without this the Back softkey did nothing at all
            // while it was open — the same defect the app picker had, in the sibling widget.
            // `Viewer` had it right all along, which is how three widgets ended up with three
            // different answers to "what closes this".
            Key::End | Key::Backspace | Key::Softkey(Softkey::Right) => {
                self.open = false;
                (Handled::Consumed, SelectAction::None)
            }
            // Swallow everything else while open, so a stray key cannot leak to the screen behind.
            _ => (Handled::Consumed, SelectAction::None),
        }
    }

    /// Draw the closed field in `r`. When open, also call [`Self::draw_popup`] over the area the
    /// popup may cover (usually the content below the field).
    pub fn draw(&mut self, c: &mut Canvas<'_>, r: Rect, theme: &Theme<'_>, options: &[&str], focused: bool) {
        let p = &theme.palette;
        if focused {
            crate::chrome::selection(c, r, theme);
        }
        let inner = r.inset_xy(theme.metrics.pad, 0);
        let value = options.get(self.index).copied().unwrap_or("");
        let color = if focused { p.selection_text } else { p.text };
        // Value right-aligned, a caret after it — the closed-drop-down look.
        c.draw_text_in(inner, value, theme.fonts.body, color, Align::End);
    }

    /// Draw the open popup over `area` (a list of the options). No-op when closed. Records the
    /// viewport and row heights so the next key press can scroll correctly.
    pub fn draw_popup(&mut self, c: &mut Canvas<'_>, area: Rect, theme: &Theme<'_>, options: &[&str]) {
        if !self.open || area.is_empty() {
            return;
        }
        let p = &theme.palette;
        self.row_h = theme.metrics.row_h;
        self.view_h = area.height();
        crate::paint::frame_sunken(c, area, p.bg.mid(), p.divider);
        c.fill_rect(area.inset(1), p.bg.mid());

        let rows = Uniform { count: options.len(), height: self.row_h };
        self.list.draw_visible(c, &rows, area, |c, i, row| {
            if i == self.list.selected {
                crate::chrome::selection(c, row, theme);
            }
            let color = if i == self.list.selected { p.selection_text } else { p.text };
            let cell = row.inset_xy(theme.metrics.pad, 0);
            c.draw_text_in(cell, options[i], theme.fonts.body, color, Align::Start);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;
    use crate::theme::Palette;

    const OPTS: &[&str] = &["Dark", "Light", "S60", "IRC"];

    fn ev(key: Key) -> KeyEvent {
        KeyEvent::new(key)
    }

    #[test]
    fn opens_navigates_and_commits() {
        let mut s = Select::new(0);
        // Give it a popup geometry so navigation can scroll.
        testing::with_theme(Palette::DARK, |th| {
            testing::with_canvas(symbian_gfx::Size::new(320, 200), |c| {
                s.open = true;
                s.draw_popup(c, symbian_gfx::Rect::from_xywh(0, 20, 320, 160), th, OPTS);
            });
        });
        assert!(s.is_open());
        s.handle_key(ev(Key::Down), OPTS);
        s.handle_key(ev(Key::Down), OPTS);
        let (h, a) = s.handle_key(ev(Key::Select), OPTS);
        assert_eq!(h, Handled::Consumed);
        assert_eq!(a, SelectAction::Changed(2));
        assert_eq!(s.selected(), 2);
        assert!(!s.is_open());
    }

    #[test]
    fn the_back_softkey_closes_the_popup_and_keeps_the_old_value() {
        // Same defect as the app picker's, in the sibling widget: the open popup is modal and its
        // catch-all ate the Back softkey. `Viewer` already handled it, which is how three modal
        // widgets ended up with three different answers to "what closes this".
        let mut s = Select::new(1);
        s.handle_key(ev(Key::Select), OPTS);
        s.handle_key(ev(Key::Down), OPTS);
        let (h, a) = s.handle_key(ev(Key::Softkey(Softkey::Right)), OPTS);
        assert_eq!(h, Handled::Consumed);
        assert_eq!(a, SelectAction::None);
        assert_eq!(s.selected(), 1, "cancelling must not commit the highlight");
        assert!(!s.is_open());
    }

    #[test]
    fn back_is_ignored_while_the_popup_is_closed() {
        // The closed field must let Back through, or a screen with a dropdown on it could never be
        // left. Consuming it here would trade one stuck screen for another.
        let mut s = Select::new(1);
        let (h, _) = s.handle_key(ev(Key::Softkey(Softkey::Right)), OPTS);
        assert_eq!(h, Handled::Ignored);
    }

    #[test]
    fn cancel_keeps_the_old_value() {
        let mut s = Select::new(1);
        s.handle_key(ev(Key::Select), OPTS); // open
        s.handle_key(ev(Key::Down), OPTS); // move highlight
        let (_, a) = s.handle_key(ev(Key::End), OPTS); // cancel
        assert_eq!(a, SelectAction::None);
        assert_eq!(s.selected(), 1);
        assert!(!s.is_open());
    }

    #[test]
    fn closed_ignores_navigation() {
        let mut s = Select::new(0);
        let (h, _) = s.handle_key(ev(Key::Up), OPTS);
        assert_eq!(h, Handled::Ignored);
    }

    #[test]
    fn draws_closed_and_open_in_every_palette() {
        for (_, palette) in Palette::ALL {
            let mut s = Select::new(2);
            let (_, px) = testing::with_canvas(symbian_gfx::Size::new(320, 200), |c| {
                testing::with_theme(palette, |th| {
                    s.draw(c, symbian_gfx::Rect::from_xywh(0, 0, 320, 20), th, OPTS, true);
                    s.open = true;
                    s.draw_popup(c, symbian_gfx::Rect::from_xywh(0, 20, 320, 160), th, OPTS);
                });
            });
            assert!(px.iter().any(|&p| p != 0));
        }
    }
}
