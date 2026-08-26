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

/// How tall the value is inside a band `band_h` pixels high: one line of body text, never the band.
///
/// The counterpart of [`crate::switch_height`] and [`crate::stepper::stepper_height`], and here for
/// the same reason they are: `symbian_decl_ui`'s `Select` draws only the value, and a list row
/// places it with `CrossAlign::Stretch` — which hands it the whole 38-pixel band. A value that
/// claimed the band would still *look* right, because
/// [`Canvas::draw_text_in`](symbian_gfx::Canvas::draw_text_in) centres within whatever it is given,
/// and would have lied to every caller that asked how big it is.
///
/// Clamped down to the band, so a select in a band shorter than a line draws where it drew before
/// this function existed.
pub fn value_height(band_h: i32, theme: &Theme<'_>) -> i32 {
    theme.fonts.body.line_height().min(band_h.max(0))
}

/// Where the value sits inside `band`: the band's full width, one line, centred across it.
///
/// Extracted so the two painters cannot disagree, exactly as [`crate::switch_track`] was.
/// [`Select::draw`] paints a whole settings row and `symbian_decl_ui`'s `Select` paints only the
/// value; before this the arithmetic lived inside the first one, so the second would have been a
/// second copy of it, agreeing on the day it was written and on no day after.
pub fn value_box(band: Rect, theme: &Theme<'_>) -> Rect {
    let h = value_height(band.height(), theme);
    Rect::from_xywh(band.x0, band.y0 + (band.height() - h) / 2, band.width(), h)
}

/// How much room the value needs: the widest option, never the current one.
///
/// The same rule [`crate::stepper::STEPPER_W`] states as a constant, arrived at by measuring
/// because a select's options are the caller's words and no constant could cover them. A field
/// sized to the *chosen* option changes width when the choice changes, and the symptom is the
/// label beside it shuffling sideways every time the user commits — the defect `STEPPER_W` exists
/// to prevent, in the one widget that cannot prevent it with a number.
///
/// It is also what lets a declarative select keep the chosen index out of its `content_hash`:
/// the size does not depend on it.
pub fn value_width(options: &[&str], theme: &Theme<'_>) -> i32 {
    options.iter().map(|s| theme.fonts.body.measure(s)).max().unwrap_or(0)
}

/// Paint `label` right-aligned in exactly `slot` — the closed drop-down.
///
/// Takes the slot rather than the band, so a caller that has already reserved room for a caption to
/// the left passes what it reserved instead of trusting this to reach the same answer twice.
///
/// `focused` picks the selection ink, because the row underneath may be carrying the selection band
/// and text in the resting colour on top of it is very nearly invisible.
pub fn draw_value(c: &mut Canvas<'_>, slot: Rect, theme: &Theme<'_>, label: &str, focused: bool) {
    let p = &theme.palette;
    let ink = if focused { p.selection_text } else { p.text };
    c.draw_text_in(slot, label, theme.fonts.body, ink, Align::End);
}

/// How tall an open popup wants to be for `count` options: every row, plus the sunken frame's
/// one-pixel border on each side.
pub fn popup_height(count: usize, theme: &Theme<'_>) -> i32 {
    count as i32 * theme.metrics.row_h + 2
}

/// Where an open popup sits inside the band `area` it was given: full width, against the bottom.
///
/// Against the bottom rather than under the field, and that is a deliberate limitation with a
/// reason. A popup anchored to its field has to *know* where its field is, and in a declarative
/// tree the popup is a sibling at the screen level — it is not inside the row, because an ancestor
/// that clips still clips and a row inside a scrolling list is clipped to the list's band. So the
/// popup is given the content band and nothing else, and the only two anchors available to it are
/// the band's edges. The bottom is the one S60 uses for a list query, which is what this is.
///
/// Shorter than the band when the options fit, so the screen behind stays visible — the S60 look —
/// and clamped to the band when they do not, because a popup that grew past its band would paint
/// over the title bar.
pub fn popup_box(area: Rect, count: usize, theme: &Theme<'_>) -> Rect {
    if area.is_empty() {
        return Rect::from_xywh(area.x0, area.y0, 0, 0);
    }
    let h = popup_height(count, theme).min(area.height()).max(0);
    Rect::from_xywh(area.x0, area.y1 - h, area.width(), h)
}

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

    /// Which option the open popup is highlighting — not the chosen one, which only a commit
    /// changes.
    ///
    /// Public so a caller that keeps the popup's state somewhere else can assert on the highlight
    /// without committing it. The distinction is the whole of what cancelling means: the highlight
    /// moved and the choice did not.
    pub fn highlight(&self) -> usize {
        self.list.selected
    }

    /// Tell it the popup's viewport and row height before a key arrives.
    ///
    /// [`draw_popup`](Self::draw_popup) records both, which is enough for a screen that always
    /// paints before it reads a key. It is not enough for a caller that knows the band *without*
    /// having painted it — `symbian_decl_ui`'s popup is handed its rect on every key dispatch — and
    /// the difference is visible: opened and scrolled before the first paint, the viewport is one
    /// pixel and `clamp_scroll` pins the offset to the top of the highlighted row, so the frame
    /// that follows shows the list scrolled to the bottom with blank rows under it.
    pub fn set_popup_metrics(&mut self, view_h: i32, row_h: i32) {
        self.view_h = view_h;
        self.row_h = row_h;
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
        if focused {
            crate::chrome::selection(c, r, theme);
        }
        let inner = r.inset_xy(theme.metrics.pad, 0);
        let value = options.get(self.index).copied().unwrap_or("");
        // Value right-aligned — the closed-drop-down look. Routed through the free functions so
        // that this row and `symbian_decl_ui`'s value-only widget are one painter and not two.
        draw_value(c, value_box(inner, theme), theme, value, focused);
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
    fn the_value_is_one_line_and_not_the_band_it_sits_in() {
        // The `Stretch` trap, from the primitive's end: `symbian_decl_ui`'s select is handed the
        // whole 38-pixel row band by `CrossAlign::Stretch`, and a value box that took the band would
        // still look right — `draw_text_in` centres in whatever it is given — while lying to every
        // caller that asked how tall it is.
        testing::with_theme(Palette::DARK, |th| {
            let band = symbian_gfx::Rect::from_xywh(0, 0, 120, 38);
            let slot = value_box(band, th);
            assert_eq!(slot.height(), th.fonts.body.line_height());
            assert!(slot.height() < band.height());
            assert_eq!(slot.y0, (38 - slot.height()) / 2, "and centred in the band");
            assert_eq!((slot.x0, slot.x1), (band.x0, band.x1), "the width is the band's");
            // A band shorter than a line clamps rather than overflowing upward.
            let squashed = value_box(symbian_gfx::Rect::from_xywh(0, 0, 120, 4), th);
            assert_eq!(squashed.height(), 4);
        });
    }

    #[test]
    fn the_field_reserves_the_widest_option_and_not_the_chosen_one() {
        // What keeps a caption from shuffling sideways when the user commits — `STEPPER_W`'s rule,
        // measured because a select's words are the caller's.
        testing::with_theme(Palette::DARK, |th| {
            let widest = OPTS.iter().map(|s| th.fonts.body.measure(s)).max().unwrap();
            assert_eq!(value_width(OPTS, th), widest);
            assert!(value_width(OPTS, th) > th.fonts.body.measure("S60"));
            assert_eq!(value_width(&[], th), 0, "no options is no reservation, not a panic");
        });
    }

    #[test]
    fn the_popup_sits_against_the_bottom_of_its_band_and_never_past_the_top() {
        testing::with_theme(Palette::DARK, |th| {
            let band = symbian_gfx::Rect::from_xywh(0, 20, 320, 200);
            let r = popup_box(band, 3, th);
            assert_eq!(r.y1, band.y1, "not anchored to the bottom");
            assert_eq!(r.height(), 3 * th.metrics.row_h + 2);
            assert!(r.y0 > band.y0, "it filled the band with room to spare");
            assert_eq!((r.x0, r.x1), (band.x0, band.x1));
            // More options than fit: clamped to the band, because a popup that grew past it would
            // paint over the title bar.
            let tall = popup_box(band, 40, th);
            assert_eq!(tall.y0, band.y0);
            assert_eq!(tall.height(), band.height());
            // An empty band is not a negative rect.
            assert!(popup_box(symbian_gfx::Rect::from_xywh(0, 0, 0, 0), 3, th).is_empty());
        });
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
