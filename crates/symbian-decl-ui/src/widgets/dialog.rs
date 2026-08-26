//! A question over the whole screen: [`symbian_ui::Modal`] as a layer a `view` can declare.
//!
//! # One node, and the reason is what it occupies
//!
//! [`Select`](super::Select) is a *pair* — a field on a settings row and a popup at the screen
//! level — because the thing it leaves behind when it closes is a visible control that has to sit
//! inside a row the list clips. There is no such half here. A dialog closed occupies nothing at all;
//! a dialog open occupies the entire screen, scrim and softkey bar included. Nothing is left behind
//! on the page, so there is nothing to place in a row, so there is no second node to hand out and no
//! [`SelectParts`](super::SelectParts) to hand it out with.
//!
//! That is the argument, and it is worth stating that symmetry would have given the wrong answer:
//! `Select` is two nodes because a `ScrollList` row clips, not because overlays come in pairs.
//!
//! # It goes over the screen, not inside it
//!
//! ```ignore
//! Stack::new(slots)
//!     .child(Screen::new().title("Links").content(list).on_action("Abrir", Msg::Ask).out(out.clone()))
//!     .layer(Dialog::new(slots, "Abrir link", &model.url)
//!         .open(model.asking)
//!         .choice("Copiar e abrir", Link::CopyOpen)
//!         .choice("Apenas abrir",  Link::Open)
//!         .out(out.clone(), Msg::Link)
//!         .on_cancel(out.clone(), Msg::DismissLink)
//!         .build())
//! ```
//!
//! The [`Stack`](super::Stack) is the **root**, with the [`Screen`](super::Screen) as its first
//! layer and this as its last. Putting the dialog inside `Screen::content` instead looks equivalent
//! and is not, for two reasons that both bite:
//!
//! * **An ancestor that clips still clips.** [`Widget::overflow_visible`](crate::Widget::overflow_visible)
//!   is declared `true` here, and it only buys the leaf's own clip — `Screen` hands its content the
//!   content band, so a dialog under it would have its scrim cut off at the title bar and its own
//!   softkey bar cut off at the bottom. This is the same wall the [`select`](super::select) module
//!   documents, met by a widget that needs the *whole* screen rather than merely more than a row.
//! * **The screen's softkey bar is drawn after its content**, so the bar this dialog paints would be
//!   painted over by the screen's own the moment the screen has any label on it.
//!
//! Neither failure panics and neither is visible from a test of this file, which is why the misuse
//! is detected at runtime and counted: see [`Dialog::misplaced`].
//!
//! # Why `open` comes from the model and not from the slot table
//!
//! [`Select`](super::Select) keeps its open flag in [`crate::slot`] and argues the case at length.
//! The opposite answer is right here, and for three reasons rather than a preference:
//!
//! 1. **The question is made of the model.** "Apagar relatorio.pdf?" cannot be written without
//!    knowing which file. The application therefore already knows it is asking; a second copy of
//!    that fact in the slot table would be two answers to one question, and the one that drifts is
//!    the one the user is looking at.
//! 2. **Nothing raises it that this widget could hear.** A drop-down is opened by pressing the field,
//!    which is a key the field receives. A dialog is raised by a verb on the softkey bar — and
//!    [`Screen::handle_key`](super::Screen) offers the bar every key *first and unconditionally*, so
//!    the press that means "ask me" never reaches the content at all. A widget that opened itself on
//!    a key it can never be given is a widget that never opens.
//! 3. **There is no hidden geometry to preserve across a close.** The popup's scroll offset was the
//!    thing `Select` could not put in the model without lying about what application state is. A
//!    dialog opens on [`default_choice`](Dialog::default_choice) every time — S60's behaviour and
//!    [`Modal`]'s — so the cursor is *reset* on the way in and is not state that has to survive
//!    anything.
//!
//! What does live in the slot table is the cursor **while the dialog is up**, because a view is
//! rebuilt every frame and a cursor rebuilt with it would be pushed back to the default between
//! `Down` and `Select`. See [`Modal::cursor`](symbian_ui::Modal::cursor), which was added for this.
//!
//! And the layer is still placed **unconditionally**, exactly as `Select`'s popup is. Not for the
//! cursor's sake this time but for its neighbours': [`crate::slot`] identity is positional, so a
//! node that appears and disappears renumbers every slot after it and reclaims the subtrees they
//! named. Closed, this measures its band, paints nothing and ignores every key.
//!
//! # How an open dialog wins the keys
//!
//! [`Stack`](super::Stack) offers a key to its **last layer first**. This is the last layer, so while
//! it is open it is asked before the screen underneath — before the screen's softkey bar, which is
//! the one thing `Select` could not get in front of and had to warn about instead. Open, it consumes
//! *everything*, answered or not, which is [`Modal::handle_key`]'s own rule: a dialog covers a screen
//! the user can no longer see, so a key that leaked would act on something invisible.
//!
//! Closed, it declines everything, which is what leaves the screen underneath working.
//!
//! # Two channels, because a way out is not a choice
//!
//! [`Dialog::out`] carries an answer and [`Dialog::on_cancel`] carries the way out — the shape
//! [`Screen::on_action`](super::Screen::on_action) and [`Screen::on_back`](super::Screen::on_back)
//! already have. Cancelling never pushes a choice, which is the defect worth naming: a "Apagar?"
//! dialog that reported `Delete` on Back would delete the file the user just backed out of.
//!
//! `on_cancel` is not optional in practice, and the reason follows from `open` living in the model:
//! if nothing tells the application to stop asking, the dialog cannot close. That cannot be typed
//! away, so it is counted — see [`Dialog::stranded`].
//!
//! # What this costs per frame
//!
//! A [`Modal`] built from `String`s, once per frame. The [`select`](super::select) module refuses
//! exactly this cost for its options and takes `&'static [&'static str]` instead; the answer is
//! different here because the shapes are different. A drop-down's options are a fixed set repeated
//! on every settings row of a form; a dialog's *body* is the URL, the filename or the error the
//! question is about, which is model-derived by definition and cannot be `&'static` without leaking.
//! There is at most one dialog per screen and it is up for a moment.
//!
//! Closed — which is nearly always — the strings are still built, because a builder cannot know it
//! will be told `.open(false)` until after `.choice(..)` has been called. Making that cheap would
//! mean making the answer depend on the order the builder methods are called in, which is a worse
//! trap than an allocation this device can afford.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::{Answer, Handled, KeyEvent, Modal, Prompt, Theme};

use crate::constraints::Constraints;
use crate::outbox::Outbox;
use crate::slot::SlotTable;
use crate::widget::{hash_str, KeyCtx, Widget, WidgetHash};
use crate::widgets::Node;

/// Where an answer goes, and how to name it once it gets there.
///
/// The alias [`Select`](super::Select) and [`Stepper`](super::Stepper) both keep, for the reason
/// they give: the pair is spelled several times and `Option<(Outbox<M>, fn(T) -> M)>` in a struct
/// reads as machinery rather than as the one thing it is.
type Report<M, T> = (Outbox<M>, fn(T) -> M);

/// Everything about a dialog that outlives the frame that drew it.
///
/// Behind a `RefCell` and not a `Cell`, which is a departure from
/// [`FocusHook`](super::focus::FocusHook)'s reasoning and is forced rather than chosen: the cursor
/// is a whole [`Prompt`], which carries a [`ListState`](symbian_ui::ListState) and is not `Copy`.
/// Nothing here is re-entrant — the borrow is taken and dropped inside one method, and no
/// `symbian_ui` call reaches back into this widget — so the runtime borrow flag has no path to a
/// panic.
struct DialogState {
    /// The panel's cursor, carried between frames. See [`Modal::cursor`].
    cursor: Prompt,
    /// Whether the previous frame saw it open, so the opening *edge* can be detected.
    ///
    /// A dialog opens on its default choice every time — S60's behaviour — and without an edge there
    /// is nothing to reset on: reading `open` alone would either never reset (the cursor of the last
    /// question answers the next one) or reset on every frame (the cursor could never move).
    was_open: bool,
    /// Answers the widget had nowhere to send. See [`Dialog::stranded`].
    stranded: u32,
    /// Frames drawn open at a rect that was not the whole screen. See [`Dialog::misplaced`].
    misplaced: u32,
}

/// A modal question over the whole screen: reports the answer, owns nothing that matters.
///
/// `T` is what an answer *means* — usually a small enum of this dialog's own, mapped into the
/// application's message by [`out`](Dialog::out). It is cloned on the way out, so keep it small;
/// that is [`Modal`]'s advice and this widget does not change it.
pub struct Dialog<M, T> {
    state: Rc<RefCell<DialogState>>,
    /// Whether the question is being asked, **as the model says it is**. This widget never writes it.
    open: bool,
    title: String,
    body: String,
    choices: Vec<(String, T)>,
    default_choice: usize,
    action_label: Option<String>,
    back_label: Option<String>,
    out: Option<Report<M, T>>,
    cancel: Option<(Outbox<M>, M)>,
}

impl<M: Clone + 'static, T: Clone + 'static> Dialog<M, T> {
    /// A dialog asking `title` about `body`.
    ///
    /// The split is [`Modal::new`]'s and the trap it prevents is worth repeating here, because this
    /// is where a caller writes it: the body wraps and the title does not, so the long thing — the
    /// URL, the filename, the error — goes in the body. Put it in the title and the part that
    /// matters is the part that gets truncated.
    ///
    /// Takes the slot table for the cursor, exactly as [`ScrollList`](super::ScrollList) takes it for
    /// a scroll offset. That is a positional slot: see [`crate::slot`] on what makes positional
    /// identity stable, and wrap a conditionally-built dialog in
    /// [`SlotTable::group`](crate::slot::SlotTable::group).
    pub fn new(slots: &mut SlotTable, title: impl Into<String>, body: impl Into<String>) -> Self {
        let state = slots
            .use_state_with(|| {
                Rc::new(RefCell::new(DialogState {
                    cursor: Prompt::new(),
                    was_open: false,
                    stranded: 0,
                    misplaced: 0,
                }))
            })
            .clone();
        Self {
            state,
            open: false,
            title: title.into(),
            body: body.into(),
            choices: Vec::new(),
            default_choice: 0,
            action_label: None,
            back_label: None,
            out: None,
            cancel: None,
        }
    }

    /// Whether the question is being asked. The model's answer, never this widget's.
    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    /// Add a choice: what it reads as, and what it means.
    ///
    /// One declaration for both halves, which is the whole of [`symbian_ui::modal`]'s argument: the
    /// launcher kept labels in one list and meanings in another, and the list that drifted opened
    /// something when the user asked to copy.
    pub fn choice(mut self, label: impl Into<String>, value: T) -> Self {
        self.choices.push((label.into(), value));
        self
    }

    /// Where the cursor starts each time the dialog is raised.
    ///
    /// Each time, not once: the reset happens on the opening edge, so a question asked twice is
    /// asked the same way twice. A dialog that reopened on the previous answer would offer "Apagar"
    /// pre-selected to a user who had just cancelled it.
    pub fn default_choice(mut self, index: usize) -> Self {
        self.default_choice = index;
        self
    }

    /// Rename the middle softkey. It always chooses; only the word changes.
    pub fn action_label(mut self, label: impl Into<String>) -> Self {
        self.action_label = Some(label.into());
        self
    }

    /// Rename the right softkey.
    ///
    /// It cannot be removed — see [`Modal::back_label`]. The right softkey is the one key a user
    /// presses without reading, and a modal with no way out is a phone that needs restarting.
    pub fn back_label(mut self, label: impl Into<String>) -> Self {
        self.back_label = Some(label.into());
        self
    }

    /// Where a chosen answer goes, and how to say it.
    ///
    /// `msg` receives the value written beside the label. A `fn` pointer and not
    /// [`Outbox::wrapped`](crate::outbox::Outbox::wrapped), for the reason
    /// [`Stepper`](super::Stepper) sets out at length: `wrapped` allocates an `Rc` and boxes a
    /// closure per call, and the call is in `view`, which runs every frame. A tuple-variant
    /// constructor coerces to `fn(T) -> M`, so `Msg::Link` is a complete argument.
    pub fn out(mut self, out: Outbox<M>, msg: fn(T) -> M) -> Self {
        self.out = Some((out, msg));
        self
    }

    /// What backing out means, and where to say it.
    ///
    /// A separate declaration from [`out`](Self::out) and not a variant of it, because a way out is
    /// not an answer: a "Apagar relatorio.pdf?" whose Back pushed a choice would delete the file the
    /// user just escaped from.
    ///
    /// Both halves together, the call [`Switch::out`](super::Switch::out) makes and for the same
    /// reason — a message with nowhere to go is a press that consumes a key and fires nothing.
    ///
    /// Leaving this out leaves the dialog with no way to close, because `open` is the model's: see
    /// [`Dialog::stranded`].
    pub fn on_cancel(mut self, out: Outbox<M>, msg: M) -> Self {
        self.cancel = Some((out, msg));
        self
    }

    /// Answers this dialog had nowhere to send.
    ///
    /// Two misuses land here, and neither can be caught by a type:
    ///
    /// * A choice made with no [`out`](Self::out) declared.
    /// * A cancel with no [`on_cancel`](Self::on_cancel) declared — the worse of the two. `open`
    ///   comes from the model, so nothing but a message can close this dialog; without a cancel
    ///   channel the right softkey does nothing at all, and the user is looking at a screen they
    ///   cannot leave. [`Modal::back_label`] refuses to let the label be removed for exactly this
    ///   reason, and this is the same refusal one layer up.
    ///
    /// Dropped and counted rather than panicked, which is [`OnKey::on`](super::OnKey::on)'s answer
    /// and [`Select::orphaned`](super::Select::orphaned)'s: a panic is a dead application on this
    /// hardware, and a dialog that will not close is a bug you can survive long enough to read a
    /// counter.
    ///
    /// **Assert `stranded() == 0` in a test of any screen with a dialog on it.**
    pub fn stranded(&self) -> u32 {
        self.state.borrow().stranded
    }

    /// Frames this dialog was drawn open at a rect smaller than the canvas.
    ///
    /// The one structural misuse, and it is the module note's: a dialog placed inside
    /// [`Screen::content`](super::Screen::content) rather than over the screen is clipped to the
    /// content band, so the scrim stops at the title bar and this widget's softkey bar is cut off
    /// and then painted over by the screen's own. It still *works* — every key is answered — which
    /// is what makes it hard to see: it reads as a theme bug rather than as a tree assembled wrong.
    ///
    /// Counted rather than corrected, because the correction is not available from here: the clip is
    /// the ancestor's and this widget cannot undo it. See [`Widget::overflow_visible`](crate::Widget::overflow_visible).
    ///
    /// **Assert `misplaced() == 0` in a test of any screen with a dialog on it.**
    pub fn misplaced(&self) -> u32 {
        self.state.borrow().misplaced
    }

    /// Which choice the cursor is on, readable while the tree is still being built.
    ///
    /// The accessor [`Select::highlight`](super::Select::highlight) is, and for the same caller: a
    /// softkey label or a title decided outside the tree entirely.
    /// Syncs first, deliberately. The cursor a caller reads while building the tree must already be
    /// the one this frame will draw, and `measure` has not run yet at that point — so a read that
    /// skipped the edge would report the *previous* question's cursor on the frame a new one is
    /// raised. Lazy rather than done in [`open`](Self::open), so the answer does not depend on the
    /// order the builder methods were called in.
    pub fn highlight(&self) -> usize {
        self.sync();
        self.state.borrow().cursor.selected()
    }

    /// The layer. Place it as the **last layer of a root-level [`Stack`](super::Stack)**.
    pub fn build(self) -> Node {
        Node::leaf(self)
    }
}

impl<M: Clone + 'static, T: Clone + 'static> Dialog<M, T> {
    /// Reset the cursor on the frame the dialog is raised, and forget it on the frame it closes.
    ///
    /// Called from `measure`, `draw` and `handle_key` rather than from one of them, because none of
    /// the three is guaranteed to run first: `measure` is skipped on a cache hit and a key can
    /// arrive before the first paint of a frame. Between the three, the edge is seen before anything
    /// reads the cursor.
    fn sync(&self) {
        let mut s = self.state.borrow_mut();
        if self.open && !s.was_open {
            s.cursor = Prompt::new();
            s.cursor.select(self.default_choice);
            s.was_open = true;
        } else if !self.open && s.was_open {
            s.was_open = false;
        }
    }

    /// The [`Modal`] this frame's declaration describes, seeded with the cursor from the last one.
    fn modal(&self) -> Modal<T> {
        let mut m = Modal::new(self.title.clone(), self.body.clone());
        for (label, value) in &self.choices {
            m = m.choice(label.clone(), value.clone());
        }
        if let Some(l) = &self.action_label {
            m = m.action_label(l.clone());
        }
        if let Some(l) = &self.back_label {
            m = m.back_label(l.clone());
        }
        // After the labels and last of all: this is where the user actually is, and a default is
        // only where they started. See `Modal::with_cursor`.
        m.with_cursor(self.state.borrow().cursor.clone())
    }
}

impl<M: Clone + 'static, T: Clone + 'static> Widget for Dialog<M, T> {
    fn content_hash(&self) -> WidgetHash {
        // A constant, and nothing folded in — not the title, not the choices, not `open`. This node
        // measures to whatever it is offered whether it is showing anything or not, which is the
        // same answer `SelectPopup` reaches and for the same reason: a layer that measured to its
        // own content would resize the layer it is in, and a `Stack` layer that resized would move
        // the screen underneath it the frame a dialog appeared.
        //
        // Never zero. Zero means "re-measure me every frame", and `Stack::layer` propagates a
        // volatile child to the whole stack — so one zero here would put every screen carrying a
        // dialog on the slow path for ever.
        hash_str(0, "dialog")
    }

    /// Everything it is offered.
    fn measure(&self, constraints: Constraints, _theme: &Theme<'_>) -> Size {
        self.sync();
        constraints.constrain(Size::new(constraints.max_w, constraints.max_h))
    }

    /// Declared, because a dialog's ink is the whole screen and its box is whatever the tree gave it.
    ///
    /// This buys only this leaf's own clip; an ancestor that clips still clips, which is why the
    /// module note insists on a root-level [`Stack`](super::Stack) and why [`Dialog::misplaced`]
    /// exists to catch the screens that did not.
    fn overflow_visible(&self) -> bool {
        true
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        self.sync();
        if !self.open {
            // Closed, this node is a hole in the layer stack: no ink, so the screen underneath is
            // simply what the phone looks like. It is still measured and still placed, which is what
            // keeps every slot after it numbered where it was.
            return;
        }
        let screen = Rect::from_size(c.size());
        if rect != screen {
            // Placed somewhere that is not the whole screen — see `Dialog::misplaced`. Drawn anyway:
            // a clipped dialog is wrong and a missing one is worse, and the counter is what says so
            // out loud.
            self.state.borrow_mut().misplaced += 1;
        }
        // `rect` is deliberately not passed on. `Modal::draw` paints the canvas — the scrim over
        // everything and its own softkey bar along the bottom — and there is no rect-taking version
        // of it to call. Handing it a band would be reimplementing the panel's arithmetic here,
        // which is the one thing this widget exists not to do.
        //
        // That is also why `CrossAlign::Stretch` has nothing to catch here: this widget measures
        // everything it is offered, so there is no smaller measurement for a stretch to exceed, and
        // the panel's centring is `Prompt::draw`'s in either case. `the_stretch_a_layer_applies...`
        // is the test.
        let mut m = self.modal();
        m.draw(c, theme);
        // Written back because `Prompt::draw` records the row height and viewport it painted at, and
        // scrolling a dialog with more choices than fit is computed from those. Dropping the modal
        // here would scroll the next key against a one-pixel viewport.
        self.state.borrow_mut().cursor = m.cursor().clone();
    }

    fn handle_key(&self, ev: KeyEvent, _rect: Rect, _cx: &mut KeyCtx<'_>) -> Handled {
        self.sync();
        if !self.open {
            // Closed, the layer declines everything — which is what lets the screen underneath keep
            // working. A layer that consumed keys while invisible would be a phone where nothing
            // responds and nothing shows why.
            return Handled::Ignored;
        }
        let mut m = self.modal();
        let answer = m.handle_key(ev);
        self.state.borrow_mut().cursor = m.cursor().clone();
        match answer {
            Some(Answer::Chosen(v)) => match &self.out {
                Some((out, msg)) => out.push(msg(v)),
                None => self.state.borrow_mut().stranded += 1,
            },
            Some(Answer::Cancelled) => match &self.cancel {
                Some((out, msg)) => out.push(msg.clone()),
                None => self.state.borrow_mut().stranded += 1,
            },
            None => {}
        }
        // Consumed whatever it was. Modal means modal: a key that leaked past would move a screen
        // the user cannot see, behind a question they have not answered. That is
        // `Modal::handle_key`'s rule and `modal::owns_keys`' whole purpose, not a new one.
        Handled::Consumed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout;
    use crate::widget::with_key_ctx;
    use crate::widgets::{Screen, Stack};
    use crate::UiCache;
    use alloc::vec::Vec;
    use core::cell::Cell;
    use symbian_gfx::Size as GSize;
    use symbian_ui::{testing, Key, Palette, Softkey};

    /// The whole screen, which is where this layer belongs.
    const SCREEN: Rect = Rect { x0: 0, y0: 0, x1: 320, y1: 240 };

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Msg {
        Link(Link),
        Dismiss,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Link {
        CopyOpen,
        Open,
        Copy,
    }

    /// One dialog over a slot table that survives frames, and the outbox it reports to.
    struct Rig {
        slots: SlotTable,
        out: Outbox<Msg>,
        open: bool,
        cancel: bool,
        report: bool,
        title: &'static str,
        body: &'static str,
    }

    impl Rig {
        fn new() -> Self {
            Self {
                slots: SlotTable::new(),
                out: Outbox::new(),
                open: false,
                cancel: true,
                report: true,
                title: "Abrir link",
                body: "https://exemplo.com",
            }
        }

        fn build(&mut self) -> Dialog<Msg, Link> {
            self.slots.begin_frame();
            let mut d = Dialog::<Msg, Link>::new(&mut self.slots, self.title, self.body)
                .open(self.open)
                .choice("Copiar e abrir", Link::CopyOpen)
                .choice("Apenas abrir", Link::Open)
                .choice("Copiar link", Link::Copy);
            if self.report {
                d = d.out(self.out.clone(), Msg::Link);
            }
            if self.cancel {
                d = d.on_cancel(self.out.clone(), Msg::Dismiss);
            }
            d
        }

        /// One frame: build, place at the whole screen, press.
        fn press(&mut self, key: Key) -> Handled {
            let d = self.build();
            testing::with_theme(Palette::DARK, |theme| {
                let node = Node::leaf(d);
                let mut cache = UiCache::with_capacity(node.slot_count());
                layout::place_frame(&node, SCREEN, &mut cache, theme);
                with_key_ctx(|cx| {
                    layout::dispatch_key_node(&node, 0, KeyEvent::new(key), &cache, cx)
                })
            })
        }

        /// Paint one frame at the whole screen and return the buffer.
        fn paint(&mut self) -> Vec<u16> {
            let d = self.build();
            with_real_theme(|theme| {
                let (_, buf) = testing::with_canvas(GSize::new(320, 240), |c| {
                    c.clear(theme.palette.bg.mid());
                    let node = Node::leaf(d);
                    let mut cache = UiCache::with_capacity(node.slot_count());
                    layout::draw_frame(&node, SCREEN, &mut cache, c, theme);
                });
                buf
            })
        }

        /// The state, read the way a screen reads it: through a freshly built declaration.
        fn probe<R>(&mut self, f: impl FnOnce(&Dialog<Msg, Link>) -> R) -> R {
            let d = self.build();
            f(&d)
        }

        fn highlight(&mut self) -> usize {
            self.probe(|d| d.highlight())
        }
    }

    /// The *real* device atlases, not the one-glyph test atlas.
    ///
    /// `testing::with_theme` loads an atlas holding exactly one glyph — lowercase 'a' — so
    /// "Copiar e abrir" paints two letters of fourteen and "https://exemplo.com" paints nothing at
    /// all. Every pixel assertion below would be vacuously satisfied under it; see
    /// `the_real_atlas_paints_the_choices_so_the_pixel_tests_can_fail`.
    fn with_real_theme<R>(f: impl FnOnce(&Theme<'_>) -> R) -> R {
        let atlases = symbian_preview::Atlases::load();
        atlases.with_fonts(|fonts| f(&symbian_ui::Theme::dark(fonts)))
    }

    // ------------------------------------------------------------------ closed

    #[test]
    fn a_closed_dialog_paints_nothing_at_all() {
        let mut rig = Rig::new();
        let bg = with_real_theme(|t| t.palette.bg.mid().to_rgb565().0);
        let closed = rig.paint();
        assert!(closed.iter().all(|&p| p == bg), "a closed dialog painted something");
        // The negative control, and it must fire: without it "nothing was painted" is satisfied by a
        // draw that never runs.
        rig.open = true;
        let open = rig.paint();
        assert!(open.iter().any(|&p| p != bg), "an open dialog painted nothing");
    }

    #[test]
    fn a_closed_dialog_answers_no_key_whatsoever() {
        // What leaves the screen underneath working. A layer that consumed keys while invisible
        // would be a phone where nothing responds and nothing shows why.
        let mut rig = Rig::new();
        for key in [
            Key::Up,
            Key::Down,
            Key::Select,
            Key::Backspace,
            Key::End,
            Key::Softkey(Softkey::Right),
            Key::Char('a'),
        ] {
            assert_eq!(rig.press(key), Handled::Ignored, "{key:?}");
        }
        assert!(rig.out.is_empty());
    }

    // ------------------------------------------------------------------ open

    #[test]
    fn an_open_dialog_swallows_every_key_answered_or_not() {
        // Modal means modal. A key that leaked would move a screen the user cannot see, behind a
        // question they have not answered.
        let mut rig = Rig::new();
        rig.open = true;
        for key in [Key::Up, Key::Down, Key::Char('x'), Key::Left, Key::Right] {
            assert_eq!(rig.press(key), Handled::Consumed, "{key:?}");
        }
        assert!(rig.out.is_empty(), "moving the cursor is not an answer");
    }

    #[test]
    fn the_cursor_moves_and_survives_the_rebuild_between_frames() {
        // The defect `Modal::cursor` was added for. A view is rebuilt every frame, so the `Modal` is
        // built every frame; without carrying the cursor out of the old one the highlight is pushed
        // back to the default between `Down` and `Select`, and the dialog answers the first choice
        // whatever the user pointed at.
        let mut rig = Rig::new();
        rig.open = true;
        assert_eq!(rig.highlight(), 0);
        rig.press(Key::Down);
        assert_eq!(rig.highlight(), 1, "the cursor did not move");
        rig.press(Key::Down);
        assert_eq!(rig.highlight(), 2, "the cursor was reset by the rebuild");
    }

    #[test]
    fn choosing_reports_the_value_written_beside_the_label() {
        let mut rig = Rig::new();
        rig.open = true;
        rig.press(Key::Down);
        assert_eq!(rig.press(Key::Select), Handled::Consumed);
        assert_eq!(rig.out.take(), alloc::vec![Msg::Link(Link::Open)]);
        // And the widget did not close itself: `open` is the model's, and this widget never writes
        // it. A widget that closed its own copy would vanish for one frame and be rebuilt open by a
        // `view` made from the model that has not changed yet — a dialog that flickers.
        assert!(rig.open);
    }

    #[test]
    fn the_default_choice_is_where_the_cursor_starts_every_time_it_opens() {
        // Every time, not once. A dialog that reopened on the previous answer would offer the
        // destructive choice pre-selected to a user who had just cancelled it.
        let mut rig = Rig::new();
        rig.open = true;
        let d = {
            rig.slots.begin_frame();
            Dialog::<Msg, Link>::new(&mut rig.slots, "t", "b")
                .open(true)
                .default_choice(2)
                .choice("a", Link::CopyOpen)
                .choice("b", Link::Open)
                .choice("c", Link::Copy)
                .out(rig.out.clone(), Msg::Link)
                .on_cancel(rig.out.clone(), Msg::Dismiss)
        };
        assert_eq!(d.highlight(), 2, "it did not start on the default");
        // Move away, close, and open again: the cursor is back on the default.
        with_key_ctx(|cx| {
            d.handle_key(KeyEvent::new(Key::Up), SCREEN, cx);
        });
        assert_eq!(d.highlight(), 1, "the cursor did not move off the default");

        let closed = {
            rig.slots.begin_frame();
            Dialog::<Msg, Link>::new(&mut rig.slots, "t", "b").open(false).default_choice(2)
        };
        // A closed dialog is not asked where its cursor is: the reset happens on the way *in*, not
        // on the way out. Resetting on close would work equally well today and would be the wrong
        // rule, because it makes the answer depend on a frame having been drawn while closed — and a
        // dialog raised and re-raised inside one `update` never gets one.
        assert_eq!(closed.highlight(), 1, "closing moved the cursor");
        let reopened = {
            rig.slots.begin_frame();
            Dialog::<Msg, Link>::new(&mut rig.slots, "t", "b")
                .open(true)
                .default_choice(2)
                .choice("a", Link::CopyOpen)
                .choice("b", Link::Open)
                .choice("c", Link::Copy)
        };
        assert_eq!(reopened.highlight(), 2, "reopening did not go back to the default");
    }

    #[test]
    fn cancelling_never_reports_a_choice() {
        // The defect worth naming: a "Apagar relatorio.pdf?" whose Back pushed a choice would delete
        // the file the user just escaped from. Both keys, because the red key and the right softkey
        // both mean "get me out" on this hardware.
        for key in [Key::Softkey(Softkey::Right), Key::End] {
            let mut rig = Rig::new();
            rig.open = true;
            rig.press(Key::Down);
            assert_eq!(rig.press(key), Handled::Consumed, "{key:?}");
            let sent = rig.out.take();
            assert_eq!(sent, alloc::vec![Msg::Dismiss], "{key:?}: {sent:?}");
            assert!(
                !sent.iter().any(|m| matches!(m, Msg::Link(_))),
                "{key:?} reported a choice"
            );
        }
    }

    #[test]
    fn a_dialog_with_no_channels_drops_its_answers_and_counts_them() {
        // The misuse the types cannot catch. Without a cancel channel the right softkey does nothing
        // at all and the user is looking at a screen they cannot leave, because `open` is the
        // model's and nothing but a message can clear it.
        let mut rig = Rig::new();
        rig.open = true;
        rig.report = false;
        rig.cancel = false;
        assert_eq!(rig.press(Key::Select), Handled::Consumed, "it must still eat the key");
        assert_eq!(rig.press(Key::Softkey(Softkey::Right)), Handled::Consumed);
        assert!(rig.out.is_empty());
        assert_eq!(rig.probe(|d| d.stranded()), 2, "the refusals were not counted");
        // The negative control: with the channels declared, nothing is stranded.
        let mut ok = Rig::new();
        ok.open = true;
        ok.press(Key::Select);
        ok.press(Key::Softkey(Softkey::Right));
        assert_eq!(ok.probe(|d| d.stranded()), 0);
        assert_eq!(ok.out.take().len(), 2);
    }

    // ------------------------------------------------------------------ the stack

    /// A widget that consumes every key and records it — the screen underneath the dialog.
    struct Taker(Rc<RefCell<Vec<Key>>>);

    impl Widget for Taker {
        fn content_hash(&self) -> WidgetHash {
            hash_str(0, "taker")
        }
        fn measure(&self, c: Constraints, _t: &Theme<'_>) -> Size {
            c.constrain(Size::new(c.max_w, c.max_h))
        }
        fn draw(&self, _c: &mut Canvas<'_>, _r: Rect, _t: &Theme<'_>) {}
        fn handle_key(&self, ev: KeyEvent, _r: Rect, _cx: &mut KeyCtx<'_>) -> Handled {
            self.0.borrow_mut().push(ev.key);
            Handled::Consumed
        }
    }

    fn stack_press(
        slots: &mut SlotTable,
        seen: &Rc<RefCell<Vec<Key>>>,
        out: &Outbox<Msg>,
        open: bool,
        key: Key,
    ) -> Handled {
        slots.begin_frame();
        let dialog = Dialog::<Msg, Link>::new(slots, "Abrir link", "https://exemplo.com")
            .open(open)
            .choice("Apenas abrir", Link::Open)
            .out(out.clone(), Msg::Link)
            .on_cancel(out.clone(), Msg::Dismiss)
            .build();
        let stack = Stack::new(slots).child(Taker(Rc::clone(seen))).layer(dialog);
        testing::with_theme(Palette::DARK, |theme| {
            let mut clip = symbian_ui::NoClipboard;
            let mut cx = KeyCtx::new(theme, &mut clip);
            stack.handle_key(KeyEvent::new(key), SCREEN, &mut cx)
        })
    }

    #[test]
    fn an_open_dialog_does_not_let_the_screen_underneath_answer() {
        // The property the whole layer design exists for, through a real `Stack`: the dialog is the
        // last layer, `Stack` offers keys to the top layer first, so while it is open the screen
        // underneath never hears the D-pad.
        let seen = Rc::new(RefCell::new(Vec::<Key>::new()));
        let out = Outbox::new();
        let mut slots = SlotTable::new();

        // Closed: the negative control, and it must fire — without it "the layer underneath saw
        // nothing" is satisfied by a stack that never dispatches at all.
        assert_eq!(
            stack_press(&mut slots, &seen, &out, false, Key::Down),
            Handled::Consumed,
            "the layer underneath was never asked"
        );
        assert_eq!(&*seen.borrow(), &[Key::Down], "the control did not fire");
        seen.borrow_mut().clear();

        // Open: same stack, same key, and now nothing below hears it.
        assert_eq!(stack_press(&mut slots, &seen, &out, true, Key::Down), Handled::Consumed);
        assert!(seen.borrow().is_empty(), "the screen underneath answered: {:?}", seen.borrow());
    }

    #[test]
    fn an_open_dialog_takes_the_key_a_screens_softkey_bar_would_have_claimed() {
        // The thing `Select` could not do and had to warn about instead: `Screen` offers its bar
        // every key first and unconditionally, so a drop-down on a screen with a labelled action
        // slot never opens. A `Stack` puts this layer *in front of the whole screen*, bar included.
        let out = Outbox::new();
        let bar = Outbox::<Msg>::new();
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let screen = Screen::<Msg>::new()
            .title("Links")
            .on_action("Abrir", Msg::Dismiss)
            .out(bar.clone());
        let dialog = Dialog::<Msg, Link>::new(&mut slots, "Abrir link", "https://exemplo.com")
            .open(true)
            .choice("Apenas abrir", Link::Open)
            .out(out.clone(), Msg::Link)
            .on_cancel(out.clone(), Msg::Dismiss)
            .build();
        let stack = Stack::new(&mut slots).child(screen).layer(dialog);
        testing::with_theme(Palette::DARK, |theme| {
            let mut clip = symbian_ui::NoClipboard;
            let mut cx = KeyCtx::new(theme, &mut clip);
            assert_eq!(
                stack.handle_key(KeyEvent::new(Key::Select), SCREEN, &mut cx),
                Handled::Consumed
            );
        });
        assert_eq!(out.take(), alloc::vec![Msg::Link(Link::Open)], "the dialog did not answer");
        assert!(bar.is_empty(), "the screen's softkey bar took a press that landed on the dialog");
    }

    // ------------------------------------------------------------------ geometry

    #[test]
    fn a_dialog_measures_the_band_it_was_offered_and_its_digest_is_not_zero() {
        // A layer that shrank to its content would move the screen underneath the frame a dialog
        // appeared — the defect `Stack::measure` documents. And a zero digest propagates through
        // `Stack::layer` to the whole stack, putting every screen carrying a dialog on the slow path.
        testing::with_theme(Palette::DARK, |theme| {
            let mut slots = SlotTable::new();
            slots.begin_frame();
            let d = Dialog::<Msg, Link>::new(&mut slots, "t", "b").open(true);
            assert_eq!(d.measure(Constraints::tight(320, 240), theme), Size::new(320, 240));
            assert_ne!(d.content_hash(), 0);
            // Constant: the size does not depend on the question, so folding the question in would
            // re-measure the layer on every keystroke to produce the same number.
            slots.begin_frame();
            let other = Dialog::<Msg, Link>::new(&mut slots, "outro", "corpo").open(false);
            assert_eq!(d.content_hash(), other.content_hash());
        });
    }

    #[test]
    fn the_stretch_a_layer_applies_changes_nothing_because_the_panel_is_centred_in_the_canvas() {
        // The `CrossAlign::Stretch` trap, and the shape it takes here. This widget measures
        // everything it is offered, so there is no smaller measurement for a stretch to exceed — and
        // the panel's position comes from `Prompt::draw` centring in the canvas either way. So a
        // rect taller, shorter or narrower than the screen must produce identical ink.
        with_real_theme(|theme| {
            let paint = |rect: Rect| {
                let mut slots = SlotTable::new();
                slots.begin_frame();
                let d = Dialog::<Msg, Link>::new(&mut slots, "Abrir link", "https://exemplo.com")
                    .open(true)
                    .choice("Apenas abrir", Link::Open);
                let (_, buf) = testing::with_canvas(GSize::new(320, 240), |c| {
                    c.clear(theme.palette.bg.mid());
                    d.draw(c, rect, theme);
                });
                buf
            };
            let full = paint(SCREEN);
            let bg = theme.palette.bg.mid().to_rgb565().0;
            assert!(full.iter().any(|&p| p != bg), "nothing was painted at all");
            assert_eq!(full, paint(Rect { x0: 0, y0: 0, x1: 320, y1: 400 }), "a taller rect moved it");
            assert_eq!(full, paint(Rect { x0: 0, y0: 0, x1: 200, y1: 100 }), "a smaller rect moved it");
        });
    }

    #[test]
    fn a_dialog_drawn_at_anything_but_the_whole_screen_is_counted() {
        // The structural misuse: placed inside `Screen::content` rather than over the screen, the
        // scrim stops at the title bar and this widget's softkey bar is cut off and then painted
        // over by the screen's own. It still answers every key, which is what makes it hard to see.
        with_real_theme(|theme| {
            let mut slots = SlotTable::new();
            slots.begin_frame();
            let d = Dialog::<Msg, Link>::new(&mut slots, "t", "b")
                .open(true)
                .choice("a", Link::Open);
            testing::with_canvas(GSize::new(320, 240), |c| {
                d.draw(c, Rect { x0: 0, y0: 20, x1: 320, y1: 200 }, theme);
            });
            assert_eq!(d.misplaced(), 1, "a band-sized dialog went unnoticed");
            // The negative control: at the whole screen it counts nothing.
            testing::with_canvas(GSize::new(320, 240), |c| {
                d.draw(c, SCREEN, theme);
            });
            assert_eq!(d.misplaced(), 1, "the whole screen was counted as a misplacement");
        });
    }

    #[test]
    fn the_scrim_dims_the_screen_behind_rather_than_erasing_it() {
        // What makes it read as a dialog over a screen rather than as one damaged picture — the
        // defect `Modal::draw` records having shipped. Asserted here because the layer is what puts
        // the screen behind it, and a layer drawn in the wrong order would have nothing to dim.
        with_real_theme(|theme| {
            let red = symbian_gfx::Color::rgb(255, 0, 0);
            let mut slots = SlotTable::new();
            slots.begin_frame();
            let d = Dialog::<Msg, Link>::new(&mut slots, "Abrir link", "https://exemplo.com")
                .open(true)
                .choice("Apenas abrir", Link::Open);
            let (_, px) = testing::with_canvas(GSize::new(320, 240), |c| {
                c.fill_rect(SCREEN, red);
                d.draw(c, SCREEN, theme);
            });
            let corner = px[0];
            let full = red.to_rgb565().0;
            assert_ne!(corner, full, "the scrim did not dim anything");
            assert_ne!(corner, 0, "the screen behind was erased, not dimmed");
            assert!(corner >> 11 > 0, "the colour behind was lost: {corner:#06x}");
            assert!(corner >> 11 < full >> 11, "not actually darker");
        });
    }

    #[test]
    fn the_real_atlas_paints_the_choices_so_the_pixel_tests_can_fail() {
        // The negative control every pixel assertion in this file leans on. `testing::with_theme`
        // loads an atlas holding exactly one glyph — lowercase 'a' — so a dialog drawn under it is a
        // scrim, a panel and a selection band with almost no text in it, and a comparison there
        // would pass whatever `draw` did with the words.
        // Whole buffers rather than a count of inked pixels: the scrim touches every pixel on the
        // screen, so "how much ink is there" is dominated by the dimming and says nothing about the
        // words. It was written that way first and passed with the two answers inverted.
        let paint = |theme: &Theme<'_>, body: &str| -> Vec<u16> {
            let mut slots = SlotTable::new();
            slots.begin_frame();
            let d = Dialog::<Msg, Link>::new(&mut slots, "Titulo", body)
                .open(true)
                .choice("Apenas abrir", Link::Open);
            let (_, px) = testing::with_canvas(GSize::new(320, 240), |c| {
                c.clear(theme.palette.bg.mid());
                d.draw(c, SCREEN, theme);
            });
            px
        };
        // Two bodies of the same length, neither containing an 'a'. The length matters: every glyph
        // in the test atlas falls back to the same advance, so a longer body would produce a wider
        // panel there and the comparison would be about geometry rather than about letters.
        //
        // Under the real atlases the words reach the canvas, so these are two different pictures.
        with_real_theme(|t| {
            assert_ne!(paint(t, "memo"), paint(t, "erro"), "the real atlas painted no text");
        });
        // Under the test atlas they are the same blank panel of the same size, because neither word
        // has the one glyph it holds — which is why every pixel assertion in this file runs against
        // the real fonts instead.
        testing::with_theme(Palette::DARK, |t| {
            assert_eq!(paint(t, "memo"), paint(t, "erro"), "the test atlas grew a font");
        });
    }

    #[test]
    fn one_dialog_is_one_slot_and_the_layer_is_placed_whether_it_is_open_or_not() {
        // The reason the layer is unconditional. `crate::slot` identity is positional, so a node
        // that appears and disappears renumbers every slot after it and reclaims the subtrees they
        // named — a `ScrollList` two layers down would lose its scroll offset the frame a dialog
        // closed.
        let seen = Rc::new(Cell::new(0usize));
        let mut slots = SlotTable::new();
        for open in [false, true, false] {
            slots.begin_frame();
            let node = Dialog::<Msg, Link>::new(&mut slots, "t", "b").open(open).build();
            if seen.get() == 0 {
                seen.set(node.slot_count());
            }
            assert_eq!(node.slot_count(), seen.get(), "the layer's shape moved with `open`");
        }
        assert_eq!(seen.get(), 1);
    }
}
