//! Driving a [`DeclarativeApp`] from the host that only knows [`symbian_ui::App`].
//!
//! The device entry point, the simulator and `symbian-app`'s `entry!` all speak one contract:
//! hand it a key, ask it to draw, ask whether it wants to close. This adapter is what lets a
//! model-update-view app answer that contract without any of them learning a second one.
//!
//! # Why the tree is kept between frames
//!
//! [`view`](DeclarativeApp::view) allocates: a screen is a [`Node`] over a handful of boxes and a
//! few `String`s. Building it inside `handle_key` — which the plan's sketch does, by
//! rebuilding inside `draw` on every frame — means a keypress that scrolls a list allocates the
//! whole screen again, and so does one that does nothing at all. On a 600 MHz ARM with a
//! non-compacting allocator that is not just slow, it is fragmentation: the same dozen boxes freed
//! and reallocated every time a thumb moves.
//!
//! So the tree is built once and kept until something could have changed it. "Could have changed
//! it" is exactly one thing: an [`update`](DeclarativeApp::update) ran, or the screen stack moved.
//! A key that maps to no message does neither — [`DeclarativeApp::on_key`] returned `None`, no
//! update ran, and the tree in hand is still correct — so the bridge returns
//! [`Handled::Ignored`] and the host does not even repaint.
//!
//! The conservative direction matters here. Rebuilding when nothing changed wastes a few hundred
//! microseconds; *not* rebuilding when something did is a screen that silently stops updating,
//! which is the bug this crate's [`content_hash`](crate::Widget::content_hash) default is also
//! written to avoid. Every path that touches the model drops the tree.
//!
//! # Except when a key arrives and there is no tree
//!
//! Then it is built, and laid out, before the key is offered to anything — see
//! [`layout::place_frame`](crate::layout::place_frame). That is not a retreat from the paragraph
//! above; it is what makes it survivable. The platform hands the host a *batch* of events and the
//! host draws once at the end of it, so the press after any press that changed the model would
//! otherwise reach widgets with no rects and be answered by nobody. Holding a direction key would
//! advance a list one row per frame rather than one per press.
//!
//! The rule that is never bent is the other one: a widget is only ever asked about a key at the rect
//! it would be drawn at *now*. A tree rebuilt and not placed would answer at last frame's positions,
//! which is worse than answering nothing.

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

use symbian_gfx::{Canvas, Rect};
use symbian_ui::{Clipboard, Handled, KeyEvent, NoClipboard, Theme};

use crate::app::DeclarativeApp;
use crate::cache::UiCache;
use crate::cmd::Cmd;
use crate::outbox::Outbox;
use crate::slot::SlotTable;
use crate::widget::KeyCtx;
use crate::widgets::Node;

/// A [`DeclarativeApp`] wearing the [`symbian_ui::App`] contract.
pub struct DeclarativeAppBridge<A: DeclarativeApp> {
    model: A::Model,
    /// The last built screen, or `None` for "stale". One `Option` is the whole dirty flag: a
    /// separate `bool` could disagree with the tree beside it, and this one cannot.
    tree: Option<Node>,
    /// Measured sizes, kept across frames — and kept *here* for exactly that reason.
    ///
    /// The bridge does not read this and does not know what is in it; `layout` owns every question
    /// about what a cache entry means and when a frame begins. What the bridge owns is the one
    /// thing layout cannot: a lifetime longer than a frame. A cache constructed inside `draw_tree`
    /// would be born and dead within the same call, every
    /// [`content_hash`](crate::Widget::content_hash) would miss, and `measure` would run on every
    /// widget on every frame — which is the whole cost this crate was built to stop paying.
    cache: UiCache,
    /// Widget state that has to survive the tree being rebuilt: a list's scroll offset, a field's
    /// caret.
    ///
    /// Here for the same reason the cache is here, and it is worth stating once and applying twice:
    /// the bridge is the only thing in this crate that outlives a frame. Everything that must
    /// persist across one is therefore stored here, and everything that knows what the contents
    /// *mean* lives elsewhere — `layout` for the cache, the widgets themselves for the slots.
    ///
    /// The bridge also runs this table's frame, which the app cannot be trusted with: an app that
    /// forgot `begin_frame` would not fail, it would slowly stop persisting anything, and the
    /// symptom — a caret that jumps to zero, sometimes — looks nothing like the cause.
    slots: SlotTable,
    /// Screens pushed above the root, deepest last. The root is not on it — see [`Self::execute`].
    stack: Vec<A::Screen>,
    /// Commands the bridge cannot carry out itself, waiting for whoever owns the platform.
    effects: Vec<Cmd<A::Screen>>,
    /// Where a widget in the tree copies to and pastes from.
    ///
    /// The bridge holds it because the bridge is where a key arrives and the only thing here that
    /// outlives a frame — the same argument as the cache and the slots above. It is a `Box<dyn>`
    /// rather than a type parameter so that adding paste to an app is one call at construction and
    /// not a new generic on every screen it owns.
    ///
    /// [`NoClipboard`] by default, so a build with no platform clipboard behaves like a device
    /// whose clipboard is empty: paste does nothing, quietly.
    clip: Box<dyn Clipboard>,
    exit: bool,
}

impl<A: DeclarativeApp> DeclarativeAppBridge<A> {
    /// Start the app: [`init`](DeclarativeApp::init), at the root screen, with nothing pending.
    ///
    /// No view is built here. The first draw builds it, which keeps construction cheap enough to
    /// do before a window exists — and there is no theme to measure against yet anyway.
    pub fn new() -> Self {
        Self::with_model(A::init())
    }

    /// The one place the fields are written, so the two constructors cannot drift.
    fn from_model(model: A::Model) -> Self {
        Self {
            model,
            tree: None,
            cache: UiCache::new(),
            slots: SlotTable::new(),
            stack: Vec::new(),
            effects: Vec::new(),
            clip: Box::new(NoClipboard),
            exit: false,
        }
    }

    /// Start from a model that was built elsewhere.
    ///
    /// [`init`](DeclarativeApp::init) is a function of nothing, which is right for an app whose
    /// starting state is a constant and wrong for one whose first model comes from the world: a
    /// dialog list read out of a cache file, a session restored from disk, a mock store in the
    /// simulator where the device would open a connection. Those differ per *host*, not per app —
    /// the same `DeclarativeApp` runs in both — so choosing between them cannot be `init`'s job
    /// without a second app type existing only to hold a different constructor.
    ///
    /// `init` is still what the trait requires and still what the ordinary constructor calls: this
    /// is the shell's door, used where the shell is the thing that knows.
    pub fn with_model(model: A::Model) -> Self {
        Self::from_model(model)
    }

    /// Give the screens a clipboard, so their text fields can copy and paste.
    ///
    /// On a device: `.with_clipboard(symbian_app::SystemClipboard)`. In a test or a preview, a
    /// `MemClipboard` makes copy-and-paste between two fields an ordinary assertion.
    ///
    /// Without this the fields still edit, select and cut *within themselves* — they simply find
    /// nothing to paste, which is what an empty clipboard looks like anyway.
    pub fn with_clipboard(mut self, clip: impl Clipboard + 'static) -> Self {
        self.clip = Box::new(clip);
        self
    }

    /// Change the clipboard after construction.
    ///
    /// [`with_clipboard`](Self::with_clipboard) is the builder and reads better where a bridge is
    /// created in one expression. This is for a *shell* that owns a bridge as a field and cannot move
    /// it out to call a builder — which is the shape every host in this SDK ends up with, since it has
    /// other things to hold as well.
    pub fn set_clipboard(&mut self, clip: impl Clipboard + 'static) {
        self.clip = Box::new(clip);
    }

    /// The model, to read.
    ///
    /// There is deliberately no `model_mut`. A caller that could reach in would be a second
    /// `update` with none of its discipline — and, worse, one the bridge does not see, so the tree
    /// would keep its stale copy of a model that had changed underneath it.
    pub fn model(&self) -> &A::Model {
        &self.model
    }

    /// Deliver a message that did not come from a key.
    ///
    /// This is how a timer completion or a socket read gets in: the host performed the effect it
    /// took off [`Self::take_effects`], and hands the result back as a message. It is the same path
    /// a keypress takes, which is the point — there is one way for the model to change.
    pub fn send(&mut self, msg: A::Message) {
        let cmd = A::update(&mut self.model, msg);
        self.invalidate();
        self.execute(cmd);
    }

    /// Carry out a command, or queue it for someone who can.
    ///
    /// # Popping the last screen is not exiting
    ///
    /// The stack holds screens pushed *above* the root, so a [`Cmd::PopScreen`] at the root has
    /// nothing to remove and does nothing. It is tempting to make it exit — S60's Back does leave
    /// an app from its main view — but that decision belongs to the app, which says so by binding
    /// its back softkey to [`Cmd::Exit`]. If the bridge guessed, an app whose Back both pops and is
    /// pressed twice quickly would close itself, and the second press is the one a user makes when
    /// the first seemed not to take.
    pub fn execute(&mut self, cmd: Cmd<A::Screen>) {
        // Iteratively, not recursively. A Symbian thread gets 8 KB of stack by default, and a
        // `Cmd::Batch` is built by app code that may well build it in a loop; a recursive walk
        // would turn a long batch into a stack overflow with no backtrace to read on a handset.
        let mut work = vec![cmd];
        while let Some(c) = work.pop() {
            match c {
                Cmd::None => {}
                Cmd::Exit => self.exit = true,
                Cmd::PushScreen(s) => {
                    self.stack.push(s);
                    self.screen_moved();
                }
                Cmd::PopScreen => {
                    if self.stack.pop().is_some() {
                        self.screen_moved();
                    }
                }
                // Reversed, so that popping off the end still runs them in the order written.
                Cmd::Batch(inner) => work.extend(inner.into_iter().rev()),
                platform => self.effects.push(platform),
            }
        }
    }

    /// The commands the bridge could not perform, taken away for the host to run.
    ///
    /// Draining rather than lending: an effect handed out twice is a socket opened twice, and the
    /// only way to be sure the host ran each one once is for the queue not to have it any more.
    pub fn take_effects(&mut self) -> Vec<Cmd<A::Screen>> {
        core::mem::take(&mut self.effects)
    }

    /// How many `measure` calls the frames so far have actually run.
    ///
    /// The one number the bridge reads out of the cache, and it reads it without interpreting it:
    /// this is a diagnostic, not a decision. It exists because "the cache works" is only provable
    /// from outside — an integration test driving whole frames has no other way to tell a screen
    /// that re-measured everything from one that re-measured nothing, and both draw identically.
    pub fn measure_calls(&self) -> u32 {
        self.cache.measure_calls()
    }

    /// The widget state table, to read.
    ///
    /// Shared as `&` and never `&mut`: everything that *writes* to the table needs `&mut` and is
    /// therefore still reachable only from inside `view`, where the ordinals are counted. What is
    /// left over is the diagnostics — `slot_count`, `group_count`, `type_mismatches` — which are
    /// exactly what a test needs to prove that a screen standing still is not quietly growing one
    /// slot per frame.
    pub fn slots(&self) -> &SlotTable {
        &self.slots
    }

    /// Where the app is now, or `None` at the root.
    pub fn screen(&self) -> Option<&A::Screen> {
        self.stack.last()
    }

    /// How many screens have been pushed above the root.
    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    /// The current screen description, built if the last one is stale.
    ///
    /// Public because the host draws it and a test wants to measure it, and because the rebuild
    /// decision has to live in exactly one place — if `draw` did it privately, anything else that
    /// needed the tree would build a second one.
    pub fn tree(&mut self) -> &Node {
        // `get_or_insert_with` rather than a check-then-unwrap: the unwrap would be provably fine
        // and still be a panic compiled into a device binary that has no console to print it on.
        // Edition 2021's per-field capture is what makes it legal — the closure borrows
        // `self.model`, not `self`, so it does not collide with the `&mut self.tree`.
        // Reborrowed by field so the closure captures `model` and `slots` rather than all of
        // `self`, which `self.tree` is already borrowed out of.
        let (model, slots) = (&self.model, &mut self.slots);
        self.tree.get_or_insert_with(|| {
            slots.begin_frame();
            A::view(model, slots)
        })
    }

    /// The model may have changed, so the screen in hand no longer describes it.
    fn invalidate(&mut self) {
        self.tree = None;
    }

    fn screen_moved(&mut self) {
        // The stack is the bridge's, but `view` only ever sees the model — so the app is told,
        // and gets to record the new screen wherever its own view reads it from.
        A::screen_changed(&mut self.model, self.stack.last());
        self.invalidate();
    }
}

impl<A: DeclarativeApp> Default for DeclarativeAppBridge<A> {
    fn default() -> Self {
        Self::new()
    }
}

impl<A: DeclarativeApp> symbian_ui::App for DeclarativeAppBridge<A> {
    fn handle_key(&mut self, ev: KeyEvent, theme: &Theme<'_>, screen: Rect) -> Handled {
        if let Some(msg) = A::on_key(&self.model, ev) {
            self.send(msg);
            return Handled::Consumed;
        }
        // Nobody at the app's level wanted it, so offer it to the widgets — step three of the
        // resolution order in `widgets::on_key`, and the step that had never been wired up. This is
        // what lets a text field be typed into at all on this path.
        //
        // `self.tree.as_ref()`, never `self.tree()`: rebuilding a view here would allocate on a
        // keystroke and, worse, would build a tree the layout has not placed — so every widget in
        // it would be asked about a key at a rect from the frame before. A tree that has never been
        // drawn simply takes no keys.
        // A key must be offered to the widgets at the rects they would be drawn at, so the tree has
        // to exist *and* have been placed. Both are available here — `screen` is the rect, `theme` is
        // what measures — so a tree that is missing is built and laid out rather than skipped.
        //
        // It is missing more often than it sounds. Every `update` drops it, and the platform hands
        // over a whole batch of events before the host draws anything: hold a direction key and the
        // first press moves the list, invalidates the tree, and every other press in that batch would
        // reach a widget with no rect and be answered by nobody. The list would advance one row per
        // frame instead of one per press, and nothing in a screenshot would say why.
        //
        // What must never happen is asking a widget about a key at a rect from an *older* layout,
        // which is what an unplaced rebuild would do. `place_frame` is that layout, minus the paint.
        if self.tree.is_none() {
            let (model, slots) = (&self.model, &mut self.slots);
            let root = self.tree.get_or_insert_with(|| {
                slots.begin_frame();
                A::view(model, slots)
            });
            crate::layout::place_frame(root, screen, &mut self.cache, theme);
        }
        // Scoped so every borrow of a field is finished before `send` wants all of `self`. The two
        // inside — `self.clip` mutably for the context, `self.model` immutably for the queue — are
        // disjoint fields, which is the only reason both can be live at once.
        let (handled, produced) = {
            let Some(root) = self.tree.as_ref() else { return Handled::Ignored };
            let mut cx = KeyCtx::new(theme, self.clip.as_mut());
            let handled = crate::layout::dispatch_key(root, ev, &self.cache, &mut cx);
            // Drained on every key, not only on a consumed one: a widget is allowed to produce a
            // message and still leave the key for something else, and a queue emptied on some paths
            // and not others is a message that arrives on whichever later press happens to drain it.
            (handled, A::outbox(&self.model).map(Outbox::take).unwrap_or_default())
        };
        // No `invalidate()` when a widget took it: what changed is slot state — a caret, an
        // offset — which the next `view` reads through the same `Rc` it always had. Rebuilding
        // would throw away a tree that is still correct and allocate to produce an identical one.
        //
        // `Ignored` for a key nothing answered is what tells the host not to repaint, and a
        // full-screen blit for a press that changed nothing is the difference between a screen that
        // feels immediate and one that does not.
        if produced.is_empty() {
            return handled;
        }
        for msg in produced {
            // The same path a softkey takes: `update`, then invalidate, then whatever `Cmd` asked
            // for. A widget's decision is not a second way for the model to change.
            self.send(msg);
        }
        // The model moved, whatever the widget said about the key itself — so the host has to
        // repaint, and `Ignored` here would be a screen that lags one press behind.
        Handled::Consumed
    }

    fn draw(&mut self, c: &mut Canvas<'_>, theme: &Theme<'_>) {
        let rect = Rect::from_size(c.size());
        // Reached field by field rather than through [`Self::tree`], because that method borrows
        // all of `self` and the cache beside it is needed in the same expression. The rule it
        // encodes is still the one rule: build only if the last one is stale.
        let (model, slots) = (&self.model, &mut self.slots);
        let root = self.tree.get_or_insert_with(|| {
            slots.begin_frame();
            A::view(model, slots)
        });

        // One call, not the measure/layout/draw triple written out here: the passes must run in
        // that order *and* the frame must be started before them, and `draw_frame` is the only
        // place that knows both. Driving the three by hand works right up until `begin_frame` is
        // forgotten, at which point every rect from last frame still reads as current and a branch
        // removed from the tree goes on painting where it used to be.
        crate::layout::draw_frame(root, rect, &mut self.cache, c, theme);
    }

    fn should_exit(&self) -> bool {
        self.exit
    }

    fn title(&self) -> &str {
        A::TITLE
    }

    /// Take the platform clipboard `entry!` hands over, so every text field on every screen can
    /// copy and paste. Replaces the `NoClipboard` the bridge starts with.
    fn install_clipboard(&mut self, clip: alloc::boxed::Box<dyn Clipboard>) {
        self.clip = clip;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;

    use symbian_gfx::Size;
    use symbian_ui::{App as _, Key, Softkey};

    use crate::constraints::Constraints;
    use crate::keys::Softkeys;
    use crate::widget::Widget;

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Screen {
        Detail(usize),
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Msg {
        Next,
        Open,
        Back,
        Quit,
        Loaded(usize),
        Refresh,
    }

    struct Model {
        selected: usize,
        loaded: usize,
        /// Where the app records the bridge's stack, because `view` never sees it.
        screen: Option<Screen>,
        /// Counted from inside `view`, which only has `&Model` — hence the cell. The counts are
        /// the deliverable: "does not rebuild" is not observable any other way.
        views: Cell<u32>,
        updates: u32,
    }

    /// A widget whose measured size is the model value that built it, so a test can ask the tree
    /// what the view saw rather than trusting that it saw anything.
    struct Mirror(i32);

    impl Widget for Mirror {
        fn measure(&self, c: Constraints, _t: &Theme<'_>) -> Size {
            c.constrain(Size::new(self.0, 1))
        }
        fn draw(&self, _c: &mut Canvas<'_>, _r: Rect, _t: &Theme<'_>) {}
    }

    struct Two;

    impl DeclarativeApp for Two {
        type Model = Model;
        type Message = Msg;
        type Screen = Screen;
        const TITLE: &'static str = "Two";

        fn init() -> Model {
            Model {
                selected: 0,
                loaded: 0,
                screen: None,
                views: Cell::new(0),
                updates: 0,
            }
        }

        fn keys(_m: &Model) -> Softkeys<Msg> {
            Softkeys::new().options("Quit", Msg::Quit).action("Open", Msg::Open).back("Back", Msg::Back)
        }

        fn on_key(m: &Model, ev: KeyEvent) -> Option<Msg> {
            match ev.key {
                Key::Down => Some(Msg::Next),
                _ => Self::keys(m).dispatch(ev),
            }
        }

        fn update(m: &mut Model, msg: Msg) -> Cmd<Screen> {
            m.updates += 1;
            match msg {
                Msg::Next => {
                    m.selected += 1;
                    Cmd::None
                }
                Msg::Open => Cmd::PushScreen(Screen::Detail(m.selected)),
                Msg::Back => Cmd::PopScreen,
                Msg::Quit => Cmd::Exit,
                Msg::Loaded(n) => {
                    m.loaded = n;
                    Cmd::None
                }
                Msg::Refresh => Cmd::Batch(alloc::vec![
                    Cmd::SetTimer { handle: 9, ms: 1000 },
                    Cmd::PushScreen(Screen::Detail(0)),
                ]),
            }
        }

        fn view(m: &Model, _slots: &mut SlotTable) -> Node {
            m.views.set(m.views.get() + 1);
            // Whatever the tree measures as is what the view read out of the model — on the detail
            // screen, the item it was opened for; on the list, the selection.
            match m.screen {
                Some(Screen::Detail(i)) => Node::leaf(Mirror(100 + i as i32)),
                None => Node::leaf(Mirror(m.selected as i32)),
            }
        }

        fn screen_changed(m: &mut Model, top: Option<&Screen>) {
            m.screen = top.cloned();
        }
    }

    fn press(k: Key) -> KeyEvent {
        KeyEvent::new(k)
    }

    fn screen_rect() -> Rect {
        Rect::from_size(Size::new(320, 240))
    }

    /// Drive `handle_key` the way the host does, theme and all.
    fn key<A: DeclarativeApp>(b: &mut DeclarativeAppBridge<A>, k: Key) -> Handled {
        let mut out = Handled::Ignored;
        symbian_ui::testing::with_theme(symbian_ui::Palette::DARK, |t| {
            out = b.handle_key(press(k), t, screen_rect());
        });
        out
    }

    /// What the tree says its size is — i.e. what `view` read out of the model.
    ///
    /// Measured through the layout pass rather than by calling `measure` on the root, because a
    /// `Node` is a tree and only the engine knows how to walk one. A throwaway cache: this asks
    /// what the tree says *now*, and a cache that persisted would answer with what it said before.
    fn mirrored<A: DeclarativeApp>(b: &mut DeclarativeAppBridge<A>) -> i32 {
        let mut w = -1;
        symbian_ui::testing::with_theme(symbian_ui::Palette::DARK, |t| {
            let mut cache = UiCache::new();
            w = crate::layout::measure_tree(b.tree(), Constraints::loose(1000, 1000), t, &mut cache).w;
        });
        w
    }

    #[test]
    fn a_message_updates_the_model_and_the_next_view_sees_it() {
        let mut b = DeclarativeAppBridge::<Two>::new();
        assert_eq!(mirrored(&mut b), 0);

        assert_eq!(key(&mut b, Key::Down), Handled::Consumed);
        assert_eq!(b.model().selected, 1);
        // The point of the whole arrangement: the view is a function of the model *after* update,
        // never of the one that was current when the key arrived.
        assert_eq!(mirrored(&mut b), 1);
    }

    #[test]
    fn exit_reaches_the_host_through_should_exit() {
        let mut b = DeclarativeAppBridge::<Two>::new();
        assert!(!b.should_exit());
        assert_eq!(key(&mut b, Key::Softkey(Softkey::Left)), Handled::Consumed);
        assert!(b.should_exit(), "Cmd::Exit must become the flag the host reads");
    }

    #[test]
    fn a_key_that_means_nothing_does_not_run_update() {
        // Counted, not inferred. An `update` that runs for every keystroke is invisible until the
        // day a model does something expensive in it.
        let mut b = DeclarativeAppBridge::<Two>::new();
        assert_eq!(b.model().updates, 0);

        assert_eq!(key(&mut b, Key::Up), Handled::Ignored);
        assert_eq!(key(&mut b, Key::Char('x')), Handled::Ignored);
        assert_eq!(key(&mut b, Key::Backspace), Handled::Ignored);
        assert_eq!(b.model().updates, 0);

        assert_eq!(key(&mut b, Key::Down), Handled::Consumed);
        assert_eq!(b.model().updates, 1);
    }

    #[test]
    fn the_view_is_rebuilt_after_an_update_and_not_after_an_ignored_key() {
        let mut b = DeclarativeAppBridge::<Two>::new();
        assert_eq!(b.model().views.get(), 0, "construction must not build a screen");

        mirrored(&mut b);
        assert_eq!(b.model().views.get(), 1);

        // Asking twice for an unchanged model reuses the tree — this is the allocation the phone
        // cannot afford per frame.
        mirrored(&mut b);
        assert_eq!(b.model().views.get(), 1);

        key(&mut b, Key::Up);
        mirrored(&mut b);
        assert_eq!(b.model().views.get(), 1, "an ignored key changed nothing to redraw");

        key(&mut b, Key::Down);
        mirrored(&mut b);
        assert_eq!(b.model().views.get(), 2, "an update did change something");
    }

    #[test]
    fn handling_a_key_does_not_build_a_view_by_itself() {
        // Keys can arrive faster than frames — auto-repeat on the D-pad does exactly that. Only
        // the draw that follows should pay for a tree, however many presses preceded it.
        let mut b = DeclarativeAppBridge::<Two>::new();
        for _ in 0..10 {
            key(&mut b, Key::Down);
        }
        assert_eq!(b.model().views.get(), 0);
        assert_eq!(b.model().updates, 10);
        mirrored(&mut b);
        assert_eq!(b.model().views.get(), 1);
    }

    #[test]
    fn pushing_and_popping_moves_between_screens() {
        let mut b = DeclarativeAppBridge::<Two>::new();
        key(&mut b, Key::Down);
        assert_eq!(mirrored(&mut b), 1);

        key(&mut b, Key::Select);
        assert_eq!(b.depth(), 1);
        assert_eq!(b.screen(), Some(&Screen::Detail(1)));
        // The push carried which item, and the view drew that one.
        assert_eq!(mirrored(&mut b), 101);

        key(&mut b, Key::Softkey(Softkey::Right));
        assert_eq!(b.depth(), 0);
        assert_eq!(b.screen(), None);
        assert_eq!(mirrored(&mut b), 1, "coming back shows the list where it was left");
    }

    #[test]
    fn popping_the_root_does_nothing_and_does_not_exit() {
        // The decision written down in `execute`: Back at the root is the app's to interpret. If
        // the bridge exited here, a second Back — the one a user presses when the first seemed not
        // to register — would close the app.
        let mut b = DeclarativeAppBridge::<Two>::new();
        assert_eq!(b.depth(), 0);

        key(&mut b, Key::Softkey(Softkey::Right));
        key(&mut b, Key::Softkey(Softkey::Right));

        assert_eq!(b.depth(), 0);
        assert!(!b.should_exit());
        assert_eq!(b.screen(), None);
    }

    #[test]
    fn the_stack_nests_and_unwinds_in_order() {
        let mut b = DeclarativeAppBridge::<Two>::new();
        b.execute(Cmd::PushScreen(Screen::Detail(1)));
        b.execute(Cmd::PushScreen(Screen::Detail(2)));
        assert_eq!(b.depth(), 2);
        assert_eq!(b.screen(), Some(&Screen::Detail(2)));

        b.execute(Cmd::PopScreen);
        assert_eq!(b.screen(), Some(&Screen::Detail(1)));
        b.execute(Cmd::PopScreen);
        assert_eq!(b.screen(), None);
        b.execute(Cmd::PopScreen);
        assert_eq!(b.depth(), 0);
    }

    #[test]
    fn a_screen_change_invalidates_the_tree_even_with_no_message() {
        // `execute` can be called by the host as well as by `update`, and navigation changes what
        // `view` returns without any model field moving on its own.
        let mut b = DeclarativeAppBridge::<Two>::new();
        mirrored(&mut b);
        let before = b.model().views.get();
        b.execute(Cmd::PushScreen(Screen::Detail(7)));
        assert_eq!(mirrored(&mut b), 107);
        assert_eq!(b.model().views.get(), before + 1);
    }

    #[test]
    fn effects_the_bridge_cannot_perform_are_handed_to_the_host() {
        let mut b = DeclarativeAppBridge::<Two>::new();
        b.execute(Cmd::SetTimer { handle: 3, ms: 250 });
        b.execute(Cmd::Send { socket: 1, data: alloc::vec![7, 8] });

        let taken = b.take_effects();
        assert_eq!(taken.len(), 2);
        assert_eq!(taken[0], Cmd::SetTimer { handle: 3, ms: 250 });
        // Drained, not lent: running an effect twice is a socket opened twice.
        assert!(b.take_effects().is_empty());
    }

    #[test]
    fn a_batch_runs_in_the_order_it_was_written() {
        let mut b = DeclarativeAppBridge::<Two>::new();
        b.execute(Cmd::Batch(alloc::vec![
            Cmd::SetTimer { handle: 1, ms: 10 },
            Cmd::PushScreen(Screen::Detail(0)),
            Cmd::Batch(alloc::vec![Cmd::SetTimer { handle: 2, ms: 20 }, Cmd::PopScreen]),
            Cmd::SetTimer { handle: 3, ms: 30 },
        ]));

        // A nested batch runs where it sits, not after everything else — otherwise a batch would
        // mean something different depending on how it was assembled.
        let handles: alloc::vec::Vec<i32> = b
            .take_effects()
            .iter()
            .filter_map(|c| match c {
                Cmd::SetTimer { handle, .. } => Some(*handle),
                _ => None,
            })
            .collect();
        assert_eq!(handles, alloc::vec![1, 2, 3]);
        assert_eq!(b.depth(), 0, "the push and the pop inside the batch both ran");
    }

    #[test]
    fn a_long_batch_does_not_walk_the_stack() {
        // 8 KB of thread stack on the device; a recursive `execute` would not survive this, and
        // the failure on a handset is a reboot with nothing written down.
        let mut b = DeclarativeAppBridge::<Two>::new();
        let mut nested: Cmd<Screen> = Cmd::SetTimer { handle: 0, ms: 1 };
        for _ in 0..10_000 {
            nested = Cmd::Batch(alloc::vec![nested]);
        }
        b.execute(nested);
        assert_eq!(b.take_effects().len(), 1);
    }

    #[test]
    fn a_message_from_the_host_takes_the_same_path_as_a_key() {
        // A timer completion or a socket read is not a key, but it must not be a second way for
        // the model to change — same `update`, same invalidation.
        let mut b = DeclarativeAppBridge::<Two>::new();
        mirrored(&mut b);
        b.send(Msg::Loaded(42));
        assert_eq!(b.model().loaded, 42);
        assert_eq!(b.model().updates, 1);
        assert_eq!(b.model().views.get(), 1, "no view until someone asks for one");
        mirrored(&mut b);
        assert_eq!(b.model().views.get(), 2);
    }

    #[test]
    fn a_command_is_acted_on_once_and_not_again_on_the_next_frame() {
        // The classic MVU bug, and one a single-frame test cannot see: a command kept somewhere
        // and re-run each time the screen is rebuilt. On this app that is a timer armed once a
        // frame until the request queue fills, and a screen pushed again every time the user
        // looks at it — with a back stack that then takes eleven presses to unwind.
        let mut b = DeclarativeAppBridge::<Two>::new();
        b.send(Msg::Refresh);

        assert_eq!(b.depth(), 1);
        for _ in 0..10 {
            // Ten frames. `update` ran once, so the effect must have happened once.
            mirrored(&mut b);
        }
        assert_eq!(b.depth(), 1, "the push in that batch happened once, not once per frame");

        let timers = b.take_effects();
        assert_eq!(timers, alloc::vec![Cmd::SetTimer { handle: 9, ms: 1000 }]);

        // And once drained, later frames do not resurrect it — the queue is the only record and
        // the host now holds it.
        for _ in 0..10 {
            mirrored(&mut b);
        }
        assert!(b.take_effects().is_empty());
        assert_eq!(b.model().updates, 1, "frames must not re-run update either");
    }

    #[test]
    fn a_model_built_outside_is_never_a_second_init() {
        // The reason `with_model` exists at all, and the reason it does not build one and throw it
        // away: this app's live initialiser opens a connection and arms a timer. Running it for a
        // model nobody keeps is a socket nobody owns.
        use core::sync::atomic::{AtomicU32, Ordering};
        static INITS: AtomicU32 = AtomicU32::new(0);

        struct Counted;
        impl DeclarativeApp for Counted {
            type Model = i32;
            type Message = ();
            type Screen = ();
            const TITLE: &'static str = "Counted";
            fn init() -> i32 {
                INITS.fetch_add(1, Ordering::Relaxed);
                -1
            }
            fn update(_m: &mut i32, _msg: ()) -> Cmd {
                Cmd::None
            }
            fn view(_m: &i32, _slots: &mut SlotTable) -> Node {
                Node::leaf(Mirror(0))
            }
        }

        let given = DeclarativeAppBridge::<Counted>::with_model(7);
        assert_eq!(*given.model(), 7);
        assert_eq!(INITS.load(Ordering::Relaxed), 0, "with_model ran the app's initialiser");

        let defaulted = DeclarativeAppBridge::<Counted>::new();
        assert_eq!(*defaulted.model(), -1, "and the ordinary constructor still uses it");
        assert_eq!(INITS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn the_host_gets_the_title_without_an_instance_of_the_app() {
        let b = DeclarativeAppBridge::<Two>::new();
        assert_eq!(b.title(), "Two");
    }
}
