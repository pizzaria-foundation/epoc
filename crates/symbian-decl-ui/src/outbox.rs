//! Messages a widget produced, on their way to `update`.
//!
//! # The gap this fills
//!
//! A key reaches a widget through [`layout::dispatch_key`](crate::layout::dispatch_key), and the
//! only thing a widget can answer with is [`Handled`](symbian_ui::Handled). That is enough for every
//! widget in the catalogue, because what they do with a key is change their own slot state — a
//! caret moves, an offset scrolls — and the model is not involved.
//!
//! It is not enough for a widget that answers a key with a *decision*. The adapter around an
//! imperative screen ([`Imperative`](crate::widgets::Imperative)) is the case this exists for: the
//! old screen hands back `(Handled, Action)` — "the list moved, and by the way the user pressed
//! Open on row 4" — and `Handled::Consumed` has nowhere to put the second half. Before this, that
//! action had to be dropped on the floor.
//!
//! The two alternatives were worse. Routing the key through
//! [`DeclarativeApp::on_key`](crate::app::DeclarativeApp::on_key) instead cannot work: an old
//! screen's `handle_key` wants the theme and the height of the band it was drawn in, and `on_key`
//! has a model and a key event. Giving widgets a typed message channel in
//! [`KeyCtx`](crate::widget::KeyCtx) would put the app's `Message` type into every widget signature
//! in the crate, for the two widgets that need one.
//!
//! So the queue lives in the *model*, where the app already keeps its own types, and the bridge is
//! told where to find it:
//!
//! ```ignore
//! struct Model { out: Outbox<Msg>, chats: Rc<RefCell<ChatList>> }
//!
//! impl DeclarativeApp for Tg {
//!     fn outbox(m: &Model) -> Option<&Outbox<Msg>> { Some(&m.out) }
//! }
//! ```
//!
//! The bridge drains it immediately after the key walk and feeds every message through
//! [`send`](crate::bridge::DeclarativeAppBridge::send) — the same path a softkey takes. There is
//! still exactly one way for the model to change.
//!
//! # Why the bridge drains it and not the app
//!
//! The hook hands out a reference rather than returning the messages, so the app cannot forget to
//! actually empty it. An app that drained its own queue would work, and would work right up until a
//! screen was added whose `view` never touched the outbox — at which point the messages would sit
//! there and arrive on whichever later key happened to drain them. A queue nobody empties is a
//! keypress that takes effect two presses later, which is the hardest kind of bug to see on a
//! device with no console.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

/// A one-way queue from a widget to [`update`](crate::app::DeclarativeApp::update).
///
/// Cloning gives another handle on the same queue — that is the point: the model keeps one and the
/// closure inside a widget keeps another, and the widget is rebuilt every frame while the queue is
/// not.
pub struct Outbox<M> {
    inner: Rc<Inner<M>>,
}

struct Inner<M> {
    sink: Sink<M>,
    dropped: Cell<u32>,
}

/// Where a pushed message actually goes.
///
/// Two shapes, because a screen inside an app has its own message type. The app's queue *holds*
/// messages; the queue a screen is handed *forwards* them, wrapping each one on the way — see
/// [`Outbox::wrapped`]. A forwarding queue holds nothing itself, which is why it has no vector: a
/// message that stopped there would be one the bridge never drains.
enum Sink<M> {
    Queue(RefCell<Vec<M>>),
    Into(Box<dyn Fn(M)>),
}

impl<M> Outbox<M> {
    pub fn new() -> Self {
        Self {
            inner: Rc::new(Inner { sink: Sink::Queue(RefCell::new(Vec::new())), dropped: Cell::new(0) }),
        }
    }

    /// A queue that accepts a *screen's* messages and delivers them here, wrapped by `f`.
    ///
    /// The counterpart of [`Softkeys::map`](crate::keys::Softkeys::map), and needed for the same
    /// reason: a screen owns its own message enum, an application owns the one its `update` receives,
    /// and a migrating app spells the join `Msg::Chats(..)`. Without this the screen would have to be
    /// generic over the app's type, or the app would have to hand it a bare closure and lose the type
    /// that says what the screen can ask for.
    ///
    /// ```ignore
    /// let mine = app_out.wrapped(Msg::Chats);   // Outbox<chats::Msg>
    /// chats::view(&store, selected, &mine, slots);
    /// ```
    ///
    /// The wrapped queue is a handle on this one: it holds nothing, forwards on push, and — like any
    /// other handle — may outlive the call that made it, which it does, inside the widgets that
    /// captured it. [`take`](Self::take) on a wrapped queue is therefore always empty; the messages
    /// are here.
    pub fn wrapped<N: 'static>(&self, f: impl Fn(N) -> M + 'static) -> Outbox<N>
    where
        M: 'static,
    {
        let target = self.clone();
        Outbox {
            inner: Rc::new(Inner {
                sink: Sink::Into(Box::new(move |n| target.push(f(n)))),
                dropped: Cell::new(0),
            }),
        }
    }

    /// Queue a message for the next drain.
    ///
    /// A push that cannot get the borrow is *counted and dropped*, not panicked on. The only way to
    /// reach that is to push from inside a drain, which nothing in the crate does; and a panic here
    /// is a dead application on a phone whose whole failure report is a dialog with a number in it,
    /// while a lost message is a keypress that did nothing. See [`dropped`](Self::dropped) — assert
    /// it is zero in a test, because nothing else will tell you.
    pub fn push(&self, msg: M) {
        match &self.inner.sink {
            Sink::Queue(q) => match q.try_borrow_mut() {
                Ok(mut q) => q.push(msg),
                Err(_) => self.inner.dropped.set(self.inner.dropped.get() + 1),
            },
            Sink::Into(f) => f(msg),
        }
    }

    /// Everything queued since the last drain, in the order it was pushed.
    ///
    /// Draining rather than lending, for the reason
    /// [`take_effects`](crate::bridge::DeclarativeAppBridge::take_effects) is: a message delivered
    /// twice is a chat opened twice, and the only way to be sure each one arrives once is for the
    /// queue not to have it any more.
    pub fn take(&self) -> Vec<M> {
        match &self.inner.sink {
            Sink::Queue(q) => match q.try_borrow_mut() {
                Ok(mut q) => core::mem::take(&mut *q),
                // Reentrant drain: the outer one still holds the messages and will deliver them.
                Err(_) => Vec::new(),
            },
            // A wrapped queue keeps nothing — the messages went to the queue it forwards to, and
            // that is the one the bridge asks.
            Sink::Into(_) => Vec::new(),
        }
    }

    /// How many messages are waiting.
    pub fn len(&self) -> usize {
        match &self.inner.sink {
            Sink::Queue(q) => q.try_borrow().map(|q| q.len()).unwrap_or(0),
            Sink::Into(_) => 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// How many pushes were lost to a borrow that was already taken. Always zero in practice.
    pub fn dropped(&self) -> u32 {
        self.inner.dropped.get()
    }
}

impl<M> Clone for Outbox<M> {
    /// Another handle on the same queue — never a copy of it. `#[derive(Clone)]` would have
    /// demanded `M: Clone` for no reason, and a message is not required to be cloneable to be sent
    /// once.
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone() }
    }
}

impl<M> Default for Outbox<M> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[derive(Debug, PartialEq, Eq)]
    enum Msg {
        Open(usize),
        Refresh,
    }

    #[test]
    fn what_goes_in_comes_out_in_order() {
        let out = Outbox::new();
        out.push(Msg::Open(4));
        out.push(Msg::Refresh);
        assert_eq!(out.take(), vec![Msg::Open(4), Msg::Refresh]);
    }

    #[test]
    fn a_drain_empties_it() {
        // The property that makes a message arrive once. A queue that lent its contents out would
        // deliver every message again on the next key.
        let out = Outbox::new();
        out.push(Msg::Refresh);
        assert_eq!(out.take().len(), 1);
        assert!(out.take().is_empty());
        assert!(out.is_empty());
    }

    #[test]
    fn a_clone_is_the_same_queue() {
        // The whole reason this is not a plain `Vec` on the model: the widget that pushes is built
        // fresh every frame and cannot hold a borrow of anything.
        let model_side = Outbox::new();
        let widget_side = model_side.clone();
        widget_side.push(Msg::Open(1));
        assert_eq!(model_side.len(), 1);
        assert_eq!(model_side.take(), vec![Msg::Open(1)]);
        assert!(widget_side.is_empty(), "one queue, seen from two places");
    }

    #[test]
    fn a_message_type_need_not_be_cloneable() {
        // `#[derive(Clone)]` on the struct would have required it, and a message that is sent once
        // has no business proving it can be duplicated.
        struct NotClone(u8);
        let out = Outbox::new();
        out.push(NotClone(7));
        let taken = out.take();
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].0, 7);
    }

    #[test]
    fn nothing_reachable_from_outside_can_lose_a_message() {
        // `dropped` counts pushes that could not get the borrow, and the only way to hold that
        // borrow is from inside this module — `take` moves the vector out and hands it back after
        // the borrow has ended, so even a message whose `Drop` pushes again arrives. The counter
        // exists for the day someone adds a method that lends the queue out instead; until then
        // this test is what says the number should never be anything else.
        let out: Outbox<Msg> = Outbox::new();
        out.push(Msg::Refresh);
        let _ = out.take();
        out.push(Msg::Open(2));
        assert_eq!(out.dropped(), 0);
    }

    #[test]
    fn a_wrapped_queue_delivers_into_the_one_it_came_from() {
        // How a screen with its own message type reaches an app's `update`. The screen pushes what
        // it knows about; the app receives what it knows about.
        #[derive(Debug, PartialEq, Eq)]
        enum Screen {
            Select(usize),
        }
        #[derive(Debug, PartialEq, Eq)]
        enum App {
            Chats(Screen),
            Other,
        }
        let app_out: Outbox<App> = Outbox::new();
        let screen_out = app_out.wrapped(App::Chats);
        screen_out.push(Screen::Select(4));
        app_out.push(App::Other);

        // Nothing stays in the wrapped one: a message that stopped there is one nobody drains.
        assert!(screen_out.is_empty());
        assert_eq!(app_out.take(), vec![App::Chats(Screen::Select(4)), App::Other]);
    }

    #[test]
    fn a_wrapped_queue_outlives_the_call_that_made_it() {
        // It has to: the closures that push are inside widgets, and they are built once and pushed
        // to on whatever key arrives three frames later.
        let app_out: Outbox<Msg> = Outbox::new();
        let pusher: alloc::boxed::Box<dyn Fn(usize)> = {
            let screen_out = app_out.wrapped(Msg::Open);
            alloc::boxed::Box::new(move |i| screen_out.push(i))
        };
        pusher(9);
        assert_eq!(app_out.take(), vec![Msg::Open(9)]);
    }

    #[test]
    fn wrapping_twice_is_still_one_queue() {
        // A screen inside a screen. Nothing in the crate does this yet; the day something does, the
        // answer should not depend on how deep it is.
        #[derive(Debug, PartialEq, Eq)]
        enum Outer {
            Inner(Msg),
        }
        let out: Outbox<Outer> = Outbox::new();
        let mid = out.wrapped(Outer::Inner);
        let inner = mid.wrapped(Msg::Open);
        inner.push(3);
        assert_eq!(out.take(), vec![Outer::Inner(Msg::Open(3))]);
    }

    #[test]
    fn an_empty_drain_does_not_allocate_a_message() {
        // `Vec::new()` is the empty case, which is the one that runs on every key nobody wanted.
        let out: Outbox<Msg> = Outbox::new();
        assert_eq!(out.take().capacity(), 0);
    }
}
