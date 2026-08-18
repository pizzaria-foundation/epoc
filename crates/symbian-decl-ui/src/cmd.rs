//! What an update asks the world to do, as a value.
//!
//! # Why `update` returns an effect instead of causing one
//!
//! The imperative screens call the platform from inside the key handler: the softkey that means
//! "refresh" opens the socket, and the one that means "quit" calls `Exit` on the framework. Two
//! things go wrong with that, and both have already happened here.
//!
//! The first is that the handler cannot be tested. Half of `handle_key` is model arithmetic and
//! half is a syscall, and there is no seam between them — so the arithmetic is only ever exercised
//! on a phone, by hand.
//!
//! The second is worse. An app must never ask Avkon to exit from inside an event callback; the
//! framework owns the loop and expects to be told *afterwards*, which is why
//! [`symbian_ui::App::should_exit`] is a question the host asks rather than a method the app calls.
//! An `update` that could reach the platform is an `update` that can make that mistake.
//!
//! So `update` returns a [`Cmd`]: a description of what should happen, which the bridge either
//! performs itself (it owns the exit flag and the screen stack) or queues for whoever owns the
//! sockets and the timers. The arithmetic becomes a pure function of model and message, and the
//! only code that touches the platform is the code that was always going to.

use alloc::string::String;
use alloc::vec::Vec;

/// An effect requested by [`update`](crate::app::DeclarativeApp::update).
///
/// `S` is the app's own screen identifier — an enum like `ChatList | Conversation(usize)`. It is a
/// type parameter rather than a fixed id because navigation targets carry payloads: "the
/// conversation" is not a destination, "conversation 4" is. An app with a single screen writes
/// `Cmd<()>`.
///
/// # Which of these the bridge performs
///
/// [`Cmd::Exit`], [`Cmd::PushScreen`], [`Cmd::PopScreen`] and [`Cmd::Batch`] are navigation and
/// lifetime, which the bridge owns outright. The rest name resources this crate deliberately cannot
/// reach — there is no dependency on `symbian-sys` here, and there should not be, or a layout test
/// would need a phone. They are queued for the host to drain with
/// [`take_effects`](crate::bridge::DeclarativeAppBridge::take_effects).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cmd<S = ()> {
    /// Nothing to do. The common case: most messages only move the model.
    None,

    /// The app wants to close. Becomes [`symbian_ui::App::should_exit`] — see the module note on
    /// why this is a flag and not a call.
    Exit,

    /// Wake me in `ms` milliseconds and give me back `handle` so I know which timer fired.
    ///
    /// The handle is the app's own number, not the platform's: the app chooses it when it asks, so
    /// the completion can be matched to the request without the bridge keeping a table.
    SetTimer { handle: i32, ms: u32 },

    /// Open a socket to `host:port`.
    ///
    /// `host` is owned rather than `&'static str`. The plan wrote it as a static string, which is
    /// right for a constant endpoint and wrong for every app that reads its server from
    /// configuration — and this crate already links `alloc`, so the borrow buys nothing.
    Connect { host: String, port: u16 },

    /// Write `data` to an already-open socket.
    ///
    /// Owned for the same reason as [`Cmd::Connect`], and more sharply: the bytes an app sends are
    /// almost always ones it just built.
    Send { socket: i32, data: Vec<u8> },

    /// Go to `screen`, keeping the current one to come back to.
    PushScreen(S),

    /// Go back. See [`DeclarativeAppBridge::execute`](crate::bridge::DeclarativeAppBridge::execute)
    /// for what happens at the bottom of the stack — it is defined, and it is not exiting.
    PopScreen,

    /// Several effects, in order.
    ///
    /// Without this, an `update` that needs to both navigate and start a request has to pick one
    /// and smuggle the other into the model, which puts effect state where the pure data lives.
    Batch(Vec<Cmd<S>>),
}

impl<S> Cmd<S> {
    /// Several commands as one, with the empties removed.
    ///
    /// Collapsing `0` to [`Cmd::None`] and `1` to itself is not just tidiness: it means a caller
    /// can build a batch out of conditionals — `[maybe_a, maybe_b]`, either of which may be `None`
    /// — without the common single-effect case paying for a `Vec` allocation.
    pub fn batch(cmds: impl IntoIterator<Item = Cmd<S>>) -> Cmd<S> {
        let mut kept: Vec<Cmd<S>> = cmds.into_iter().filter(|c| !c.is_none()).collect();
        match kept.len() {
            0 => Cmd::None,
            1 => kept.pop().unwrap_or(Cmd::None),
            _ => Cmd::Batch(kept),
        }
    }

    /// Whether this command asks for nothing at all.
    pub fn is_none(&self) -> bool {
        matches!(self, Cmd::None)
    }

    /// Whether the bridge can carry this out on its own.
    ///
    /// The complement is what lands in the host's effect queue, so the two must stay exhaustive
    /// together; matching on every variant here rather than listing the platform ones means a new
    /// variant fails to compile instead of silently being dropped on the floor.
    pub fn is_navigation(&self) -> bool {
        match self {
            Cmd::None | Cmd::Exit | Cmd::PushScreen(_) | Cmd::PopScreen | Cmd::Batch(_) => true,
            Cmd::SetTimer { .. } | Cmd::Connect { .. } | Cmd::Send { .. } => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Screen {
        List,
        Detail(usize),
    }

    #[test]
    fn a_screen_target_can_carry_which_one() {
        // The reason `Cmd` is generic at all: "open the conversation" is not a destination, and an
        // opaque numeric id would push the payload into the model where it would drift.
        assert_eq!(Cmd::PushScreen(Screen::Detail(4)), Cmd::PushScreen(Screen::Detail(4)));
        assert_ne!(Cmd::PushScreen(Screen::Detail(4)), Cmd::PushScreen(Screen::Detail(5)));
        assert_ne!(Cmd::PushScreen(Screen::Detail(0)), Cmd::PushScreen(Screen::List));
    }

    #[test]
    fn batching_nothing_costs_nothing() {
        // Built from conditionals that mostly produce `None`, a batch must not allocate for the
        // ordinary case — that is the whole point of collapsing here rather than in `execute`.
        assert_eq!(Cmd::<()>::batch([]), Cmd::None);
        assert_eq!(Cmd::<()>::batch([Cmd::None, Cmd::None]), Cmd::None);
        assert_eq!(Cmd::<()>::batch([Cmd::None, Cmd::Exit, Cmd::None]), Cmd::Exit);
    }

    #[test]
    fn batching_two_keeps_both_and_their_order() {
        let c = Cmd::<()>::batch([
            Cmd::SetTimer { handle: 1, ms: 500 },
            Cmd::None,
            Cmd::Exit,
        ]);
        assert_eq!(c, Cmd::Batch(vec![Cmd::SetTimer { handle: 1, ms: 500 }, Cmd::Exit]));
    }

    #[test]
    fn the_split_between_navigation_and_platform_is_exhaustive() {
        // Every variant must fall on one side. If it did not, the bridge would neither perform it
        // nor hand it to the host, and the effect would vanish with no error anywhere.
        assert!(Cmd::<()>::None.is_navigation());
        assert!(Cmd::<()>::Exit.is_navigation());
        assert!(Cmd::PushScreen(Screen::List).is_navigation());
        assert!(Cmd::<()>::PopScreen.is_navigation());
        assert!(Cmd::<()>::Batch(vec![]).is_navigation());

        assert!(!Cmd::<()>::SetTimer { handle: 0, ms: 1 }.is_navigation());
        assert!(!Cmd::<()>::Connect { host: String::from("h"), port: 1 }.is_navigation());
        assert!(!Cmd::<()>::Send { socket: 0, data: vec![1] }.is_navigation());
    }

    #[test]
    fn an_endpoint_can_come_from_configuration() {
        // The deviation from the plan, stated as a test: a host read at runtime has no static
        // lifetime, and an app whose server is a setting is the normal case rather than the exotic
        // one.
        let from_settings = String::from("api.example.org");
        let c: Cmd<()> = Cmd::Connect { host: from_settings, port: 443 };
        assert!(!c.is_navigation());
    }
}
