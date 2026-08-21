//! A multi-line text area, declared — the full-screen editor half of the text component.
//!
//! Where [`super::text_field`] is one line with horizontal scroll, this is a box that soft-wraps to
//! its width, breaks on Enter, moves the caret up and down between lines, and scrolls vertically to
//! keep the caret in view. It is the same [`edit::TextField`] buffer underneath — built with
//! `.multiline(true)` — so an app can hand a field's buffer to a `TextArea` on a bigger screen and
//! the two share one caret and one string (see the `Rc` handle in [`TextArea::with_buffer`]).
//!
//! # No wrapping arithmetic of its own
//!
//! The line breaking is [`symbian_gfx::Font::wrap`], the same routine [`super::text`] uses, which
//! already handles both explicit `\n` and greedy word wrap with a hard mid-word break for a word
//! too long to fit. This widget only turns the wrapped slices back into byte ranges (via their
//! offset within the buffer string) so the caret and selection land on the right visual line.

use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use symbian_gfx::{Canvas, Point, Rect, Size};
use symbian_ui::{edit, paint, Clipboard, Font, Handled, KeyEvent, Theme};

use crate::constraints::Constraints;
use crate::slot::SlotTable;
use crate::widget::{hash_i32, hash_str, KeyCtx, Widget, WidgetHash};

/// A multi-line editor backed by a multi-line [`edit::TextField`].
pub struct TextArea {
    state: Rc<RefCell<edit::TextField>>,
    /// Vertical scroll (first visible visual line), kept in the slot table so it survives the tree
    /// being rebuilt but not the editor leaving the screen — exactly as a caret does.
    scroll: Rc<Cell<i32>>,
    focused: bool,
    placeholder: Option<alloc::string::String>,
}

impl TextArea {
    /// An area over a buffer the caller owns. The buffer should be `.multiline(true)` for Enter and
    /// Up/Down to work; a single-line buffer still displays soft-wrapped (useful to see a long URL).
    pub fn with_buffer(slots: &mut SlotTable, buffer: Rc<RefCell<edit::TextField>>) -> Self {
        let scroll = slots.use_state_with(|| Rc::new(Cell::new(0i32))).clone();
        Self { state: buffer, scroll, focused: false, placeholder: None }
    }

    pub fn focused(mut self, on: bool) -> Self {
        self.focused = on;
        self
    }

    pub fn placeholder(mut self, s: impl Into<alloc::string::String>) -> Self {
        self.placeholder = Some(s.into());
        self
    }

    /// Offer a key to the editor. Ignored unless focused. Clipboard rides in the caller's context.
    pub fn edit(&self, ev: KeyEvent, clip: &mut dyn Clipboard) -> Handled {
        if !self.focused {
            return Handled::Ignored;
        }
        self.state.borrow_mut().handle_key(ev, clip)
    }
}

/// Byte ranges of each wrapped visual line within `display`, derived from the slices `Font::wrap`
/// hands back (they borrow `display`, so their address gives their offset). Always at least one
/// line, so an empty buffer still has a line to put the caret on.
fn wrapped_ranges(display: &str, font: &dyn Font, width: i32) -> Vec<(usize, usize)> {
    let base = display.as_ptr() as usize;
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    font.wrap(display, width.max(1), &mut |line: &str| {
        let off = line.as_ptr() as usize - base;
        ranges.push((off, off + line.len()));
    });
    if ranges.is_empty() {
        ranges.push((0, 0));
    }
    ranges
}

impl Widget for TextArea {
    fn content_hash(&self) -> WidgetHash {
        let f = self.state.borrow();
        let h = hash_str(0, f.text());
        hash_i32(h, self.focused as i32)
    }

    fn measure(&self, constraints: Constraints, _theme: &Theme<'_>) -> Size {
        // Fills whatever the parent gives it — a text area is a region, not a line.
        constraints.constrain(Size::new(constraints.max_w, constraints.max_h))
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        let p = &theme.palette;
        let body = theme.fonts.body;
        let lh = body.line_height().max(1);

        paint::band(c, rect, &p.chrome);

        let pad = 6;
        let x0 = rect.x0 + pad;
        let content_w = (rect.x1 - pad) - x0;
        let top = rect.y0 + 4;
        let visible = (((rect.y1 - top) / lh).max(1)) as usize;

        let f = self.state.borrow();
        let display = f.display();

        if display.is_empty() {
            if let Some(ph) = self.placeholder.as_deref().filter(|s| !s.is_empty()) {
                c.draw_text(Point::new(x0, top + body.ascent()), ph, body, p.dim);
            }
            if self.focused {
                c.fill_rect(Rect::new(x0, top, x0 + 1, top + lh), p.accent);
            }
            return;
        }

        let ranges = wrapped_ranges(&display, &body, content_w);
        let caret_off = f.display_offset(f.cursor()).min(display.len());
        let caret_line = ranges.iter().rposition(|&(o, _)| o <= caret_off).unwrap_or(0);

        // Scroll the caret line into view, moving as little as possible.
        let mut scroll = self.scroll.get().max(0) as usize;
        if caret_line < scroll {
            scroll = caret_line;
        } else if caret_line >= scroll + visible {
            scroll = caret_line + 1 - visible;
        }
        if ranges.len() > visible && scroll + visible > ranges.len() {
            scroll = ranges.len() - visible;
        } else if ranges.len() <= visible {
            scroll = 0;
        }
        self.scroll.set(scroll as i32);

        let sel = f.selection();
        let saved = c.save();
        c.clip_to(Rect::new(rect.x0, rect.y0, rect.x1, rect.y1));
        let mut y = top;
        for li in scroll..(scroll + visible).min(ranges.len()) {
            let (ls, le) = ranges[li];
            let line = &display[ls..le];
            if let Some((sf, st)) = sel {
                let a = sf.max(ls);
                let b = st.min(le);
                if a < b {
                    paint::text_selection(
                        c, x0, y, y + lh, line, a - ls, b - ls, body, p.selection.mid(),
                    );
                }
            }
            c.draw_text(Point::new(x0, y + body.ascent()), line, body, p.text);
            if self.focused && li == caret_line {
                // Measured from the line start; caret_off is within [ls, le] for the chosen line.
                let cx = x0 + body.measure(&display[ls..caret_off.min(le)]);
                c.fill_rect(Rect::new(cx, y, cx + 1, y + lh), p.accent);
            }
            y += lh;
        }
        c.restore(saved);
    }

    fn handle_key(&self, ev: KeyEvent, _rect: Rect, cx: &mut KeyCtx<'_>) -> Handled {
        self.edit(ev, cx.clip)
    }
}
