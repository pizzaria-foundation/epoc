//! A hand-written screen driven as a declarative app.
//!
//! The adapter's real test is not that it forwards two calls — its unit tests say that — but that an
//! *application* built the way a migration builds one actually works: an old screen inside an MVU
//! app, keys arriving through the bridge, the old screen's decisions reaching `update`, and the
//! model change coming back out as a repaint. That whole path crosses four files and nothing inside
//! any one of them can see it.
//!
//! Written from outside the crate on purpose, like `tests/screen.rs`: what a test here cannot reach
//! is what an app cannot reach.
//!
//! # The shape a migrating app has
//!
//! ```text
//!   Model { store, chats: Rc<RefCell<ChatList>>, out: Outbox<Msg> }
//!                              │                        │
//!   view ─── Node::leaf(Imperative::new(chats.clone())) │   ← still the screen that ships
//!                              │                        │
//!   key ──► bridge ──► on_key (nothing) ──► widget walk ┘ ──► drain ──► update ──► Cmd
//! ```
//!
//! `keys()` is deliberately empty while a screen is wrapped: the old screen draws its own softkey
//! bar and routes its own softkeys, so an app that also declared them would answer `Softkey::Left`
//! at the bridge and the old screen would never see it. That is asserted below rather than left as
//! advice, because it is the one mistake this arrangement invites.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use symbian_decl_ui::app::DeclarativeApp;
use symbian_decl_ui::bridge::DeclarativeAppBridge;
use symbian_decl_ui::cmd::Cmd;
use symbian_decl_ui::keys::Softkeys;
use symbian_decl_ui::outbox::Outbox;
use symbian_decl_ui::slot::SlotTable;
use symbian_decl_ui::widget::KeyCtx;
use symbian_decl_ui::widgets::{Imperative, Node};

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::{chrome, testing, App as _, Frame, Handled, Key, KeyEvent, Palette, Softkey, Theme};

const W: i32 = 320;
const H: i32 = 240;

// ---------------------------------------------------------------- the old screen

/// A list written the way this project's screens are written: it owns its selection, draws its own
/// chrome from the canvas size, and answers a key with `(Handled, Action)`.
struct OldList {
    selected: usize,
    count: usize,
    /// The content-band height it was told about, per key. The number an old `handle_key` cannot
    /// work out for itself, and the reason the adapter hands the rect over.
    viewports: Vec<i32>,
    draws: u32,
}

#[derive(Debug, PartialEq, Eq)]
enum Action {
    None,
    Open(usize),
    Exit,
}

impl OldList {
    fn new(count: usize) -> Self {
        Self { selected: 0, count, viewports: Vec::new(), draws: 0 }
    }

    fn draw(&mut self, c: &mut Canvas<'_>, theme: &Theme<'_>) {
        self.draws += 1;
        // `Rect::from_size(c.size())`, exactly as `chats.rs` does: an old screen believes it owns
        // the framebuffer. The engine's clip is what makes that survivable.
        let screen = Rect::from_size(c.size());
        let f = Frame::split(screen, theme, true, true);
        chrome::clear(c, theme);
        chrome::title_bar(c, f.title, theme, "Antigo", None);
        chrome::softkey_bar(c, f.softkeys, theme, [Some("Atualizar"), Some("Abrir"), Some("Sair")]);
    }

    fn handle_key(&mut self, ev: KeyEvent, viewport_h: i32) -> (Handled, Action) {
        self.viewports.push(viewport_h);
        match ev.key {
            Key::Down if self.selected + 1 < self.count => {
                self.selected += 1;
                (Handled::Consumed, Action::None)
            }
            Key::Select => (Handled::Consumed, Action::Open(self.selected)),
            Key::Softkey(Softkey::Right) => (Handled::Consumed, Action::Exit),
            _ => (Handled::Ignored, Action::None),
        }
    }
}

// ---------------------------------------------------------------- the app around it

#[derive(Clone, Debug, PartialEq, Eq)]
enum Msg {
    Open(usize),
    Quit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Page {
    Conversation(usize),
}

struct Model {
    /// The old screen, on the model rather than in the slot table: its selection has to survive
    /// being navigated away from, or coming back from a conversation would lose the reader's place.
    list: Rc<RefCell<OldList>>,
    out: Outbox<Msg>,
    opened: Option<usize>,
    updates: u32,
    views: Cell<u32>,
}

struct Migrating;

impl DeclarativeApp for Migrating {
    type Model = Model;
    type Message = Msg;
    type Screen = Page;
    const TITLE: &'static str = "Migrating";

    fn init() -> Model {
        Model {
            list: Rc::new(RefCell::new(OldList::new(20))),
            out: Outbox::new(),
            opened: None,
            updates: 0,
            views: Cell::new(0),
        }
    }

    /// Empty, and that is the whole point of the comment at the top of this file: the wrapped screen
    /// draws and routes its own bar.
    fn keys(_m: &Model) -> Softkeys<Msg> {
        Softkeys::new()
    }

    fn outbox(m: &Model) -> Option<&Outbox<Msg>> {
        Some(&m.out)
    }

    fn update(m: &mut Model, msg: Msg) -> Cmd<Page> {
        m.updates += 1;
        match msg {
            Msg::Open(i) => {
                m.opened = Some(i);
                Cmd::PushScreen(Page::Conversation(i))
            }
            Msg::Quit => Cmd::Exit,
        }
    }

    fn view(m: &Model, _slots: &mut SlotTable) -> Node {
        m.views.set(m.views.get() + 1);
        let out = m.out.clone();
        Node::leaf(
            Imperative::new(m.list.clone(), |list, c, _rect, theme| list.draw(c, theme)).on_key(
                move |list: &mut OldList, ev: KeyEvent, rect: Rect, cx: &mut KeyCtx<'_>| {
                    // The band the old screen drew into, computed the same way its app used to
                    // compute it — from the rect and the theme, which is why `KeyCtx` carries one.
                    let band = Frame::split(rect, cx.theme, true, true).content;
                    let (handled, action) = list.handle_key(ev, band.height());
                    match action {
                        Action::Open(i) => out.push(Msg::Open(i)),
                        Action::Exit => out.push(Msg::Quit),
                        Action::None => {}
                    }
                    handled
                },
            ),
        )
    }
}

// ---------------------------------------------------------------- driving it

fn key(b: &mut DeclarativeAppBridge<Migrating>, k: Key) -> Handled {
    let mut out = Handled::Ignored;
    testing::with_theme(Palette::DARK, |t: &Theme<'_>| {
        out = b.handle_key(KeyEvent::new(k), t, Rect::from_xywh(0, 0, W, H));
    });
    out
}

fn frame(b: &mut DeclarativeAppBridge<Migrating>) {
    testing::with_theme(Palette::DARK, |t: &Theme<'_>| {
        let mut buf = vec![0u16; (W * H) as usize];
        let mut c = Canvas::from_slice(&mut buf, Size::new(W, H));
        b.draw(&mut c, t);
    });
}

/// The content band for this screen, which is what the old `handle_key` should be told.
fn band_height() -> i32 {
    let mut h = 0;
    testing::with_theme(Palette::DARK, |t: &Theme<'_>| {
        h = Frame::split(Rect::from_xywh(0, 0, W, H), t, true, true).content.height();
    });
    h
}

// ---------------------------------------------------------------- the tests

#[test]
fn the_old_screen_draws_and_keeps_its_own_state() {
    let mut b = DeclarativeAppBridge::<Migrating>::new();
    frame(&mut b);
    assert_eq!(b.model().list.borrow().draws, 1);

    // Its selection moves under its own arithmetic, and the view is rebuilt around it without
    // touching it.
    assert_eq!(key(&mut b, Key::Down), Handled::Consumed);
    assert_eq!(key(&mut b, Key::Down), Handled::Consumed);
    assert_eq!(b.model().list.borrow().selected, 2);
    frame(&mut b);
    assert_eq!(b.model().list.borrow().selected, 2, "a frame does not reset the wrapped screen");
    assert_eq!(b.model().updates, 0, "a cursor that moved is not an application event");
}

#[test]
fn the_old_screen_is_told_the_band_it_was_drawn_in() {
    // The number an imperative `handle_key` was always given by its app and cannot work out for
    // itself. Getting it wrong is a list that paginates one row early or never.
    let mut b = DeclarativeAppBridge::<Migrating>::new();
    frame(&mut b);
    key(&mut b, Key::Down);
    assert_eq!(b.model().list.borrow().viewports, vec![band_height()]);
    assert!(band_height() < H, "the band is the content, not the screen");
}

#[test]
fn a_decision_the_old_screen_made_reaches_update() {
    // The path the adapter exists for: `(Consumed, Open(3))` out of a screen that has no message
    // type, into an `update` that does.
    let mut b = DeclarativeAppBridge::<Migrating>::new();
    frame(&mut b);
    for _ in 0..3 {
        key(&mut b, Key::Down);
    }
    assert_eq!(key(&mut b, Key::Select), Handled::Consumed);
    assert_eq!(b.model().opened, Some(3));
    assert_eq!(b.model().updates, 1);
    // And the command that came with it ran: the app is one screen deeper.
    assert_eq!(b.depth(), 1);
    assert_eq!(b.screen(), Some(&Page::Conversation(3)));
}

#[test]
fn the_queue_is_empty_again_afterwards() {
    // A message delivered twice is a chat opened twice. The drain is the only record.
    let mut b = DeclarativeAppBridge::<Migrating>::new();
    frame(&mut b);
    key(&mut b, Key::Select);
    assert_eq!(b.model().updates, 1);
    assert!(b.model().out.is_empty());
    for _ in 0..5 {
        frame(&mut b);
    }
    assert_eq!(b.model().updates, 1, "frames must not redeliver a message");
    assert_eq!(b.model().out.dropped(), 0);
}

#[test]
fn a_message_forces_the_view_to_be_rebuilt() {
    // The failure this guards is subtle: a widget consuming a key does *not* invalidate the tree,
    // by design, because slot state is read through the same `Rc`. A message is different — the
    // model moved, and a tree built from the old one would keep showing it.
    let mut b = DeclarativeAppBridge::<Migrating>::new();
    frame(&mut b);
    let views = b.model().views.get();

    key(&mut b, Key::Down);
    frame(&mut b);
    assert_eq!(b.model().views.get(), views, "a key the widget absorbed changed no model state");

    key(&mut b, Key::Select);
    frame(&mut b);
    assert_eq!(b.model().views.get(), views + 1, "a message did");
}

#[test]
fn the_old_screens_back_softkey_still_leaves_the_app() {
    // Right softkey → the old screen's `Action::Exit` → `Msg::Quit` → `Cmd::Exit` → the flag the
    // host reads. Four hops, none of which the old screen knows about.
    let mut b = DeclarativeAppBridge::<Migrating>::new();
    frame(&mut b);
    assert!(!b.should_exit());
    assert_eq!(key(&mut b, Key::Softkey(Softkey::Right)), Handled::Consumed);
    assert!(b.should_exit());
}

#[test]
fn a_key_nobody_wanted_costs_nothing() {
    // No update, no message, no repaint: `Ignored` is what tells the host not to blit.
    let mut b = DeclarativeAppBridge::<Migrating>::new();
    frame(&mut b);
    let draws = b.model().list.borrow().draws;
    assert_eq!(key(&mut b, Key::Char('x')), Handled::Ignored);
    assert_eq!(b.model().updates, 0);
    assert_eq!(b.model().views.get(), 1);
    assert_eq!(b.model().list.borrow().draws, draws);
}

#[test]
fn a_key_before_the_first_frame_is_answered_rather_than_dropped() {
    // The bridge needs a tree that has been *placed* — a widget asked about a key at a stale rect is
    // the failure it is guarding against — and both halves of a layout are available on this path:
    // the host hands over the screen rect and the theme. So the tree is built and laid out here
    // rather than the key being thrown away.
    let mut b = DeclarativeAppBridge::<Migrating>::new();
    assert_eq!(key(&mut b, Key::Select), Handled::Consumed);
    assert_eq!(b.model().opened, Some(0));
    // And the old screen was told the real band, not a guess: the layout that answered the key is
    // the same one a frame would have produced.
    assert_eq!(b.model().list.borrow().viewports, vec![band_height()]);
}

#[test]
fn every_key_in_a_batch_reaches_the_screen() {
    // The case that made the above necessary, and it is not a corner: the platform hands the host a
    // whole batch of events and the host draws once, at the end. The first press changes the model,
    // which drops the tree — and every press after it in that batch would have found no rects.
    //
    // Held direction keys are exactly this. A list that moved one row per frame instead of one per
    // press would feel broken and look perfect in a screenshot.
    let mut b = DeclarativeAppBridge::<Migrating>::new();
    frame(&mut b);
    for _ in 0..5 {
        key(&mut b, Key::Down);
    }
    assert_eq!(b.model().list.borrow().selected, 5, "presses went missing between frames");

    // The same with an update in the middle of the batch: `Select` invalidates, and the presses
    // after it still arrive.
    key(&mut b, Key::Select);
    for _ in 0..3 {
        key(&mut b, Key::Down);
    }
    assert_eq!(b.model().list.borrow().selected, 8);
}

#[test]
fn an_idle_frame_does_not_re_measure_the_adapter() {
    // The one number the plan asks for per stage. The adapter's digest is constant because its size
    // is a function of the offer, so a screen that changed nothing measures nothing — the same
    // property every widget in the catalogue has, kept by the one that wraps arbitrary old code.
    let mut b = DeclarativeAppBridge::<Migrating>::new();
    frame(&mut b);
    assert_eq!(b.measure_calls(), 1, "the first frame measures once");
    frame(&mut b);
    assert_eq!(b.measure_calls(), 0, "and an unchanged one not at all");
    key(&mut b, Key::Down);
    frame(&mut b);
    assert_eq!(b.measure_calls(), 0, "the wrapped screen's own state is not this widget's size");
}

#[test]
fn the_wrapped_screen_cannot_paint_outside_the_rect_it_was_given() {
    // An old screen clears the whole canvas from `c.size()` and ignores its rect. Inside a band, the
    // engine's clip is the only thing standing between that and the chrome around it — so this is
    // what makes the adapter safe to place anywhere, even though it is meant for whole screens.
    struct Panel;
    struct PanelModel {
        list: Rc<RefCell<OldList>>,
    }
    impl DeclarativeApp for Panel {
        type Model = PanelModel;
        type Message = ();
        type Screen = ();
        const TITLE: &'static str = "Panel";
        fn init() -> PanelModel {
            PanelModel { list: Rc::new(RefCell::new(OldList::new(3))) }
        }
        fn update(_m: &mut PanelModel, _msg: ()) -> Cmd<()> {
            Cmd::None
        }
        fn view(m: &PanelModel, _slots: &mut SlotTable) -> Node {
            // Half the screen, on purpose.
            Node::Group(
                symbian_decl_ui::widgets::Column::new()
                    .child(
                        Imperative::new(m.list.clone(), |l, c, _r, t| l.draw(c, t))
                            .fill(1),
                    )
                    .height(H / 2)
                    .stretch_width(),
            )
        }
    }

    let mut buf = vec![0u16; (W * H) as usize];
    let mut b = DeclarativeAppBridge::<Panel>::new();
    testing::with_theme(Palette::DARK, |t: &Theme<'_>| {
        let mut c = Canvas::from_slice(&mut buf, Size::new(W, H));
        b.draw(&mut c, t);
    });
    // The bottom half was never written to: still the zero the buffer started as.
    let bottom = &buf[((H / 2 + 4) * W) as usize..];
    assert!(bottom.iter().all(|&px| px == 0), "the old screen escaped its band");
    // And the top half was drawn, so the test is not passing by drawing nothing at all.
    assert!(buf[..(W * 4) as usize].iter().any(|&px| px != 0));
}
