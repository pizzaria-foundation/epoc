//! Softkeys declared where they are handled.
//!
//! # Why a label and an action are one thing
//!
//! In the imperative toolkit a screen draws its softkey bar in `draw` and routes keys in
//! `handle_key`, and nothing connects the two. The failure that produces is not hypothetical: the
//! launcher's task manager shipped with a bar reading `Sort` in the middle slot and its handler
//! bound to `Softkey::Middle`, an event S60 never sends — so the label promised one thing and the
//! key did another, and the code was perfectly consistent with itself in both places.
//!
//! Here a softkey is a label *and* the message it sends, declared together when the screen object
//! is built. You cannot label a key you do not handle, and you cannot handle one you did not
//! label, because there is only one place to say either.
//!
//! ```ignore
//! Screen::new()
//!     .title("Recent")
//!     .on_options("Refresh", Msg::Refresh)   // left softkey
//!     .on_action("Open", Msg::Open)          // D-pad centre — see below
//!     .on_back("Back", Msg::Back)            // right softkey
//! ```
//!
//! # The middle slot is not a softkey
//!
//! S60 wires the centre of the bar to the selection key: it arrives as [`Key::Select`], never as
//! `Softkey::Middle`. [`Softkeys::dispatch`] knows that, so a screen says "the action is Open" and
//! the right key fires it. It is the one piece of platform trivia this module exists to absorb.

use alloc::string::String;

use symbian_ui::{Key, KeyEvent, Softkey};

/// One softkey: what it says, and what it means.
///
/// `M` is the screen's message type — the enum an MVU `update` receives. A softkey carries a value
/// rather than a closure so a screen stays a plain description that can be built, compared and
/// tested without running anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SoftkeyDef<M> {
    pub label: String,
    pub msg: M,
}

/// The three slots, in the SDK's order: options, action, back.
///
/// Every field is optional because most screens use two of the three. See the crate documentation
/// in `symbian-ui` for what each slot means and why the arrangement is the native one.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Softkeys<M> {
    /// Left softkey — the secondary offer. Refresh, a mode switch, a menu.
    pub options: Option<SoftkeyDef<M>>,
    /// The action, on the centre of the D-pad. What this screen is *for*.
    pub action: Option<SoftkeyDef<M>>,
    /// Right softkey — the way out, and never a second action.
    pub back: Option<SoftkeyDef<M>>,
}

impl<M: Clone> Softkeys<M> {
    pub fn new() -> Self {
        Self { options: None, action: None, back: None }
    }

    pub fn options(mut self, label: impl Into<String>, msg: M) -> Self {
        self.options = Some(SoftkeyDef { label: label.into(), msg });
        self
    }

    pub fn action(mut self, label: impl Into<String>, msg: M) -> Self {
        self.action = Some(SoftkeyDef { label: label.into(), msg });
        self
    }

    pub fn back(mut self, label: impl Into<String>, msg: M) -> Self {
        self.back = Some(SoftkeyDef { label: label.into(), msg });
        self
    }

    /// The labels, in the order [`symbian_ui::chrome::softkey_bar`] wants them.
    pub fn labels(&self) -> [Option<&str>; 3] {
        [
            self.options.as_ref().map(|d| d.label.as_str()),
            self.action.as_ref().map(|d| d.label.as_str()),
            self.back.as_ref().map(|d| d.label.as_str()),
        ]
    }

    /// The same bar, with every message put through `f`.
    ///
    /// For a screen that owns its softkeys and an application that owns the enum they arrive in: a
    /// migrating app wraps a screen's messages in a variant of its own — `Msg::Chats(..)` — and the
    /// bar has to travel with them. Written by hand that is three `Option::map`s and a struct
    /// literal per screen, and the one slot somebody forgets is a key that stops working.
    ///
    /// Labels are untouched. This changes who a message is addressed to, never what it says.
    pub fn map<N: Clone>(self, f: impl Fn(M) -> N) -> Softkeys<N> {
        let one = |d: Option<SoftkeyDef<M>>| {
            d.map(|d| SoftkeyDef { label: d.label, msg: f(d.msg) })
        };
        Softkeys { options: one(self.options), action: one(self.action), back: one(self.back) }
    }

    /// The message this key press means, if it means one.
    ///
    /// The mapping, and the reason this function exists rather than each screen writing it out:
    ///
    /// | key | slot |
    /// |---|---|
    /// | `Softkey::Left` | options |
    /// | `Select`, `Enter`, `Softkey::Middle` | action |
    /// | `Softkey::Right`, `End` | back |
    ///
    /// `Softkey::Middle` is accepted for the action even though S60 does not send it: a host
    /// simulator or a future device might, and treating it as anything else would be a trap laid
    /// for whoever meets that platform. `End` maps to back because the red key means "get me out"
    /// on this hardware and a screen that ignored it would feel stuck.
    pub fn dispatch(&self, ev: KeyEvent) -> Option<M> {
        let slot = match ev.key {
            Key::Softkey(Softkey::Left) => self.options.as_ref(),
            Key::Select | Key::Enter | Key::Softkey(Softkey::Middle) => self.action.as_ref(),
            Key::Softkey(Softkey::Right) | Key::End => self.back.as_ref(),
            _ => None,
        };
        slot.map(|d| d.msg.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Msg {
        Refresh,
        Open,
        Back,
    }

    fn bar() -> Softkeys<Msg> {
        Softkeys::new()
            .options("Refresh", Msg::Refresh)
            .action("Open", Msg::Open)
            .back("Back", Msg::Back)
    }

    fn press(k: Key) -> KeyEvent {
        KeyEvent::new(k)
    }

    #[test]
    fn the_dpad_centre_fires_the_action_not_a_softkey() {
        // The whole reason this module exists. A screen labels the middle slot and S60 delivers
        // `Select`; binding to `Softkey::Middle` is how a real screen ended up with a key that did
        // something other than what its label said.
        assert_eq!(bar().dispatch(press(Key::Select)), Some(Msg::Open));
        assert_eq!(bar().dispatch(press(Key::Enter)), Some(Msg::Open));
        assert_eq!(bar().dispatch(press(Key::Softkey(Softkey::Middle))), Some(Msg::Open));
    }

    #[test]
    fn the_outer_softkeys_go_where_the_convention_says() {
        assert_eq!(bar().dispatch(press(Key::Softkey(Softkey::Left))), Some(Msg::Refresh));
        assert_eq!(bar().dispatch(press(Key::Softkey(Softkey::Right))), Some(Msg::Back));
    }

    #[test]
    fn the_red_key_is_a_way_out() {
        // On this hardware End means "get me out of here", and a screen that ignored it feels
        // stuck in a way no label can explain.
        assert_eq!(bar().dispatch(press(Key::End)), Some(Msg::Back));
    }

    #[test]
    fn a_slot_with_no_label_has_nothing_to_fire() {
        // The invariant the type buys: you cannot handle a key you did not label, because the
        // label and the message are the same declaration.
        let only_back = Softkeys::new().back("Back", Msg::Back);
        assert_eq!(only_back.dispatch(press(Key::Select)), None);
        assert_eq!(only_back.dispatch(press(Key::Softkey(Softkey::Left))), None);
        assert_eq!(only_back.dispatch(press(Key::Softkey(Softkey::Right))), Some(Msg::Back));
    }

    #[test]
    fn an_ordinary_key_belongs_to_the_screen_not_the_bar() {
        // Up/Down/typing must reach the content. A bar that swallowed them would break every list.
        assert_eq!(bar().dispatch(press(Key::Up)), None);
        assert_eq!(bar().dispatch(press(Key::Down)), None);
        assert_eq!(bar().dispatch(press(Key::Char('a'))), None);
        assert_eq!(bar().dispatch(press(Key::Backspace)), None);
    }

    #[test]
    fn a_bar_can_be_readdressed_without_losing_a_slot() {
        // A migrating app wraps a screen's messages in a variant of its own. Every slot has to make
        // the journey: the one that is forgotten by hand is a key that silently stops working.
        #[derive(Clone, Debug, PartialEq, Eq)]
        enum Outer {
            Screen(Msg),
        }
        let mapped = bar().map(Outer::Screen);
        assert_eq!(mapped.labels(), [Some("Refresh"), Some("Open"), Some("Back")]);
        assert_eq!(mapped.dispatch(press(Key::Softkey(Softkey::Left))), Some(Outer::Screen(Msg::Refresh)));
        assert_eq!(mapped.dispatch(press(Key::Select)), Some(Outer::Screen(Msg::Open)));
        assert_eq!(mapped.dispatch(press(Key::End)), Some(Outer::Screen(Msg::Back)));
        // An empty slot stays empty rather than becoming a key that does nothing.
        let partial = Softkeys::new().back("Back", Msg::Back).map(Outer::Screen);
        assert_eq!(partial.labels(), [None, None, Some("Back")]);
        assert_eq!(partial.dispatch(press(Key::Select)), None);
    }

    #[test]
    fn labels_come_out_in_the_order_the_bar_draws_them() {
        assert_eq!(bar().labels(), [Some("Refresh"), Some("Open"), Some("Back")]);
        assert_eq!(Softkeys::<Msg>::new().labels(), [None, None, None]);
    }
}
