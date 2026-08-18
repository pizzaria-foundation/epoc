//! An app as three functions: a model, an update, and a view.
//!
//! # Why the model is not the app
//!
//! [`symbian_ui::App`] is an object with mutable state and two methods that both reach into it —
//! `handle_key` changes things and `draw` reads them. Nothing in that shape says which of the two
//! is allowed to change what, and in practice the line moved: screens ended up with a `draw` that
//! fixed up scroll offsets, because that was the only place that knew the height of the list.
//! Scrolling then depended on having been painted, and a screen that was updated while hidden
//! forgot where it was.
//!
//! Here the two are different functions with different powers. [`update`](DeclarativeApp::update)
//! takes `&mut Model` and no canvas — it cannot draw, so it cannot be tempted to compute layout.
//! [`view`](DeclarativeApp::view) takes `&Model` and returns a description — it cannot write, so a
//! frame cannot change the app. Neither is a rule anyone has to remember; both are the signature.
//!
//! # Why `update` and `view` take no `self`
//!
//! There is no third place for state to hide. If the type could carry fields, the interesting
//! question about any bug becomes "is this in the model or on the app?" — and the answer decides
//! whether a test that builds a model can reproduce it. Associated functions make the model the
//! only answer.
//!
//! ```ignore
//! impl DeclarativeApp for Recent {
//!     type Model = Model;
//!     type Message = Msg;
//!     type Screen = ();
//!     const TITLE: &'static str = "Recent";
//!
//!     fn init() -> Model { Model::default() }
//!
//!     fn keys(_m: &Model) -> Softkeys<Msg> {
//!         Softkeys::new().action("Open", Msg::Open).back("Back", Msg::Back)
//!     }
//!
//!     fn update(m: &mut Model, msg: Msg) -> Cmd { ... }
//!     fn view(m: &Model, slots: &mut SlotTable) -> Node { ... }
//! }
//! ```

use symbian_ui::KeyEvent;

use crate::cmd::Cmd;
use crate::keys::Softkeys;
use crate::slot::SlotTable;
use crate::widgets::Node;

/// An app described as model, message, update and view.
pub trait DeclarativeApp {
    /// Everything the app knows. The only mutable state there is.
    type Model;

    /// What can happen to it. `Clone` because a softkey holds its message and hands out copies —
    /// see [`Softkeys::dispatch`].
    type Message: Clone;

    /// The app's own screen identifier, for [`Cmd::PushScreen`].
    ///
    /// An app that never navigates writes `type Screen = ();`. Rust has no stable default for an
    /// associated type, so that one line is the cost of not making every app that *does* navigate
    /// squeeze its destinations through an integer.
    type Screen: Clone;

    /// The simulator's window caption. The device takes its title from the registration resource
    /// instead, so this is a development convenience, not the app's name.
    const TITLE: &'static str;

    /// The model an app starts with.
    fn init() -> Self::Model;

    /// The softkeys this screen offers, as labels bound to messages.
    ///
    /// One declaration serves both the bar that is drawn and the keys that are routed, which is
    /// the defect [`crate::keys`] exists to make impossible. It takes the model because the offer
    /// changes with the state — a list with nothing in it has no "Open".
    ///
    /// The default is a bar with nothing on it, so a screen driven entirely by
    /// [`Self::on_key`] does not have to write an empty one out.
    fn keys(_model: &Self::Model) -> Softkeys<Self::Message> {
        Softkeys::new()
    }

    /// What this key press means, if it means anything.
    ///
    /// Returning `None` is a real answer and the reason this returns an `Option` rather than a
    /// message with an `Ignored` variant: the bridge skips `update` entirely for a key nobody
    /// wanted, so an unbound key costs no model churn and no repaint. On a phone where every
    /// keystroke is a full-screen blit, "did nothing" has to actually do nothing.
    ///
    /// The default routes the softkeys and nothing else. A screen with a list overrides it to
    /// claim `Up`/`Down` first and then falls back:
    ///
    /// ```ignore
    /// fn on_key(m: &Model, ev: KeyEvent) -> Option<Msg> {
    ///     match ev.key {
    ///         Key::Up => Some(Msg::Prev),
    ///         Key::Down => Some(Msg::Next),
    ///         _ => Self::keys(m).dispatch(ev),
    ///     }
    /// }
    /// ```
    fn on_key(model: &Self::Model, ev: KeyEvent) -> Option<Self::Message> {
        Self::keys(model).dispatch(ev)
    }

    /// Apply a message to the model and say what should happen next.
    ///
    /// Pure in the sense that matters here: no canvas, no platform, no clock. Everything it wants
    /// the world to do comes back as a [`Cmd`].
    fn update(model: &mut Self::Model, msg: Self::Message) -> Cmd<Self::Screen>;

    /// Describe the screen for this model.
    ///
    /// Called after a change, not once per key and not once per frame — see
    /// [`DeclarativeAppBridge`](crate::bridge::DeclarativeAppBridge). It may allocate; it must not
    /// have opinions, because it may be skipped.
    ///
    /// Returns a [`Node`] rather than a bare [`Widget`](crate::Widget) because the layout engine
    /// needs to see the tree's *structure* — a group's axis, gap and padding, and above all a
    /// deterministic `slot_count` for the slot-indexed cache. A trait object can only offer that by
    /// convention; the enum offers it by construction. A screen that is a single widget says
    /// `Node::leaf(w)`.
    ///
    /// `slots` is where a widget's own state lives across the rebuild — a list's scroll offset, a
    /// field's caret. It is a parameter rather than something the app keeps because the first
    /// version of this trait did not have it, and the first application to put a list on a screen
    /// had to smuggle a `SlotTable` into its own model behind a `RefCell` to get one. That is not a
    /// thing an app author would invent; it is a thing they would copy from us.
    ///
    /// The table is begun and ended by the bridge. An app that had to remember `begin_frame` is an
    /// app whose state silently stops persisting when it forgets.
    fn view(model: &Self::Model, slots: &mut SlotTable) -> Node;

    /// The navigation stack moved; `top` is where the app is now, or `None` at the root.
    ///
    /// The stack lives in the bridge, but `view` only ever sees the model — so an app whose screens
    /// differ has to be told. The plan left this gap open: its example pushes
    /// `ScreenId::Conversation(i)` and then draws from `model.screen`, with nothing connecting the
    /// two. This is that connection, and it is a hook rather than a rule so that an app with one
    /// screen never writes it.
    ///
    /// ```ignore
    /// fn screen_changed(m: &mut Model, top: Option<&ScreenId>) {
    ///     m.screen = top.cloned().unwrap_or(ScreenId::ChatList);
    /// }
    /// ```
    fn screen_changed(_model: &mut Self::Model, _top: Option<&Self::Screen>) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_gfx::{Canvas, Rect, Size};
    use symbian_ui::{Key, Softkey, Theme};

    use crate::constraints::Constraints;
    use crate::widget::Widget;

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Msg {
        Next,
        Open,
        Quit,
    }

    #[derive(Default)]
    struct Model {
        selected: usize,
        items: usize,
    }

    struct Empty;

    impl Widget for Empty {
        fn measure(&self, c: Constraints, _t: &Theme<'_>) -> Size {
            c.constrain(Size::new(0, 0))
        }
        fn draw(&self, _c: &mut Canvas<'_>, _r: Rect, _t: &Theme<'_>) {}
    }

    struct Listy;

    impl DeclarativeApp for Listy {
        type Model = Model;
        type Message = Msg;
        type Screen = ();
        const TITLE: &'static str = "Listy";

        fn init() -> Model {
            Model { selected: 0, items: 3 }
        }

        fn keys(m: &Model) -> Softkeys<Msg> {
            let bar = Softkeys::new().back("Back", Msg::Quit);
            // An empty list has nothing to open, and the bar must say so rather than offering a
            // key that quietly does nothing.
            if m.items > 0 { bar.action("Open", Msg::Open) } else { bar }
        }

        fn on_key(m: &Model, ev: KeyEvent) -> Option<Msg> {
            match ev.key {
                Key::Down => Some(Msg::Next),
                _ => Self::keys(m).dispatch(ev),
            }
        }

        fn update(m: &mut Model, msg: Msg) -> Cmd {
            match msg {
                Msg::Next => {
                    m.selected = (m.selected + 1) % m.items.max(1);
                    Cmd::None
                }
                Msg::Open => Cmd::None,
                Msg::Quit => Cmd::Exit,
            }
        }

        fn view(_m: &Model, _slots: &mut SlotTable) -> Node {
            Node::leaf(Empty)
        }
    }

    fn press(k: Key) -> KeyEvent {
        KeyEvent::new(k)
    }

    #[test]
    fn the_offered_keys_follow_the_state() {
        // The bar is a function of the model, so a screen cannot label an action it is not in a
        // position to perform.
        let full = Model { selected: 0, items: 3 };
        let empty = Model { selected: 0, items: 0 };
        assert_eq!(Listy::keys(&full).labels(), [None, Some("Open"), Some("Back")]);
        assert_eq!(Listy::keys(&empty).labels(), [None, None, Some("Back")]);
        assert_eq!(Listy::on_key(&empty, press(Key::Select)), None);
    }

    #[test]
    fn an_override_claims_its_keys_and_leaves_the_rest_to_the_bar() {
        let m = Listy::init();
        assert_eq!(Listy::on_key(&m, press(Key::Down)), Some(Msg::Next));
        assert_eq!(Listy::on_key(&m, press(Key::Select)), Some(Msg::Open));
        assert_eq!(Listy::on_key(&m, press(Key::Softkey(Softkey::Right))), Some(Msg::Quit));
    }

    #[test]
    fn a_key_nobody_wanted_produces_no_message() {
        // Not a detail: this `None` is what lets the bridge skip `update` and the repaint. If an
        // app returned a `Msg::Ignored` instead, every stray keypress would cost a full blit.
        let m = Listy::init();
        assert_eq!(Listy::on_key(&m, press(Key::Up)), None);
        assert_eq!(Listy::on_key(&m, press(Key::Char('q'))), None);
    }

    #[test]
    fn the_default_bar_is_empty_rather_than_absent() {
        struct Bare;
        impl DeclarativeApp for Bare {
            type Model = ();
            type Message = ();
            type Screen = ();
            const TITLE: &'static str = "Bare";
            fn init() {}
            fn update(_m: &mut (), _msg: ()) -> Cmd {
                Cmd::None
            }
            fn view(_m: &(), _slots: &mut SlotTable) -> Node {
                Node::leaf(Empty)
            }
        }
        // Four items is the whole obligation for an app that draws one screen and never navigates.
        assert_eq!(Bare::keys(&()).labels(), [None, None, None]);
        assert_eq!(Bare::on_key(&(), press(Key::Select)), None);
    }

    #[test]
    fn update_is_a_function_of_model_and_message_only() {
        // The plan's third Fase-4 test, stated as far as the type system allows: the same model
        // and the same message give the same model and the same command, with no canvas in reach.
        let mut a = Listy::init();
        let mut b = Listy::init();
        assert_eq!(Listy::update(&mut a, Msg::Next), Cmd::None);
        assert_eq!(Listy::update(&mut b, Msg::Next), Cmd::None);
        assert_eq!(a.selected, b.selected);
        assert_eq!(a.selected, 1);
        assert_eq!(Listy::update(&mut a, Msg::Quit), Cmd::Exit);
    }

    #[test]
    fn wrapping_past_the_end_does_not_divide_by_zero() {
        // `% items` with an empty list is a panic on a phone with no console. The guard is in the
        // app here, but the case is the one every list gets wrong once.
        let mut m = Model { selected: 0, items: 0 };
        assert_eq!(Listy::update(&mut m, Msg::Next), Cmd::None);
        assert_eq!(m.selected, 0);
    }
}
