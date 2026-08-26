//! The side drawer: which subject the application is on, and the way to another one.
//!
//! # Why it is a layer, and why the argument is short here
//!
//! [`DetailSheet`](super::DetailSheet) had to argue that an overlay covering *every pixel* is still
//! a [`Stack`](super::Stack) layer, because there the case is invisible: the screen underneath
//! contributes nothing to the frame, so only [`crate::slot`] settles it — a group not entered on a
//! frame is dropped with everything under it, so replacing the content would reclaim the list's
//! scroll offset and land the reader back at the top of a list they were forty rows into.
//!
//! That argument applies here unchanged, and here it is the *second* reason. The first is on the
//! screen: [`symbian_ui::Drawer::width`] is two thirds of the frame, and only of the content band —
//! the title bar and the softkey bar are not covered at all. The screen behind a drawer has to be
//! **painted**, not merely remembered, so there is nothing to decide. A drawer that replaced the
//! screen's content would leave the right-hand third blank and stop being a layer at the moment it
//! stopped looking like one; `symbian_ui::drawer` says that in as many words — a full-width drawer
//! is just another screen, and the visible strip is what tells you this is a place you are passing
//! through rather than one you have arrived at.
//!
//! The interesting half is that the two widgets reach the same answer from opposite ends. A partial
//! cover *needs* the layer to draw. A full cover only needs it to remember. If the second reason had
//! not held, a full-frame overlay would have been free to replace the content and this file would be
//! the only one still stacking.
//!
//! # Where it goes
//!
//! ```ignore
//! Stack::new(slots)
//!     .child(Screen::new().title("Boot").content(list))
//!     .child(Drawer::new(slots, m.section)
//!         .section(Section::new("Boot").note("4 at boot"))
//!         .section(Section::new("Packages").note("2 in the queue"))
//!         .section(Section::new("Settings"))
//!         .out(out.clone(), Msg::GoTo))
//! ```
//!
//! At the **frame level**, above the whole [`Screen`](super::Screen) rather than inside its content:
//! [`symbian_ui::Drawer`] carves the content band out of the frame with `Frame::split` itself, and
//! handed a band it would split that band again and paint its panel over the top third of a list.
//!
//! # The softkey it opens on, and the trap that comes with it
//!
//! `Softkey::Left` by default — the options slot, which is what the imperative drawer's own module
//! note says opens it, and the same key closes it again because that is what everybody tries first.
//!
//! Because this layer sits *above* the screen and [`Stack`](super::Stack) asks its last layer first,
//! the closed drawer takes that key before [`Screen::handle_key`](super::Screen) offers it to the
//! bar. So a screen carrying a drawer must leave [`Screen::on_options`](super::Screen) unlabelled:
//! the label would promise a message that can never be delivered, which is the defect `screen.rs`
//! exists to prevent — a bar saying one thing while the key does another. This is
//! [`Select`](super::Select)'s trap pointing the other way: there a labelled bar means the overlay
//! never opens, here an opening overlay means a labelled bar never fires.
//!
//! # No arithmetic of its own
//!
//! The panel width, the row height, the scrollbar and the marked current section all come from
//! [`symbian_ui::drawer`]. There is no [`Gap`](crate::spacing::Gap), no
//! [`Pad`](crate::spacing::Pad) and no `theme.metrics` lookup in this file, which is the stronger
//! form of the rule: the numbers are not merely named here, they are not here. Reimplementing
//! `width` would have been a second drawer, agreeing with the first on the day it was written.
//!
//! # What is kept between frames, and what is rebuilt
//!
//! The [`symbian_ui::Drawer`] itself lives in the slot table, because it holds the cursor and the
//! panel's own scroll and neither is application state. The [`Section`](symbian_ui::Section)s do
//! *not*: they are rebuilt from the model every frame and handed to `draw` and `handle_key` as
//! arguments, which is what lets a note read `3 in the queue` and mean it. That split is why this
//! widget needs no accessors on the imperative type, where
//! [`DetailSheet`](super::DetailSheet) needed [`symbian_ui::Sheet::cursor`]: a drawer's state and
//! its content were already separate.

use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::{drawer as ui, Handled, Key, KeyEvent, Softkey, Theme};

use crate::constraints::Constraints;
use crate::outbox::Outbox;
use crate::slot::SlotTable;
use crate::widget::{hash_str, KeyCtx, Widget, WidgetHash};

/// Where a chosen section goes, and how to name it once it gets there.
///
/// A `fn` pointer, for the reason [`Stepper`](super::Stepper) sets out at length:
/// [`Outbox::wrapped`](crate::outbox::Outbox) allocates an `Rc` and boxes a closure per call, and
/// the call is in `view`, which runs every frame.
type Report<M> = (Outbox<M>, fn(usize) -> M);

/// A side drawer of sections: reports where the user wants to go, owns nothing that matters.
pub struct Drawer<M> {
    /// The open drawer, or `None` for a closed one.
    ///
    /// An `Option` rather than a flag beside a permanent [`symbian_ui::Drawer`], because
    /// [`symbian_ui::Drawer::open`] is what puts the cursor on the section the application is in —
    /// and that is the one thing this widget must get right every time it opens, from a `current`
    /// that only the *new* frame's model knows. A kept drawer would need a setter for it and would
    /// still be showing the last visit's cursor if anyone forgot to call it.
    state: Rc<RefCell<Option<ui::Drawer>>>,
    /// This frame's sections, rebuilt from the model. Passed to `draw` and `handle_key` rather than
    /// stored, which is [`symbian_ui::Drawer`]'s own shape.
    sections: Vec<ui::Section>,
    /// Where the application is *now*, read only at the moment the drawer opens.
    current: usize,
    open_key: Key,
    out: Option<Report<M>>,
}

impl<M: 'static> Drawer<M> {
    /// A closed drawer that will open on the section the application is in.
    ///
    /// Takes the slot table for the open drawer, exactly as [`ScrollList`](super::ScrollList) takes
    /// it for a scroll offset — and, like that one, this is a positional slot: wrap a conditional
    /// drawer in [`SlotTable::group`](crate::slot::SlotTable::group) or the calls after it shift by
    /// one the frame the condition flips.
    ///
    /// An out-of-range `current` is not corrected here. [`symbian_ui::Drawer`] marks nothing and
    /// draws normally, and clamping in the constructor would mean this widget disagreed with the
    /// model about what the model said — [`Select::new`](super::Select::new)'s rule.
    pub fn new(slots: &mut SlotTable, current: usize) -> Self {
        let state = slots.use_state_with(|| Rc::new(RefCell::new(None::<ui::Drawer>))).clone();
        Self {
            state,
            sections: Vec::new(),
            current,
            open_key: Key::Softkey(Softkey::Left),
            out: None,
        }
    }

    /// One place the application can be.
    pub fn section(mut self, section: ui::Section) -> Self {
        self.sections.push(section);
        self
    }

    /// Several, which is how they usually arrive — built from the model with their notes filled in.
    pub fn sections(mut self, sections: impl IntoIterator<Item = ui::Section>) -> Self {
        self.sections.extend(sections);
        self
    }

    /// The key that opens the drawer while it is closed.
    ///
    /// The left softkey by default: the SDK's own convention already calls that the options slot,
    /// and it is the key the imperative drawer answers to for closing. Worth changing only for a
    /// screen whose options key already means something the drawer is not.
    pub fn opens_on(mut self, key: Key) -> Self {
        self.open_key = key;
        self
    }

    /// Where a chosen section goes, and how to say it.
    ///
    /// `msg` receives the section's index in declaration order. Dismissing reports nothing, for the
    /// reason [`Select`](super::Select) reports nothing on cancel: a message meaning "the user
    /// looked and left" is an `update`, a [`Cmd`](crate::cmd::Cmd) and a repaint for a model that
    /// did not move. A drawer that reported its own dismissal would do that on every glance.
    pub fn out(mut self, out: Outbox<M>, msg: fn(usize) -> M) -> Self {
        self.out = Some((out, msg));
        self
    }

    /// Whether the panel is showing, readable while the tree is still being built.
    ///
    /// Not needed to place it — the layer is unconditional. It is here for the caller
    /// [`FocusStops`](super::FocusStops) exists for: a screen whose title has to say something else
    /// while a navigator is over it, decided outside the tree entirely.
    pub fn is_open(&self) -> bool {
        self.state.borrow().is_some()
    }

    /// Which section the cursor is on, or `None` while it is closed.
    pub fn selected(&self) -> Option<usize> {
        self.state.borrow().as_ref().map(ui::Drawer::selected)
    }

    /// Close the panel and put the message for `action` in the outbox, if there is one.
    ///
    /// One place for both outcomes, because they differ only in whether anything is reported and
    /// writing that twice is how a `Went` that forgot to close gets written.
    fn settle(&self, action: ui::DrawerAction) {
        match action {
            ui::DrawerAction::Went(i) => {
                *self.state.borrow_mut() = None;
                if let Some((out, msg)) = &self.out {
                    out.push(msg(i));
                }
            }
            // Both close the panel, and here that is all either can do: this layer sits inside a
            // screen, so "one level up" from it is the screen — which already owns Back through its
            // softkey bar. The distinction matters to a host that stacks *areas*; see
            // `symbian_ui::DrawerAction::WentUp`.
            ui::DrawerAction::Dismissed | ui::DrawerAction::WentUp => {
                *self.state.borrow_mut() = None
            }
            ui::DrawerAction::None => {}
        }
    }
}

impl<M: 'static> Widget for Drawer<M> {
    /// A constant, and nothing about the sections folded in.
    ///
    /// This layer measures to whatever it is offered whether it is showing anything or not, so its
    /// size is a function of the offer alone — and the cache keys every entry on the offer as well
    /// as on this digest (see `cache::Entry::offer`), which is what makes a constant honest here
    /// rather than merely cheap. The sections decide what is *painted* and how wide the panel is
    /// *inside* this rect; neither changes the rect. Folding a note in would re-measure the whole
    /// frame every time a queue count ticked, to produce the same number.
    ///
    /// Never zero. Zero means "re-measure me every frame", and a volatile child makes its whole
    /// ancestry volatile — [`Stack::layer`](super::Stack::layer) says so — so one zero here would
    /// put every screen carrying a drawer on the slow path for ever.
    fn content_hash(&self) -> WidgetHash {
        hash_str(0, "drawer")
    }

    /// Everything it is offered, exactly as a [`Stack`](super::Stack) layer does.
    ///
    /// Not the panel's width. A layer that measured its own two thirds would be placed in two thirds
    /// of the frame and would then split *that* into a further two thirds — and the screen behind it
    /// would never be asked for a key at a rect the panel actually occupies.
    fn measure(&self, constraints: Constraints, _theme: &Theme<'_>) -> Size {
        constraints.constrain(Size::new(constraints.max_w, constraints.max_h))
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        // `try_borrow_mut` for the reason `Imperative` gives: a panic inside `draw` is the
        // application gone on a device whose whole failure report is a dialog with a number in it,
        // and a frame that did not paint is a frame that did not paint.
        let Ok(mut state) = self.state.try_borrow_mut() else { return };
        // Closed, this layer is a hole in the stack: no ink, so the screen underneath is simply what
        // the phone looks like. It is still measured and still placed, which is what keeps that
        // screen's scroll offset alive across the close.
        let Some(drawer) = state.as_mut() else { return };
        // `rect`, never the canvas: `Drawer::draw` re-splits whatever rectangle it is handed, which
        // is what makes this survive being placed taller than it measured. A layer placed at a rect
        // it did not measure moves its panel with the rect instead of painting at a y it remembered.
        drawer.draw(c, rect, theme, &self.sections);
    }

    fn handle_key(&self, ev: KeyEvent, rect: Rect, cx: &mut KeyCtx<'_>) -> Handled {
        let action = {
            let Ok(mut state) = self.state.try_borrow_mut() else { return Handled::Ignored };
            match state.as_mut() {
                None => {
                    if ev.key == self.open_key {
                        // The cursor goes on the section the application is in, from *this* frame's
                        // model — the difference between a navigator and a list, and the reason the
                        // drawer is created here rather than kept and reset.
                        *state = Some(ui::Drawer::open(self.current));
                        return Handled::Consumed;
                    }
                    // Everything else falls through, which is what lets the screen underneath keep
                    // its navigation. A closed layer that consumed `Down` would be a screen where
                    // nothing works and nothing on it shows why.
                    return Handled::Ignored;
                }
                Some(drawer) => drawer.handle_key(ev, &self.sections, cx.theme, rect).1,
            }
        };
        // Outside the borrow: `settle` takes the same `RefCell`, and a `borrow_mut` held across it
        // is the one way this widget can panic in a key dispatch.
        self.settle(action);
        // Consumed whatever the imperative drawer answered — it is modal by its own rule, and
        // nothing behind it may act while it is open or the screen would answer a question nobody
        // asked.
        Handled::Consumed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout;
    use crate::widget::{hash_i32, with_key_ctx};
    use crate::widgets::{Node, Stack};
    use crate::UiCache;
    use core::cell::RefCell as StdRefCell;
    use symbian_gfx::{Color, Size as GSize};
    use symbian_ui::{chrome, testing, Palette};

    /// The device's frame, which is the only rect this widget is meant to be placed at.
    const SCREEN: Rect = Rect { x0: 0, y0: 0, x1: 320, y1: 240 };

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Msg {
        GoTo(usize),
    }

    /// One drawer over a slot table that survives frames, plus the outbox it reports to.
    struct Rig {
        slots: SlotTable,
        out: Outbox<Msg>,
        current: usize,
        /// The note on the second section, which a real screen rebuilds from its model every frame.
        queue: &'static str,
    }

    impl Rig {
        fn new(current: usize) -> Self {
            Self { slots: SlotTable::new(), out: Outbox::new(), current, queue: "2 in the queue" }
        }

        fn build(&mut self) -> Drawer<Msg> {
            self.slots.begin_frame();
            Drawer::<Msg>::new(&mut self.slots, self.current)
                .section(ui::Section::new("Boot").note("4 at boot"))
                .section(ui::Section::new("Packages").note(self.queue))
                .section(ui::Section::new("Settings"))
                .out(self.out.clone(), Msg::GoTo)
        }

        fn press(&mut self, key: Key) -> Handled {
            let w = self.build();
            testing::with_theme(Palette::DARK, |theme| {
                let mut clip = symbian_ui::NoClipboard;
                let mut cx = KeyCtx::new(theme, &mut clip);
                w.handle_key(KeyEvent::new(key), SCREEN, &mut cx)
            })
        }

        fn is_open(&mut self) -> bool {
            self.build().is_open()
        }

        fn selected(&mut self) -> Option<usize> {
            self.build().selected()
        }
    }

    /// The real device atlases, not the one-glyph test atlas. See
    /// `the_real_atlas_paints_the_sections_so_the_pixel_tests_above_can_fail`.
    fn with_real_theme<R>(f: impl FnOnce(&Theme<'_>) -> R) -> R {
        let atlases = symbian_preview::Atlases::load();
        atlases.with_fonts(|fonts| f(&symbian_ui::Theme::dark(fonts)))
    }

    /// Paint one frame of the drawer into `rect`, over a background of `under`.
    fn paint(rig: &mut Rig, theme: &Theme<'_>, rect: Rect, under: Color) -> Vec<u16> {
        let widget = rig.build();
        testing::with_canvas(GSize::new(320, 240), |c| {
            c.clear(under);
            widget.draw(c, rect, theme);
        })
        .1
    }

    fn at(buf: &[u16], x: i32, y: i32) -> u16 {
        buf[(y * 320 + x) as usize]
    }

    // ------------------------------------------------------------------ closed

    #[test]
    fn a_closed_drawer_paints_nothing_and_leaves_the_screen_behind_its_keys() {
        let under = Color::hex(0x00FF00);
        with_real_theme(|theme| {
            let mut rig = Rig::new(0);
            let closed = paint(&mut rig, theme, SCREEN, under);
            assert!(
                closed.iter().all(|&p| p == under.to_rgb565().0),
                "a closed drawer painted something"
            );
            // The negative control, and it must fire: without it "the closed drawer painted nothing"
            // is satisfied by a widget whose `draw` is empty in both states.
            rig.press(Key::Softkey(Softkey::Left));
            let open = paint(&mut rig, theme, SCREEN, under);
            assert!(
                open.iter().any(|&p| p != under.to_rgb565().0),
                "an open drawer painted nothing"
            );
        });

        let mut rig = Rig::new(0);
        for key in [Key::Up, Key::Down, Key::Left, Key::Right, Key::Select, Key::Char('a')] {
            assert_eq!(rig.press(key), Handled::Ignored, "{key:?}");
            assert!(!rig.is_open(), "{key:?} opened it");
        }
        assert!(rig.out.is_empty());
    }

    #[test]
    fn the_open_key_is_the_one_the_caller_named() {
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let w = Drawer::<Msg>::new(&mut slots, 0)
            .section(ui::Section::new("Boot"))
            .opens_on(Key::Char('m'));
        with_key_ctx(|cx| {
            let bar = KeyEvent::new(Key::Softkey(Softkey::Left));
            assert_eq!(w.handle_key(bar, SCREEN, cx), Handled::Ignored);
            assert!(!w.is_open(), "the default opener still fired after being replaced");
            assert_eq!(w.handle_key(KeyEvent::new(Key::Char('m')), SCREEN, cx), Handled::Consumed);
            assert!(w.is_open());
        });
    }

    // ------------------------------------------------------------------ open

    #[test]
    fn it_opens_on_the_section_the_application_is_in() {
        // The first thing somebody wants to know when they open a navigator is where they are — and
        // the reason the imperative drawer is created at the moment of opening rather than kept.
        let mut rig = Rig::new(2);
        assert_eq!(rig.press(Key::Softkey(Softkey::Left)), Handled::Consumed);
        assert_eq!(rig.selected(), Some(2));
        assert!(rig.out.is_empty(), "opening is not a move");
        // And a later frame that moved the application elsewhere opens there instead, which a kept
        // drawer with a stale `current` would get wrong.
        rig.press(Key::Left);
        rig.current = 0;
        rig.press(Key::Softkey(Softkey::Left));
        assert_eq!(rig.selected(), Some(0));
    }

    #[test]
    fn the_key_that_opened_it_closes_it_again() {
        // Everybody tries that first, and a drawer that ignored its own key is one people learn to
        // distrust.
        let mut rig = Rig::new(0);
        rig.press(Key::Softkey(Softkey::Left));
        assert_eq!(rig.press(Key::Softkey(Softkey::Left)), Handled::Consumed);
        assert!(!rig.is_open());
        assert!(rig.out.is_empty(), "closing reported a move");
    }

    #[test]
    fn the_arrows_move_the_cursor_without_going_anywhere() {
        let mut rig = Rig::new(0);
        rig.press(Key::Softkey(Softkey::Left));
        assert_eq!(rig.press(Key::Down), Handled::Consumed);
        assert_eq!(rig.selected(), Some(1));
        assert_eq!(rig.press(Key::Up), Handled::Consumed);
        assert_eq!(rig.selected(), Some(0));
        assert!(rig.out.is_empty(), "an arrow ran `update` and whatever `Cmd` it returned");
        assert!(rig.is_open());
    }

    #[test]
    fn going_to_a_section_reports_its_index_and_closes_the_drawer() {
        for key in [Key::Select, Key::Right] {
            let mut rig = Rig::new(0);
            rig.press(Key::Softkey(Softkey::Left));
            rig.press(Key::Down);
            assert_eq!(rig.press(key), Handled::Consumed, "{key:?}");
            assert_eq!(rig.out.take(), alloc::vec![Msg::GoTo(1)], "{key:?}");
            assert!(!rig.is_open(), "{key:?} left the navigator up over where it navigated to");
        }
    }

    #[test]
    fn left_dismisses_it_without_reporting_anything() {
        // The gesture a thumb finds on a left-hand panel without being told.
        let mut rig = Rig::new(1);
        rig.press(Key::Softkey(Softkey::Left));
        rig.press(Key::Down);
        assert_eq!(rig.press(Key::Left), Handled::Consumed);
        assert!(!rig.is_open());
        assert!(rig.out.is_empty(), "backing out of a navigator moved the application");
    }

    #[test]
    fn a_note_rebuilt_from_the_model_reaches_the_open_panel() {
        // The sections are arguments, not state — which is what lets `3 in the queue` mean it.
        with_real_theme(|theme| {
            let mut rig = Rig::new(0);
            rig.press(Key::Softkey(Softkey::Left));
            let two = paint(&mut rig, theme, SCREEN, theme.palette.bg.mid());
            rig.queue = "9 in the queue";
            let nine = paint(&mut rig, theme, SCREEN, theme.palette.bg.mid());
            assert_ne!(two, nine, "the panel is drawing a note it captured when it opened");
        });
    }

    // ------------------------------------------------------------------ the stack

    /// A widget that consumes every key and records it — the screen underneath the drawer.
    struct Taker(Rc<StdRefCell<Vec<Key>>>);

    impl Widget for Taker {
        fn content_hash(&self) -> WidgetHash {
            hash_i32(0, 0x7A_4D)
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

    /// One frame of `Stack { Taker, drawer }` — the shape a screen has, with the screen underneath
    /// standing in for a whole `Screen` — pressed at `key`.
    fn stack_press(slots: &mut SlotTable, seen: &Rc<StdRefCell<Vec<Key>>>, key: Key) -> Handled {
        slots.begin_frame();
        let drawer = Drawer::<Msg>::new(slots, 0)
            .section(ui::Section::new("Boot"))
            .section(ui::Section::new("Packages"));
        let stack = Stack::new(slots).child(Taker(Rc::clone(seen))).child(drawer);
        testing::with_theme(Palette::DARK, |theme| {
            let mut clip = symbian_ui::NoClipboard;
            let mut cx = KeyCtx::new(theme, &mut clip);
            stack.handle_key(KeyEvent::new(key), SCREEN, &mut cx)
        })
    }

    #[test]
    fn a_closed_drawer_takes_the_options_key_before_anything_under_it_in_the_stack() {
        // The inverted softkey trap: a `Screen` under this layer never hears the options key, so a
        // screen carrying a drawer must leave that slot unlabelled or its bar promises a message
        // that cannot be delivered.
        let seen = Rc::new(StdRefCell::new(Vec::<Key>::new()));
        let mut slots = SlotTable::new();

        // The negative control first, and it must fire — without it "the layer underneath saw
        // nothing" is satisfied by a stack that never dispatches at all.
        assert_eq!(stack_press(&mut slots, &seen, Key::Down), Handled::Consumed);
        assert_eq!(&*seen.borrow(), &[Key::Down], "the control did not fire");
        seen.borrow_mut().clear();

        let opts = Key::Softkey(Softkey::Left);
        assert_eq!(stack_press(&mut slots, &seen, opts), Handled::Consumed);
        assert!(seen.borrow().is_empty(), "the screen under it answered: {:?}", seen.borrow());
    }

    #[test]
    fn an_open_drawer_does_not_let_the_screen_underneath_answer_anything() {
        // Modal, even though two thirds of the frame is all it covers: the screen behind is visible
        // and must not act, or it would answer a question nobody asked.
        let seen = Rc::new(StdRefCell::new(Vec::<Key>::new()));
        let mut slots = SlotTable::new();
        stack_press(&mut slots, &seen, Key::Softkey(Softkey::Left));
        seen.borrow_mut().clear();
        for key in [Key::Down, Key::Up, Key::Char('a'), Key::Backspace] {
            assert_eq!(stack_press(&mut slots, &seen, key), Handled::Consumed, "{key:?}");
            assert!(seen.borrow().is_empty(), "{key:?} reached the screen underneath");
        }
    }

    // ------------------------------------------------------------------ geometry

    #[test]
    fn the_open_panel_leaves_the_right_hand_band_to_the_screen_behind_it() {
        // Which is what says this is a layer and not a new place — and the reason the stack-or-
        // replace question does not even need the slot-table argument here.
        with_real_theme(|theme| {
            let under = Color::hex(0x00FF00).to_rgb565().0;
            let mut rig = Rig::new(0);
            rig.press(Key::Softkey(Softkey::Left));
            let buf = paint(&mut rig, theme, SCREEN, Color::hex(0x00FF00));

            let w = ui::Drawer::width(SCREEN);
            assert!(w < SCREEN.width(), "a full-width drawer proves nothing here");
            let content = chrome::Frame::split(SCREEN, theme, true, true).content;
            assert!(content.height() > 0);

            // The screen behind is untouched to the right of the panel...
            for y in content.y0..content.y1 {
                for x in (w + 1)..SCREEN.x1 {
                    assert_eq!(at(&buf, x, y), under, "the panel painted at ({x}, {y})");
                }
            }
            // ...and the title and softkey bands are not covered at all, which is more of the screen
            // left alone than `width` alone would suggest.
            let f = chrome::Frame::split(SCREEN, theme, true, true);
            for band in [f.title, f.softkeys] {
                for y in band.y0..band.y1 {
                    assert_eq!(at(&buf, 0, y), under, "the drawer painted over a chrome band at y={y}");
                }
            }
            // The negative control: the panel's own band is *not* the background, or the loops above
            // would pass over a drawer that drew nothing whatsoever.
            assert!(
                (content.y0..content.y1).any(|y| (0..w).any(|x| at(&buf, x, y) != under)),
                "the panel itself painted nothing"
            );
        });
    }

    #[test]
    fn the_layer_is_placed_at_the_whole_frame_whether_it_is_open_or_closed() {
        // The property that keeps the state underneath alive: a closed drawer is still measured and
        // still placed, so nothing is dropped out of the slot table when it closes. And the rect is
        // the *frame*, not the panel — a layer measured at its own two thirds would split those two
        // thirds again.
        with_real_theme(|theme| {
            let mut rig = Rig::new(0);
            for open in [false, true] {
                if open {
                    rig.press(Key::Softkey(Softkey::Left));
                }
                let node = Node::leaf(rig.build());
                let mut cache = UiCache::with_capacity(node.slot_count());
                layout::place_frame(&node, SCREEN, &mut cache, theme);
                assert_eq!(cache.rect(0), Some(SCREEN), "open={open}");
                assert_eq!(cache.size(0), Some(Size::new(320, 240)), "open={open}");
            }
            // And `width` really is narrower than what the layer measured, or the assertion above
            // would be satisfied by a widget that measured the panel.
            assert!(ui::Drawer::width(SCREEN) < SCREEN.width());
        });
    }

    #[test]
    fn a_rect_taller_than_the_drawer_measured_moves_its_panel_instead_of_stranding_it() {
        // The `Stretch` trap. `Drawer::draw` re-splits whatever rectangle it is handed, so a layer
        // placed at a rect it did not measure paints its panel inside that rect. Asserted as a
        // *shift* and not as "the ink is somewhere sensible": painting straight into the canvas
        // would also land in the right place on the first rect and still be wrong on the second.
        with_real_theme(|theme| {
            let under = Color::hex(0x00FF00);
            let mut rig = Rig::new(0);
            rig.press(Key::Softkey(Softkey::Left));
            let rows = |buf: &Vec<u16>| -> Vec<i32> {
                (0..240)
                    .filter(|&y| (0..320).any(|x| at(buf, x, y) != under.to_rgb565().0))
                    .collect()
            };
            let top = rows(&paint(&mut rig, theme, SCREEN, under));
            assert!(!top.is_empty(), "nothing was painted at all");

            let lower = Rect { x0: 0, y0: 20, x1: 320, y1: 240 };
            let moved = rows(&paint(&mut rig, theme, lower, under));
            assert!(!moved.is_empty());
            assert_eq!(*moved.first().unwrap(), top.first().unwrap() + 20, "the panel stayed put");
            assert!(
                moved.iter().all(|&y| y >= lower.y0 && y < lower.y1),
                "ink at rows {moved:?} escaped the rect {lower:?}"
            );
        });
    }

    #[test]
    fn the_digest_is_not_zero_and_does_not_move_with_the_sections() {
        // Constant because the size is a function of the offer alone — the sections decide what is
        // painted and how wide the panel is inside this rect, not how big the rect is. Not zero
        // because zero means "re-measure me every frame", and a volatile layer makes the whole
        // `Stack` above it volatile too.
        let mut rig = Rig::new(0);
        let a = rig.build().content_hash();
        assert_ne!(a, 0);
        rig.queue = "9 in the queue";
        rig.current = 2;
        assert_eq!(rig.build().content_hash(), a);
        rig.press(Key::Softkey(Softkey::Left));
        assert_eq!(rig.build().content_hash(), a, "opening re-measured the whole screen");
    }

    #[test]
    fn it_measures_everything_it_is_offered_in_both_states() {
        with_real_theme(|theme| {
            let mut rig = Rig::new(0);
            assert_eq!(rig.build().measure(Constraints::tight(320, 240), theme), Size::new(320, 240));
            rig.press(Key::Softkey(Softkey::Left));
            assert_eq!(rig.build().measure(Constraints::loose(320, 240), theme), Size::new(320, 240));
        });
    }

    #[test]
    fn an_empty_drawer_and_a_tiny_frame_do_not_panic() {
        with_real_theme(|theme| {
            let mut slots = SlotTable::new();
            slots.begin_frame();
            let w = Drawer::<Msg>::new(&mut slots, 0);
            with_key_ctx(|cx| {
                w.handle_key(KeyEvent::new(Key::Softkey(Softkey::Left)), SCREEN, cx);
                w.handle_key(KeyEvent::new(Key::Down), SCREEN, cx);
                w.handle_key(KeyEvent::new(Key::Select), SCREEN, cx);
            });
            testing::with_canvas(GSize::new(40, 30), |c| {
                w.draw(c, Rect::from_xywh(0, 0, 40, 30), theme);
            });
        });
    }

    #[test]
    fn the_real_atlas_paints_the_sections_so_the_pixel_tests_above_can_fail() {
        // The negative control every pixel assertion in this file leans on.
        // `symbian_ui::testing::with_theme` loads an atlas holding exactly one glyph — lowercase
        // 'a' — so "Boot", "Settings" and "2 in the queue" paint almost nothing under it. The
        // panel's own fill and its hairline *do* survive it, which is worse than a blank screen: a
        // test looking only for "something was drawn" would pass with every label missing.
        //
        // Two notes that differ only in a digit, because comparing two whole panels by ink *count*
        // would not do it: the content band's height moves with the fonts, so the two atlases
        // disagree about how big the panel is before anything is drawn into it.
        let painted = |theme: &Theme<'_>, queue: &'static str| {
            let mut rig = Rig::new(0);
            rig.queue = queue;
            rig.press(Key::Softkey(Softkey::Left));
            paint(&mut rig, theme, SCREEN, theme.palette.bg.mid())
        };
        with_real_theme(|theme| {
            let bg = theme.palette.bg.mid().to_rgb565().0;
            let two = painted(theme, "2 in the queue");
            assert!(two.iter().any(|&p| p != bg), "the real atlas painted nothing");
            assert_ne!(two, painted(theme, "9 in the queue"), "the note never reached the canvas");
        });
        // The other half: under the test atlas the two panels are the same pixels, because the
        // digit that differs is not the one glyph it has.
        testing::with_theme(Palette::DARK, |t| {
            let bg = t.palette.bg.mid().to_rgb565().0;
            let two = painted(t, "2 in the queue");
            assert!(two.iter().any(|&p| p != bg), "not even the panel fill arrived");
            assert_eq!(
                two,
                painted(t, "9 in the queue"),
                "the test atlas grew a font, and the comparison above is no longer a control"
            );
        });
    }
}
