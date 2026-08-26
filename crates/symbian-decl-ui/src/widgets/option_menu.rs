//! The Options list that rises from the left softkey: [`symbian_ui::menu`] as a declarable layer.
//!
//! # One node, and the reason is what it occupies
//!
//! [`Select`](super::Select) is a *pair* — a field on a settings row plus a popup at the screen
//! level — because what it leaves behind when it closes is a visible control that has to sit inside
//! a row the list clips. This has no such half either, and for a sharper reason than
//! [`Dialog`](super::Dialog)'s: the thing left behind when an options menu closes is the word
//! **"Options" on the softkey bar**, and that word is already a widget with an owner. It is
//! [`Screen::on_options`](super::Screen::on_options), declared beside the message it fires, drawn
//! and dispatched from one [`Softkeys`](crate::keys::Softkeys) value.
//!
//! Handing this widget a second half to paint there would put two things in the bar's left slot: one
//! that knows the label and one that knows the message. That is the exact defect
//! [`Screen`](super::Screen)'s own doc comment was written about — a task manager whose middle label
//! read `Sort` and whose handler was bound to an event S60 never sends. So the field half is not
//! missing here, it is *taken*, and one node is the whole widget.
//!
//! # It goes over the screen, not inside it
//!
//! ```ignore
//! Stack::new(slots)
//!     .child(Screen::new()
//!         .title("Endereco")
//!         .content(page)
//!         .on_options("Opcoes", Msg::OpenMenu)     // raises it
//!         .out(out.clone()))
//!     .layer(OptionMenu::new(slots)
//!         .open(model.menu_open)
//!         .item("Recarregar",       Opt::Reload)
//!         .item("Ir para endereco", Opt::Goto)
//!         .out(out.clone(), Msg::Option)
//!         .on_cancel(out.clone(), Msg::CloseMenu)
//!         .build())
//! ```
//!
//! The [`Stack`](super::Stack) is the **root**, with the [`Screen`](super::Screen) as its first
//! layer and this as its last. Inside `Screen::content` instead, the panel is clipped to the content
//! band — and this menu is anchored to the *bottom* of the screen, sitting on top of the softkey bar
//! it rose from, so clipping it to the content band removes precisely the edge that makes it a menu.
//! [`Widget::overflow_visible`](crate::Widget::overflow_visible) is declared `true` here and buys
//! only this leaf's own clip; an ancestor that clips still clips, which is the wall the
//! [`select`](super::select) module documents. The misuse is detected and counted rather than
//! panicked: see [`OptionMenu::misplaced`].
//!
//! # Why `open` comes from the model
//!
//! Because the key that raises it cannot reach this widget. [`Screen::handle_key`](super::Screen)
//! offers the softkey bar every key **first and unconditionally** — see [`crate::keys`] for why that
//! is structural — and the left softkey is the bar's. A menu that opened itself on
//! `Softkey::Left` would work only on a screen with no "Options" label to press, which is a menu
//! nobody can find.
//!
//! So the bar raises it the way it raises anything: `.on_options("Opcoes", Msg::OpenMenu)` pushes a
//! message, `update` sets the flag, and the next `view` declares the layer open. That also settles
//! the second half of the S60 shape for free — while the menu is up the bar reads *Select / Cancel*
//! rather than *Options / Back*, and the bar is built from the model in `view`, so the model is the
//! one place that flag can live and still be read in time.
//!
//! The counter-argument [`select`](super::select) makes — that a flag in the model is `Msg::Open`
//! and `Msg::Close` in the application's enum for nothing — does not carry here, because those two
//! messages are not *for nothing*: they are the softkey bar changing. And the state `Select` could
//! not put in the model, a popup's scroll offset, has no counterpart: an options menu opens on its
//! first entry every time, which is S60's behaviour and [`Menu`]'s default.
//!
//! What does live in [`crate::slot`] is the highlight **while the menu is up**, because a view is
//! rebuilt every frame and a highlight rebuilt with it could never move: `Down` would advance it and
//! the next frame would put it back on the first entry.
//!
//! The layer is placed **unconditionally** all the same, exactly as `Select`'s popup is — slot
//! identity is positional, so a node that appears and disappears renumbers every slot after it and
//! reclaims the subtrees they named. Closed, this measures its band, paints nothing and ignores
//! every key.
//!
//! # How an open menu wins the keys
//!
//! [`Stack`](super::Stack) offers a key to its **last layer first**. This is the last layer, so while
//! it is open it is asked before the screen underneath — including before the screen's softkey bar,
//! which is what lets the *same* left softkey that opened the menu choose an entry in it. Open, it
//! consumes everything: `Up`/`Down` move, `Select`/`Enter`/the left softkey choose, the right softkey
//! and either horizontal arrow dismiss, and anything else is swallowed so a stray press cannot act
//! on the page behind a panel covering it.
//!
//! # No scrim, and that is [`symbian_ui::menu`]'s decision, not a shortcut
//!
//! A dialog dims what is behind it because the screen is not available until you answer. A menu is
//! dismissed with one key and the page underneath is still the subject — often the subject the verbs
//! act on. Dimming it would say otherwise. The panel earns its separation with an edge and a shadow.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::Cell;

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::menu::{Menu, MenuAction};
use symbian_ui::{Handled, KeyEvent, Theme};

use crate::constraints::Constraints;
use crate::outbox::Outbox;
use crate::slot::SlotTable;
use crate::widget::{hash_str, KeyCtx, Widget, WidgetHash};
use crate::widgets::Node;

/// Where a chosen verb goes, and how to name it once it gets there.
///
/// The alias [`Select`](super::Select) and [`Stepper`](super::Stepper) both keep, for the reason
/// they give: the pair is spelled several times and the tuple in a struct definition reads as
/// machinery rather than as the one thing it is.
type Report<M, T> = (Outbox<M>, fn(T) -> M);

/// Everything about a menu that outlives the frame that drew it.
///
/// `Copy`, so it can live in a `Cell` rather than a `RefCell` — the choice
/// [`FocusHook`](super::focus::FocusHook) documents: no borrow flag to get wrong and no runtime
/// panic path in a key dispatch, on a device whose whole failure report is a dialog with a number in
/// it. [`Dialog`](super::Dialog) could not have this because its cursor is a whole
/// [`Prompt`](symbian_ui::Prompt); a menu's is one index, because [`Menu`] scrolls nothing.
#[derive(Copy, Clone)]
struct MenuState {
    /// Which entry is highlighted, carried between frames.
    highlight: usize,
    /// Whether the previous frame saw it open, so the opening *edge* can be detected.
    ///
    /// An options menu opens on its first entry every time — S60's behaviour. Without an edge there
    /// is nothing to reset on: reading `open` alone would either never reset (the last verb chosen
    /// stays highlighted the next time the menu is raised) or reset on every frame (the highlight
    /// could never move).
    was_open: bool,
    /// Answers the widget had nowhere to send. See [`OptionMenu::stranded`].
    stranded: u32,
    /// Frames drawn open at a rect that was not the whole screen. See [`OptionMenu::misplaced`].
    misplaced: u32,
}

/// A menu of verbs anchored to the left softkey: reports the chosen one, owns nothing that matters.
///
/// `T` is what an entry *means* — usually a small enum of this screen's own, mapped into the
/// application's message by [`out`](OptionMenu::out). [`MenuAction`] carries the value registered
/// beside the label and never an index, which is the mistake [`symbian_ui::menu`] records: the first
/// caller hid an entry that did not apply and every index after it pointed at the wrong verb.
pub struct OptionMenu<M, T> {
    state: Rc<Cell<MenuState>>,
    /// Whether the menu is up, **as the model says it is**. This widget never writes it.
    open: bool,
    items: Vec<(String, T)>,
    out: Option<Report<M, T>>,
    cancel: Option<(Outbox<M>, M)>,
}

impl<M: Clone + 'static, T: Clone + 'static> OptionMenu<M, T> {
    /// An empty menu.
    ///
    /// Takes the slot table for the highlight, exactly as [`ScrollList`](super::ScrollList) takes it
    /// for a scroll offset. That is a positional slot: see [`crate::slot`] on what makes positional
    /// identity stable, and wrap a conditionally-built menu in
    /// [`SlotTable::group`](crate::slot::SlotTable::group).
    pub fn new(slots: &mut SlotTable) -> Self {
        let state = slots
            .use_state_with(|| {
                Rc::new(Cell::new(MenuState {
                    highlight: 0,
                    was_open: false,
                    stranded: 0,
                    misplaced: 0,
                }))
            })
            .clone();
        Self { state, open: false, items: Vec::new(), out: None, cancel: None }
    }

    /// Whether the menu is up. The model's answer, never this widget's.
    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    /// Add a verb: what it reads as, and what it means.
    ///
    /// One declaration for both halves. A caller that builds its entries conditionally — hiding
    /// "Paste" when the clipboard is empty — cannot shift what the remaining ones mean, because
    /// there is no index to shift.
    pub fn item(mut self, label: impl Into<String>, value: T) -> Self {
        self.items.push((label.into(), value));
        self
    }

    /// Where a chosen verb goes, and how to say it.
    ///
    /// A `fn` pointer and not [`Outbox::wrapped`](crate::outbox::Outbox::wrapped), for the reason
    /// [`Stepper`](super::Stepper) sets out at length: `wrapped` allocates an `Rc` and boxes a
    /// closure per call, and the call is in `view`, which runs every frame. A tuple-variant
    /// constructor coerces to `fn(T) -> M`, so `Msg::Option` is a complete argument.
    pub fn out(mut self, out: Outbox<M>, msg: fn(T) -> M) -> Self {
        self.out = Some((out, msg));
        self
    }

    /// What dismissing the menu means, and where to say it.
    ///
    /// A separate declaration from [`out`](Self::out), because dismissing is not choosing a verb —
    /// a menu whose Back pushed an entry would do whatever happened to be highlighted.
    ///
    /// Both halves together, the call [`Switch::out`](super::Switch::out) makes and for the same
    /// reason. Leaving it out leaves the menu with no way to close, because `open` is the model's:
    /// see [`OptionMenu::stranded`].
    pub fn on_cancel(mut self, out: Outbox<M>, msg: M) -> Self {
        self.cancel = Some((out, msg));
        self
    }

    /// How many verbs there are, for a caller that builds them conditionally.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Which entry is highlighted, readable while the tree is still being built.
    ///
    /// Syncs first, deliberately — the same reasoning [`Dialog::highlight`](super::Dialog::highlight)
    /// gives: `measure` has not run when a caller reads this, so a read that skipped the opening edge
    /// would report the previous session's highlight on the frame the menu is raised. Lazy rather
    /// than done in [`open`](Self::open), so the answer does not depend on the order the builder
    /// methods were called in.
    pub fn highlight(&self) -> usize {
        self.sync();
        self.state.get().highlight
    }

    /// Answers this menu had nowhere to send.
    ///
    /// Two misuses land here, and neither can be caught by a type:
    ///
    /// * A verb chosen with no [`out`](Self::out) declared — a menu where pressing an entry does
    ///   nothing.
    /// * A dismissal with no [`on_cancel`](Self::on_cancel) declared, which is the worse one.
    ///   `open` comes from the model, so nothing but a message can take the menu down; without a
    ///   cancel channel the right softkey does nothing at all and the panel is stuck over the page.
    ///
    /// Dropped and counted rather than panicked, which is [`OnKey::on`](super::OnKey::on)'s answer
    /// and [`Select::orphaned`](super::Select::orphaned)'s: a panic is a dead application on this
    /// hardware, and a menu that will not close is a bug you can survive long enough to read a
    /// counter.
    ///
    /// **Assert `stranded() == 0` in a test of any screen with an options menu on it.**
    pub fn stranded(&self) -> u32 {
        self.state.get().stranded
    }

    /// Frames this menu was drawn open at a rect smaller than the canvas.
    ///
    /// The structural misuse the module note describes: placed inside
    /// [`Screen::content`](super::Screen::content) rather than over the screen, the panel is clipped
    /// to the content band — and this panel is anchored to the *bottom* of the screen, so what gets
    /// clipped away is the edge that makes it read as a menu at all. Every key is still answered,
    /// which is what makes it hard to see.
    ///
    /// Counted rather than corrected, because the correction is not available from here: the clip is
    /// the ancestor's and this widget cannot undo it.
    ///
    /// **Assert `misplaced() == 0` in a test of any screen with an options menu on it.**
    pub fn misplaced(&self) -> u32 {
        self.state.get().misplaced
    }

    /// The layer. Place it as the **last layer of a root-level [`Stack`](super::Stack)**.
    pub fn build(self) -> Node {
        Node::leaf(self)
    }

    /// Reset the highlight on the frame the menu is raised, and forget it on the frame it closes.
    ///
    /// Called from `measure`, `draw`, `handle_key` and [`highlight`](Self::highlight) rather than
    /// from one of them, because none is guaranteed to run first: `measure` is skipped on a cache
    /// hit and a key can arrive before the first paint of a frame.
    fn sync(&self) {
        let mut s = self.state.get();
        if self.open && !s.was_open {
            s.highlight = 0;
            s.was_open = true;
            self.state.set(s);
        } else if !self.open && s.was_open {
            s.was_open = false;
            self.state.set(s);
        }
    }

    /// The [`Menu`] this frame's declaration describes, seeded with the highlight from the last one.
    ///
    /// Seeded through [`Menu::step`] and not a setter, because there is no setter: a fresh `Menu`
    /// starts on entry zero and `step(n)` from zero lands on `n`, wrapping if the model has since
    /// dropped entries out from under the highlight. That wrap is the behaviour wanted anyway — a
    /// highlight left pointing past the end of a shortened menu would choose nothing.
    fn menu(&self) -> Menu<T> {
        let mut m = Menu::new();
        for (label, value) in &self.items {
            m = m.item(label.clone(), value.clone());
        }
        m.step(self.state.get().highlight as i32);
        m
    }
}

impl<M: Clone + 'static, T: Clone + 'static> Widget for OptionMenu<M, T> {
    fn content_hash(&self) -> WidgetHash {
        // A constant, and nothing folded in — not the entries, not `open`. This node measures to
        // whatever it is offered whether it is showing anything or not, which is the answer
        // `SelectPopup` reaches and for the same reason: a layer that measured to its own content
        // would resize the layer it is in, and a `Stack` layer that resized would move the screen
        // underneath it the frame a menu appeared.
        //
        // Never zero. Zero means "re-measure me every frame", and `Stack::layer` propagates a
        // volatile child to the whole stack — so one zero here would put every screen carrying an
        // options menu on the slow path for ever.
        hash_str(0, "option-menu")
    }

    /// Everything it is offered.
    fn measure(&self, constraints: Constraints, _theme: &Theme<'_>) -> Size {
        self.sync();
        constraints.constrain(Size::new(constraints.max_w, constraints.max_h))
    }

    /// Declared, because the panel is anchored to the screen and its box is whatever the tree gave
    /// it.
    ///
    /// This buys only this leaf's own clip; an ancestor that clips still clips, which is why the
    /// module note insists on a root-level [`Stack`](super::Stack) and why [`OptionMenu::misplaced`]
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
        if rect != Rect::from_size(c.size()) {
            // Placed somewhere that is not the whole screen — see `OptionMenu::misplaced`. Drawn
            // anyway: a clipped menu is wrong and a missing one is worse, and the counter is what
            // says so out loud.
            let mut s = self.state.get();
            s.misplaced += 1;
            self.state.set(s);
        }
        // `rect` is deliberately not passed on. `Menu::draw` anchors to the canvas — bottom-left,
        // sitting on the softkey bar it rose from — and `Menu::panel` is the arithmetic that decides
        // where, which this widget exists not to write a second copy of.
        //
        // That is also why `CrossAlign::Stretch` has nothing to catch here: this widget measures
        // everything it is offered, so there is no smaller measurement for a stretch to exceed, and
        // the anchor comes from the canvas in either case. `the_stretch_a_layer_applies...` is the
        // test.
        self.menu().draw(c, theme);
    }

    fn handle_key(&self, ev: KeyEvent, _rect: Rect, _cx: &mut KeyCtx<'_>) -> Handled {
        self.sync();
        if !self.open {
            // Closed, the layer declines everything — which is what lets the screen underneath keep
            // working, and in particular what lets its softkey bar receive the press that raises
            // this menu in the first place.
            return Handled::Ignored;
        }
        let mut m = self.menu();
        let action = m.handle_key(ev);
        let mut s = self.state.get();
        s.highlight = m.selected();
        match action {
            MenuAction::Chosen(v) => match &self.out {
                Some((out, msg)) => out.push(msg(v)),
                None => s.stranded += 1,
            },
            MenuAction::Cancelled => match &self.cancel {
                Some((out, msg)) => out.push(msg.clone()),
                None => s.stranded += 1,
            },
            MenuAction::None => {}
        }
        self.state.set(s);
        // Consumed whatever it was, which is `menu::owns_keys`' rule rather than a new one: a key
        // that leaked past a panel covering the page would act on the page behind it.
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
    use core::cell::RefCell;
    use symbian_gfx::Size as GSize;
    use symbian_ui::{testing, Key, Palette, Softkey};

    /// The whole screen, which is where this layer belongs.
    const SCREEN: Rect = Rect { x0: 0, y0: 0, x1: 320, y1: 240 };

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Msg {
        Option(Opt),
        Close,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Opt {
        Reload,
        Goto,
        Home,
    }

    /// One menu over a slot table that survives frames, and the outbox it reports to.
    struct Rig {
        slots: SlotTable,
        out: Outbox<Msg>,
        open: bool,
        report: bool,
        cancel: bool,
    }

    impl Rig {
        fn new() -> Self {
            Self {
                slots: SlotTable::new(),
                out: Outbox::new(),
                open: false,
                report: true,
                cancel: true,
            }
        }

        fn build(&mut self) -> OptionMenu<Msg, Opt> {
            self.slots.begin_frame();
            let mut m = OptionMenu::<Msg, Opt>::new(&mut self.slots)
                .open(self.open)
                .item("Recarregar", Opt::Reload)
                .item("Ir para endereco", Opt::Goto)
                .item("Inicio", Opt::Home);
            if self.report {
                m = m.out(self.out.clone(), Msg::Option);
            }
            if self.cancel {
                m = m.on_cancel(self.out.clone(), Msg::Close);
            }
            m
        }

        /// One frame: build, place at the whole screen, press.
        fn press(&mut self, key: Key) -> Handled {
            let m = self.build();
            testing::with_theme(Palette::DARK, |theme| {
                let node = Node::leaf(m);
                let mut cache = UiCache::with_capacity(node.slot_count());
                layout::place_frame(&node, SCREEN, &mut cache, theme);
                with_key_ctx(|cx| {
                    layout::dispatch_key_node(&node, 0, KeyEvent::new(key), &cache, cx)
                })
            })
        }

        fn paint(&mut self) -> Vec<u16> {
            let m = self.build();
            with_real_theme(|theme| {
                let (_, buf) = testing::with_canvas(GSize::new(320, 240), |c| {
                    c.clear(theme.palette.bg.mid());
                    let node = Node::leaf(m);
                    let mut cache = UiCache::with_capacity(node.slot_count());
                    layout::draw_frame(&node, SCREEN, &mut cache, c, theme);
                });
                buf
            })
        }

        fn probe<R>(&mut self, f: impl FnOnce(&OptionMenu<Msg, Opt>) -> R) -> R {
            let m = self.build();
            f(&m)
        }

        fn highlight(&mut self) -> usize {
            self.probe(|m| m.highlight())
        }
    }

    /// The *real* device atlases, not the one-glyph test atlas.
    ///
    /// `testing::with_theme` loads an atlas holding exactly one glyph — lowercase 'a' — so
    /// "Recarregar" paints three letters of ten and "Inicio" paints nothing at all. A pixel test
    /// there sees the panel, its shadow and its selection band, and none of the words; see
    /// `the_real_atlas_paints_the_labels_so_the_pixel_tests_can_fail`.
    fn with_real_theme<R>(f: impl FnOnce(&Theme<'_>) -> R) -> R {
        let atlases = symbian_preview::Atlases::load();
        atlases.with_fonts(|fonts| f(&symbian_ui::Theme::dark(fonts)))
    }

    // ------------------------------------------------------------------ closed

    #[test]
    fn a_closed_menu_paints_nothing_at_all() {
        let mut rig = Rig::new();
        let bg = with_real_theme(|t| t.palette.bg.mid().to_rgb565().0);
        assert!(rig.paint().iter().all(|&p| p == bg), "a closed menu painted something");
        // The negative control, and it must fire: without it "nothing was painted" is satisfied by a
        // draw that never runs.
        rig.open = true;
        assert!(rig.paint().iter().any(|&p| p != bg), "an open menu painted nothing");
    }

    #[test]
    fn a_closed_menu_answers_no_key_whatsoever() {
        // What lets the screen underneath keep working — and in particular what lets its softkey bar
        // receive the press that raises this menu in the first place. A layer that ate
        // `Softkey::Left` while closed would be a menu that can never be opened.
        let mut rig = Rig::new();
        for key in [
            Key::Up,
            Key::Down,
            Key::Select,
            Key::Softkey(Softkey::Left),
            Key::Softkey(Softkey::Right),
            Key::Char('a'),
        ] {
            assert_eq!(rig.press(key), Handled::Ignored, "{key:?}");
        }
        assert!(rig.out.is_empty());
    }

    // ------------------------------------------------------------------ open

    #[test]
    fn an_open_menu_swallows_every_key_answered_or_not() {
        let mut rig = Rig::new();
        rig.open = true;
        for key in [Key::Up, Key::Down, Key::Char('x'), Key::Backspace] {
            assert_eq!(rig.press(key), Handled::Consumed, "{key:?}");
        }
        assert!(rig.out.is_empty(), "moving the highlight is not a choice");
    }

    #[test]
    fn the_highlight_moves_and_survives_the_rebuild_between_frames() {
        // The defect the slot table is here for. A view is rebuilt every frame, so the `Menu` is
        // built every frame; without carrying the highlight out of the old one `Down` advances it
        // and the next frame puts it straight back on the first entry.
        let mut rig = Rig::new();
        rig.open = true;
        assert_eq!(rig.highlight(), 0);
        rig.press(Key::Down);
        assert_eq!(rig.highlight(), 1, "the highlight did not move");
        rig.press(Key::Down);
        assert_eq!(rig.highlight(), 2, "the highlight was reset by the rebuild");
        // And it wraps, which is `Menu::step`'s decision: with three entries, "down" from the last
        // meaning nothing is a key press that does nothing, and a reader cannot tell that from a menu
        // that has stopped responding.
        rig.press(Key::Down);
        assert_eq!(rig.highlight(), 0);
    }

    #[test]
    fn choosing_reports_the_value_written_beside_the_label() {
        // The value, not the row it sat in. A caller that hides an entry which does not apply must
        // not shift what the others mean.
        let mut rig = Rig::new();
        rig.open = true;
        rig.press(Key::Down);
        assert_eq!(rig.press(Key::Select), Handled::Consumed);
        assert_eq!(rig.out.take(), alloc::vec![Msg::Option(Opt::Goto)]);
        // And the widget did not close itself: `open` is the model's, and this widget never writes
        // it.
        assert!(rig.open);
    }

    #[test]
    fn the_left_softkey_that_opened_it_also_chooses_in_it() {
        // S60's shape: the same physical key reads *Options* and then *Select*. It reaches this layer
        // rather than the bar because the layer is on top of the whole screen — see
        // `an_open_menu_takes_the_key_a_screens_softkey_bar_would_have_claimed`.
        let mut rig = Rig::new();
        rig.open = true;
        assert_eq!(rig.press(Key::Softkey(Softkey::Left)), Handled::Consumed);
        assert_eq!(rig.out.take(), alloc::vec![Msg::Option(Opt::Reload)]);
    }

    #[test]
    fn dismissing_never_reports_a_verb() {
        // A menu whose Back pushed an entry would do whatever happened to be highlighted. Three
        // keys, because a menu that rose from a corner is left by moving away from it — either
        // horizontal arrow — as well as by the right softkey.
        for key in [Key::Softkey(Softkey::Right), Key::Left, Key::Right] {
            let mut rig = Rig::new();
            rig.open = true;
            rig.press(Key::Down);
            assert_eq!(rig.press(key), Handled::Consumed, "{key:?}");
            let sent = rig.out.take();
            assert_eq!(sent, alloc::vec![Msg::Close], "{key:?}: {sent:?}");
            assert!(
                !sent.iter().any(|m| matches!(m, Msg::Option(_))),
                "{key:?} reported a verb"
            );
        }
    }

    #[test]
    fn a_menu_reopens_on_its_first_entry() {
        // S60's behaviour, and the reason the opening edge is tracked at all: a menu that reopened
        // on the last verb chosen would offer "Delete" pre-selected to a user who had just used it.
        let mut rig = Rig::new();
        rig.open = true;
        rig.press(Key::Down);
        rig.press(Key::Down);
        assert_eq!(rig.highlight(), 2);
        rig.open = false;
        assert_eq!(rig.highlight(), 2, "a closed menu is not asked where its highlight is");
        rig.open = true;
        assert_eq!(rig.highlight(), 0, "reopening did not go back to the first entry");
    }

    #[test]
    fn a_menu_with_no_channels_drops_its_answers_and_counts_them() {
        // The misuse the types cannot catch. Without a cancel channel the right softkey does nothing
        // at all and the panel is stuck over the page, because `open` is the model's and nothing but
        // a message can clear it.
        let mut rig = Rig::new();
        rig.open = true;
        rig.report = false;
        rig.cancel = false;
        assert_eq!(rig.press(Key::Select), Handled::Consumed, "it must still eat the key");
        assert_eq!(rig.press(Key::Softkey(Softkey::Right)), Handled::Consumed);
        assert!(rig.out.is_empty());
        assert_eq!(rig.probe(|m| m.stranded()), 2, "the refusals were not counted");
        // The negative control: with the channels declared, nothing is stranded.
        let mut ok = Rig::new();
        ok.open = true;
        ok.press(Key::Select);
        ok.press(Key::Softkey(Softkey::Right));
        assert_eq!(ok.probe(|m| m.stranded()), 0);
        assert_eq!(ok.out.take().len(), 2);
    }

    // ------------------------------------------------------------------ the stack

    /// A widget that consumes every key and records it — the screen underneath the menu.
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
        let menu = OptionMenu::<Msg, Opt>::new(slots)
            .open(open)
            .item("Recarregar", Opt::Reload)
            .out(out.clone(), Msg::Option)
            .on_cancel(out.clone(), Msg::Close)
            .build();
        let stack = Stack::new(slots).child(Taker(Rc::clone(seen))).layer(menu);
        testing::with_theme(Palette::DARK, |theme| {
            let mut clip = symbian_ui::NoClipboard;
            let mut cx = KeyCtx::new(theme, &mut clip);
            stack.handle_key(KeyEvent::new(key), SCREEN, &mut cx)
        })
    }

    #[test]
    fn an_open_menu_does_not_let_the_screen_underneath_answer() {
        // The property the layer design exists for, through a real `Stack`: the menu is the last
        // layer, `Stack` offers keys to the top layer first, so while it is open the screen
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
    fn an_open_menu_takes_the_key_a_screens_softkey_bar_would_have_claimed() {
        // The whole reason this is a layer over the screen rather than a widget inside it. `Screen`
        // offers its bar every key first and unconditionally, so the left softkey belongs to the bar
        // — which is how the menu gets raised. Once it is up, the layer in front takes the same key
        // and chooses with it, which is S60's *Options* → *Select*.
        let out = Outbox::new();
        let bar = Outbox::<Msg>::new();
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let screen = Screen::<Msg>::new()
            .title("Endereco")
            .on_options("Opcoes", Msg::Close)
            .out(bar.clone());
        let menu = OptionMenu::<Msg, Opt>::new(&mut slots)
            .open(true)
            .item("Recarregar", Opt::Reload)
            .out(out.clone(), Msg::Option)
            .on_cancel(out.clone(), Msg::Close)
            .build();
        let stack = Stack::new(&mut slots).child(screen).layer(menu);
        testing::with_theme(Palette::DARK, |theme| {
            let mut clip = symbian_ui::NoClipboard;
            let mut cx = KeyCtx::new(theme, &mut clip);
            assert_eq!(
                stack.handle_key(KeyEvent::new(Key::Softkey(Softkey::Left)), SCREEN, &mut cx),
                Handled::Consumed
            );
        });
        assert_eq!(out.take(), alloc::vec![Msg::Option(Opt::Reload)], "the menu did not answer");
        assert!(bar.is_empty(), "the screen's softkey bar took a press that landed on the menu");
    }

    #[test]
    fn a_closed_menu_leaves_the_left_softkey_to_the_bar_that_raises_it() {
        // The other half of the test above, and the one that would fail if this layer were greedy:
        // with the menu down, `Softkey::Left` must reach the screen's bar or nothing can ever open
        // it.
        let out = Outbox::new();
        let bar = Outbox::<Msg>::new();
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let screen = Screen::<Msg>::new().on_options("Opcoes", Msg::Close).out(bar.clone());
        let menu = OptionMenu::<Msg, Opt>::new(&mut slots)
            .open(false)
            .item("Recarregar", Opt::Reload)
            .out(out.clone(), Msg::Option)
            .on_cancel(out.clone(), Msg::Close)
            .build();
        let stack = Stack::new(&mut slots).child(screen).layer(menu);
        testing::with_theme(Palette::DARK, |theme| {
            let mut clip = symbian_ui::NoClipboard;
            let mut cx = KeyCtx::new(theme, &mut clip);
            stack.handle_key(KeyEvent::new(Key::Softkey(Softkey::Left)), SCREEN, &mut cx);
        });
        assert_eq!(bar.take(), alloc::vec![Msg::Close], "the closed layer ate the opening key");
        assert!(out.is_empty());
    }

    // ------------------------------------------------------------------ geometry

    #[test]
    fn a_menu_measures_the_band_it_was_offered_and_its_digest_is_not_zero() {
        // A layer that shrank to its content would move the screen underneath the frame a menu
        // appeared — the defect `Stack::measure` documents. And a zero digest propagates through
        // `Stack::layer` to the whole stack, putting every screen carrying a menu on the slow path.
        testing::with_theme(Palette::DARK, |theme| {
            let mut slots = SlotTable::new();
            slots.begin_frame();
            let m = OptionMenu::<Msg, Opt>::new(&mut slots).open(true).item("a", Opt::Reload);
            assert_eq!(m.measure(Constraints::tight(320, 240), theme), Size::new(320, 240));
            assert_ne!(m.content_hash(), 0);
            // Constant: the size does not depend on the entries, so folding them in would re-measure
            // the layer on every keystroke to produce the same number.
            slots.begin_frame();
            let other = OptionMenu::<Msg, Opt>::new(&mut slots).open(false);
            assert_eq!(m.content_hash(), other.content_hash());
        });
    }

    #[test]
    fn the_panel_is_anchored_to_the_bottom_left_of_the_canvas_and_not_to_its_rect() {
        // The two edges that make it a menu and not a dialog, seen from the canvas. Asserted against
        // `Menu::panel`, so this widget and the imperative one can never be two menus.
        with_real_theme(|theme| {
            let mut slots = SlotTable::new();
            slots.begin_frame();
            let m = OptionMenu::<Msg, Opt>::new(&mut slots)
                .open(true)
                .item("Recarregar", Opt::Reload)
                .item("Inicio", Opt::Home);
            let (_, px) = testing::with_canvas(GSize::new(320, 240), |c| {
                c.clear(theme.palette.bg.mid());
                m.draw(c, SCREEN, theme);
            });
            let bg = theme.palette.bg.mid().to_rgb565().0;
            let inked: Vec<i32> = (0..240)
                .filter(|&y| (0..320).any(|x| px[(y * 320 + x) as usize] != bg))
                .collect();
            assert!(!inked.is_empty(), "the panel drew nothing");

            let want = Menu::<Opt>::new()
                .item("Recarregar", Opt::Reload)
                .item("Inicio", Opt::Home)
                .panel(SCREEN, theme);
            assert!(want.y0 > SCREEN.y0, "a panel filling the screen would prove nothing");
            assert_eq!(*inked.first().unwrap(), want.y0, "the panel did not start where it should");
            // The shadow reaches two rows past the panel; nothing beyond that.
            assert!(
                *inked.last().unwrap() < want.y1 + 4,
                "ink below the panel and its shadow: {:?}",
                inked.last()
            );
            // And it sits on the softkey bar rather than at the very bottom of the screen.
            assert_eq!(want.y1, SCREEN.y1 - theme.metrics.softkey_h);
        });
    }

    #[test]
    fn the_stretch_a_layer_applies_changes_nothing_because_the_panel_is_anchored_to_the_canvas() {
        // The `CrossAlign::Stretch` trap, and the shape it takes here. This widget measures
        // everything it is offered, so there is no smaller measurement for a stretch to exceed — and
        // the anchor comes from `Menu::panel` against the canvas either way. So a rect taller,
        // shorter or narrower than the screen must produce identical ink.
        with_real_theme(|theme| {
            let paint = |rect: Rect| {
                let mut slots = SlotTable::new();
                slots.begin_frame();
                let m = OptionMenu::<Msg, Opt>::new(&mut slots)
                    .open(true)
                    .item("Recarregar", Opt::Reload);
                let (_, buf) = testing::with_canvas(GSize::new(320, 240), |c| {
                    c.clear(theme.palette.bg.mid());
                    m.draw(c, rect, theme);
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
    fn a_menu_drawn_at_anything_but_the_whole_screen_is_counted() {
        // The structural misuse: placed inside `Screen::content` rather than over the screen, the
        // panel is clipped to the content band — and what gets clipped away is the bottom edge, the
        // one that makes it read as a menu at all. Every key is still answered, which is what makes
        // it hard to see.
        with_real_theme(|theme| {
            let mut slots = SlotTable::new();
            slots.begin_frame();
            let m = OptionMenu::<Msg, Opt>::new(&mut slots).open(true).item("a", Opt::Reload);
            testing::with_canvas(GSize::new(320, 240), |c| {
                m.draw(c, Rect { x0: 0, y0: 20, x1: 320, y1: 200 }, theme);
            });
            assert_eq!(m.misplaced(), 1, "a band-sized menu went unnoticed");
            // The negative control: at the whole screen it counts nothing.
            testing::with_canvas(GSize::new(320, 240), |c| {
                m.draw(c, SCREEN, theme);
            });
            assert_eq!(m.misplaced(), 1, "the whole screen was counted as a misplacement");
        });
    }

    #[test]
    fn the_real_atlas_paints_the_labels_so_the_pixel_tests_can_fail() {
        // The negative control the pixel assertions in this file lean on. `testing::with_theme` loads
        // an atlas holding exactly one glyph — lowercase 'a' — so a menu drawn under it is a panel,
        // a shadow and a selection band with almost no text in it, and a width comparison there
        // would pass whatever `draw` did with the words.
        let paint = |theme: &Theme<'_>, label: &str| -> Vec<u16> {
            let mut slots = SlotTable::new();
            slots.begin_frame();
            let m = OptionMenu::<Msg, Opt>::new(&mut slots).open(true).item(label, Opt::Reload);
            let (_, px) = testing::with_canvas(GSize::new(320, 240), |c| {
                c.clear(theme.palette.bg.mid());
                m.draw(c, SCREEN, theme);
            });
            px
        };
        // Two labels of the same length, neither containing an 'a'. The length matters: every glyph
        // in the test atlas falls back to the same advance, so labels of different lengths would
        // produce panels of different widths there and the comparison would be about geometry
        // rather than about letters.
        with_real_theme(|t| {
            assert_ne!(paint(t, "Inicio"), paint(t, "Voltei"), "the real atlas painted no text");
        });
        // Under the test atlas the two panels are identical, because neither word has the one glyph
        // it holds — which is why the pixel assertions in this file run against the real fonts.
        testing::with_theme(Palette::DARK, |t| {
            assert_eq!(paint(t, "Inicio"), paint(t, "Voltei"), "the test atlas grew a font");
        });
    }

    #[test]
    fn one_menu_is_one_slot_and_the_layer_is_placed_whether_it_is_open_or_not() {
        // The reason the layer is unconditional. `crate::slot` identity is positional, so a node that
        // appears and disappears renumbers every slot after it and reclaims the subtrees they named
        // — a `ScrollList` two layers down would lose its scroll offset the frame a menu closed.
        let mut slots = SlotTable::new();
        for open in [false, true, false] {
            slots.begin_frame();
            let node = OptionMenu::<Msg, Opt>::new(&mut slots).open(open).build();
            assert_eq!(node.slot_count(), 1, "the layer's shape moved with `open`");
        }
    }
}
