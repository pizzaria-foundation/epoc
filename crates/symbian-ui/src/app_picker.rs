//! A filter-as-you-type picker drawer over a caller-supplied list of items.
//!
//! Built for the one job the keypad-era `Select` popup is bad at: choosing one entry out of a long,
//! flat list — the 30+ installed apps a launcher offers when a hardware button is bound to "open an
//! app". Cycling a `Select` through thirty options one press at a time is unusable; a QWERTY device
//! can do far better. So this widget shows the whole list, and every printable key the window server
//! translates narrows it by substring, case-insensitively, against each item's label. Backspace
//! widens it again.
//!
//! Like [`crate::select::Select`] it ships no text and stores nothing about the items: the caller
//! passes the [`Item`] slice on every call, and the picker holds only the filter string, the list
//! cursor, and the last-drawn geometry so a key press can scroll without being handed a rect. It is
//! a modal overlay — while shown it consumes every key — so the screen behind it simply routes all
//! input here and draws it last, the same way a dialog is layered.

use alloc::string::String;
use alloc::vec::Vec;

use symbian_gfx::{Align, Canvas, Rect};

use crate::input::{Handled, Key, KeyEvent};
use crate::list::{ListState, Uniform};
use crate::theme::Theme;

/// One pickable row: an opaque id the caller cares about, the label the user reads and filters on,
/// and an optional icon. `id` is deliberately untyped (a launcher passes an app UID3) so the widget
/// stays free of any app-specific type.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Item<'a> {
    pub id: u32,
    pub label: &'a str,
    /// `Some(seed)` draws the seeded [`crate::letter_tile`] (the launcher's fake app icon) at the
    /// left of the row; `None` leaves the icon gutter blank — used to set non-app "command" rows
    /// apart from real apps. The seed is usually the same id.
    pub tile: Option<u32>,
}

impl<'a> Item<'a> {
    /// A plain row with no icon.
    pub const fn new(id: u32, label: &'a str) -> Self {
        Self { id, label, tile: None }
    }

    /// A row that draws the seeded letter-tile icon (pass the app UID as the seed for a stable hue).
    pub const fn with_tile(id: u32, label: &'a str, seed: u32) -> Self {
        Self { id, label, tile: Some(seed) }
    }
}

/// What a key did to the picker.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum PickerAction {
    /// The user committed the highlighted row; carries its [`Item::id`].
    Picked(u32),
    /// The user backed out (red/Escape, or Backspace on an empty filter). Nothing chosen.
    Cancelled,
    /// The filter or the selection changed, but nothing was committed or cancelled yet.
    None,
}

/// A filter-as-you-type list drawer. Owns the filter text and the cursor; the items are the
/// caller's and are passed in on every call.
#[derive(Clone, Debug, Default)]
pub struct AppPicker {
    filter: String,
    list: ListState,
    /// Viewport height and row height from the last draw, so key handling can scroll the filtered
    /// list without being handed a rect. Zero until first drawn.
    view_h: i32,
    row_h: i32,
}

impl AppPicker {
    /// Open fresh: empty filter, the whole list shown, the cursor at the top.
    pub fn new() -> Self {
        Self::default()
    }

    /// The current filter text, for a caller that wants to show it in a title.
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// The indices of `items` that pass the current filter, in the incoming order (which the caller
    /// has already sorted — the picker never re-sorts). An empty filter matches everything.
    ///
    /// Substring, case-insensitive: typing "ap" finds both "Maps" and "Apps". Lower-casing both
    /// sides each call is cheap at the few dozen entries this ever holds, and avoids caring about
    /// where in the label the match falls.
    pub fn matches(&self, items: &[Item<'_>]) -> Vec<usize> {
        if self.filter.is_empty() {
            return (0..items.len()).collect();
        }
        let needle = self.filter.to_lowercase();
        items
            .iter()
            .enumerate()
            .filter(|(_, it)| it.label.to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect()
    }

    /// The id of the row currently highlighted under the active filter, if any. Lets a caller act on
    /// the selection without committing it — e.g. a recent-apps list killing the highlighted app on
    /// Delete. Returns `None` when the filter matches nothing.
    pub fn current(&self, items: &[Item<'_>]) -> Option<u32> {
        let m = self.matches(items);
        m.get(self.list.selected.min(m.len().saturating_sub(1))).map(|&i| items[i].id)
    }

    /// Route a key. Printable characters extend the filter; Backspace shortens it (and, on an empty
    /// filter, backs out); Up/Down move within the filtered rows; Select/Enter commit the highlight;
    /// the red key cancels. Everything else is swallowed — the drawer is modal, so no stray key may
    /// leak to the screen behind it.
    pub fn handle_key(&mut self, ev: KeyEvent, items: &[Item<'_>]) -> (Handled, PickerAction) {
        match ev.key {
            Key::Char(ch) if !ch.is_control() => {
                self.filter.push(ch);
                // A narrower list invalidates the old cursor position; start again at the top.
                self.list.selected = 0;
                self.list.scroll = 0;
                (Handled::Consumed, PickerAction::None)
            }
            Key::Backspace => {
                if self.filter.pop().is_none() {
                    return (Handled::Consumed, PickerAction::Cancelled);
                }
                self.reclamp(items);
                (Handled::Consumed, PickerAction::None)
            }
            Key::End => (Handled::Consumed, PickerAction::Cancelled),
            Key::Up | Key::Down => {
                let n = self.matches(items).len();
                let rows = Uniform { count: n, height: self.row_h.max(1) };
                let delta = if matches!(ev.key, Key::Up) { -1 } else { 1 };
                self.list.move_selection(delta, &rows, self.view_h.max(1));
                (Handled::Consumed, PickerAction::None)
            }
            Key::Select | Key::Enter => {
                let m = self.matches(items);
                match m.get(self.list.selected.min(m.len().saturating_sub(1))) {
                    Some(&i) => (Handled::Consumed, PickerAction::Picked(items[i].id)),
                    // No matches: there is nothing to commit, so stay open rather than pick nothing.
                    None => (Handled::Consumed, PickerAction::None),
                }
            }
            // Modal: eat the rest.
            _ => (Handled::Consumed, PickerAction::None),
        }
    }

    /// Pull the cursor and scroll back into range after the filter widened or narrowed.
    fn reclamp(&mut self, items: &[Item<'_>]) {
        let n = self.matches(items).len();
        let rows = Uniform { count: n, height: self.row_h.max(1) };
        self.list.clamp(&rows, self.view_h.max(1));
    }

    /// Draw the drawer filling `area`: a search-field header showing the filter, then the filtered
    /// list (or a "no matches" line). Records the viewport and row heights so the next key press
    /// scrolls correctly. `no_match` is the caller's text for the empty-result state (the crate
    /// ships no English).
    pub fn draw(
        &mut self,
        c: &mut Canvas<'_>,
        area: Rect,
        theme: &Theme<'_>,
        items: &[Item<'_>],
        no_match: &str,
    ) {
        if area.is_empty() {
            return;
        }
        let p = &theme.palette;
        let m = &theme.metrics;
        self.row_h = m.row_h;

        // A raised drawer over whatever is behind it.
        crate::paint::frame_sunken(c, area, p.bg.mid(), p.divider);
        c.fill_rect(area.inset(1), p.bg.mid());

        // The search-field header carries the filter as its own band, so it reads as a field above
        // the list rather than as the first row of it.
        let inner = area.inset(1);
        let (header, list_area) = inner.split_top(m.row_h);
        crate::paint::band(c, header, &p.chrome);
        let hcell = header.inset_xy(m.pad, 0);
        if self.filter.is_empty() {
            c.draw_text_in(hcell, "Type to filter", theme.fonts.body, p.dim, Align::Start);
        } else {
            c.draw_text_in(hcell, &self.filter, theme.fonts.strong, p.chrome_text, Align::Start);
            // A caret so the field reads as live text entry.
            let w = theme.fonts.strong.measure(&self.filter);
            let caret = Rect::from_xywh(hcell.x0 + w + 1, hcell.y0 + 3, 1, hcell.height() - 6);
            c.fill_rect(caret, p.accent);
        }

        self.view_h = list_area.height();
        let m_idx = self.matches(items);
        if m_idx.is_empty() {
            crate::chrome::placeholder(c, list_area, theme, no_match);
            return;
        }
        // Keep the cursor valid against the freshly filtered set before drawing the highlight.
        let rows = Uniform { count: m_idx.len(), height: self.row_h };
        self.list.clamp(&rows, self.view_h);
        let sel = self.list.selected;
        self.list.for_visible(&rows, list_area, |row_i, row| {
            if row_i == sel {
                crate::chrome::selection(c, row, theme);
            }
            let color = if row_i == sel { p.selection_text } else { p.text };
            let it = &items[m_idx[row_i]];
            // A fixed icon gutter down the left so app rows (with a tile) and command rows (without)
            // keep their labels aligned. An app draws its seeded letter-tile there; a command leaves
            // it blank.
            let cell = row.inset_xy(m.pad, 0);
            let gutter = row.height();
            let (icon_area, text_area) = (
                Rect::from_xywh(cell.x0, cell.y0, gutter, cell.height()),
                Rect { x0: cell.x0 + gutter + m.pad, ..cell },
            );
            if let Some(seed) = it.tile {
                let side = (icon_area.height() - 4).clamp(4, gutter);
                let iy = icon_area.y0 + (icon_area.height() - side) / 2;
                crate::tile::letter_tile(c, Rect::from_xywh(icon_area.x0, iy, side, side), it.label, seed, theme);
            }
            c.draw_text_in(text_area, it.label, theme.fonts.body, color, Align::Start);
        });
        crate::chrome::scrollbar(c, list_area, theme, self.list.scrollbar(&rows, self.view_h));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;
    use crate::theme::Palette;

    fn items<'a>() -> Vec<Item<'a>> {
        alloc::vec![
            Item::new(0x01, "Calculator"),
            Item::new(0x02, "Calendar"),
            Item::new(0x03, "Camera"),
            Item::new(0x04, "Maps"),
            Item::new(0x05, "Messaging"),
            Item::new(0x06, "Web"),
        ]
    }

    fn ev(key: Key) -> KeyEvent {
        KeyEvent::new(key)
    }

    /// Give the picker a real geometry so navigation can scroll.
    fn lay_out(p: &mut AppPicker, its: &[Item<'_>]) {
        testing::with_theme(Palette::DARK, |th| {
            testing::with_canvas(symbian_gfx::Size::new(320, 200), |c| {
                p.draw(c, symbian_gfx::Rect::from_xywh(0, 0, 320, 200), th, its, "No matches");
            });
        });
    }

    #[test]
    fn empty_filter_shows_everything() {
        let p = AppPicker::new();
        assert_eq!(p.matches(&items()).len(), 6);
    }

    #[test]
    fn typing_narrows_the_list() {
        let its = items();
        let mut p = AppPicker::new();
        for ch in "cal".chars() {
            p.handle_key(ev(Key::Char(ch)), &its);
        }
        let m = p.matches(&its);
        // "Calculator" and "Calendar", not "Camera".
        assert_eq!(m.len(), 2);
        assert_eq!(its[m[0]].label, "Calculator");
        assert_eq!(its[m[1]].label, "Calendar");
    }

    #[test]
    fn filtering_is_case_insensitive_and_substring() {
        let its = items();
        let mut p = AppPicker::new();
        for ch in "AME".chars() {
            p.handle_key(ev(Key::Char(ch)), &its);
        }
        // "AME" matches "Camera" (substring, any case) — not a prefix.
        let m = p.matches(&its);
        assert_eq!(m.len(), 1);
        assert_eq!(its[m[0]].label, "Camera");
    }

    #[test]
    fn backspace_widens_again() {
        let its = items();
        let mut p = AppPicker::new();
        for ch in "cam".chars() {
            p.handle_key(ev(Key::Char(ch)), &its);
        }
        assert_eq!(p.matches(&its).len(), 1);
        p.handle_key(ev(Key::Backspace), &its); // "ca"
        assert_eq!(p.matches(&its).len(), 3, "Calculator, Calendar, Camera");
        assert_eq!(p.filter(), "ca");
    }

    #[test]
    fn up_down_move_within_the_filtered_set_and_select_returns_its_id() {
        let its = items();
        let mut p = AppPicker::new();
        for ch in "ca".chars() {
            p.handle_key(ev(Key::Char(ch)), &its);
        }
        lay_out(&mut p, &its); // hand it a viewport
        // Filtered: Calculator(0), Calendar(1), Camera(2). Down twice -> Camera.
        p.handle_key(ev(Key::Down), &its);
        let (h, a) = p.handle_key(ev(Key::Down), &its);
        assert_eq!(h, Handled::Consumed);
        assert_eq!(a, PickerAction::None);
        let (_, picked) = p.handle_key(ev(Key::Select), &its);
        assert_eq!(picked, PickerAction::Picked(0x03), "Camera's id");
    }

    #[test]
    fn enter_also_commits() {
        let its = items();
        let mut p = AppPicker::new();
        lay_out(&mut p, &its);
        let (_, a) = p.handle_key(ev(Key::Enter), &its);
        assert_eq!(a, PickerAction::Picked(0x01), "top of the unfiltered list");
    }

    #[test]
    fn red_key_cancels() {
        let its = items();
        let mut p = AppPicker::new();
        let (h, a) = p.handle_key(ev(Key::End), &its);
        assert_eq!(h, Handled::Consumed);
        assert_eq!(a, PickerAction::Cancelled);
    }

    #[test]
    fn backspace_on_an_empty_filter_backs_out() {
        let its = items();
        let mut p = AppPicker::new();
        let (_, a) = p.handle_key(ev(Key::Backspace), &its);
        assert_eq!(a, PickerAction::Cancelled);
    }

    #[test]
    fn rows_with_a_tile_draw_the_icon_without_panicking() {
        let its = alloc::vec![
            Item::new(0x01, "Settings option"),      // command row, no icon
            Item::with_tile(0x02, "Calculator", 0x02), // app row, letter tile
        ];
        let mut p = AppPicker::new();
        let (_, px) = testing::with_canvas(symbian_gfx::Size::new(320, 200), |c| {
            testing::with_theme(Palette::DARK, |th| {
                p.draw(c, symbian_gfx::Rect::from_xywh(0, 0, 320, 200), th, &its, "No matches");
            });
        });
        assert!(px.iter().any(|&v| v != 0));
    }

    #[test]
    fn current_tracks_the_highlight_under_the_filter() {
        let its = items();
        let mut p = AppPicker::new();
        assert_eq!(p.current(&its), Some(0x01), "top of the unfiltered list");
        for ch in "ca".chars() {
            p.handle_key(ev(Key::Char(ch)), &its);
        }
        lay_out(&mut p, &its);
        p.handle_key(ev(Key::Down), &its); // Calculator -> Calendar
        assert_eq!(p.current(&its), Some(0x02), "Calendar under filter 'ca'");
        // A filter that matches nothing has no current row.
        for ch in "zzz".chars() {
            p.handle_key(ev(Key::Char(ch)), &its);
        }
        assert_eq!(p.current(&its), None);
    }

    #[test]
    fn no_matches_is_a_graceful_dead_end() {
        let its = items();
        let mut p = AppPicker::new();
        for ch in "zzz".chars() {
            p.handle_key(ev(Key::Char(ch)), &its);
        }
        assert!(p.matches(&its).is_empty());
        lay_out(&mut p, &its);
        // Select cannot commit nothing: it stays open, reporting None.
        let (h, a) = p.handle_key(ev(Key::Select), &its);
        assert_eq!(h, Handled::Consumed);
        assert_eq!(a, PickerAction::None);
    }

    #[test]
    fn selection_does_not_dangle_when_the_filter_narrows() {
        let its = items();
        let mut p = AppPicker::new();
        lay_out(&mut p, &its);
        // Move to the last row of the full list.
        for _ in 0..5 {
            p.handle_key(ev(Key::Down), &its);
        }
        // Narrow hard so far fewer rows remain, then commit: must land on a real row, not panic.
        for ch in "cal".chars() {
            p.handle_key(ev(Key::Char(ch)), &its);
        }
        lay_out(&mut p, &its);
        let (_, a) = p.handle_key(ev(Key::Select), &its);
        assert!(matches!(a, PickerAction::Picked(_)));
    }

    #[test]
    fn draws_in_every_palette_filtered_and_empty() {
        for (_, palette) in Palette::ALL {
            let its = items();
            let mut p = AppPicker::new();
            let (_, px) = testing::with_canvas(symbian_gfx::Size::new(320, 200), |c| {
                testing::with_theme(palette, |th| {
                    p.draw(c, symbian_gfx::Rect::from_xywh(0, 20, 320, 160), th, &its, "No matches");
                });
            });
            assert!(px.iter().any(|&v| v != 0));

            // And the empty-result state must draw something too.
            for ch in "zzz".chars() {
                p.handle_key(ev(Key::Char(ch)), &its);
            }
            let (_, px2) = testing::with_canvas(symbian_gfx::Size::new(320, 200), |c| {
                testing::with_theme(palette, |th| {
                    p.draw(c, symbian_gfx::Rect::from_xywh(0, 20, 320, 160), th, &its, "No matches");
                });
            });
            assert!(px2.iter().any(|&v| v != 0));
        }
    }
}
