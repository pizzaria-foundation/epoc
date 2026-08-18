//! A whole small app, driven the way the device drives one.
//!
//! Every other test in this crate proves one piece in isolation: the softkey table dispatches, the
//! slot table keeps a caret, the cache stores a size. This file is the one that says the pieces add
//! up, because they can each be right while the assembly is wrong — the defect this crate exists to
//! prevent was exactly that, two halves that were individually correct and disagreed with each
//! other.
//!
//! It is written from outside the crate on purpose. An integration test can only touch what an
//! application can touch, so anything it cannot reach is something an app cannot reach either, and
//! that is worth finding out here rather than in `apps/`.
//!
//! # What this file used to contain, and no longer does
//!
//! The first version of this app kept a `SlotTable` in its own model behind a `RefCell`, because
//! `view` took `&Model` and nothing else and there was no other way to hand a list the table its
//! scroll offset lives in. It worked, and it was the kind of thing an app author would never invent
//! but would certainly copy from us — so every app on the SDK would have had one.
//!
//! `view` now takes `&mut SlotTable`, the bridge owns the table and runs its frame, and the model
//! below is plain application state again. That is what this file was for: the gap was easier to
//! see in an app than in an argument.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use symbian_decl_ui::app::DeclarativeApp;
use symbian_decl_ui::bridge::DeclarativeAppBridge;
use symbian_decl_ui::cmd::Cmd;
use symbian_decl_ui::keys::Softkeys;
use symbian_decl_ui::slot::SlotTable;
use symbian_decl_ui::widgets::{
    screen::Screen, scroll_list::ScrollList, text::Text, text_field::TextField, Node,
};

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::{testing, App as _, Handled, Key, KeyEvent, Palette, Softkey, Theme};

// ---------------------------------------------------------------- allocation counting

// Counts allocations on the calling thread only.
//
// Thread-local rather than a global counter because the test harness runs these in parallel, and a
// shared count would make every assertion here depend on what some other test happened to be doing
// at the time. `const`-initialised with no destructor so that reading it from inside the allocator
// cannot itself allocate and recurse.
thread_local! {
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
}

struct Counting;

// The one `unsafe` in this file, and unavoidable: measuring allocation means being the allocator.
// Every method forwards straight to the system allocator and only adds a count.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // Counted: a `Vec` that grows every frame is exactly the accumulation this file is looking
        // for, and it shows up as reallocation rather than as a fresh allocation.
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Allocations made by `f`, on this thread.
fn allocations(f: impl FnOnce()) -> u64 {
    let before = ALLOCS.with(|c| c.get());
    f();
    ALLOCS.with(|c| c.get()) - before
}

// ---------------------------------------------------------------- the app

const ROWS: usize = 200;
const ROW_H: i32 = 38;
const W: i32 = 320;
const H: i32 = 240;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Msg {
    Prev,
    Next,
    Open,
    Refresh,
    Quit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Page {
    Detail(usize),
}

struct Model {
    selected: usize,
    refreshed: u32,
    open: Option<usize>,
    /// What the last `view` actually saw, recorded from inside it — the only way to prove the view
    /// read the *updated* model rather than the one that was current when the key arrived.
    viewed_selection: Cell<usize>,
    views: Cell<u32>,
    /// Which rows the list asked to build, most recent frame last. `Rc` because the row closure is
    /// `'static` and cannot borrow the model — which is the same constraint any real app meets the
    /// moment it puts its data in a row.
    rows_built: Rc<RefCell<Vec<usize>>>,
}

struct Recent;

impl DeclarativeApp for Recent {
    type Model = Model;
    type Message = Msg;
    type Screen = Page;
    const TITLE: &'static str = "Recent";

    fn init() -> Model {
        Model {
            selected: 0,
            refreshed: 0,
            open: None,
            viewed_selection: Cell::new(usize::MAX),
            views: Cell::new(0),
            rows_built: Rc::new(RefCell::new(Vec::new())),
        }
    }

    fn keys(_m: &Model) -> Softkeys<Msg> {
        Softkeys::new()
            .options("Refresh", Msg::Refresh)
            .action("Open", Msg::Open)
            .back("Back", Msg::Quit)
    }

    fn on_key(m: &Model, ev: KeyEvent) -> Option<Msg> {
        match ev.key {
            Key::Up => Some(Msg::Prev),
            Key::Down => Some(Msg::Next),
            _ => Self::keys(m).dispatch(ev),
        }
    }

    fn update(m: &mut Model, msg: Msg) -> Cmd<Page> {
        match msg {
            Msg::Prev => {
                m.selected = m.selected.saturating_sub(1);
                Cmd::None
            }
            Msg::Next => {
                m.selected = (m.selected + 1).min(ROWS - 1);
                Cmd::None
            }
            Msg::Open => {
                m.open = Some(m.selected);
                Cmd::PushScreen(Page::Detail(m.selected))
            }
            Msg::Refresh => {
                m.refreshed += 1;
                Cmd::None
            }
            Msg::Quit => Cmd::Exit,
        }
    }

    fn view(m: &Model, slots: &mut SlotTable) -> Node {
        m.views.set(m.views.get() + 1);
        m.viewed_selection.set(m.selected);

        // The row builder records what it was asked for, which is how the "only visible rows" claim
        // is checked from outside rather than taken on trust.
        let built = m.rows_built.clone();
        let list = ScrollList::new(slots, ROWS, ROW_H)
            .selected(m.selected)
            .row(move |i, selected| {
                built.borrow_mut().push(i);
                Node::leaf(Text::new(if selected { "> row" } else { "row" }))
            });

        Node::leaf(
            Screen::new()
                .title("Recent")
                .content(list)
                .on_options("Refresh", Msg::Refresh)
                .on_action("Open", Msg::Open)
                .on_back("Back", Msg::Quit),
        )
    }
}

// ---------------------------------------------------------------- driving it

fn press(k: Key) -> KeyEvent {
    KeyEvent::new(k)
}

/// One key, through the host's path: the bridge decides whether it means anything.
fn key(b: &mut DeclarativeAppBridge<Recent>, k: Key) -> Handled {
    let mut out = Handled::Ignored;
    testing::with_theme(Palette::DARK, |t: &Theme<'_>| {
        out = b.handle_key(press(k), t, Rect::from_xywh(0, 0, W, H));
    });
    out
}

/// One frame, into a real canvas.
fn frame(b: &mut DeclarativeAppBridge<Recent>) {
    testing::with_theme(Palette::DARK, |t: &Theme<'_>| {
        let mut buf = vec![0u16; (W * H) as usize];
        let mut c = Canvas::from_slice(&mut buf, Size::new(W, H));
        b.draw(&mut c, t);
    });
}

/// A frame drawn into a buffer the caller keeps, so a test can compare pixels between frames.
fn frame_into(b: &mut DeclarativeAppBridge<Recent>, buf: &mut [u16]) {
    testing::with_theme(Palette::DARK, |t: &Theme<'_>| {
        let mut c = Canvas::from_slice(buf, Size::new(W, H));
        b.draw(&mut c, t);
    });
}

fn rows_built(b: &DeclarativeAppBridge<Recent>) -> Vec<usize> {
    b.model().rows_built.borrow().clone()
}

fn clear_rows(b: &DeclarativeAppBridge<Recent>) {
    b.model().rows_built.borrow_mut().clear();
}

// ---------------------------------------------------------------- the tests

#[test]
fn a_key_press_changes_the_model_and_the_next_frame_shows_it() {
    let mut b = DeclarativeAppBridge::<Recent>::new();
    frame(&mut b);
    assert_eq!(b.model().viewed_selection.get(), 0);

    assert_eq!(key(&mut b, Key::Down), Handled::Consumed);
    assert_eq!(b.model().selected, 1, "the message reached update");

    clear_rows(&b);
    frame(&mut b);
    assert_eq!(b.model().viewed_selection.get(), 1, "the view saw the model update had produced");
    assert!(rows_built(&b).contains(&1), "and the list drew the row that is now selected");
}

#[test]
fn the_action_key_fires_the_action_the_bar_promises() {
    // The convention, end to end and through every layer: the screen labels its middle slot
    // "Open", S60 delivers the D-pad centre as `Select`, and the app's `Msg::Open` arm runs. This
    // is the defect that shipped in the launcher, asserted from the outside.
    let mut b = DeclarativeAppBridge::<Recent>::new();
    frame(&mut b);

    key(&mut b, Key::Down);
    key(&mut b, Key::Down);
    assert_eq!(key(&mut b, Key::Select), Handled::Consumed);

    assert_eq!(b.model().open, Some(2), "Select must fire the action, not something else");
    assert_eq!(b.screen(), Some(&Page::Detail(2)), "and its Cmd must have been carried out");
    assert_eq!(b.depth(), 1);
}

#[test]
fn the_outer_softkeys_go_where_the_convention_says() {
    let mut b = DeclarativeAppBridge::<Recent>::new();
    frame(&mut b);

    key(&mut b, Key::Softkey(Softkey::Left));
    assert_eq!(b.model().refreshed, 1, "the left softkey is options");
    assert!(!b.should_exit());

    key(&mut b, Key::Softkey(Softkey::Right));
    assert!(b.should_exit(), "the right softkey is the way out");
}

#[test]
fn the_red_key_leaves_too() {
    // `End` means "get me out" on this hardware, and a screen that ignored it feels stuck.
    let mut b = DeclarativeAppBridge::<Recent>::new();
    frame(&mut b);
    key(&mut b, Key::End);
    assert!(b.should_exit());
}

#[test]
fn an_app_that_exits_stays_exited() {
    let mut b = DeclarativeAppBridge::<Recent>::new();
    frame(&mut b);
    key(&mut b, Key::Softkey(Softkey::Right));
    assert!(b.should_exit());

    // Drawing and more keys must not un-exit it: the host may well draw one more frame before it
    // acts on the flag, and an app that flickered back to life would never close.
    frame(&mut b);
    key(&mut b, Key::Down);
    assert!(b.should_exit());
}

#[test]
fn a_key_nobody_bound_costs_nothing_at_all() {
    // `Ignored` is what tells the host not to repaint. On a device where every keystroke is a
    // full-screen blit, "did nothing" has to actually do nothing.
    let mut b = DeclarativeAppBridge::<Recent>::new();
    frame(&mut b);
    let views = b.model().views.get();

    assert_eq!(key(&mut b, Key::Char('q')), Handled::Ignored);
    assert_eq!(key(&mut b, Key::Backspace), Handled::Ignored);

    assert_eq!(b.model().views.get(), views, "an unbound key must not rebuild the view");
    frame(&mut b);
    assert_eq!(b.model().views.get(), views, "nor make the next frame rebuild it");
}

#[test]
fn ten_idle_frames_re_measure_only_what_declares_that_it_must() {
    // The cache's reason to exist, from outside — and stated as what is actually true rather than
    // as the round number one would like.
    //
    // Idle frames do not re-measure the *tree*: every node whose digest is a real one hits. What
    // they do re-measure is the root, because `Screen::content_hash` deliberately returns zero —
    // its size is a function of the offer rather than of any property, so a digest would make a
    // screen handed a different rect keep the old size. Measuring it is a clamp; hashing it would
    // cost more.
    //
    // So the property is: the per-frame cost is constant and tiny. A cache that had stopped
    // working would show a cost proportional to the tree, and this list has 200 rows behind it.
    let mut b = DeclarativeAppBridge::<Recent>::new();
    frame(&mut b);
    frame(&mut b);

    let mut per_frame = Vec::new();
    for _ in 0..10 {
        frame(&mut b);
        // Read absolutely, not as a delta: `draw_frame` calls `begin_frame`, which resets the
        // counter, so `measure_calls()` is already the count for the frame just drawn. Subtracting
        // the previous frame's reading would compare two per-frame numbers and always yield zero —
        // a test that passes by measuring nothing.
        per_frame.push(b.measure_calls());
    }

    let first = per_frame[0];
    assert!(
        per_frame.iter().all(|&n| n == first),
        "the cost of a still frame moved: {per_frame:?}"
    );
    assert!(
        first <= 2,
        "a still frame measured {first} nodes — the cache is not holding the tree"
    );
}

#[test]
fn ten_idle_frames_do_not_accumulate() {
    // "Allocates nothing new" as the property that is actually true and actually matters: a frame
    // costs what the frame before it cost, so nothing is growing. It is not zero — a `ScrollList`
    // builds a widget per visible row, which is the deliberate trade documented on `RowFn`: about
    // six boxes a frame instead of two hundred. What would be a bug is that number rising.
    let mut b = DeclarativeAppBridge::<Recent>::new();
    frame(&mut b);
    frame(&mut b);

    // `rows_built` is this file's own instrument and it grows by a row per frame, so it reallocates
    // on its own schedule. Clearing it keeps its capacity and takes the measurement's own cost out
    // of the measurement — the first version of this test caught the recorder rather than the app.
    let mut costs = Vec::new();
    for _ in 0..12 {
        clear_rows(&b);
        costs.push(allocations(|| frame(&mut b)));
    }

    let steady = costs[0];
    assert!(
        costs.iter().all(|&c| c == steady),
        "a frame's cost moved while the app sat still: {costs:?}"
    );
    // Non-zero and bounded: a `ScrollList` builds a widget per visible row, which is the trade
    // documented on `RowFn` — about six boxes a frame instead of two hundred. The number is
    // asserted so that a change to it is a decision somebody made rather than a drift.
    assert!(steady < 40, "a still frame allocated {steady} times");
}

#[test]
fn the_slot_table_does_not_grow_while_the_screen_stands_still() {
    // The other half of "nothing accumulates", asked of the table itself. The keys matter: the
    // bridge only rebuilds the view when something changed, so idle frames never re-enter the
    // table at all and would prove nothing. Pressing Down/Up a hundred times runs `view` — and
    // therefore `begin_frame` — a hundred times, which is what reclamation has to survive.
    let mut b = DeclarativeAppBridge::<Recent>::new();
    frame(&mut b);
    frame(&mut b);
    let (slots, groups) = (b.slots().slot_count(), b.slots().group_count());

    for _ in 0..50 {
        key(&mut b, Key::Down);
        frame(&mut b);
        key(&mut b, Key::Up);
        frame(&mut b);
    }
    assert!(b.model().views.get() >= 100, "the view really was rebuilt each time");

    assert_eq!(b.slots().slot_count(), slots);
    assert_eq!(b.slots().group_count(), groups);
    assert_eq!(b.slots().type_mismatches(), 0, "a stable view must never shift an ordinal");
}

#[test]
fn only_the_rows_on_screen_are_ever_built() {
    // Two hundred rows exist; a 240px screen less its chrome shows a handful. Building all of them
    // would allocate two hundred boxes to draw six, every frame.
    let mut b = DeclarativeAppBridge::<Recent>::new();
    frame(&mut b);
    clear_rows(&b);
    frame(&mut b);

    let built = rows_built(&b);
    assert!(!built.is_empty(), "nothing was drawn at all");
    assert!(built.len() <= 8, "built {} rows for one screenful", built.len());
    assert!(built.iter().all(|&i| i < 8), "built a row nowhere near the viewport");
}

#[test]
fn scrolling_to_the_end_still_builds_only_a_screenful() {
    let mut b = DeclarativeAppBridge::<Recent>::new();
    frame(&mut b);
    for _ in 0..ROWS {
        key(&mut b, Key::Down);
    }
    assert_eq!(b.model().selected, ROWS - 1, "selection clamps at the end rather than wrapping");

    clear_rows(&b);
    frame(&mut b);
    let built = rows_built(&b);
    assert!(built.len() <= 8, "built {} rows at the bottom of the list", built.len());
    assert!(built.contains(&(ROWS - 1)), "the selected row must be on screen");
    assert!(!built.contains(&0), "and the top of the list must not be");
}

#[test]
fn the_scroll_offset_survives_every_rebuild_the_keys_cause() {
    // Each keypress rebuilds the tree, so the offset is rebuilt-away or kept a hundred times over
    // the course of this test. It is the slot table that keeps it.
    let mut b = DeclarativeAppBridge::<Recent>::new();
    frame(&mut b);

    for _ in 0..100 {
        key(&mut b, Key::Down);
        frame(&mut b);
    }

    clear_rows(&b);
    frame(&mut b);
    let built = rows_built(&b);
    assert!(
        built.contains(&100),
        "row 100 is selected but is not on screen — the scroll offset was lost in a rebuild"
    );
    assert!(!built.contains(&0));
}

#[test]
fn a_frame_drawn_twice_produces_the_same_pixels() {
    // Drawing is never cached, so two frames of an unchanged model must be identical. If they were
    // not, something in the draw path is carrying state it should not.
    let mut b = DeclarativeAppBridge::<Recent>::new();
    let mut first = vec![0u16; (W * H) as usize];
    let mut second = vec![0u16; (W * H) as usize];

    frame_into(&mut b, &mut first);
    frame_into(&mut b, &mut second);
    assert_eq!(first, second);

    // And a key that changes the selection must change them.
    key(&mut b, Key::Down);
    frame_into(&mut b, &mut second);
    assert_ne!(first, second, "moving the selection did not change the screen");
}

// ------------------------------------------------- a key that reaches a widget
//
// Everything above drives the model through `on_key`. This is the other half, and until now it did
// not work at all: a key that no softkey and no hatch claims is offered to the widgets in the tree.
// A text field is the case that proves it, because a field the user cannot type into is not a
// field — and typing is exactly what the app level has no business turning into messages, one
// message per character, when the buffer that knows about carets is right there.

#[derive(Clone, Debug, PartialEq, Eq)]
enum ComposeMsg {
    /// Take the keyboard away from the field, so a test can press the same key with and without it.
    Blur,
    Quit,
}

struct ComposeModel {
    /// Whether the field has the keyboard. From the model, as focus always is — a screen with two
    /// fields moves this, and that is the whole mechanism that stops both of them answering.
    focused: bool,
    /// A handle on the field's buffer, kept from the frame that built it.
    ///
    /// Not a copy of the text: a copy would be stale the moment a key was pressed, because a key
    /// that a *widget* answers does not rebuild the tree — nothing in the model changed. This is
    /// how a real screen reads what was typed when the user finally presses Send, and the test
    /// reads it the same way.
    buffer: RefCell<Option<Rc<RefCell<symbian_ui::TextField>>>>,
}

struct Compose;

impl DeclarativeApp for Compose {
    type Model = ComposeModel;
    type Message = ComposeMsg;
    type Screen = Page;
    const TITLE: &'static str = "Compose";

    fn init() -> ComposeModel {
        ComposeModel { focused: true, buffer: RefCell::new(None) }
    }

    fn keys(_m: &ComposeModel) -> Softkeys<ComposeMsg> {
        Softkeys::new().back("Back", ComposeMsg::Quit)
    }

    fn update(m: &mut ComposeModel, msg: ComposeMsg) -> Cmd<Page> {
        match msg {
            ComposeMsg::Blur => {
                m.focused = false;
                Cmd::None
            }
            ComposeMsg::Quit => Cmd::Exit,
        }
    }

    fn view(m: &ComposeModel, slots: &mut SlotTable) -> Node {
        let field = TextField::new(slots).focused(m.focused);
        // Kept before the field is moved into the tree. The handle points at the slot, so it stays
        // valid for as long as the field is on screen.
        *m.buffer.borrow_mut() = Some(field.buffer());
        Node::leaf(
            Screen::new()
                .title("Compose")
                .content(field)
                .on_back("Back", ComposeMsg::Quit),
        )
    }
}

fn compose_key(b: &mut DeclarativeAppBridge<Compose>, k: Key) -> Handled {
    let mut out = Handled::Ignored;
    testing::with_theme(Palette::DARK, |t: &Theme<'_>| {
        out = b.handle_key(press(k), t, Rect::from_xywh(0, 0, W, H));
    });
    out
}

fn compose_frame(b: &mut DeclarativeAppBridge<Compose>) {
    testing::with_theme(Palette::DARK, |t: &Theme<'_>| {
        let mut buf = vec![0u16; (W * H) as usize];
        let mut c = Canvas::from_slice(&mut buf, Size::new(W, H));
        b.draw(&mut c, t);
    });
}

/// What is in the field right now, read through the handle the view kept.
fn typed(b: &DeclarativeAppBridge<Compose>) -> String {
    match &*b.model().buffer.borrow() {
        Some(buf) => String::from(buf.borrow().text()),
        None => String::new(),
    }
}

#[test]
fn typing_reaches_a_field_inside_the_tree() {
    let mut b = DeclarativeAppBridge::<Compose>::new();
    // A frame first: the walk reads the rects the layout wrote, and before any frame there are
    // none. This is the ordering the whole design rests on, so the test states it rather than
    // arranging it by luck.
    compose_frame(&mut b);

    for ch in "ola".chars() {
        assert_eq!(compose_key(&mut b, Key::Char(ch)), Handled::Consumed, "{ch} was dropped");
    }
    assert_eq!(typed(&b), "ola", "the keys reached the buffer");
    // And the screen still draws afterwards — a key that a widget answered leaves the tree valid
    // rather than stale, which is why the bridge does not rebuild for one.
    compose_frame(&mut b);
    assert_eq!(typed(&b), "ola", "a redraw did not reset the field");
}

#[test]
fn a_key_before_the_first_frame_is_answered_at_a_layout_of_its_own() {
    // No frame has been drawn, so no widget has a rect — and answering a key at a *stale* rect is a
    // widget acting at a position it no longer occupies, which is the thing to avoid. The way to
    // avoid it is not to drop the key: this path is handed the screen rect and the theme, so the
    // tree is built and placed here and the field is asked at the rect it is about to be drawn at.
    //
    // What makes this matter is not the first frame, it is every frame after it. An `update` drops
    // the tree, and the platform delivers a whole batch of key events before the host draws once —
    // so without this, the second press of a held key would be answered by nobody.
    let mut b = DeclarativeAppBridge::<Compose>::new();
    assert_eq!(compose_key(&mut b, Key::Char('x')), Handled::Consumed);
    assert_eq!(typed(&b), "x");
    compose_frame(&mut b);
    assert_eq!(typed(&b), "x", "and the frame that followed did not reset it");
}

#[test]
fn an_unfocused_field_does_not_take_the_key() {
    // The same press, twice, with only the focus flag between them. That is what stops two fields
    // on one screen both answering — the walk offers the key to everything and every field vetoes
    // unless it is the one with the keyboard.
    let mut b = DeclarativeAppBridge::<Compose>::new();
    compose_frame(&mut b);
    assert_eq!(compose_key(&mut b, Key::Char('a')), Handled::Consumed);
    assert_eq!(typed(&b), "a");

    b.send(ComposeMsg::Blur);
    compose_frame(&mut b);
    assert_eq!(compose_key(&mut b, Key::Char('b')), Handled::Ignored);
    assert_eq!(typed(&b), "a", "an unfocused field must not have taken the key");
}

#[test]
fn the_softkey_bar_wins_a_key_the_field_would_also_have_taken() {
    // Step one of the resolution order beats step three. If the walk ran first, a field would eat
    // Back and the label on the bar would be a promise the screen does not keep.
    let mut b = DeclarativeAppBridge::<Compose>::new();
    compose_frame(&mut b);
    assert_eq!(compose_key(&mut b, Key::Softkey(Softkey::Right)), Handled::Consumed);
    assert!(b.should_exit(), "Back reached the bar, not the text field");
}

#[test]
fn pasting_reaches_the_field_through_the_bridges_clipboard() {
    // The clipboard the bridge was built with, all the way into the buffer — the reason the key
    // walk carries a context rather than only a rect.
    let mut b = DeclarativeAppBridge::<Compose>::new()
        .with_clipboard(symbian_ui::MemClipboard::with_text("colado"));
    compose_frame(&mut b);

    assert_eq!(compose_key(&mut b, Key::Ctrl('v')), Handled::Consumed);
    compose_frame(&mut b);
    assert_eq!(typed(&b), "colado");
}

#[test]
fn a_bridge_with_no_clipboard_pastes_nothing_and_says_so() {
    // The default. `Ignored` is what lets a screen put its own behaviour under a chord the field
    // could not honour.
    let mut b = DeclarativeAppBridge::<Compose>::new();
    compose_frame(&mut b);
    assert_eq!(compose_key(&mut b, Key::Ctrl('v')), Handled::Ignored);
    compose_frame(&mut b);
    assert_eq!(typed(&b), "");
}
