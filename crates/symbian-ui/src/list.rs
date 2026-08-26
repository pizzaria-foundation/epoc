//! Scrolling, selection and row virtualisation.
//!
//! Split from drawing on purpose: all the arithmetic that is easy to get subtly
//! wrong — minimal scrolling, clamping, which rows are actually on screen — lives
//! here, is pure, and is unit-tested. Widgets on top only draw.
//!
//! Row heights are queried through [`Rows`] rather than assumed uniform, because
//! a chat transcript needs per-message heights while a contact list does not.
//! Offsets are walked rather than cached: at the few hundred rows this device will
//! ever hold, an O(n) walk over an array of heights is far cheaper than
//! maintaining a prefix-sum tree, and it cannot go stale.

use crate::input::{Handled, Key, KeyEvent};
use symbian_gfx::{Canvas, Rect};

pub trait Rows {
    fn len(&self) -> usize;
    /// Height of row `index` in pixels. Must be >= 0 and stable for a given state.
    fn height(&self, index: usize) -> i32;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Rows that are all the same height.
#[derive(Copy, Clone, Debug)]
pub struct Uniform {
    pub count: usize,
    pub height: i32,
}

impl Rows for Uniform {
    fn len(&self) -> usize {
        self.count
    }
    fn height(&self, _: usize) -> i32 {
        self.height
    }
}

/// Rows whose heights come from a slice.
impl Rows for [i32] {
    fn len(&self) -> usize {
        <[i32]>::len(self)
    }
    fn height(&self, index: usize) -> i32 {
        self[index]
    }
}

#[derive(Copy, Clone, Debug, Default)]
pub struct ListState {
    /// Index of the focused row. Meaningless when the list is empty.
    pub selected: usize,
    /// Pixels of content scrolled past the top of the viewport. Never negative,
    /// never past the end — maintained by [`ListState::clamp_scroll`].
    pub scroll: i32,
}

impl ListState {
    pub const fn new() -> Self {
        Self { selected: 0, scroll: 0 }
    }

    pub fn content_height<R: Rows + ?Sized>(rows: &R) -> i32 {
        (0..rows.len()).map(|i| rows.height(i)).sum()
    }

    /// Distance from the top of the content to the top of row `index`.
    pub fn row_top<R: Rows + ?Sized>(rows: &R, index: usize) -> i32 {
        (0..index.min(rows.len())).map(|i| rows.height(i)).sum()
    }

    /// The row containing content-space `y`, if any.
    pub fn row_at<R: Rows + ?Sized>(rows: &R, y: i32) -> Option<usize> {
        if y < 0 {
            return None;
        }
        let mut acc = 0;
        for i in 0..rows.len() {
            acc += rows.height(i);
            if y < acc {
                return Some(i);
            }
        }
        None
    }

    /// Pull `scroll` back into range. Content shorter than the viewport pins to
    /// the top rather than leaving a gap above.
    pub fn clamp_scroll<R: Rows + ?Sized>(&mut self, rows: &R, viewport_h: i32) {
        let max = (Self::content_height(rows) - viewport_h).max(0);
        self.scroll = self.scroll.clamp(0, max);
    }

    /// Bring both the selection and the scroll offset back into range after the
    /// row set changed underneath us — messages deleted, a search filtered the
    /// list. Call this whenever `rows` may have shrunk; clamping only the scroll
    /// leaves `selected` dangling past the end, which shows up later as a list
    /// with no visible highlight.
    pub fn clamp<R: Rows + ?Sized>(&mut self, rows: &R, viewport_h: i32) {
        self.selected = self.selected.min(rows.len().saturating_sub(1));
        self.clamp_scroll(rows, viewport_h);
    }

    /// Scroll the minimum amount needed to bring the selected row fully into
    /// view. A row taller than the viewport aligns to its top, since showing the
    /// start of an over-long message is more useful than showing its end.
    pub fn ensure_visible<R: Rows + ?Sized>(&mut self, rows: &R, viewport_h: i32) {
        if rows.is_empty() || viewport_h <= 0 {
            self.scroll = 0;
            return;
        }
        // Write the clamp back rather than only using it locally: a `selected`
        // that silently disagrees with `rows` is how stale indices survive.
        self.selected = self.selected.min(rows.len() - 1);
        let i = self.selected;
        let top = Self::row_top(rows, i);
        let bottom = top + rows.height(i);

        if top < self.scroll {
            self.scroll = top;
        } else if bottom > self.scroll + viewport_h {
            self.scroll = (bottom - viewport_h).min(top);
        }
        self.clamp_scroll(rows, viewport_h);
    }

    /// Move the selection, clamping at both ends. Returns whether it moved.
    ///
    /// No wraparound: on a device where the same D-pad drives every list, silently
    /// jumping from the last row to the first is the kind of surprise that makes
    /// people lose their place.
    pub fn move_selection<R: Rows + ?Sized>(
        &mut self,
        delta: isize,
        rows: &R,
        viewport_h: i32,
    ) -> bool {
        if rows.is_empty() {
            return false;
        }
        let last = rows.len() - 1;
        let want = (self.selected as isize).saturating_add(delta).clamp(0, last as isize) as usize;
        let moved = want != self.selected;
        self.selected = want;
        self.ensure_visible(rows, viewport_h);
        moved
    }

    pub fn select<R: Rows + ?Sized>(&mut self, index: usize, rows: &R, viewport_h: i32) {
        if rows.is_empty() {
            return;
        }
        self.selected = index.min(rows.len() - 1);
        self.ensure_visible(rows, viewport_h);
    }

    /// Park at the bottom and select the last row — what a chat transcript wants
    /// on open, and after sending.
    pub fn scroll_to_end<R: Rows + ?Sized>(&mut self, rows: &R, viewport_h: i32) {
        if rows.is_empty() {
            self.selected = 0;
            self.scroll = 0;
            return;
        }
        self.selected = rows.len() - 1;
        self.scroll = (Self::content_height(rows) - viewport_h).max(0);
    }

    /// How many whole rows fit, used to size a page jump.
    fn page<R: Rows + ?Sized>(rows: &R, viewport_h: i32) -> isize {
        if rows.is_empty() {
            return 1;
        }
        let avg = (Self::content_height(rows) / rows.len() as i32).max(1);
        ((viewport_h / avg) as isize - 1).max(1)
    }

    /// Standard up/down/page handling. Returns `Ignored` for anything else so the
    /// caller can layer its own bindings on top.
    pub fn handle_key<R: Rows + ?Sized>(
        &mut self,
        ev: KeyEvent,
        rows: &R,
        viewport_h: i32,
    ) -> Handled {
        let delta = match ev.key {
            Key::Up => -1,
            Key::Down => 1,
            Key::Left => -Self::page(rows, viewport_h),
            Key::Right => Self::page(rows, viewport_h),
            _ => return Handled::Ignored,
        };
        self.move_selection(delta, rows, viewport_h);
        Handled::Consumed
    }

    /// Draw each visible row, clipped to the viewport.
    ///
    /// This is [`for_visible`](Self::for_visible) with the canvas passed through and the clip
    /// applied, and it is what a *drawing* caller should use. Every one of them wants the clip and
    /// none of them can be trusted to remember it — the count when this was added was eight loops
    /// across the SDK and the launcher with no clip at all, and two in the Telegram client that had
    /// hand-rolled the same `clip_to` at their own call sites. See [`for_visible`](Self::for_visible)
    /// for why an unclipped row rect is correct and dangerous at the same time.
    ///
    /// The canvas is a parameter of the closure rather than something it captures, because the
    /// method needs it too and the borrow checker is right to refuse both.
    ///
    /// ```ignore
    /// self.list.draw_visible(c, &rows, area, |c, i, row| {
    ///     if i == sel { chrome::selection(c, row, theme); }
    ///     c.draw_text_in(row.inset_xy(pad, 0), label, theme.fonts.body, colour, Align::Start);
    /// });
    /// ```
    pub fn draw_visible<R: Rows + ?Sized>(
        &self,
        c: &mut Canvas<'_>,
        rows: &R,
        viewport: Rect,
        mut f: impl FnMut(&mut Canvas<'_>, usize, Rect),
    ) {
        let saved = c.save();
        c.clip_to(viewport);
        self.for_visible(rows, viewport, |i, r| f(c, i, r));
        c.restore(saved);
    }

    /// Call `f` for each row intersecting the viewport, with the row's rect in
    /// viewport coordinates — already shifted by the scroll offset, so a widget
    /// can draw straight into it.
    ///
    /// Rows are visited in order and stop as soon as one starts past the bottom,
    /// so cost is proportional to what is on screen plus the walk to reach it.
    ///
    /// # The rect can start above the viewport, and that is on purpose
    ///
    /// A partially-visible first row *is* partly above the band, and a caller that wants to draw
    /// the visible sliver of it needs to know where the whole row would have gone — text has to be
    /// positioned against the row it belongs to, not against the edge that cut it. So this hands
    /// out the true rect and leaves the trimming to the caller.
    ///
    /// Which every caller forgot. Unclipped, that row's text lands on whatever is above the list,
    /// and on a screen that is the title bar. **Prefer [`draw_visible`](Self::draw_visible)** unless
    /// you want geometry without a canvas — a hit test, a measurement, a test.
    pub fn for_visible<R: Rows + ?Sized>(
        &self,
        rows: &R,
        viewport: Rect,
        mut f: impl FnMut(usize, Rect),
    ) {
        let h = viewport.height();
        if h <= 0 {
            return;
        }
        let mut y = -self.scroll;
        for i in 0..rows.len() {
            let rh = rows.height(i);
            if y >= h {
                break;
            }
            if y + rh > 0 {
                f(
                    i,
                    Rect::new(viewport.x0, viewport.y0 + y, viewport.x1, viewport.y0 + y + rh),
                );
            }
            y += rh;
        }
    }

    /// Fraction of the content visible and where, for a scrollbar. `None` when
    /// everything fits and no bar should be drawn.
    pub fn scrollbar<R: Rows + ?Sized>(&self, rows: &R, viewport_h: i32) -> Option<(i32, i32)> {
        let total = Self::content_height(rows);
        if total <= viewport_h || viewport_h <= 0 {
            return None;
        }
        // Keep the thumb grabbable even on a very long list.
        let thumb = ((viewport_h as i64 * viewport_h as i64) / total as i64).max(8) as i32;
        let travel = viewport_h - thumb;
        let max_scroll = total - viewport_h;
        let y = ((self.scroll as i64 * travel as i64) / max_scroll as i64) as i32;
        Some((y, thumb))
    }
}

#[cfg(test)]
extern crate alloc;

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    const VP: i32 = 200;

    fn uniform(n: usize) -> Uniform {
        Uniform { count: n, height: 40 }
    }

    #[test]
    fn empty_list_is_inert() {
        let rows = uniform(0);
        let mut s = ListState::new();
        assert!(!s.move_selection(1, &rows, VP));
        s.ensure_visible(&rows, VP);
        assert_eq!(s.scroll, 0);
        let mut seen = 0;
        s.for_visible(&rows, Rect::from_xywh(0, 0, 320, VP), |_, _| seen += 1);
        assert_eq!(seen, 0);
        assert_eq!(s.scrollbar(&rows, VP), None);
    }

    #[test]
    fn selection_clamps_at_both_ends_without_wrapping() {
        let rows = uniform(3);
        let mut s = ListState::new();
        assert!(!s.move_selection(-1, &rows, VP), "already at the top");
        assert_eq!(s.selected, 0);
        assert!(s.move_selection(10, &rows, VP));
        assert_eq!(s.selected, 2);
        assert!(!s.move_selection(1, &rows, VP), "already at the end");
        assert_eq!(s.selected, 2);
    }

    #[test]
    fn content_shorter_than_viewport_never_scrolls() {
        let rows = uniform(3); // 120px in a 200px viewport
        let mut s = ListState::new();
        s.move_selection(2, &rows, VP);
        assert_eq!(s.scroll, 0);
        assert_eq!(s.scrollbar(&rows, VP), None);
    }

    #[test]
    fn scrolling_down_is_minimal() {
        let rows = uniform(10); // 400px
        let mut s = ListState::new();
        // Rows 0..4 (0..200) fit exactly; selecting row 5 should reveal just it.
        s.select(5, &rows, VP);
        assert_eq!(s.scroll, 40, "should scroll by exactly one row");
        s.select(6, &rows, VP);
        assert_eq!(s.scroll, 80);
    }

    #[test]
    fn scrolling_back_up_aligns_the_row_to_the_top() {
        let rows = uniform(10);
        let mut s = ListState::new();
        s.select(9, &rows, VP);
        assert_eq!(s.scroll, 200);
        s.select(2, &rows, VP);
        assert_eq!(s.scroll, 80, "row 2 top-aligned");
    }

    #[test]
    fn a_row_taller_than_the_viewport_aligns_to_its_top() {
        let heights: Vec<i32> = alloc::vec![40, 500, 40];
        let mut s = ListState::new();
        s.select(1, heights.as_slice(), VP);
        // Bottom-aligning would hide the start of the message; prefer the top.
        assert_eq!(s.scroll, 40);
    }

    #[test]
    fn scroll_to_end_lands_flush_with_the_bottom() {
        let rows = uniform(10); // 400px
        let mut s = ListState::new();
        s.scroll_to_end(&rows, VP);
        assert_eq!(s.selected, 9);
        assert_eq!(s.scroll, 200);
        // And is already clamped.
        let before = s.scroll;
        s.clamp_scroll(&rows, VP);
        assert_eq!(s.scroll, before);
    }

    #[test]
    fn visible_rows_are_exactly_those_on_screen() {
        let rows = uniform(10);
        let mut s = ListState::new();
        s.scroll = 50; // shows the tail of row 1 through the head of row 6
        let vp = Rect::from_xywh(0, 0, 320, VP);
        let mut got = Vec::new();
        s.for_visible(&rows, vp, |i, r| got.push((i, r.y0, r.y1)));

        assert_eq!(got.first().unwrap().0, 1, "row 1 is partially visible");
        assert_eq!(got.last().unwrap().0, 6, "row 6 is partially visible");
        // Row 1 spans 40..80 in content space, so 40-50 = -10 in viewport space.
        assert_eq!(got[0].1, -10);
        for (_, y0, y1) in &got {
            assert!(*y1 > 0 && *y0 < VP, "row {y0}..{y1} is off screen");
        }
    }

    #[test]
    fn visible_rects_are_offset_by_the_viewport_origin() {
        let rows = uniform(2);
        let s = ListState::new();
        let vp = Rect::from_xywh(7, 20, 300, VP);
        let mut got = Vec::new();
        s.for_visible(&rows, vp, |i, r| got.push((i, r)));
        assert_eq!(got[0].1.y0, 20);
        assert_eq!(got[0].1.x0, 7);
        assert_eq!(got[0].1.x1, 307);
        assert_eq!(got[1].1.y0, 60);
    }

    #[test]
    fn row_at_maps_content_y_back_to_an_index() {
        let heights: Vec<i32> = alloc::vec![10, 20, 30];
        let r = heights.as_slice();
        assert_eq!(ListState::row_at(r, 0), Some(0));
        assert_eq!(ListState::row_at(r, 9), Some(0));
        assert_eq!(ListState::row_at(r, 10), Some(1));
        assert_eq!(ListState::row_at(r, 29), Some(1));
        assert_eq!(ListState::row_at(r, 30), Some(2));
        assert_eq!(ListState::row_at(r, 59), Some(2));
        assert_eq!(ListState::row_at(r, 60), None);
        assert_eq!(ListState::row_at(r, -1), None);
    }

    #[test]
    fn scrollbar_thumb_stays_inside_the_track() {
        let rows = uniform(50); // 2000px of content
        let mut s = ListState::new();
        for sel in 0..50 {
            s.select(sel, &rows, VP);
            let (y, h) = s.scrollbar(&rows, VP).expect("content overflows");
            assert!(h >= 8, "thumb too small to see: {h}");
            assert!(y >= 0, "thumb above the track at row {sel}: {y}");
            assert!(y + h <= VP, "thumb past the track at row {sel}: {y}+{h}");
        }
        // Flush at both extremes.
        s.scroll = 0;
        assert_eq!(s.scrollbar(&rows, VP).unwrap().0, 0);
        s.scroll_to_end(&rows, VP);
        let (y, h) = s.scrollbar(&rows, VP).unwrap();
        assert_eq!(y + h, VP);
    }

    #[test]
    fn page_keys_move_more_than_one_row_but_stay_in_range() {
        let rows = uniform(50);
        let mut s = ListState::new();
        s.handle_key(KeyEvent::new(Key::Right), &rows, VP);
        assert!(s.selected > 1 && s.selected < 50);
        let after_page = s.selected;
        s.handle_key(KeyEvent::new(Key::Left), &rows, VP);
        assert!(s.selected < after_page);
    }

    #[test]
    fn unrelated_keys_are_ignored_so_the_app_can_use_them() {
        let rows = uniform(5);
        let mut s = ListState::new();
        assert_eq!(s.handle_key(KeyEvent::new(Key::Select), &rows, VP), Handled::Ignored);
        assert_eq!(s.handle_key(KeyEvent::new(Key::Char('a')), &rows, VP), Handled::Ignored);
        assert_eq!(s.handle_key(KeyEvent::new(Key::Down), &rows, VP), Handled::Consumed);
    }

    #[test]
    fn scroll_stays_valid_when_rows_disappear_beneath_it() {
        let mut s = ListState::new();
        let long = uniform(20);
        s.scroll_to_end(&long, VP);
        assert_eq!(s.scroll, 600);
        // The list shrinks (messages deleted); scroll must come back in range.
        let short = uniform(2);
        s.clamp_scroll(&short, VP);
        assert_eq!(s.scroll, 0);
    }

    #[test]
    fn clamp_also_rescues_a_selection_left_past_the_end() {
        let mut s = ListState::new();
        let long = uniform(20);
        s.scroll_to_end(&long, VP);
        assert_eq!(s.selected, 19);

        let short = uniform(3);
        s.clamp(&short, VP);
        assert_eq!(s.selected, 2, "selection must not dangle past the last row");
        assert_eq!(s.scroll, 0);

        // Down to zero rows: nothing to select, and nothing may panic.
        s.clamp(&uniform(0), VP);
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn ensure_visible_writes_back_the_clamped_selection() {
        let mut s = ListState::new();
        s.selected = 99;
        s.ensure_visible(&uniform(4), VP);
        assert_eq!(s.selected, 3, "must not silently disagree with the row count");
    }
}
