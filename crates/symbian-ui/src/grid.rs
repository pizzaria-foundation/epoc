//! Cursor movement and cell geometry for a fixed-column grid.
//!
//! The two-dimensional counterpart of [`crate::list`], and split from drawing for the same reason:
//! the arithmetic is what is easy to get subtly wrong, so it lives here, is pure, and is
//! unit-tested. Widgets on top only draw.
//!
//! # Why this exists rather than each grid doing its own
//!
//! It was written twice before it was written once. The launcher's home has a `cols`×`rows` block
//! of shortcuts with its own `grid_cells` helper and its own four arrow arms; a calendar's month
//! view is six rows of seven days with the same cursor and the same edges. Both had to answer the
//! same four questions — where does cell `i` land, what does Right do in the last column, what does
//! Down do from a half-filled last row, and how does a cursor stay on screen — and the second one
//! was about to answer them differently.
//!
//! # A grid is a list of rows
//!
//! Vertical scrolling is not reimplemented here. [`GridShape`] implements [`Rows`], one entry per
//! *row* of cells, so [`ListState`]'s scrolling, clamping and visibility all apply unchanged. What
//! is genuinely new is the horizontal half: which column a cursor is in, and what happens at the
//! edges.
//!
//! # Columns divide exactly, remainder included
//!
//! Cell edges are computed from the area's own coordinates rather than from a rounded-down cell
//! width. `width / cols` drops up to `cols - 1` pixels on the right, which on a 320-pixel screen
//! with seven columns is a six-pixel gap the eye reads as a missing border. Each edge is placed at
//! `x0 + col * width / cols`, so the last column ends exactly on `x1` and the leftover is spread
//! one pixel at a time.

use crate::input::{Handled, Key, KeyEvent};
use crate::list::{ListState, Rows};
use symbian_gfx::{Canvas, Rect};

/// The shape of a grid: how wide, how many cells, and how tall a row is.
///
/// A value rather than state — it is rebuilt from the model every frame, and holding it in
/// [`GridState`] would let the two disagree about how many cells there are.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct GridShape {
    /// Cells across. At least 1; a zero would divide by zero in every calculation here.
    pub cols: usize,
    /// How many cells there are in total. The last row may be partly empty.
    pub count: usize,
    /// Height of one row of cells, in pixels.
    pub cell_h: i32,
}

impl GridShape {
    pub fn new(cols: usize, count: usize, cell_h: i32) -> Self {
        Self { cols: cols.max(1), count, cell_h: cell_h.max(1) }
    }

    /// How many rows the cells occupy, the last one possibly short.
    pub fn rows(&self) -> usize {
        self.count.div_ceil(self.cols)
    }

    pub fn row_of(&self, index: usize) -> usize {
        index / self.cols
    }

    pub fn col_of(&self, index: usize) -> usize {
        index % self.cols
    }

    /// The left edge of column `col` inside `area`. See the module note on exact division.
    pub fn col_x(&self, area: Rect, col: usize) -> i32 {
        area.x0 + (col.min(self.cols) as i64 * area.width() as i64 / self.cols as i64) as i32
    }

    /// Where cell `index` lands inside `area`, in *content* space — that is, before the scroll
    /// offset is taken off. `None` for an index past the end.
    pub fn cell_rect(&self, area: Rect, index: usize) -> Option<Rect> {
        if index >= self.count {
            return None;
        }
        let (row, col) = (self.row_of(index), self.col_of(index));
        let y0 = area.y0 + row as i32 * self.cell_h;
        Some(Rect {
            x0: self.col_x(area, col),
            y0,
            x1: self.col_x(area, col + 1),
            y1: y0 + self.cell_h,
        })
    }
}

/// Rows of cells, so a grid scrolls through [`ListState`] rather than through arithmetic of its own.
impl Rows for GridShape {
    fn len(&self) -> usize {
        self.rows()
    }

    fn height(&self, _index: usize) -> i32 {
        self.cell_h
    }
}

/// Which way a navigation key was reaching when it ran out of grid.
///
/// Four, unlike a list's two, and that is the whole reason a calendar wanted this: `Left` on the
/// first column of a month view is "the previous month", not "nothing happened". A cursor that
/// merely clamps cannot say which of those it meant.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum GridEdge {
    Top,
    Bottom,
    Left,
    Right,
}

/// Where the cursor is, and how far the grid has scrolled.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub struct GridState {
    /// The focused cell. Meaningless when the grid is empty.
    pub cursor: usize,
    /// Pixels of content scrolled past the top of the viewport.
    pub scroll: i32,
}

impl GridState {
    pub const fn new() -> Self {
        Self { cursor: 0, scroll: 0 }
    }

    /// The row-oriented view of this state, for the scrolling machinery.
    fn as_list(&self, shape: &GridShape) -> ListState {
        ListState { selected: shape.row_of(self.cursor), scroll: self.scroll }
    }

    /// Bring the cursor and the scroll back into range after the cell set changed underneath them.
    pub fn clamp(&mut self, shape: &GridShape, viewport_h: i32) {
        self.cursor = self.cursor.min(shape.count.saturating_sub(1));
        let mut l = self.as_list(shape);
        l.clamp_scroll(shape, viewport_h);
        self.scroll = l.scroll;
    }

    /// Scroll the least amount that puts the cursor's row fully on screen.
    pub fn ensure_visible(&mut self, shape: &GridShape, viewport_h: i32) {
        self.cursor = self.cursor.min(shape.count.saturating_sub(1));
        let mut l = self.as_list(shape);
        l.ensure_visible(shape, viewport_h);
        self.scroll = l.scroll;
    }

    /// Put the cursor on `index` and scroll to it.
    pub fn select(&mut self, index: usize, shape: &GridShape, viewport_h: i32) {
        self.cursor = index.min(shape.count.saturating_sub(1));
        self.ensure_visible(shape, viewport_h);
    }

    /// Move the cursor one step, or report the edge it ran into.
    ///
    /// `Ok(true)` means it moved, `Ok(false)` that the key was not a direction, and `Err(edge)`
    /// that there was nowhere to go that way.
    ///
    /// # Down out of a full row into a short one
    ///
    /// Seven columns and ten cells: from cell 6 — the end of the first row — `Down` would land on
    /// cell 13, which does not exist. It moves to cell 9, the last one there is, rather than
    /// refusing. The row below *is* there and a cursor that would not enter it reads as a broken
    /// key. Only a cursor already in the last row reports [`GridEdge::Bottom`].
    pub fn step(&mut self, key: Key, shape: &GridShape) -> Result<bool, GridEdge> {
        if shape.count == 0 {
            return Ok(false);
        }
        let last = shape.count - 1;
        let (row, col) = (shape.row_of(self.cursor), shape.col_of(self.cursor));
        match key {
            Key::Up => {
                if row == 0 {
                    return Err(GridEdge::Top);
                }
                self.cursor -= shape.cols;
                Ok(true)
            }
            Key::Down => {
                if row + 1 >= shape.rows() {
                    return Err(GridEdge::Bottom);
                }
                self.cursor = (self.cursor + shape.cols).min(last);
                Ok(true)
            }
            Key::Left => {
                if col == 0 {
                    return Err(GridEdge::Left);
                }
                self.cursor -= 1;
                Ok(true)
            }
            Key::Right => {
                if col + 1 >= shape.cols || self.cursor == last {
                    return Err(GridEdge::Right);
                }
                self.cursor += 1;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// Apply a key, scrolling to follow the cursor. Consumes every direction key, including one
    /// that ran into an edge.
    ///
    /// Consuming a clamped arrow is deliberate and matches [`ListState::handle_key`]: the caller
    /// that wants to page a month on `Left` learns about it through the edge, and letting the key
    /// fall through to the screen underneath would give the same press two meanings.
    pub fn handle_key(
        &mut self,
        ev: KeyEvent,
        shape: &GridShape,
        viewport_h: i32,
    ) -> (Handled, Option<GridEdge>) {
        match self.step(ev.key, shape) {
            Ok(true) => {
                self.ensure_visible(shape, viewport_h);
                (Handled::Consumed, None)
            }
            Ok(false) => (Handled::Ignored, None),
            Err(edge) => (Handled::Consumed, Some(edge)),
        }
    }

    /// Call `f` for every cell whose row is on screen, with the rect it occupies.
    ///
    /// Cells only, not rows: the caller draws cells and the row is an implementation detail of the
    /// scrolling. A partly visible row still reports all its cells, with rects that reach outside
    /// the viewport — [`GridState::draw_visible`] is the one that clips.
    pub fn for_visible(&self, shape: &GridShape, viewport: Rect, mut f: impl FnMut(usize, Rect)) {
        let l = self.as_list(shape);
        l.for_visible(shape, viewport, |row, row_rect| {
            let first = row * shape.cols;
            for col in 0..shape.cols {
                let i = first + col;
                if i >= shape.count {
                    break;
                }
                f(
                    i,
                    Rect {
                        x0: shape.col_x(viewport, col),
                        y0: row_rect.y0,
                        x1: shape.col_x(viewport, col + 1),
                        y1: row_rect.y1,
                    },
                );
            }
        });
    }

    /// [`GridState::for_visible`], clipped to the viewport.
    ///
    /// The clip is the whole difference and it is not optional: the top row of a scrolled grid gets
    /// a rect that starts above the band, and unclipped its ink lands on the title bar. This is the
    /// same lesson `ListState::draw_visible` records, applied where a second set of callers would
    /// otherwise have to remember it.
    pub fn draw_visible(
        &self,
        c: &mut Canvas<'_>,
        shape: &GridShape,
        viewport: Rect,
        mut f: impl FnMut(&mut Canvas<'_>, usize, Rect),
    ) {
        let saved = c.save();
        c.clip_to(viewport);
        // The closure borrows the canvas for the whole walk rather than collecting the cells
        // first: a `Vec` here would be one allocation per frame, on the hot path of a grid that
        // redraws whenever the cursor moves.
        self.for_visible(shape, viewport, |i, r| f(c, i, r));
        c.restore(saved);
    }

    /// Thumb position and length for a scrollbar, or `None` when the whole grid fits.
    pub fn scrollbar(&self, shape: &GridShape, viewport_h: i32) -> Option<(i32, i32)> {
        self.as_list(shape).scrollbar(shape, viewport_h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// A calendar month: six rows of seven, always.
    fn month() -> GridShape {
        GridShape::new(7, 42, 30)
    }

    fn area() -> Rect {
        Rect::new(0, 0, 320, 180)
    }

    #[test]
    fn the_columns_divide_the_area_exactly_with_no_gap_on_the_right() {
        // 320 / 7 is 45.7. Rounded down and multiplied back it is 315, leaving a six-pixel strip
        // the eye reads as a missing border. Every edge is computed from the area instead.
        let g = month();
        let a = area();
        assert_eq!(g.col_x(a, 0), 0);
        assert_eq!(g.col_x(a, 7), 320, "the last column must end on the right edge");
        // Widths differ by at most one pixel, which is what spreading the remainder means.
        let widths: Vec<i32> = (0..7).map(|c| g.col_x(a, c + 1) - g.col_x(a, c)).collect();
        assert_eq!(widths.iter().sum::<i32>(), 320);
        assert!(widths.iter().max().unwrap() - widths.iter().min().unwrap() <= 1, "{widths:?}");
    }

    #[test]
    fn a_cell_lands_in_its_own_row_and_column() {
        let g = month();
        let a = area();
        let c0 = g.cell_rect(a, 0).unwrap();
        let c8 = g.cell_rect(a, 8).unwrap(); // row 1, column 1
        assert_eq!(c0.y0, 0);
        assert_eq!(c8.y0, 30);
        assert_eq!(c8.x0, g.col_x(a, 1));
        assert_eq!(g.cell_rect(a, 42), None, "past the end is not a cell");
    }

    #[test]
    fn the_arrows_move_one_step_each_way() {
        let g = month();
        let mut s = GridState { cursor: 8, scroll: 0 };
        assert_eq!(s.step(Key::Right, &g), Ok(true));
        assert_eq!(s.cursor, 9);
        assert_eq!(s.step(Key::Down, &g), Ok(true));
        assert_eq!(s.cursor, 16);
        assert_eq!(s.step(Key::Left, &g), Ok(true));
        assert_eq!(s.cursor, 15);
        assert_eq!(s.step(Key::Up, &g), Ok(true));
        assert_eq!(s.cursor, 8);
    }

    #[test]
    fn each_edge_is_reported_as_the_direction_that_ran_out() {
        // The reason this type exists: a month view pages backwards on Left and forwards on Right,
        // and a clamped cursor cannot tell those apart from each other or from nothing happening.
        let g = month();
        let mut top_left = GridState { cursor: 0, scroll: 0 };
        assert_eq!(top_left.step(Key::Up, &g), Err(GridEdge::Top));
        assert_eq!(top_left.step(Key::Left, &g), Err(GridEdge::Left));
        assert_eq!(top_left.cursor, 0, "a refused step must not move the cursor");

        let mut bottom_right = GridState { cursor: 41, scroll: 0 };
        assert_eq!(bottom_right.step(Key::Down, &g), Err(GridEdge::Bottom));
        assert_eq!(bottom_right.step(Key::Right, &g), Err(GridEdge::Right));
    }

    #[test]
    fn right_at_the_end_of_a_row_is_an_edge_rather_than_a_wrap_to_the_next() {
        // Wrapping would be defensible for a list of icons and is wrong for a calendar, where the
        // cell to the right of Sunday is next week's Monday only if you also moved down a row.
        let g = month();
        let mut s = GridState { cursor: 6, scroll: 0 };
        assert_eq!(s.step(Key::Right, &g), Err(GridEdge::Right));
        assert_eq!(s.cursor, 6);
    }

    #[test]
    fn down_from_a_full_row_into_a_short_one_lands_on_the_last_cell() {
        // Seven columns, ten cells. From cell 6 the arithmetic says 13, which does not exist —
        // and refusing reads as a broken key, because the row below is plainly there.
        let g = GridShape::new(7, 10, 30);
        let mut s = GridState { cursor: 6, scroll: 0 };
        assert_eq!(s.step(Key::Down, &g), Ok(true));
        assert_eq!(s.cursor, 9);
        // From inside that last row there is nowhere further down.
        assert_eq!(s.step(Key::Down, &g), Err(GridEdge::Bottom));
    }

    #[test]
    fn an_empty_grid_answers_no_key_and_does_not_index_anything() {
        let g = GridShape::new(7, 0, 30);
        let mut s = GridState::new();
        assert_eq!(s.step(Key::Down, &g), Ok(false));
        assert_eq!(s.cursor, 0);
        assert_eq!(g.rows(), 0);
        assert_eq!(g.cell_rect(area(), 0), None);
    }

    #[test]
    fn a_key_that_is_not_a_direction_is_left_alone() {
        // Select has to reach the screen: it is what opens the day under the cursor.
        let g = month();
        let mut s = GridState::new();
        let (handled, edge) = s.handle_key(KeyEvent::new(Key::Select), &g, 180);
        assert_eq!(handled, Handled::Ignored);
        assert_eq!(edge, None);
    }

    #[test]
    fn a_clamped_arrow_is_still_consumed_and_names_its_edge() {
        // Consumed, so the same press cannot also mean something to the screen underneath; the
        // edge is how the app hears about it.
        let g = month();
        let mut s = GridState::new();
        let (handled, edge) = s.handle_key(KeyEvent::new(Key::Left), &g, 180);
        assert_eq!(handled, Handled::Consumed);
        assert_eq!(edge, Some(GridEdge::Left));
    }

    #[test]
    fn scrolling_follows_the_cursor_by_whole_rows() {
        // A viewport three rows tall over six rows of cells.
        let g = month();
        let viewport = 90;
        let mut s = GridState::new();
        s.select(0, &g, viewport);
        assert_eq!(s.scroll, 0);
        // Cell 28 is row 4, which is below the fold.
        s.select(28, &g, viewport);
        assert_eq!(s.scroll, 30 * 5 - 90, "the least scroll that shows row 4 whole");
        s.select(0, &g, viewport);
        assert_eq!(s.scroll, 0, "and back");
    }

    #[test]
    fn only_the_visible_cells_are_offered_and_they_are_the_right_ones() {
        // The property that keeps a large grid affordable: cells are built for what is on screen.
        let g = month();
        let viewport = Rect::new(0, 0, 320, 90); // three rows
        let mut s = GridState::new();
        s.select(0, &g, viewport.height());
        let mut seen = Vec::new();
        s.for_visible(&g, viewport, |i, _| seen.push(i));
        assert_eq!(seen.first(), Some(&0));
        assert_eq!(seen.last(), Some(&20), "three rows of seven, from the top");
        assert_eq!(seen.len(), 21);
    }

    #[test]
    fn a_short_last_row_offers_only_the_cells_it_has() {
        let g = GridShape::new(7, 10, 30);
        let viewport = Rect::new(0, 0, 320, 90);
        let s = GridState::new();
        let mut seen = Vec::new();
        s.for_visible(&g, viewport, |i, _| seen.push(i));
        assert_eq!(seen.len(), 10, "not fourteen: the last row is short");
        assert_eq!(*seen.last().unwrap(), 9);
    }

    #[test]
    fn clamping_rescues_a_cursor_left_past_the_end() {
        // The month shrank from 42 cells to 10 — a filtered grid, a shorter list of shortcuts —
        // and a cursor left dangling shows up much later as a grid with no visible highlight.
        let mut s = GridState { cursor: 40, scroll: 500 };
        s.clamp(&GridShape::new(7, 10, 30), 90);
        assert_eq!(s.cursor, 9);
        assert_eq!(s.scroll, 0, "ten cells in two rows fit, so there is nothing to scroll");
    }
}
