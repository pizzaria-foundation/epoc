//! A grid of equal cells with a two-dimensional cursor.
//!
//! [`ScrollList`](crate::widgets::ScrollList)'s sibling, and built the same way: no arithmetic
//! lives here. Cursor movement, cell geometry, edge reporting and scrolling are all
//! [`symbian_ui::grid`], which is pure and unit-tested; this widget owns *where the state lives*
//! and *when the cells are built*, and nothing else. If you are about to add an `i32` calculation
//! below, look in `grid.rs` first.
//!
//! # Two ways to be tall
//!
//! [`Grid::new`] takes a cell height and scrolls when the cells do not fit — a menu of applications.
//! [`Grid::fitted`] takes a number of rows and divides the band by it, so the grid always fills its
//! space exactly and never scrolls — a calendar month, which is six rows whatever the month is.
//!
//! The second is not a convenience. A month view whose cell height is a constant leaves a strip of
//! background at the bottom of a 176-pixel band, and — worse — a grid that is one pixel too tall
//! silently starts scrolling, so the top row creeps under the title bar as the cursor moves down.
//!
//! # Cells are built for what is on screen
//!
//! Like a list's rows: [`GridState::for_visible`] reports the cells in view and only those get a
//! widget. A 300-application grid builds a screenful, not three hundred.

use alloc::boxed::Box;
use alloc::rc::Rc;
use core::cell::{Cell, RefCell};

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::{chrome, grid::GridShape, Handled, KeyEvent, Theme};

// Re-exported under its own name rather than as `Edge`: the list's two-ended one is already
// called that at `widgets::Edge`, and two different enums sharing a name in one module is how a
// caller ends up matching on the wrong one.
pub use symbian_ui::grid::GridEdge;
use symbian_ui::grid::GridState;

use crate::cache::UiCache;
use crate::constraints::Constraints;
use crate::layout;
use crate::slot::SlotTable;
use crate::widget::{hash_i32, KeyCtx, Widget, WidgetHash};
use crate::widgets::Node;

/// How tall a row of cells is.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Height {
    /// A constant, in pixels. The grid scrolls when its cells do not fit.
    Fixed(i32),
    /// Exactly this many rows fill the band, whatever the band turns out to be.
    Fit(usize),
}

/// Builds one cell: its index, and whether the cursor is on it.
pub type CellFn = dyn Fn(usize, bool) -> Node;

pub struct Grid {
    cols: usize,
    count: usize,
    height: Height,
    /// The cursor as the app sees it. `None` leaves the grid to keep its own in the slot.
    cursor: Option<usize>,
    focused: bool,
    scrollbar: bool,
    /// Whether to paint the selection highlight under the focused cell.
    highlight: bool,
    /// Shared with the slot table, so the cursor and offset survive the tree being rebuilt.
    state: Rc<Cell<GridState>>,
    /// Measured sizes for the cell subtrees, kept across frames for the same reason the bridge
    /// keeps one: a cache that did not outlive the frame would miss every time.
    cache: Rc<RefCell<UiCache>>,
    cell: Box<CellFn>,
    moved: Option<Box<dyn Fn(usize)>>,
    edge: Option<Box<dyn Fn(GridEdge)>>,
}

impl Grid {
    /// A grid of `count` cells, `cols` across, each row `cell_h` pixels tall.
    ///
    /// Scrolls when the rows do not fit. Takes the slot table because that is where the cursor and
    /// the offset have to come from: this struct is rebuilt every frame and they must not be.
    pub fn new(slots: &mut SlotTable, cols: usize, count: usize, cell_h: i32) -> Self {
        Self::build(slots, cols, count, Height::Fixed(cell_h.max(1)))
    }

    /// A grid whose `rows` fill the band exactly and which therefore never scrolls.
    ///
    /// `rows` is what the *layout* is, not what the data is: a calendar month is six rows even in a
    /// February that fits in four, because a grid that changed shape between months would make the
    /// whole screen twitch when the user pages.
    pub fn fitted(slots: &mut SlotTable, cols: usize, count: usize, rows: usize) -> Self {
        let mut me = Self::build(slots, cols, count, Height::Fit(rows.max(1)));
        me.scrollbar = false;
        me
    }

    fn build(slots: &mut SlotTable, cols: usize, count: usize, height: Height) -> Self {
        let state = slots.use_state_with(|| Rc::new(Cell::new(GridState::new()))).clone();
        let cache = slots.use_state_with(|| Rc::new(RefCell::new(UiCache::new()))).clone();
        Self {
            cols: cols.max(1),
            count,
            height,
            cursor: None,
            focused: false,
            // Off by default, unlike a list. A grid is usually a page that fits, and a gutter it
            // does not need eats a column's worth of width on a 320-pixel screen.
            scrollbar: false,
            highlight: true,
            state,
            cache,
            cell: Box::new(|_, _| Node::leaf(Empty)),
            moved: None,
            edge: None,
        }
    }

    /// Drive the cursor from the model.
    pub fn cursor(mut self, index: usize) -> Self {
        self.cursor = Some(index);
        self
    }

    /// Let this grid move its own cursor on the arrows.
    ///
    /// **Off by default**, and for the reason [`ScrollList::focused`](crate::widgets::ScrollList::focused)
    /// spells out: with keys reaching widgets, a grid that answered arrows unconditionally would
    /// move its cursor *as well as* the `update` the app wrote for the same keys, and two owners of
    /// one cursor drift apart the first time one of them clamps.
    pub fn focused(mut self, on: bool) -> Self {
        self.focused = on;
        self
    }

    /// Whether to reserve and draw a scrollbar gutter. Off by default; irrelevant to
    /// [`Grid::fitted`], which cannot scroll.
    pub fn scrollbar(mut self, on: bool) -> Self {
        self.scrollbar = on;
        self
    }

    /// Whether the focused cell gets the standard selection fill underneath it.
    ///
    /// On by default: with no pointer, the highlight is the only thing saying where you are. Turn
    /// it off for a grid whose cells draw their own focus — a calendar day that rings the number
    /// rather than flooding the cell.
    pub fn highlight(mut self, on: bool) -> Self {
        self.highlight = on;
        self
    }

    /// How to build the widget for a cell. Called only for cells on screen.
    pub fn cell(mut self, f: impl Fn(usize, bool) -> Node + 'static) -> Self {
        self.cell = Box::new(f);
        self
    }

    /// Move the cursor here, and tell the app where it went.
    ///
    /// Implies [`focused`](Self::focused), and carries the same warning: do not *also* claim the
    /// arrows in [`DeclarativeApp::on_key`](crate::app::DeclarativeApp::on_key), which runs first
    /// and would make this silently never fire.
    pub fn on_move(mut self, f: impl Fn(usize) + 'static) -> Self {
        self.moved = Some(Box::new(f));
        self.focused = true;
        self
    }

    /// An arrow arrived with nowhere to go that way.
    ///
    /// The four-sided version of a list's two ends, and the reason a grid needed its own state
    /// type: `Left` on the first column of a month view means "the previous month", `Right` on the
    /// last means "the next", and a cursor that merely clamped could report neither. Fires instead
    /// of a move, never as well.
    pub fn on_edge(mut self, f: impl Fn(GridEdge) + 'static) -> Self {
        self.edge = Some(Box::new(f));
        self.focused = true;
        self
    }

    /// Where the cursor is, as it stands after the last draw or key.
    pub fn selection(&self) -> usize {
        self.state.get().cursor
    }

    /// This frame's shape, for a band of the given height.
    fn shape(&self, band_h: i32) -> GridShape {
        let cell_h = match self.height {
            Height::Fixed(px) => px,
            // Divided, not rounded: `band / rows` with the remainder left over would leave a strip
            // at the bottom. The rows themselves absorb it one pixel at a time through the same
            // exact-division rule the columns use — so the cell height here is the floor and the
            // last row is drawn to the band's own edge by the geometry in `symbian_ui::grid`.
            Height::Fit(rows) => (band_h / rows as i32).max(1),
        };
        GridShape::new(self.cols, self.count, cell_h)
    }

    /// Reconcile the kept state with this frame's cells before using it.
    fn sync(&self, band_h: i32) -> (GridState, GridShape) {
        let shape = self.shape(band_h);
        let mut st = self.state.get();
        match self.cursor {
            Some(i) => st.select(i, &shape, band_h),
            None => st.clamp(&shape, band_h),
        }
        self.state.set(st);
        (st, shape)
    }

    /// The band cells are drawn into, with the scrollbar gutter taken off the right.
    fn content(&self, rect: Rect, theme: &Theme<'_>) -> Rect {
        if self.scrollbar {
            Rect { x1: rect.x1 - chrome::scrollbar_gutter(theme, true), ..rect }
        } else {
            rect
        }
    }
}

impl Widget for Grid {
    fn content_hash(&self) -> WidgetHash {
        // Scroll and cursor are deliberately absent: moving a cursor moves the highlight, it does
        // not resize anything, and including them would re-measure every cell on every keypress.
        let h = hash_i32(hash_i32(0, self.cols as i32), self.count as i32);
        let h = match self.height {
            Height::Fixed(px) => hash_i32(hash_i32(h, 1), px),
            Height::Fit(rows) => hash_i32(hash_i32(h, 2), rows as i32),
        };
        hash_i32(h, self.scrollbar as i32)
    }

    fn measure(&self, constraints: Constraints, _theme: &Theme<'_>) -> Size {
        // A grid takes everything it is offered, for the same reason a list does: its content
        // height is not its size, and asking for the content would make a long grid demand a
        // height no parent could give it.
        constraints.constrain(Size::new(constraints.max_w, constraints.max_h))
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        let band = self.content(rect, theme);
        let (st, shape) = self.sync(band.height());
        let cursor = st.cursor;
        let has_cells = self.count > 0;

        let mut cache = self.cache.borrow_mut();
        // This grid's own frame over this grid's own cache. The bridge must not call `begin_frame`
        // on the screen's cache — that is layout's phase — but this one is not the screen's.
        cache.begin_frame();

        let mut slot = 0usize;
        st.draw_visible(c, &shape, band, |c, i, r| {
            // The highlight goes down first and full-bleed, so a cell drawing its own background
            // cannot cover the only thing that says where the cursor is.
            if self.highlight && has_cells && i == cursor {
                chrome::selection(c, r, theme);
            }
            let node = (self.cell)(i, i == cursor);
            // The same three passes the bridge runs, on a subtree. `slot` advances by the whole
            // subtree's width or two cells would write over each other's rects.
            layout::measure_node(
                &node,
                slot,
                Constraints::tight(r.width(), r.height()),
                theme,
                &mut cache,
            );
            layout::layout_node(&node, slot, r, &mut cache);
            layout::draw_node(&node, slot, &cache, c, theme);
            slot += node.slot_count();
        });
        drop(cache);

        if self.scrollbar {
            chrome::scrollbar(c, rect, theme, st.scrollbar(&shape, band.height()));
        }
    }

    fn handle_key(&self, ev: KeyEvent, rect: Rect, _cx: &mut KeyCtx<'_>) -> Handled {
        if !self.focused {
            return Handled::Ignored;
        }
        // Reconciled against this frame's cells *before* the key: the app may have moved the
        // cursor since the last draw, and stepping from a stale one moves from where it used to be.
        let (mut st, shape) = self.sync(rect.height());
        let before = st.cursor;
        let (out, edge) = st.handle_key(ev, &shape, rect.height());
        self.state.set(st);
        if out != Handled::Consumed {
            return out;
        }
        match (edge, st.cursor == before) {
            // It ran into a side. Reported by direction, not by position — a single-column grid is
            // at the left and the right at once, and `Right` still means "onward".
            (Some(e), _) => {
                if let Some(report) = &self.edge {
                    report(e);
                }
            }
            (None, false) => {
                if let Some(report) = &self.moved {
                    report(st.cursor);
                }
            }
            (None, true) => {}
        }
        out
    }
}

/// A cell builder that was never set draws nothing rather than panicking: a grid with no `.cell()`
/// is an unfinished screen, and an unfinished screen should be visibly empty, not a dead phone.
struct Empty;

impl Widget for Empty {
    fn measure(&self, c: Constraints, _t: &Theme<'_>) -> Size {
        c.constrain(Size::new(0, 0))
    }
    fn draw(&self, _c: &mut Canvas<'_>, _r: Rect, _t: &Theme<'_>) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::with_key_ctx;
    use crate::widgets::text::Text;
    use alloc::rc::Rc;
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use symbian_gfx::Size;
    use symbian_ui::{testing, Key, Palette};

    const BAND: Rect = Rect { x0: 0, y0: 0, x1: 320, y1: 180 };

    fn month(slots: &mut SlotTable) -> Grid {
        Grid::fitted(slots, 7, 42, 6)
    }

    #[test]
    fn a_fitted_grid_divides_the_band_by_its_rows() {
        let mut slots = SlotTable::new();
        let g = month(&mut slots);
        // 180 over six rows is 30, and the shape it hands the geometry says so.
        assert_eq!(g.shape(180).cell_h, 30);
        // A band that does not divide evenly still gets whole cells; the geometry spreads the rest.
        assert_eq!(g.shape(176).cell_h, 29);
    }

    #[test]
    fn a_fitted_grid_never_scrolls_however_the_cursor_moves() {
        // The defect this constructor exists for: a cell height that is one pixel too tall makes
        // the grid scroll, and the top row creeps under the title bar as the cursor goes down.
        let mut slots = SlotTable::new();
        let g = month(&mut slots).cursor(41);
        let (st, _) = g.sync(176);
        assert_eq!(st.scroll, 0);
        assert_eq!(st.cursor, 41);
    }

    #[test]
    fn the_cursor_survives_the_tree_being_rebuilt() {
        // The whole reason the state is in the slot table: `view` builds a new Grid every frame.
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let g = month(&mut slots).focused(true);
        with_key_ctx(|cx| {
            g.handle_key(KeyEvent::new(Key::Right), BAND, cx);
            g.handle_key(KeyEvent::new(Key::Down), BAND, cx);
        });
        assert_eq!(g.selection(), 8);
        slots.end_frame();

        // Next frame, a fresh widget over the same table.
        slots.begin_frame();
        let g2 = month(&mut slots).focused(true);
        assert_eq!(g2.selection(), 8, "a rebuilt grid forgot where the cursor was");
        slots.end_frame();
    }

    #[test]
    fn an_edge_is_reported_by_direction_and_the_key_is_still_consumed() {
        let mut slots = SlotTable::new();
        let seen: Rc<RefCell<Vec<GridEdge>>> = Rc::new(RefCell::new(Vec::new()));
        let sink = seen.clone();
        let g = month(&mut slots).on_edge(move |e| sink.borrow_mut().push(e));
        with_key_ctx(|cx| {
            // Cell 0: up and left both run out.
            assert_eq!(g.handle_key(KeyEvent::new(Key::Left), BAND, cx), Handled::Consumed);
            assert_eq!(g.handle_key(KeyEvent::new(Key::Up), BAND, cx), Handled::Consumed);
        });
        assert_eq!(*seen.borrow(), alloc::vec![GridEdge::Left, GridEdge::Top]);
        assert_eq!(g.selection(), 0, "an edge must not move the cursor");
    }

    #[test]
    fn a_move_is_reported_once_and_an_edge_is_not_a_move() {
        let mut slots = SlotTable::new();
        let moves: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));
        let edges: Rc<RefCell<Vec<GridEdge>>> = Rc::new(RefCell::new(Vec::new()));
        let (m, e) = (moves.clone(), edges.clone());
        let g = month(&mut slots)
            .on_move(move |i| m.borrow_mut().push(i))
            .on_edge(move |x| e.borrow_mut().push(x));
        with_key_ctx(|cx| {
            g.handle_key(KeyEvent::new(Key::Right), BAND, cx);
            g.handle_key(KeyEvent::new(Key::Left), BAND, cx); // back to 0
            g.handle_key(KeyEvent::new(Key::Left), BAND, cx); // and off the edge
        });
        assert_eq!(*moves.borrow(), alloc::vec![1, 0]);
        assert_eq!(*edges.borrow(), alloc::vec![GridEdge::Left]);
    }

    #[test]
    fn a_grid_nobody_focused_answers_no_key_at_all() {
        // The default. Otherwise a grid whose cursor the model owns would move it twice.
        let mut slots = SlotTable::new();
        let g = month(&mut slots);
        with_key_ctx(|cx| {
            assert_eq!(g.handle_key(KeyEvent::new(Key::Right), BAND, cx), Handled::Ignored);
        });
        assert_eq!(g.selection(), 0);
    }

    #[test]
    fn select_is_left_for_the_screen_to_answer() {
        // It is what opens the day under the cursor; a grid that consumed it would swallow the
        // only action the screen has.
        let mut slots = SlotTable::new();
        let g = month(&mut slots).focused(true);
        with_key_ctx(|cx| {
            assert_eq!(g.handle_key(KeyEvent::new(Key::Select), BAND, cx), Handled::Ignored);
        });
    }

    #[test]
    fn only_the_cells_on_screen_are_built() {
        // The property that makes a long grid affordable. A fixed-height grid of 300 cells in a
        // 180-pixel band builds a screenful, not three hundred.
        let mut slots = SlotTable::new();
        let built = Rc::new(Cell::new(0usize));
        let counter = built.clone();
        let g = Grid::new(&mut slots, 4, 300, 40).cell(move |i, _| {
            counter.set(counter.get() + 1);
            Node::leaf(Text::new(if i == 0 { "a" } else { "b" }))
        });
        testing::with_canvas(Size::new(320, 240), |c| {
            testing::with_theme(Palette::DARK, |t| g.draw(c, BAND, t));
        });
        let n = built.get();
        assert!(n > 0, "nothing was drawn at all");
        assert!(n <= 5 * 4, "built {n} cells for a band holding at most five rows");
    }

    #[test]
    fn a_frame_actually_paints_the_cells() {
        let mut slots = SlotTable::new();
        let g = month(&mut slots).cell(|_, _| Node::leaf(Text::new("8")));
        let (_, px) = testing::with_canvas(Size::new(320, 240), |c| {
            testing::with_theme(Palette::DARK, |t| g.draw(c, BAND, t));
        });
        assert!(px.iter().any(|&p| p != 0), "the grid drew nothing");
    }
}
