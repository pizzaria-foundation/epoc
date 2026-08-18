//! A scrolling list of uniform rows.
//!
//! # This file deliberately contains no scrolling arithmetic
//!
//! Every index calculation a list needs — minimal scrolling, which rows intersect the viewport, a
//! scrollbar thumb that stays inside its track, rescuing a selection after the rows underneath it
//! were deleted — already exists in [`symbian_ui::list`], is pure, and has a test for each of the
//! ways it is easy to get wrong. A second implementation here would be a second set of those same
//! bugs, arriving later and diverging quietly.
//!
//! So this widget is a shell: it owns *where the state lives* and *when the rows are built*, and
//! delegates everything else to [`ListState`]. If you find yourself adding an `i32` calculation
//! below, check `list.rs` first — it is probably already there and already tested.
//!
//! # Where the scroll offset lives, and why it is not in the model
//!
//! The plan's sketch had both `.selected(i)` and `.scroll(px)` come from the app model. Selection
//! belongs there; scroll does not, and the reason is that a scroll offset cannot be computed
//! without knowing the viewport height — which the model does not know and should not, because it
//! is a consequence of layout that changes when a title bar appears or the softkey labels wrap.
//! An `update` that set `scroll` would be guessing at a number that only `draw` can know.
//!
//! So the split is:
//!
//! * **Selection is the app's.** `update` moves it, because "which chat is selected" is what
//!   `Cmd::PushScreen(Detail(i))` is made of. It comes in through [`ScrollList::selected`].
//! * **Scroll is the slot table's.** It is derived from the selection by
//!   [`ListState::ensure_visible`], survives the tree being rebuilt because the slot outlives the
//!   frame, and is never named by the app at all.
//!
//! That is the same rule [`crate::slot`] states for a caret, applied to the other case it names.
//!
//! # Rows are built for what is on screen, not for what exists
//!
//! A 200-row list builds about six row widgets per frame — the ones [`ListState::for_visible`]
//! reports — not 200. That is the difference between a list this device can scroll and one it
//! cannot: building all 200 would allocate two hundred boxes per frame to draw six of them.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::Cell;

use core::cell::RefCell;

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::{chrome, Handled, Key, KeyEvent, ListState, Rows, Theme, Uniform};

use crate::layout::MainAlign;

use crate::cache::UiCache;
use crate::constraints::Constraints;
use crate::layout;
use crate::slot::SlotTable;
use crate::widget::{hash_i32, KeyCtx, Widget, WidgetHash};
use crate::widgets::Node;

/// Builds one row: its index, and whether it is the selected one.
///
/// Returns a [`Node`] and not a bare widget, because a row is realistically a `Row` of `Text`s —
/// a group, with a gap and an alignment. A closure that could only return a leaf would push every
/// caller into writing its own row layout by hand, which is the work this crate exists to remove.
///
/// No `&mut SlotTable` here, unlike the plan's sketch. Rows are built inside `draw`, which has
/// `&self` and no way to reach the table — and threading one in would mean a row's slot identity
/// depended on scroll position, so a row's state would follow the *screen position* rather than the
/// row. Per-row state therefore lives in the app model, keyed by whatever the row is keyed by.
pub type RowFn = dyn Fn(usize, bool) -> Node;

/// A vertically scrolling list of equal-height rows.
/// How tall the rows are.
///
/// Two shapes, because two screens genuinely differ. A dialog list is a stack of identical rows and
/// wants to say so once; a chat transcript is a stack of bubbles whose height is a function of how
/// much text each one holds, and there is no single number to give. [`symbian_ui::Rows`] already
/// models both — this is only the owned form of it, since a widget rebuilt every frame cannot hold
/// a borrow of the caller's slice.
enum Heights {
    Uniform(Uniform),
    /// One height per row, in order. Built by the caller, which is the only party that knows how
    /// tall a wrapped message turns out to be.
    Varying(Vec<i32>),
}

impl Rows for Heights {
    fn len(&self) -> usize {
        match self {
            Heights::Uniform(u) => u.count,
            Heights::Varying(v) => v.len(),
        }
    }

    fn height(&self, index: usize) -> i32 {
        match self {
            Heights::Uniform(u) => u.height,
            // Clamped rather than indexed blind: `for_visible` walks `0..len`, so this cannot be
            // reached out of range today, and a panic in a draw on this device reports as a dialog
            // with a number in it.
            Heights::Varying(v) => v.get(index).copied().unwrap_or(0),
        }
    }
}

pub struct ScrollList {
    rows: Heights,
    /// Selection as the app sees it. `None` leaves the list to keep its own in the slot, which is
    /// what a list nobody navigates from the model wants.
    selected: Option<usize>,
    /// Whether this list answers navigation keys itself. See [`ScrollList::focused`].
    focused: bool,
    scrollbar: bool,
    /// Where content shorter than the viewport sits — see [`ScrollList::anchor`].
    anchor: MainAlign,
    /// Shared with the slot table, so the offset the last frame computed is the offset this frame
    /// starts from. `Cell` rather than `RefCell` because [`ListState`] is `Copy`: there is no
    /// borrow flag to get wrong and no runtime panic path, which matters in a `draw` on a device
    /// whose only failure report is a dialog with a number in it.
    state: Rc<Cell<ListState>>,
    /// Measured sizes for the row subtrees, kept across frames for the same reason the bridge keeps
    /// one: a cache that did not outlive the frame would miss every time.
    ///
    /// A list needs its own rather than sharing the screen's, because the engine cannot see inside
    /// a leaf — a `ScrollList` is one node to the pass above it, and the rows underneath are this
    /// widget's private tree. Slots are handed out by *screen position*, not by row index: the
    /// visible window is a handful of slots that scroll past a hundred rows, and keying by index
    /// would grow an entry per row ever seen. A row arriving at a position another row occupied
    /// last frame simply misses on its digest and re-measures, which is a cost, not a bug.
    cache: Rc<RefCell<UiCache>>,
    row: Box<RowFn>,
    /// Where the cursor went, for a list that moves its own. See [`ScrollList::on_move`].
    moved: Option<Box<dyn Fn(usize)>>,
    /// A navigation key that had nowhere to go. See [`ScrollList::on_edge`].
    edge: Option<Box<dyn Fn(Edge)>>,
}

/// Which end of a list a key ran into.
///
/// Reported by [`ScrollList::on_edge`], which exists for the one thing a clamped cursor cannot say:
/// the dialog list asks the server for another page when Down is pressed on the last row, and a
/// selection that simply refused to move looks identical to one that never got the key.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Edge {
    /// Up or page-up, already at the first row. A transcript pages *older* here.
    Top,
    /// Down or page-down, already at the last row. A dialog list asks for the next page.
    Bottom,
}

impl ScrollList {
    /// A list of `count` rows, each `row_height` pixels tall.
    ///
    /// Takes the slot table because that is where the scroll offset has to come from: this struct
    /// is rebuilt every frame and the offset must not be.
    pub fn new(slots: &mut SlotTable, count: usize, row_height: i32) -> Self {
        let state = slots
            .use_state_with(|| Rc::new(Cell::new(ListState::new())))
            .clone();
        let cache = slots
            .use_state_with(|| Rc::new(RefCell::new(UiCache::new())))
            .clone();
        Self {
            rows: Heights::Uniform(Uniform { count, height: row_height.max(1) }),
            selected: None,
            focused: false,
            scrollbar: true,
            anchor: MainAlign::Start,
            state,
            cache,
            row: Box::new(|_, _| Node::leaf(Empty)),
            moved: None,
            edge: None,
        }
    }

    /// A list whose rows each have their own height.
    ///
    /// For a transcript, where a row is as tall as its wrapped text and no two agree. The caller
    /// measures — it is the one that knows the font, the width and the message — and this places.
    /// A height of zero is legal and means a row that occupies no space; negative ones are clamped
    /// away, since a negative height would walk the scroll offset backwards.
    pub fn varying(slots: &mut SlotTable, heights: Vec<i32>) -> Self {
        let mut me = Self::new(slots, 0, 1);
        me.rows = Heights::Varying(heights.into_iter().map(|h| h.max(0)).collect());
        me
    }

    /// Where content shorter than the viewport sits — CSS's `justify-content`, for the one axis a
    /// list has.
    ///
    /// [`MainAlign::Start`] is the default and is what a dialog list wants: two chats sit at the
    /// top and the rest of the panel is empty. A transcript wants the opposite. Every chat client
    /// hangs a short conversation from the composer, and the alternative — two messages at the top
    /// with a wall of empty space under them — reads as a screen that failed to draw rather than as
    /// a conversation that is short.
    ///
    /// This only moves content that *fits*. Once there is more than the viewport holds there is no
    /// slack to place and the scroll offset decides everything, so the setting quietly stops
    /// mattering — which is the correct behaviour and worth knowing before wondering why it appears
    /// to have no effect.
    pub fn anchor(mut self, anchor: MainAlign) -> Self {
        self.anchor = anchor;
        self
    }

    /// Drive the selection from the model.
    pub fn selected(mut self, index: usize) -> Self {
        self.selected = Some(index);
        self
    }

    /// Let this list move its own cursor on Up/Down and the page keys.
    ///
    /// **Off by default, and that is the important half.** Now that keys reach widgets
    /// ([`crate::layout::dispatch_key`]), a list that answered arrows unconditionally would move
    /// its cursor *as well as* the `update` that the app already wrote for the same keys — two
    /// owners of one selection, drifting apart the first time one of them clamps and the other does
    /// not. That is the failure this widget's own header warns about.
    ///
    /// So the default keeps the MVU contract: the model owns the selection and the list is told it
    /// through [`Self::selected`]. Turn this on for a list whose cursor is nobody else's business —
    /// a picker inside a dialog, a screen with no `update` of its own.
    pub fn focused(mut self, on: bool) -> Self {
        self.focused = on;
        self
    }

    /// Whether to reserve and draw the scrollbar gutter. On by default, because
    /// [`chrome::scrollbar`] draws a full-height thumb when everything fits — "this is all of it"
    /// is an answer, and a bar that comes and goes reflows the row text beside it.
    pub fn scrollbar(mut self, on: bool) -> Self {
        self.scrollbar = on;
        self
    }

    /// How to build the widget for a row. Called only for rows on screen.
    pub fn row(mut self, f: impl Fn(usize, bool) -> Node + 'static) -> Self {
        self.row = Box::new(f);
        self
    }

    /// Move the cursor here, and tell the app where it went.
    ///
    /// # Why a list is ever allowed to move its own cursor
    ///
    /// [`focused`](Self::focused) states the rule this bends: the model owns the selection, because
    /// two owners drift. The rule has one problem, and it is not a matter of taste — moving a cursor
    /// correctly needs the *viewport*. `Left` and `Right` are page keys in
    /// [`ListState::handle_key`], a page is "how many rows fit on screen", and the model does not
    /// know how tall the band is. An `update` that paged would be guessing at a number only the
    /// layout knows, which is the same objection [`crate::slot`] makes about scroll offsets.
    ///
    /// So the list moves the cursor — it has the rect, so it has the viewport — and hands the new
    /// index straight back to the app, which records it in the model like any other message:
    ///
    /// ```ignore
    /// let out = m.out.clone();
    /// ScrollList::new(slots, n, row_h)
    ///     .selected(m.selected)                      // the model still drives it
    ///     .on_move(move |i| out.push(Msg::Select(i))) // and is told where it moved to
    /// ```
    ///
    /// That is one owner, not two: the model holds the value and the list is the only thing that
    /// changes it. The closure pushes into an [`Outbox`](crate::outbox::Outbox) — a `dyn Fn(usize)`
    /// rather than a message and a queue, so this widget never learns the app's message type.
    ///
    /// Implies [`focused`](Self::focused): a list that reports movement has to be doing the moving.
    /// Do not *also* claim `Up`/`Down` in [`DeclarativeApp::on_key`](crate::app::DeclarativeApp::on_key)
    /// — that runs first, so the app would win and this would silently never fire.
    pub fn on_move(mut self, f: impl Fn(usize) + 'static) -> Self {
        self.moved = Some(Box::new(f));
        self.focused = true;
        self
    }

    /// A navigation key arrived with nowhere left to go.
    ///
    /// The dialog list's pagination: `chats.rs` asks the server for another page when Down is
    /// pressed on the last row, and a clamped cursor cannot report that — nothing moved, so
    /// [`on_move`](Self::on_move) is silent, and the press is indistinguishable from one that landed
    /// on a list already at rest.
    ///
    /// Fires *instead of* a move, never as well: it is the answer to "this key had no effect on the
    /// selection". The key is still consumed, exactly as [`ListState::handle_key`] consumes a
    /// clamped arrow — the app asked for a page, which is something happening.
    ///
    /// Also implies [`focused`](Self::focused), for the same reason.
    pub fn on_edge(mut self, f: impl Fn(Edge) + 'static) -> Self {
        self.edge = Some(Box::new(f));
        self.focused = true;
        self
    }

    /// The scroll offset in pixels, as it stands after the last draw or key.
    pub fn scroll(&self) -> i32 {
        self.state.get().scroll
    }

    /// The focused row. After a draw this is always a row that exists — see [`Self::sync`].
    pub fn selection(&self) -> usize {
        self.state.get().selected
    }

    /// Reconcile the kept state with this frame's rows before using it.
    ///
    /// Two things can have changed since the offset was computed: the app moved the selection, and
    /// the row count changed underneath it. [`ListState::select`] and [`ListState::clamp`] both
    /// already handle the second — `clamp` is the one that stops a selection dangling past the end
    /// after messages are deleted, which otherwise shows up much later as a list with no visible
    /// highlight and no obvious cause.
    fn sync(&self, viewport_h: i32) -> ListState {
        let mut st = self.state.get();
        match self.selected {
            Some(i) => st.select(i, &self.rows, viewport_h),
            None => st.clamp(&self.rows, viewport_h),
        }
        self.state.set(st);
        st
    }

    /// How far down the band the first row starts, when everything fits.
    ///
    /// Zero unless [`anchor`](Self::anchor) says otherwise, and zero whenever the content overflows
    /// — there is no slack to distribute then, and offsetting anyway would push the top row off the
    /// screen with no way to scroll back to it.
    fn slack_offset(&self, band_h: i32) -> i32 {
        let slack = band_h - ListState::content_height(&self.rows);
        if slack <= 0 {
            return 0;
        }
        match self.anchor {
            MainAlign::Start => 0,
            MainAlign::End => slack,
            MainAlign::Center => slack / 2,
            // A list has one child per row and no joins to share the leftover between, so
            // `SpaceBetween` has nothing to space. Treated as `Start` rather than silently as
            // something else.
            MainAlign::SpaceBetween => 0,
        }
    }

    /// The band rows are drawn into, with the scrollbar gutter taken off the right.
    fn content(&self, rect: Rect, theme: &Theme<'_>) -> Rect {
        if self.scrollbar {
            Rect { x1: rect.x1 - chrome::scrollbar_gutter(theme, true), ..rect }
        } else {
            rect
        }
    }
}

impl Widget for ScrollList {
    fn content_hash(&self) -> WidgetHash {
        // Scroll is deliberately absent: scrolling moves rows, it does not resize the list, and
        // including it would re-measure the whole subtree on every keypress.
        let h = hash_i32(0, self.rows.len() as i32);
        // Every height, not just the count: two transcripts with the same number of messages and
        // different wrapping are different lists, and a digest that missed that would keep the old
        // measurements for the new text.
        let h = match &self.rows {
            Heights::Uniform(u) => hash_i32(h, u.height),
            Heights::Varying(v) => v.iter().fold(h, |acc, &px| hash_i32(acc, px)),
        };
        hash_i32(h, self.scrollbar as i32)
    }

    fn measure(&self, constraints: Constraints, _theme: &Theme<'_>) -> Size {
        // A list takes everything it is offered. Its content height is not its size — that is the
        // whole point of scrolling — so asking for the content would make a 200-row list demand
        // 8000 pixels and break every parent that believed it.
        constraints.constrain(Size::new(constraints.max_w, constraints.max_h))
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        let band = self.content(rect, theme);
        let st = self.sync(band.height());
        let sel = st.selected;
        let has_rows = self.rows.len() > 0;

        let mut cache = self.cache.borrow_mut();
        // This list's own frame, over this list's own cache. The bridge is forbidden from calling
        // `begin_frame` on the screen's cache — that is layout's phase to run — but this one is not
        // the screen's, it is this widget's, and nobody else is in a position to start its frame.
        // Without it the rects of rows that scrolled away would still look current.
        cache.begin_frame();

        // `draw_visible`, not `for_visible`: the first partially-visible row gets a rect that
        // starts above the band, and unclipped its text lands on the title bar. This crate carried
        // its own `clip_to` here for a while, which fixed this list and left the seven other row
        // loops in the SDK and the launcher still bleeding. The clip now lives in `ListState`,
        // where every caller gets it and none has to remember.
        let mut slot = 0usize;
        // The rows walk from `band` shifted down by whatever slack the anchor claims.
        //
        // `draw_visible` clips to the viewport it is handed, so this shifts the clip too — safe
        // only because the offset is non-zero exactly when the content fits, and content that fits
        // inside `band` still fits inside `band` moved down by its own slack. Were the offset ever
        // applied to overflowing content it would trim the last row by that many pixels, which is
        // why `slack_offset` returns zero there rather than leaving it to this call site.
        let rows_at = Rect { y0: band.y0 + self.slack_offset(band.height()), ..band };
        st.draw_visible(c, &self.rows, rows_at, |c, i, r| {
            // The highlight goes down first and full-bleed: with no pointer it is the only thing
            // saying where you are, so a row drawing its own background must not cover it.
            if has_rows && i == sel {
                chrome::selection(c, r, theme);
            }
            let node = (self.row)(i, i == sel);
            // The same three passes the bridge runs, on a subtree: measure, place, draw. `slot` is
            // the base for this row's subtree and advances by the whole subtree's width, or two
            // rows would write over each other's rects.
            layout::measure_node(&node, slot, Constraints::tight(r.width(), r.height()), theme, &mut cache);
            layout::layout_node(&node, slot, r, &mut cache);
            layout::draw_node(&node, slot, &cache, c, theme);
            slot += node.slot_count();
        });
        drop(cache);

        if self.scrollbar {
            chrome::scrollbar(c, rect, theme, st.scrollbar(&self.rows, band.height()));
        }
    }

    fn handle_key(&self, ev: KeyEvent, rect: Rect, _cx: &mut KeyCtx<'_>) -> Handled {
        // Only when the list was told it owns its cursor. The default is that the model does, and
        // moving the selection here as well would give one list two drivers — see
        // [`ScrollList::focused`].
        if !self.focused {
            return Handled::Ignored;
        }
        // `rect.height()` and not the content band: the scrollbar gutter is taken off the width,
        // never the height, and there is no theme here to ask for its width anyway.
        // Reconciled against this frame's rows *before* the key, not after: the app may have moved
        // the selection since the last draw, and moving from a stale cursor is how a list scrolls
        // from where it used to be. `sync` also clamps a selection left dangling by deleted rows.
        let mut st = self.sync(rect.height());
        let before = st.selected;
        let out = st.handle_key(ev, &self.rows, rect.height());
        self.state.set(st);
        if out != Handled::Consumed {
            return out;
        }
        match (st.selected == before, &self.moved, &self.edge) {
            // It moved, and somebody wants to know where to.
            (false, Some(report), _) => report(st.selected),
            // It did not, so this key ran into an end. Which end is decided by the key rather than
            // by the position, because a one-row list is at both at once and `Down` still means
            // "further on" — which is where another page would be.
            (true, _, Some(report)) => {
                if let Some(edge) = edge_of(ev) {
                    report(edge);
                }
            }
            _ => {}
        }
        out
    }
}

/// Which end a navigation key was reaching for, or `None` if it was not a navigation key.
///
/// Reads the key rather than the position, so a list with one row — which is at the top and the
/// bottom simultaneously — still reports `Bottom` for Down, which is the direction another page
/// would come from.
fn edge_of(ev: KeyEvent) -> Option<Edge> {
    match ev.key {
        Key::Up | Key::Left => Some(Edge::Top),
        Key::Down | Key::Right => Some(Edge::Bottom),
        _ => None,
    }
}

/// A row builder that was never set draws nothing rather than panicking: a list with no `.row()`
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
    // ---- a list that moves its own cursor ---------------------------------------------------------

    /// The list, the reports it made, and the state behind it — assembled once per test.
    fn reporting(
        slots: &mut SlotTable,
        count: usize,
        selected: usize,
    ) -> (ScrollList, Rc<RefCell<Vec<usize>>>, Rc<RefCell<Vec<Edge>>>) {
        let moves: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));
        let edges: Rc<RefCell<Vec<Edge>>> = Rc::new(RefCell::new(Vec::new()));
        let (m, e) = (moves.clone(), edges.clone());
        let list = ScrollList::new(slots, count, ROW)
            .selected(selected)
            .on_move(move |i| m.borrow_mut().push(i))
            .on_edge(move |edge| e.borrow_mut().push(edge));
        (list, moves, edges)
    }

    #[test]
    fn a_reporting_list_says_where_its_cursor_went() {
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let (list, moves, edges) = reporting(&mut slots, 10, 3);
        crate::widget::with_key_ctx(|cx| {
            assert_eq!(list.handle_key(press(Key::Down), viewport(), cx), Handled::Consumed);
        });
        assert_eq!(*moves.borrow(), alloc::vec![4]);
        assert!(edges.borrow().is_empty(), "it moved, so it did not run into anything");
        assert_eq!(list.selection(), 4);
    }

    #[test]
    fn the_page_keys_move_by_a_viewport_the_model_could_not_have_known() {
        // The reason a list is allowed to move its own cursor at all: `Left`/`Right` page by "how
        // many rows fit", which is a layout fact. H/ROW rows fit here.
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let (list, moves, _) = reporting(&mut slots, 100, 0);
        crate::widget::with_key_ctx(|cx| {
            list.handle_key(press(Key::Right), viewport(), cx);
        });
        let page = *moves.borrow().first().expect("a page key reported nothing");
        assert!(page > 1, "a page is more than one row");
        assert!(page <= (H / ROW) as usize, "and no more than a screenful");
    }

    #[test]
    fn down_on_the_last_row_reports_the_bottom_rather_than_a_move() {
        // The dialog list's pagination. Nothing moved, so `on_move` is silent — and a press that
        // changed nothing is exactly what asking for another page looks like from here.
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let (list, moves, edges) = reporting(&mut slots, 10, 9);
        crate::widget::with_key_ctx(|cx| {
            assert_eq!(list.handle_key(press(Key::Down), viewport(), cx), Handled::Consumed);
        });
        assert!(moves.borrow().is_empty());
        assert_eq!(*edges.borrow(), alloc::vec![Edge::Bottom]);
    }

    #[test]
    fn up_on_the_first_row_reports_the_top() {
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let (list, moves, edges) = reporting(&mut slots, 10, 0);
        crate::widget::with_key_ctx(|cx| {
            list.handle_key(press(Key::Up), viewport(), cx);
        });
        assert!(moves.borrow().is_empty());
        assert_eq!(*edges.borrow(), alloc::vec![Edge::Top]);
    }

    #[test]
    fn a_one_row_list_still_has_two_ends() {
        // It is at the top and the bottom at once, so the *key* decides which end was reached.
        // Deciding from the position would make `Down` on a single row report `Top`.
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let (list, _, edges) = reporting(&mut slots, 1, 0);
        crate::widget::with_key_ctx(|cx| {
            list.handle_key(press(Key::Down), viewport(), cx);
            list.handle_key(press(Key::Up), viewport(), cx);
        });
        assert_eq!(*edges.borrow(), alloc::vec![Edge::Bottom, Edge::Top]);
    }

    #[test]
    fn a_key_that_is_not_navigation_reports_nothing_and_is_not_taken() {
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let (list, moves, edges) = reporting(&mut slots, 10, 3);
        crate::widget::with_key_ctx(|cx| {
            assert_eq!(list.handle_key(press(Key::Char('a')), viewport(), cx), Handled::Ignored);
            assert_eq!(list.handle_key(press(Key::Select), viewport(), cx), Handled::Ignored);
        });
        assert!(moves.borrow().is_empty());
        assert!(edges.borrow().is_empty(), "Select is the action key, not an end of the list");
    }

    #[test]
    fn an_unfocused_list_reports_nothing_because_it_never_sees_the_key() {
        // The default. `on_move` turns focus on for you; without either, the model is the only thing
        // moving the cursor and this widget must not answer arrows at all.
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let list = ScrollList::new(&mut slots, 10, ROW).selected(3);
        crate::widget::with_key_ctx(|cx| {
            assert_eq!(list.handle_key(press(Key::Down), viewport(), cx), Handled::Ignored);
        });
        // Untouched: `sync` runs in `draw` and in a focused `handle_key`, and neither happened —
        // so the slot still holds the initial cursor rather than the model's, which is exactly what
        // "this widget did nothing" looks like from outside.
        assert_eq!(list.selection(), 0, "the cursor moved behind the model's back");
    }

    #[test]
    fn the_cursor_starts_from_where_the_model_left_it() {
        // A key can arrive before the next draw, so the widget's own state may be a frame behind
        // what the model says. Moving from the stale value is how a list jumps back to where it was
        // two presses ago.
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let (list, moves, _) = reporting(&mut slots, 20, 0);
        crate::widget::with_key_ctx(|cx| {
            list.handle_key(press(Key::Down), viewport(), cx);
        });
        assert_eq!(*moves.borrow(), alloc::vec![1]);

        // Next frame: the app decided the selection is 10, and the key that follows moves from
        // there rather than from 1.
        slots.begin_frame();
        let (list, moves, _) = reporting(&mut slots, 20, 10);
        crate::widget::with_key_ctx(|cx| {
            list.handle_key(press(Key::Down), viewport(), cx);
        });
        assert_eq!(*moves.borrow(), alloc::vec![11]);
    }


    use super::*;
    use alloc::rc::Rc;
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use symbian_gfx::Size as GSize;
    use symbian_ui::{testing, Key, Palette};

    const W: i32 = 320;
    const H: i32 = 200;
    const ROW: i32 = 40;

    fn press(k: Key) -> KeyEvent {
        KeyEvent::new(k)
    }

    fn viewport() -> Rect {
        Rect::from_xywh(0, 0, W, H)
    }

    /// Build a list the way a frame does, recording which rows the builder was asked for.
    fn list_of(slots: &mut SlotTable, count: usize, selected: usize, built: &Rc<RefCell<Vec<usize>>>) -> ScrollList {
        let seen = built.clone();
        ScrollList::new(slots, count, ROW)
            .selected(selected)
            .row(move |i, _sel| {
                seen.borrow_mut().push(i);
                Node::leaf(Empty)
            })
    }

    /// One frame: build the tree, draw it, drop it — exactly what the bridge does.
    fn frame(list: &ScrollList) {
        testing::with_theme(Palette::DARK, |t| {
            let mut buf = alloc::vec![0u16; (W * H) as usize];
            let mut c = Canvas::from_slice(&mut buf, GSize::new(W, H));
            list.draw(&mut c, viewport(), t);
        });
    }

    #[test]
    fn a_long_list_scrolls_and_the_offset_survives_the_tree_being_rebuilt() {
        // The slot table's whole job, stated as the case it was built for: the widget below is
        // constructed and dropped three times, and the scroll offset is not.
        let mut slots = SlotTable::new();
        let built = Rc::new(RefCell::new(Vec::new()));

        slots.begin_frame();
        let l = list_of(&mut slots, 200, 0, &built);
        frame(&l);
        assert_eq!(l.scroll(), 0);
        drop(l);

        // A later frame, a lower selection: the list scrolls to reveal it.
        slots.begin_frame();
        let l = list_of(&mut slots, 200, 60, &built);
        frame(&l);
        let scrolled = l.scroll();
        assert!(scrolled > 0, "selecting row 60 of 200 must scroll");
        drop(l);

        // A third frame with the same selection must not jump back to the top — which is exactly
        // what happens when the offset lives in the widget instead of the slot.
        slots.begin_frame();
        let l = list_of(&mut slots, 200, 60, &built);
        frame(&l);
        assert_eq!(l.scroll(), scrolled, "a rebuilt tree must not reset the scroll offset");
    }

    #[test]
    fn only_the_rows_on_screen_are_built() {
        // The allocation bound. 200 rows exist; a 200px viewport of 40px rows shows five, six
        // while straddling. Building all 200 would be 200 boxes per frame to draw five.
        let mut slots = SlotTable::new();
        let built = Rc::new(RefCell::new(Vec::new()));

        slots.begin_frame();
        let l = list_of(&mut slots, 200, 0, &built);
        frame(&l);

        let seen = built.borrow();
        assert!(!seen.is_empty(), "something must be drawn");
        assert!(seen.len() <= 6, "built {} rows for a screen that shows five", seen.len());
        assert!(seen.iter().all(|&i| i < 6), "built a row that is nowhere near the viewport");
    }

    #[test]
    fn scrolling_far_down_still_builds_only_a_screenful() {
        let mut slots = SlotTable::new();
        let built = Rc::new(RefCell::new(Vec::new()));

        slots.begin_frame();
        let l = list_of(&mut slots, 200, 150, &built);
        frame(&l);

        let seen = built.borrow();
        assert!(seen.len() <= 6, "built {} rows", seen.len());
        // And they are the rows around the selection, not the ones at the top of the list.
        assert!(seen.contains(&150), "the selected row must be on screen");
        assert!(!seen.contains(&0));
    }

    #[test]
    fn the_row_count_shrinking_below_the_cursor_does_not_leave_it_past_the_end() {
        // Messages deleted, a search filtered the list. `ListState::clamp` is what rescues this and
        // the reason this widget calls it on every frame rather than only when it notices a change
        // — noticing would mean keeping a copy of the old count, which is one more thing to get
        // wrong than simply asking.
        let mut slots = SlotTable::new();
        let built = Rc::new(RefCell::new(Vec::new()));

        slots.begin_frame();
        let l = list_of(&mut slots, 20, 19, &built);
        frame(&l);
        assert_eq!(l.selection(), 19);
        assert!(l.scroll() > 0);
        drop(l);

        // The next frame has three rows and no selection from the model: the kept state must come
        // back into range on its own.
        slots.begin_frame();
        let l = ScrollList::new(&mut slots, 3, ROW).row(|_, _| Node::leaf(Empty));
        frame(&l);
        assert_eq!(l.selection(), 2, "selection must not dangle past the last row");
        assert_eq!(l.scroll(), 0, "three rows fit, so there is nothing to scroll");
    }

    #[test]
    fn an_empty_list_draws_nothing_and_panics_at_nothing() {
        let mut slots = SlotTable::new();
        let built = Rc::new(RefCell::new(Vec::new()));

        slots.begin_frame();
        let l = list_of(&mut slots, 0, 0, &built);
        frame(&l);
        assert!(built.borrow().is_empty());
        assert_eq!(l.scroll(), 0);
    }

    #[test]
    fn two_lists_on_one_screen_do_not_share_a_scroll_offset() {
        // Positional slot identity: two `new` calls, two slots. If they collided, opening a screen
        // with a list above and a list below would scroll both at once.
        let mut slots = SlotTable::new();
        let built = Rc::new(RefCell::new(Vec::new()));

        slots.begin_frame();
        let top = list_of(&mut slots, 200, 0, &built);
        let bottom = list_of(&mut slots, 200, 60, &built);
        frame(&top);
        frame(&bottom);

        assert_eq!(top.scroll(), 0);
        assert!(bottom.scroll() > 0);
        assert_eq!(slots.type_mismatches(), 0);
    }

    #[test]
    fn the_selected_row_is_told_that_it_is() {
        let mut slots = SlotTable::new();
        let flags = Rc::new(RefCell::new(Vec::new()));
        let seen = flags.clone();

        slots.begin_frame();
        let l = ScrollList::new(&mut slots, 5, ROW).selected(2).row(move |i, sel| {
            seen.borrow_mut().push((i, sel));
            Node::leaf(Empty)
        });
        frame(&l);

        let got = flags.borrow();
        assert_eq!(got.iter().filter(|(_, sel)| *sel).count(), 1, "exactly one row is selected");
        assert!(got.contains(&(2, true)));
    }

    #[test]
    fn a_focused_list_moves_its_own_cursor_and_leaves_select_alone() {
        // For a list that owns its cursor. It must delegate — Down moves by one and pages exist —
        // and it must leave `Select` alone, or the screen loses the key that opens the row the
        // list is highlighting.
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let l = ScrollList::new(&mut slots, 200, ROW).focused(true);

        crate::widget::with_key_ctx(|cx| {
            assert_eq!(l.handle_key(press(Key::Down), viewport(), cx), Handled::Consumed);
            assert_eq!(l.selection(), 1);
            assert_eq!(l.handle_key(press(Key::Select), viewport(), cx), Handled::Ignored);
            assert_eq!(l.handle_key(press(Key::Char('a')), viewport(), cx), Handled::Ignored);
            assert_eq!(l.selection(), 1, "an ignored key must not have moved anything");
        });
    }

    #[test]
    fn a_list_driven_by_the_model_refuses_to_move_itself() {
        // The default, and the one that matters now that keys reach widgets: an app that maps
        // Up/Down in `update` would otherwise have its selection moved twice by one press — once by
        // the message and once here — and the two would part company the first time one clamped.
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let l = ScrollList::new(&mut slots, 200, ROW).selected(3);

        crate::widget::with_key_ctx(|cx| {
            assert_eq!(l.handle_key(press(Key::Down), viewport(), cx), Handled::Ignored);
            assert_eq!(l.handle_key(press(Key::Up), viewport(), cx), Handled::Ignored);
        });
    }

    #[test]
    fn the_size_is_the_offer_not_the_content() {
        // A 200-row list that asked for its content height would demand 8000 pixels and break
        // every parent that believed the answer.
        testing::with_theme(Palette::DARK, |t| {
            let mut slots = SlotTable::new();
            slots.begin_frame();
            let l = ScrollList::new(&mut slots, 200, ROW);
            assert_eq!(l.measure(Constraints::tight(W, H), t), Size::new(W, H));
            assert_eq!(l.measure(Constraints::loose(W, H), t), Size::new(W, H));
        });
    }

    #[test]
    fn scrolling_never_runs_off_either_end() {
        // Borrowed from list.rs's own hazard: the thumb and the offset must stay in range at both
        // extremes, for every selection in a long list.
        let mut slots = SlotTable::new();
        for sel in 0..200 {
            slots.begin_frame();
            let l = ScrollList::new(&mut slots, 200, ROW).selected(sel);
            frame(&l);
            let s = l.scroll();
            assert!(s >= 0, "negative scroll at row {sel}");
            assert!(s <= 200 * ROW - H, "scrolled past the end at row {sel}: {s}");
        }
    }
}

#[cfg(test)]
mod varying_tests {
    use super::*;
    use symbian_ui::{testing, Palette};

    /// The top and bottom of row `selected`, read off the screen.
    ///
    /// The selected row draws `chrome::selection` full-bleed across its whole rect, so the inked
    /// band *is* the row's rectangle — no arithmetic here repeats the arithmetic under test. The
    /// first version of this probe looked for each row's text instead and reported one row where
    /// there were three: the `testing` theme's font measures but does not ink, so every glyph drew
    /// nothing at all. A probe that finds no evidence and says nothing is worse than one that fails.
    fn row_span(heights: Vec<i32>, selected: usize) -> (i32, i32) {
        let (mut top, mut bottom) = (-1, -1);
        testing::with_theme(Palette::DARK, |t| {
            let mut slots = SlotTable::new();
            let list = ScrollList::varying(&mut slots, heights.clone())
                .selected(selected)
                .scrollbar(false);
            let (_, px) = testing::with_canvas(Size::new(100, 120), |c| {
                list.draw(c, Rect::from_xywh(0, 0, 100, 120), t);
            });
            for y in 0..120usize {
                if (0..100).any(|x| px[y * 100 + x] != 0) {
                    if top < 0 {
                        top = y as i32;
                    }
                    bottom = y as i32 + 1;
                }
            }
        });
        (top, bottom)
    }

    #[test]
    fn a_row_is_as_tall_as_it_asked_to_be() {
        assert_eq!(row_span(alloc::vec![20, 20, 20], 0), (0, 20));
        assert_eq!(row_span(alloc::vec![40, 20, 20], 0), (0, 40), "the first row's own height");
        assert_eq!(row_span(alloc::vec![20, 35, 20], 1), (20, 55), "and the second row's");
    }

    #[test]
    fn a_taller_row_pushes_the_next_one_further_down() {
        // Stated as a comparison rather than as coordinates: the same rows with the first one 20
        // pixels taller start the second row 20 pixels lower, and it keeps its own height.
        let (even_top, even_bottom) = row_span(alloc::vec![20, 20, 20], 1);
        let (uneven_top, uneven_bottom) = row_span(alloc::vec![40, 20, 20], 1);
        assert_eq!(uneven_top - even_top, 20, "the second row moves by exactly the extra height");
        assert_eq!(uneven_bottom - uneven_top, even_bottom - even_top, "and does not change size");
    }

    #[test]
    fn uniform_and_varying_agree_when_the_heights_are_the_same() {
        // The two constructors must not become two layouts. If they ever disagree, one is doing
        // arithmetic the other is not.
        let varying = row_span(alloc::vec![20, 20, 20], 1);
        let mut uniform = (-1, -1);
        testing::with_theme(Palette::DARK, |t| {
            let mut slots = SlotTable::new();
            let list = ScrollList::new(&mut slots, 3, 20).selected(1).scrollbar(false);
            let (_, px) = testing::with_canvas(Size::new(100, 120), |c| {
                list.draw(c, Rect::from_xywh(0, 0, 100, 120), t);
            });
            let (mut top, mut bottom) = (-1, -1);
            for y in 0..120usize {
                if (0..100).any(|x| px[y * 100 + x] != 0) {
                    if top < 0 {
                        top = y as i32;
                    }
                    bottom = y as i32 + 1;
                }
            }
            uniform = (top, bottom);
        });
        assert_eq!(varying, uniform);
    }

    #[test]
    fn the_digest_moves_when_a_single_row_changes_height() {
        // The trap this exists for: hashing only the row count would let a re-wrapped transcript
        // keep the old measurements, which shows up as bubbles overlapping and nothing else.
        let mut a_slots = SlotTable::new();
        let a = ScrollList::varying(&mut a_slots, alloc::vec![10, 20, 30]);
        let mut b_slots = SlotTable::new();
        let b = ScrollList::varying(&mut b_slots, alloc::vec![10, 25, 30]);
        assert_ne!(a.content_hash(), b.content_hash());
    }

    #[test]
    fn a_negative_height_cannot_walk_the_list_backwards() {
        let mut slots = SlotTable::new();
        let list = ScrollList::varying(&mut slots, alloc::vec![20, -5, 20]);
        assert_eq!(list.rows.height(1), 0, "clamped to zero, not carried through");
        assert_eq!(list.rows.len(), 3, "and the row still exists");
    }
}

#[cfg(test)]
mod anchor_tests {
    use super::*;
    use symbian_ui::{testing, Palette};

    /// Where the selected row's highlight lands in a 100-tall viewport.
    fn span(rows: usize, row_h: i32, anchor: MainAlign, selected: usize) -> (i32, i32) {
        let (mut top, mut bottom) = (-1, -1);
        testing::with_theme(Palette::DARK, |t| {
            let mut slots = SlotTable::new();
            let list = ScrollList::new(&mut slots, rows, row_h)
                .selected(selected)
                .anchor(anchor)
                .scrollbar(false);
            let (_, px) = testing::with_canvas(Size::new(100, 100), |c| {
                list.draw(c, Rect::from_xywh(0, 0, 100, 100), t);
            });
            for y in 0..100usize {
                if (0..100).any(|x| px[y * 100 + x] != 0) {
                    if top < 0 {
                        top = y as i32;
                    }
                    bottom = y as i32 + 1;
                }
            }
        });
        (top, bottom)
    }

    #[test]
    fn a_short_list_hangs_from_the_bottom_when_asked() {
        // Two rows of 20 in a 100-tall band: anchored to the start they sit at 0..40, anchored to
        // the end the last one finishes flush with the bottom edge.
        assert_eq!(span(2, 20, MainAlign::Start, 1), (20, 40));
        assert_eq!(span(2, 20, MainAlign::End, 1), (80, 100), "the last row ends at the band's foot");
        assert_eq!(span(2, 20, MainAlign::Center, 1), (50, 70));
    }

    #[test]
    fn a_full_list_ignores_the_anchor_entirely() {
        // Five rows of 20 exactly fill 100: there is no slack, so every anchor must agree. This is
        // the boundary where an off-by-one would push the top row off the screen with no way back.
        let start = span(5, 20, MainAlign::Start, 0);
        assert_eq!(span(5, 20, MainAlign::End, 0), start);
        assert_eq!(span(5, 20, MainAlign::Center, 0), start);
        assert_eq!(start, (0, 20));
    }

    #[test]
    fn an_overflowing_list_ignores_the_anchor_and_keeps_its_last_row() {
        // Six rows of 20 in 100: more than fits. The anchor must not shift anything — the clip
        // moves with the rows, so an offset applied here would trim the bottom row silently.
        let start = span(6, 20, MainAlign::Start, 0);
        assert_eq!(span(6, 20, MainAlign::End, 0), start);

        // And the last row, selected, still reaches the bottom edge of the band under both.
        assert_eq!(span(6, 20, MainAlign::End, 5), span(6, 20, MainAlign::Start, 5));
        assert_eq!(span(6, 20, MainAlign::End, 5).1, 100, "the last row is not trimmed");
    }

    #[test]
    fn the_default_is_the_top_because_that_is_what_every_existing_list_assumed() {
        assert_eq!(span(2, 20, MainAlign::Start, 0), (0, 20));
        let mut slots = SlotTable::new();
        assert_eq!(ScrollList::new(&mut slots, 2, 20).anchor, MainAlign::Start);
    }
}
