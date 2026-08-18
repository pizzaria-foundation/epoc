//! A hand-written screen, inside a declarative tree.
//!
//! # Why a migration needs this
//!
//! An application is one [`symbian_ui::App`], so becoming a
//! [`DeclarativeApp`](crate::app::DeclarativeApp) looks like a big bang: every screen at once, in
//! one commit, with nothing to compare against until the last one lands. That is the shape of
//! rewrite this project has already decided it does not want — `docs/decl-ui.md` says a working
//! screen rewritten declaratively is a working screen with new bugs in it, and the answer to that
//! is not courage, it is arriving one screen at a time with a comparison behind each one.
//!
//! This widget is what makes one screen at a time possible. It is a [`Node::leaf`](crate::widgets::Node::leaf)
//! that draws by calling an old screen's `draw` and answers keys by calling its `handle_key`. An app
//! becomes MVU on the first day, with every screen still the screen that ships; screens then leave
//! the adapter one by one, each with its own pixel comparison, and the day the last one leaves this
//! widget is deleted.
//!
//! ```ignore
//! // In `view`: the chat list, still the hand-written one.
//! let (screen, store, out) = (m.chats.clone(), m.store.clone(), m.out.clone());
//! Node::leaf(
//!     Imperative::new(screen, move |list, c, _rect, theme| list.draw(c, &store, theme))
//!         .on_key(move |list, ev, rect, cx| {
//!             let band = Frame::split(rect, cx.theme, true, true).content;
//!             let (handled, action) = list.handle_key(ev, &store, cx.theme, band.height());
//!             match action {
//!                 ChatListAction::Open(i) => out.push(Msg::Open(i)),
//!                 ChatListAction::None => {}
//!                 // ...
//!             }
//!             handled
//!         }),
//! )
//! ```
//!
//! # Where the wrapped screen's state lives
//!
//! In an `Rc<RefCell<S>>` the *caller* owns, because a widget is rebuilt every
//! frame and an old screen's state must not be. Which `Rc` to pass is a real decision:
//!
//! * **On the model**, for a screen whose state has to survive being navigated away from. A chat
//!   list that lost its selection every time a conversation was opened and closed would lose the
//!   reader's place, which is the whole complaint about the screen it replaced.
//! * **In the slot table** — `slots.use_state_with(|| Rc::new(RefCell::new(ChatList::new())))` —
//!   for a screen whose state is genuinely a consequence of being on screen. Note the rule in
//!   [`crate::slot`]: a group that is not entered this frame is dropped at the end of it, so this
//!   is the option that forgets.
//!
//! # What the wrapped screen must be told, and what it must not
//!
//! The closures get the rect this widget was placed at, and an old screen that was written as a
//! whole screen will ignore it: `chats.rs` starts its `draw` with `Rect::from_size(c.size())` and
//! then clears the entire canvas. That is survivable and not a licence — the engine clips a leaf to
//! its rect (see [`crate::layout::draw_node`]), so a screen that paints outside its box paints
//! nothing outside its box. It does mean this adapter is for *whole screens*: put one in a band and
//! the old code will believe it owns the phone.
//!
//! Keys are the other half, and the rect is not optional there. An old `handle_key` typically wants
//! the height of the *content band*, which it never computed itself — the app did, with
//! `Frame::split`, before calling it. The closure has to do the same, from the rect it is given and
//! the theme in [`KeyCtx`], which is exactly why that context carries a theme at all.
//!
//! # How a decision gets out
//!
//! [`Widget::handle_key`] answers [`Handled`] and nothing else, and an old screen answers
//! `(Handled, Action)`. The second half travels through an [`Outbox`](crate::outbox::Outbox) the
//! closure captures — see that module for why the queue is on the model rather than in the key
//! context. This widget does not know the app's message type and does not want to.

use alloc::boxed::Box;
use alloc::rc::Rc;
use core::cell::RefCell;

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::{Handled, KeyEvent, Theme};

use crate::constraints::Constraints;
use crate::widget::{hash_i32, KeyCtx, Widget, WidgetHash};

/// How the wrapped screen paints: its own state, a canvas, the rect it was placed at, the theme.
pub type DrawFn<S> = dyn Fn(&mut S, &mut Canvas<'_>, Rect, &Theme<'_>);

/// How the wrapped screen answers a key. `&mut KeyCtx` carries the theme and the clipboard.
pub type KeyFn<S> = dyn Fn(&mut S, KeyEvent, Rect, &mut KeyCtx<'_>) -> Handled;

/// An imperative screen wearing the [`Widget`] contract.
pub struct Imperative<S> {
    state: Rc<RefCell<S>>,
    draw: Box<DrawFn<S>>,
    key: Option<Box<KeyFn<S>>>,
    weight: i32,
}

impl<S: 'static> Imperative<S> {
    /// Wrap `state`, drawn by `draw`.
    ///
    /// Keys are ignored until [`on_key`](Self::on_key) says otherwise, which is the right default
    /// for the two screens that genuinely have none — a viewer that only ever backs out through the
    /// softkey bar, and a splash.
    pub fn new(
        state: Rc<RefCell<S>>,
        draw: impl Fn(&mut S, &mut Canvas<'_>, Rect, &Theme<'_>) + 'static,
    ) -> Self {
        Self { state, draw: Box::new(draw), key: None, weight: 0 }
    }

    /// Route keys into the wrapped screen.
    ///
    /// Return [`Handled::Consumed`] for a key the old screen acted on, exactly as its own
    /// `handle_key` does: `Ignored` is what lets the key carry on to whatever encloses this, and
    /// what tells the host not to repaint for a press that changed nothing.
    ///
    /// A decision the old screen made — an action, an index — goes into an
    /// [`Outbox`](crate::outbox::Outbox) this closure captures. Consuming the key is not the same as
    /// producing a message and the two are independent: a cursor that moved consumes and says
    /// nothing, and the bridge treats a message as reason to repaint whatever this returns.
    pub fn on_key(
        mut self,
        f: impl Fn(&mut S, KeyEvent, Rect, &mut KeyCtx<'_>) -> Handled + 'static,
    ) -> Self {
        self.key = Some(Box::new(f));
        self
    }

    /// Take a share of the parent's leftover space, for an adapter used as a band rather than as a
    /// whole screen. A root leaf is given the whole rect regardless and needs none of this.
    pub fn fill(mut self, weight: i32) -> Self {
        self.weight = weight;
        self
    }
}

/// Distinguishes this widget's constant digest from any other constant in the tree.
const TAG: i32 = 0x1_1A_DA;

impl<S: 'static> Widget for Imperative<S> {
    /// Constant, and deliberately not zero.
    ///
    /// Zero means "measure me every frame", which every widget that cannot describe its own size
    /// has to say. This one *can*: it always takes the whole offer, so its size is a function of the
    /// offer and of nothing else — and the cache keys every entry on the offer it was measured
    /// against as well as on this digest (see `cache::Entry::offer`). A cached size for this slot is
    /// therefore always the answer to the question being asked, however much the wrapped screen has
    /// changed inside.
    ///
    /// The weight is folded in because a parent's own size can depend on how its children divide its
    /// line, and a parent that cached across a changed weight would keep the old division.
    fn content_hash(&self) -> WidgetHash {
        hash_i32(hash_i32(0, TAG), self.weight)
    }

    /// Everything it was offered. An imperative screen carves its own bands out of what it is given
    /// and has no smaller size to report.
    fn measure(&self, constraints: Constraints, _theme: &Theme<'_>) -> Size {
        constraints.constrain(Size::new(constraints.max_w, constraints.max_h))
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        // `try_borrow_mut`, not `borrow_mut`. The only way to fail is to be already inside this
        // screen's own draw — one adapter drawing another over the same state — and the honest
        // response is to draw nothing rather than to panic in a frame on a device whose entire
        // failure report is a dialog with a number in it. A blank band is visible; a dead
        // application takes the log with it.
        if let Ok(mut state) = self.state.try_borrow_mut() {
            (self.draw)(&mut state, c, rect, theme);
        }
    }

    fn handle_key(&self, ev: KeyEvent, rect: Rect, cx: &mut KeyCtx<'_>) -> Handled {
        let Some(f) = &self.key else { return Handled::Ignored };
        match self.state.try_borrow_mut() {
            Ok(mut state) => f(&mut state, ev, rect, cx),
            // Same reasoning as `draw`, with the better outcome: an unanswered key is a key the
            // enclosing scope still gets to have.
            Err(_) => Handled::Ignored,
        }
    }

    fn flex_weight(&self) -> i32 {
        self.weight
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;
    use symbian_gfx::Color;
    use symbian_ui::{testing, Key, MemClipboard, Palette};

    use crate::widget::with_key_ctx;

    /// Stands in for an old screen: records what it was asked to do, and holds state across frames.
    #[derive(Default)]
    struct OldScreen {
        draws: Vec<Rect>,
        keys: Vec<Key>,
        selected: usize,
    }

    fn wrap(state: &Rc<RefCell<OldScreen>>) -> Imperative<OldScreen> {
        Imperative::new(state.clone(), |s, c, rect, theme| {
            s.draws.push(rect);
            c.fill_rect(rect, theme.palette.accent);
        })
        .on_key(|s, ev, _rect, _cx| {
            s.keys.push(ev.key);
            match ev.key {
                Key::Down => {
                    s.selected += 1;
                    Handled::Consumed
                }
                _ => Handled::Ignored,
            }
        })
    }

    fn canvas<R>(f: impl FnOnce(&mut Canvas<'_>) -> R) -> R {
        let mut buf = alloc::vec![0u16; 320 * 240];
        let mut c = Canvas::from_slice(&mut buf, Size::new(320, 240));
        f(&mut c)
    }

    #[test]
    fn drawing_reaches_the_old_screen_with_the_rect_it_was_placed_at() {
        let state = Rc::new(RefCell::new(OldScreen::default()));
        let band = Rect::from_xywh(0, 20, 320, 180);
        testing::with_theme(Palette::DARK, |t| {
            canvas(|c| wrap(&state).draw(c, band, t));
        });
        assert_eq!(state.borrow().draws, alloc::vec![band]);
    }

    #[test]
    fn the_state_outlives_the_widget_describing_it() {
        // The reason the `Rc` belongs to the caller. A view is rebuilt every frame; an old screen's
        // selection is not, and a wrapper that owned its state would reset the list on every key.
        let state = Rc::new(RefCell::new(OldScreen::default()));
        with_key_ctx(|cx| {
            let first = wrap(&state);
            assert_eq!(first.handle_key(KeyEvent::new(Key::Down), Rect::from_xywh(0, 0, 320, 240), cx), Handled::Consumed);
        });
        assert_eq!(state.borrow().selected, 1);

        // A whole new widget, as the next frame would build.
        with_key_ctx(|cx| {
            let second = wrap(&state);
            second.handle_key(KeyEvent::new(Key::Down), Rect::from_xywh(0, 0, 320, 240), cx);
        });
        assert_eq!(state.borrow().selected, 2, "the second frame's widget found the first's state");
    }

    #[test]
    fn a_key_the_old_screen_declined_is_left_for_whatever_encloses_it() {
        let state = Rc::new(RefCell::new(OldScreen::default()));
        with_key_ctx(|cx| {
            let w = wrap(&state);
            assert_eq!(w.handle_key(KeyEvent::new(Key::Up), Rect::from_xywh(0, 0, 320, 240), cx), Handled::Ignored);
        });
        assert_eq!(state.borrow().keys, alloc::vec![Key::Up], "it was offered the key all the same");
    }

    #[test]
    fn an_adapter_with_no_key_handler_answers_nothing() {
        let state = Rc::new(RefCell::new(OldScreen::default()));
        let w = Imperative::new(state.clone(), |_s, _c, _r, _t| {});
        with_key_ctx(|cx| {
            assert_eq!(w.handle_key(KeyEvent::new(Key::Down), Rect::from_xywh(0, 0, 320, 240), cx), Handled::Ignored);
        });
    }

    #[test]
    fn the_key_context_carries_the_clipboard_through() {
        // An old composer pastes, and it must not have to be rewritten to reach the platform's
        // clipboard: the same `KeyCtx` a declarative field gets is handed to the closure.
        let state = Rc::new(RefCell::new(OldScreen::default()));
        let pasted = Rc::new(RefCell::new(alloc::string::String::new()));
        let sink = pasted.clone();
        let w = Imperative::new(state.clone(), |_s, _c, _r, _t| {}).on_key(move |_s, _ev, _rect, cx| {
            if let Some(text) = cx.clip.get() {
                *sink.borrow_mut() = text;
            }
            Handled::Consumed
        });
        testing::with_theme(Palette::DARK, |t| {
            let mut clip = MemClipboard::with_text("vou passar aí");
            let mut cx = KeyCtx::new(t, &mut clip);
            w.handle_key(KeyEvent::new(Key::Char('v')), Rect::from_xywh(0, 0, 320, 240), &mut cx);
        });
        assert_eq!(&*pasted.borrow(), "vou passar aí");
    }

    #[test]
    fn it_measures_to_everything_it_is_offered() {
        let state = Rc::new(RefCell::new(OldScreen::default()));
        testing::with_theme(Palette::DARK, |t| {
            let w = wrap(&state);
            assert_eq!(w.measure(Constraints::loose(320, 240), t), Size::new(320, 240));
            assert_eq!(w.measure(Constraints::tight(120, 40), t), Size::new(120, 40));
        });
    }

    #[test]
    fn the_digest_is_constant_because_the_size_is_a_function_of_the_offer() {
        // Two adapters over two entirely different states hash the same, which is the claim: what
        // the wrapped screen holds cannot change what this widget measures to.
        let a = Rc::new(RefCell::new(OldScreen::default()));
        let b = Rc::new(RefCell::new(OldScreen { selected: 9, ..Default::default() }));
        assert_eq!(wrap(&a).content_hash(), wrap(&b).content_hash());
        assert_ne!(wrap(&a).content_hash(), 0, "zero would mean re-measuring every frame");
        // The weight is not the size, but it changes how a parent divides its line.
        assert_ne!(wrap(&a).content_hash(), wrap(&a).fill(1).content_hash());
        assert_eq!(wrap(&a).fill(2).flex_weight(), 2);
    }

    #[test]
    fn drawing_from_inside_a_draw_is_skipped_rather_than_fatal() {
        // Reentrancy over one `RefCell`, which is the one way this adapter can be misused into a
        // panic. On the device a panic in `draw` is the application gone; a band that did not paint
        // is a band that did not paint.
        let state: Rc<RefCell<OldScreen>> = Rc::new(RefCell::new(OldScreen::default()));
        let inner = state.clone();
        let outer = Imperative::new(state.clone(), move |s, c, rect, theme| {
            s.draws.push(rect);
            // The nested adapter cannot get the borrow, so it draws nothing and returns.
            let nested = Imperative::new(inner.clone(), |s2: &mut OldScreen, c2: &mut Canvas<'_>, r, _t| {
                s2.draws.push(r);
                c2.fill_rect(r, Color::hex(0xFF00FF));
            });
            nested.draw(c, rect, theme);
        });
        testing::with_theme(Palette::DARK, |t| {
            canvas(|c| outer.draw(c, Rect::from_xywh(0, 0, 320, 240), t));
        });
        assert_eq!(state.borrow().draws.len(), 1, "the outer draw ran; the nested one gave up quietly");
    }
}
