//! The detail of one thing, over the screen that listed it: facts, then actions.
//!
//! # Stack or replace: why an overlay that covers every pixel is still a layer
//!
//! [`Select`](super::Select)'s popup is a small box in the middle of a band, and nobody would
//! argue for drawing it by throwing the screen behind it away. This one is different, and the
//! question has to be asked properly: a sheet takes the *whole frame* — it draws its own title bar,
//! its own softkey bar, and clears the canvas between them — so the screen underneath contributes
//! not one pixel while it is up. Placing it as a [`Stack`](super::Stack) layer means measuring and
//! laying out a list that cannot be seen, every frame, for nothing.
//!
//! It is still a layer, and the reason is not the pixels. It is [`crate::slot`]: **a group not
//! entered on a frame is dropped at the end of it, with everything under it.** The screen behind a
//! detail sheet is, in every case this widget was written for, a [`ScrollList`](super::ScrollList),
//! and a scroll offset is exactly the kind of state that module says belongs in a slot rather than
//! in the model — it is a consequence of having drawn the list there, not a fact about the
//! application. Replace the content and that node is absent for as long as the sheet is up, so the
//! offset is reclaimed; back out of the sheet and you are at the top of a list you were forty rows
//! into, with the cursor on a row you did not choose. The same reclamation takes every text field's
//! caret and every [`Select`](super::Select)'s highlight on the same screen with it.
//!
//! So the trade is: a layout pass over an invisible list, against the reader's place in it. The
//! layout pass is bounded and cheap — `ScrollList` is a leaf that measures to the band it is
//! offered, and the cache keys on the offer, so a frame where nothing changed is a handful of hash
//! comparisons. Losing the reader's place is the complaint the sheet exists to answer in the first
//! place; the boot manager's `Entry` tab lost it every time, and that is in
//! [`symbian_ui::drawer`]'s own module note.
//!
//! The second reason is the one [`Select`](super::Select) already sets out and it applies unchanged:
//! placed unconditionally, there is no branch in the screen, no `Msg::OpenSheet` and
//! `Msg::CloseSheet` in the application's enum, and no slot shifting under a conditional.
//!
//! [`Drawer`](super::Drawer) reaches the same answer by a much shorter road — it covers two thirds
//! of the content band, so the screen behind it has to be *painted*, and there is nothing to argue
//! about. This file is where the argument had to be made, because here it is invisible.
//!
//! # Where it goes: above the screen, not inside it
//!
//! ```ignore
//! Stack::new(slots)
//!     .child(Screen::new().title("Packages").content(list))   // still the whole screen
//!     .child(DetailSheet::new(slots, &m.name, &m.version)
//!         .rows(facts)
//!         .action("Install")
//!         .action("Pin")
//!         .out(out.clone(), Msg::Chose))
//! ```
//!
//! At the **frame level**, as a sibling of the whole [`Screen`](super::Screen) — not in that
//! screen's `content`. [`symbian_ui::Sheet::draw`] starts with `Frame::split` and paints a title bar
//! and a softkey bar of its own; handed a content band it would draw a second title bar *inside*
//! the first screen's, which is what [`Imperative`](super::Imperative) means when it says the
//! adapter is for whole screens.
//!
//! # The softkey trap, inverted
//!
//! [`Select`](super::Select)'s popup lives inside a screen's content, so
//! [`Screen::handle_key`](super::Screen) offers the bar every key first and a labelled action slot
//! means the popup never opens. This widget sits *above* the screen in a [`Stack`], and [`Stack`]
//! asks its last layer first — so the arrow points the other way: **the closed sheet takes
//! `Key::Select` before the screen's bar ever sees it.**
//!
//! A screen carrying a detail sheet must therefore leave [`Screen::on_action`](super::Screen) out.
//! Not because the overlay would fail to open — it opens fine — but because the label would promise
//! a message that can never be delivered, which is the exact defect `screen.rs` was written against:
//! a bar that says one thing and a key that does another. The centre key opening the highlighted
//! row is the S60 convention and needs no middle label; once the sheet is up it draws its own bar,
//! with the focused action's name on the left and `Back` on the right.
//! [`a_closed_sheet_takes_the_centre_key_before_anything_under_it_in_the_stack`] is the test.
//!
//! # It is modal, and [`symbian_ui::Sheet`] is not
//!
//! `Sheet::handle_key` answers [`Handled::Ignored`] for anything that is not an arrow, a commit or
//! a Back — a digit, a letter, the green key. That is right for a screen that owns the phone and
//! wrong for a layer: this one covers every pixel, so a declined key would fall through to the list
//! underneath and reach whatever it does with letters. On the packages screen that is
//! search-as-you-type, so typing a note to yourself while reading a sheet would silently re-filter
//! the list you were about to come back to. Open, this widget consumes everything.
//!
//! # No arithmetic of its own
//!
//! Every rectangle on screen comes from [`symbian_ui::sheet`] — the band split, the action rows,
//! the wrap of a note, the scroll. There is no [`Gap`](crate::spacing::Gap) or
//! [`Pad`](crate::spacing::Pad) in this file and no `theme.metrics` lookup either, which is the
//! stronger version of the same rule: the numbers are not merely named here, they are not here.
//! Reimplementing the action-band height would have been a second sheet, agreeing with the first on
//! the day it was written.

use alloc::rc::Rc;
use alloc::string::String;
use core::cell::{Cell, RefCell};

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::{sheet as ui, Handled, Key, KeyEvent, Theme};

use crate::constraints::Constraints;
use crate::outbox::Outbox;
use crate::slot::SlotTable;
use crate::widget::{hash_str, KeyCtx, Widget, WidgetHash};

/// Where a chosen action goes, and how to name it once it gets there.
///
/// A `fn` pointer, for the reason [`Stepper`](super::Stepper) sets out at length:
/// [`Outbox::wrapped`](crate::outbox::Outbox) allocates an `Rc` and boxes a closure per call, and
/// the call is in `view`, which runs every frame.
type Report<M> = (Outbox<M>, fn(usize) -> M);

/// Everything about a sheet that outlives the frame that drew it.
///
/// `Copy`, so it lives in a [`Cell`] rather than a `RefCell` — the choice
/// [`FocusHook`](super::focus::FocusHook) documents: no borrow flag to get wrong and no runtime
/// panic path in a key dispatch, on a device whose whole failure report is a dialog with a number
/// in it.
///
/// The [`symbian_ui::Sheet`] itself is *not* in here, and that is the point of
/// [`symbian_ui::Sheet::cursor`]: the facts are rebuilt from the model every frame, so keeping last
/// frame's sheet would show last frame's version number. Only the cursor is carried over.
#[derive(Copy, Clone)]
struct SheetState {
    open: bool,
    /// Which action the cursor is on. Clamped by `set_cursor` against the actions the *new* frame
    /// declared, so a sheet that lost an action does not focus past the end of its own list.
    focus: usize,
    /// How far the facts are scrolled, in [`symbian_ui::ListState`] units.
    scroll: usize,
}

/// A full-frame detail view: facts, actions, and the key that opens it.
pub struct DetailSheet<M> {
    state: Rc<Cell<SheetState>>,
    /// This frame's sheet, built from this frame's model.
    ///
    /// A [`RefCell`] because [`Widget::draw`] takes `&self` and
    /// [`symbian_ui::Sheet::draw`] takes `&mut self` — it records its own scroll as it paints. Not
    /// an `Rc`: this cell is *per frame*, thrown away with the widget, and the only thing that
    /// crosses a frame boundary is [`SheetState`].
    sheet: RefCell<ui::Sheet>,
    /// The key that opens it while it is closed. See the module note on the inverted softkey trap.
    open_key: Key,
    out: Option<Report<M>>,
}

impl<M: 'static> DetailSheet<M> {
    /// A sheet titled `title`, subtitled `subtitle`, closed until its open key is pressed.
    ///
    /// Takes the slot table for the open flag and the cursor, exactly as
    /// [`ScrollList`](super::ScrollList) takes it for a scroll offset — and, like that one, this is
    /// a positional slot: wrap a conditional sheet in
    /// [`SlotTable::group`](crate::slot::SlotTable::group) or the calls after it shift by one the
    /// frame the condition flips.
    pub fn new(slots: &mut SlotTable, title: impl Into<String>, subtitle: impl Into<String>) -> Self {
        let state = slots
            .use_state_with(|| Rc::new(Cell::new(SheetState { open: false, focus: 0, scroll: 0 })))
            .clone();
        Self {
            state,
            sheet: RefCell::new(ui::Sheet::new(title, subtitle)),
            open_key: Key::Select,
            out: None,
        }
    }

    /// Rebuild the frame's sheet through one of [`symbian_ui::Sheet`]'s own consuming builders.
    ///
    /// `mem::replace` and not an `Option`, because an `Option` here would be a second way to
    /// represent "there is no sheet" that cannot happen — every path puts one straight back, and
    /// the empty stand-in allocates nothing (`String::new` does not).
    fn with(mut self, f: impl FnOnce(ui::Sheet) -> ui::Sheet) -> Self {
        let slot = self.sheet.get_mut();
        let taken = core::mem::replace(slot, ui::Sheet::new("", ""));
        *slot = f(taken);
        self
    }

    /// One fact.
    pub fn row(self, row: ui::Row) -> Self {
        self.with(|s| s.row(row))
    }

    /// Several facts, which is how they usually arrive — a `Vec<SheetRow>` built from the model.
    pub fn rows(self, rows: impl IntoIterator<Item = ui::Row>) -> Self {
        self.with(|s| s.rows(rows))
    }

    /// Add an action. The order is the order they appear and the first is focused, so the most
    /// likely thing goes first — [`symbian_ui::Sheet::action`]'s rule, unchanged.
    pub fn action(self, label: impl Into<String>) -> Self {
        self.with(|s| s.action(label))
    }

    /// The key that opens the sheet while it is closed.
    ///
    /// `Key::Select` by default: the centre key on the highlighted row is what opens a detail on
    /// S60, and it is the key the sheet's own bar takes back once it is up. Worth changing for a
    /// screen whose centre key already means something the sheet is not — a message list where
    /// `Select` opens the conversation and the sheet holds the *contact*.
    pub fn opens_on(mut self, key: Key) -> Self {
        self.open_key = key;
        self
    }

    /// Where a chosen action goes, and how to say it.
    ///
    /// `msg` receives the index of the action in declaration order. Backing out reports nothing, for
    /// the reason [`Select`](super::Select) reports nothing on cancel: a message meaning "the user
    /// looked and left" is an `update`, a [`Cmd`](crate::cmd::Cmd) and a repaint for a model that
    /// did not move.
    pub fn out(mut self, out: Outbox<M>, msg: fn(usize) -> M) -> Self {
        self.out = Some((out, msg));
        self
    }

    /// Whether the sheet is showing, readable while the tree is still being built.
    ///
    /// Not needed to place it — the layer is unconditional, which is the whole of the module note
    /// above. It is here for the caller [`FocusStops`](super::FocusStops) exists for: a screen that
    /// wants its own title bar to say something different while a detail is up, decided outside the
    /// tree entirely.
    pub fn is_open(&self) -> bool {
        self.state.get().open
    }

    /// The label the sheet's own softkey bar is showing, or `None` while it is closed.
    ///
    /// The S60 convention [`symbian_ui::Sheet::action_label`] documents: the left softkey says what
    /// the centre key will do. Exposed because a screen may want to mirror it somewhere; the bar
    /// itself is drawn by the sheet and needs no help.
    pub fn action_label(&self) -> Option<String> {
        if !self.state.get().open {
            return None;
        }
        let mut sheet = self.sheet.borrow_mut();
        let s = self.state.get();
        sheet.set_cursor(s.focus, s.scroll);
        sheet.action_label().map(String::from)
    }
}

impl<M: 'static> Widget for DetailSheet<M> {
    /// A constant, and nothing about the facts folded in.
    ///
    /// This layer measures to whatever it is offered whether it is showing anything or not, so its
    /// size is a function of the offer alone — and the cache keys every entry on the offer as well
    /// as on this digest (see `cache::Entry::offer`), which is what makes a constant honest here
    /// rather than merely cheap. The rows decide what is *painted*, not what is measured; folding
    /// them in would re-measure the whole frame every time a poll changed a version string, to
    /// produce the same number.
    ///
    /// Never zero. Zero means "re-measure me every frame", and a volatile child makes its whole
    /// ancestry volatile — [`Stack::layer`](super::Stack::layer) says so — so one zero here would
    /// put every screen carrying a detail sheet on the slow path for ever.
    fn content_hash(&self) -> WidgetHash {
        hash_str(0, "detail-sheet")
    }

    /// Everything it is offered, exactly as a [`Stack`](super::Stack) layer does.
    ///
    /// A layer that shrank to its content would move the band its callers are trying to hold still,
    /// and this one has no smaller size to report anyway: a sheet carves its own three bands out of
    /// what it is given.
    fn measure(&self, constraints: Constraints, _theme: &Theme<'_>) -> Size {
        constraints.constrain(Size::new(constraints.max_w, constraints.max_h))
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        let s = self.state.get();
        if !s.open {
            // Closed, this layer is a hole in the stack: no ink, so the screen underneath is simply
            // what the phone looks like. It is still measured and still placed, which is what keeps
            // the list's scroll offset — and this sheet's own cursor — alive across the close.
            return;
        }
        // `try_borrow_mut` for the reason `Imperative` gives: a panic inside `draw` is the
        // application gone on a device whose entire failure report is a dialog with a number in it,
        // and a frame that did not paint is a frame that did not paint. Nothing here can actually
        // re-enter — a sheet calls no caller-supplied code — so this is a guard rather than a path.
        let Ok(mut sheet) = self.sheet.try_borrow_mut() else { return };
        sheet.set_cursor(s.focus, s.scroll);
        // `rect`, never the canvas: `Sheet::draw` re-splits whatever rectangle it is handed into its
        // three bands, which is what makes this survive being placed taller than it measured. A
        // `CrossAlign::Stretch` layer, or a root that grew, moves the bands with it instead of
        // painting a title bar at a y this widget remembered.
        sheet.draw(c, rect, theme);
        // `Sheet::draw` records its own scroll as it paints, so the cursor goes back to the cell it
        // came from. Skipping this would lose the clamping the paint pass did and scroll the facts
        // twice for one press.
        let (focus, scroll) = sheet.cursor();
        self.state.set(SheetState { open: true, focus, scroll });
    }

    fn handle_key(&self, ev: KeyEvent, _rect: Rect, _cx: &mut KeyCtx<'_>) -> Handled {
        let s = self.state.get();
        if !s.open {
            if ev.key == self.open_key {
                // Opening starts at the top with the first action focused, which is
                // `symbian_ui::Sheet`'s own default and the right one: the sheet describes whatever
                // row the list is on *now*, and carrying the last thing's cursor into it would put
                // the highlight on an action belonging to a different package.
                self.state.set(SheetState { open: true, focus: 0, scroll: 0 });
                return Handled::Consumed;
            }
            // Everything else falls through, and that is what lets the list underneath keep its
            // navigation. A closed layer that consumed `Down` would be a screen where nothing works
            // and nothing on it shows why.
            return Handled::Ignored;
        }
        let Ok(mut sheet) = self.sheet.try_borrow_mut() else { return Handled::Ignored };
        sheet.set_cursor(s.focus, s.scroll);
        let (_, action) = sheet.handle_key(ev);
        let (focus, scroll) = sheet.cursor();
        match action {
            ui::SheetAction::Chose(i) => {
                // Closing on the way out, for the same reason a `Select` closes on commit: the
                // action the sheet just launched is the reason it was opened, and a sheet left up
                // over the screen its own action navigated away from is a dialog nobody dismissed.
                self.state.set(SheetState { open: false, focus: 0, scroll: 0 });
                if let Some((out, msg)) = &self.out {
                    out.push(msg(i));
                }
            }
            ui::SheetAction::Back => {
                self.state.set(SheetState { open: false, focus: 0, scroll: 0 });
            }
            ui::SheetAction::None => {
                self.state.set(SheetState { open: true, focus, scroll });
            }
        }
        // Consumed whatever the sheet answered — see the module note. `Sheet::handle_key` declines
        // a letter, and a letter that fell through this layer would reach the search field of a list
        // covered by every pixel of the thing that declined it.
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
    use alloc::vec::Vec;
    use core::cell::RefCell as StdRefCell;
    use symbian_gfx::Size as GSize;
    use symbian_ui::{chrome, testing, Palette, Softkey};

    /// The device's frame, which is the only rect this widget is meant to be placed at.
    const SCREEN: Rect = Rect { x0: 0, y0: 0, x1: 320, y1: 240 };

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Msg {
        Chose(usize),
    }

    /// One sheet over a slot table that survives frames, plus the outbox it reports to.
    struct Rig {
        slots: SlotTable,
        out: Outbox<Msg>,
        /// The facts, which a real screen rebuilds from its model every frame. Changing this between
        /// frames is how `the_cursor_survives_a_frame_in_which_the_facts_changed` works.
        available: &'static str,
        actions: &'static [&'static str],
    }

    impl Rig {
        fn new() -> Self {
            Self {
                slots: SlotTable::new(),
                out: Outbox::new(),
                available: "0.2.0",
                actions: &["Install", "Pin"],
            }
        }

        /// Build one frame's widget, the way `view` does.
        fn build(&mut self) -> DetailSheet<Msg> {
            self.slots.begin_frame();
            let mut s = DetailSheet::<Msg>::new(&mut self.slots, "Launcher", "0.1.0 -> 0.2.0")
                .row(ui::Row::pair("Installed", "0.1.0"))
                .row(ui::Row::pair("Available", self.available))
                .out(self.out.clone(), Msg::Chose);
            for a in self.actions {
                s = s.action(*a);
            }
            s
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

        fn label(&mut self) -> Option<String> {
            self.build().action_label()
        }
    }

    /// The real device atlases, not the one-glyph test atlas. See
    /// `the_real_atlas_paints_the_sheet_so_the_pixel_tests_above_can_fail`.
    fn with_real_theme<R>(f: impl FnOnce(&Theme<'_>) -> R) -> R {
        let atlases = symbian_preview::Atlases::load();
        atlases.with_fonts(|fonts| f(&symbian_ui::Theme::dark(fonts)))
    }

    /// Paint one frame of the sheet into `rect` over a `w`x`h` canvas.
    fn paint(rig: &mut Rig, theme: &Theme<'_>, rect: Rect, w: i32, h: i32) -> Vec<u16> {
        let widget = rig.build();
        testing::with_canvas(GSize::new(w, h), |c| {
            c.clear(theme.palette.bg.mid());
            widget.draw(c, rect, theme);
        })
        .1
    }

    /// Which rows of a `w`-wide buffer have something other than the background in them.
    fn inked_rows(theme: &Theme<'_>, buf: &[u16], w: i32) -> Vec<i32> {
        let bg = theme.palette.bg.mid().to_rgb565().0;
        let h = buf.len() as i32 / w;
        (0..h).filter(|&y| (0..w).any(|x| buf[(y * w + x) as usize] != bg)).collect()
    }

    // ------------------------------------------------------------------ closed

    #[test]
    fn a_closed_sheet_paints_nothing_and_leaves_the_screen_behind_its_keys() {
        with_real_theme(|theme| {
            let bg = theme.palette.bg.mid().to_rgb565().0;
            let mut rig = Rig::new();
            let closed = paint(&mut rig, theme, SCREEN, 320, 240);
            assert!(closed.iter().all(|&p| p == bg), "a closed sheet painted something");

            // The negative control, and it must fire: without it "the closed sheet painted nothing"
            // is satisfied by a widget whose `draw` is empty in both states.
            rig.press(Key::Select);
            let open = paint(&mut rig, theme, SCREEN, 320, 240);
            assert!(open.iter().any(|&p| p != bg), "an open sheet painted nothing");
        });

        // And every key that is not the opener falls through, which is what keeps a list underneath
        // able to move its cursor.
        let mut rig = Rig::new();
        for key in [Key::Up, Key::Down, Key::Left, Key::Right, Key::Backspace, Key::Char('a')] {
            assert_eq!(rig.press(key), Handled::Ignored, "{key:?}");
            assert!(!rig.is_open(), "{key:?} opened it");
        }
        assert!(rig.out.is_empty());
    }

    #[test]
    fn the_open_key_is_the_one_the_caller_named() {
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let w = DetailSheet::<Msg>::new(&mut slots, "t", "s")
            .action("Go")
            .opens_on(Key::Softkey(Softkey::Left));
        with_key_ctx(|cx| {
            assert_eq!(w.handle_key(KeyEvent::new(Key::Select), SCREEN, cx), Handled::Ignored);
            assert!(!w.is_open(), "the default opener still fired after being replaced");
            let ev = KeyEvent::new(Key::Softkey(Softkey::Left));
            assert_eq!(w.handle_key(ev, SCREEN, cx), Handled::Consumed);
            assert!(w.is_open());
        });
    }

    // ------------------------------------------------------------------ open

    #[test]
    fn opening_focuses_the_first_action_and_reports_nothing() {
        let mut rig = Rig::new();
        assert_eq!(rig.press(Key::Select), Handled::Consumed);
        assert!(rig.is_open());
        assert_eq!(rig.label().as_deref(), Some("Install"));
        assert!(rig.out.is_empty(), "opening is not a choice");
    }

    #[test]
    fn the_arrows_move_the_focused_action_without_choosing_one() {
        let mut rig = Rig::new();
        rig.press(Key::Select);
        assert_eq!(rig.press(Key::Down), Handled::Consumed);
        assert_eq!(rig.label().as_deref(), Some("Pin"));
        assert!(rig.out.is_empty(), "an arrow ran `update` and whatever `Cmd` it returned");
        assert_eq!(rig.press(Key::Up), Handled::Consumed);
        assert_eq!(rig.label().as_deref(), Some("Install"));
        assert!(rig.is_open());
    }

    #[test]
    fn choosing_an_action_reports_its_index_and_closes_the_sheet() {
        let mut rig = Rig::new();
        rig.press(Key::Select);
        rig.press(Key::Down);
        assert_eq!(rig.press(Key::Select), Handled::Consumed);
        assert_eq!(rig.out.take(), alloc::vec![Msg::Chose(1)]);
        assert!(!rig.is_open(), "the sheet stayed up over the screen its action navigated to");
        // And the next open starts on the first action again: the sheet now describes whatever row
        // the list is on, and carrying the cursor over would highlight another package's action.
        rig.press(Key::Select);
        assert_eq!(rig.label().as_deref(), Some("Install"));
    }

    #[test]
    fn backing_out_closes_it_and_reports_nothing() {
        // `Softkey::Right` is the key `symbian_ui::Sheet` answers to, and it reaches this layer
        // before any screen's bar because the layer is above the screen in the stack — which is
        // what makes it usable at all. Inside a screen's content it would be the bar's, and a
        // labelled Back would leave the sheet with no way out.
        let mut rig = Rig::new();
        rig.press(Key::Select);
        rig.press(Key::Down);
        assert_eq!(rig.label().as_deref(), Some("Pin"), "nothing to back out of");
        assert_eq!(rig.press(Key::Softkey(Softkey::Right)), Handled::Consumed);
        assert!(!rig.is_open(), "it did not close the sheet");
        assert!(rig.out.is_empty(), "backing out chose the focused action");
    }

    #[test]
    fn an_open_sheet_consumes_a_key_it_has_no_use_for() {
        // The modal rule, and the reason it is not `symbian_ui::Sheet`'s: that one declines a letter
        // because it owns the phone, and this one covers a list that would take it as a filter.
        let mut rig = Rig::new();
        rig.press(Key::Select);
        assert_eq!(rig.press(Key::Char('a')), Handled::Consumed);
        assert_eq!(rig.press(Key::Char('9')), Handled::Consumed);
        assert!(rig.is_open());
        assert!(rig.out.is_empty());
    }

    #[test]
    fn the_cursor_survives_a_frame_in_which_the_facts_changed() {
        // Why `symbian_ui::Sheet::cursor` exists. The `Available` row is filled in by a poll that
        // lands seconds after the sheet opens; a sheet rebuilt from the new model must not put a
        // finger resting on `Pin` back on `Install`.
        let mut rig = Rig::new();
        rig.press(Key::Select);
        rig.press(Key::Down);
        assert_eq!(rig.label().as_deref(), Some("Pin"));
        rig.available = "0.3.0";
        assert_eq!(rig.label().as_deref(), Some("Pin"), "the rebuild moved the cursor");
        assert_eq!(rig.press(Key::Select), Handled::Consumed);
        assert_eq!(rig.out.take(), alloc::vec![Msg::Chose(1)]);
    }

    #[test]
    fn a_frame_that_offers_fewer_actions_does_not_focus_past_the_end_of_them() {
        // A package that finished installing loses its `Install` action. Without the clamp in
        // `set_cursor` the sheet would highlight nothing and its softkey would go blank.
        let mut rig = Rig::new();
        rig.press(Key::Select);
        rig.press(Key::Down);
        rig.actions = &["Pin"];
        assert_eq!(rig.label().as_deref(), Some("Pin"));
        assert_eq!(rig.press(Key::Select), Handled::Consumed);
        assert_eq!(rig.out.take(), alloc::vec![Msg::Chose(0)]);
    }

    // ------------------------------------------------------------------ the stack

    /// A widget that consumes every key and records it — the screen underneath the sheet.
    struct Taker(Rc<StdRefCell<Vec<Key>>>);

    impl Widget for Taker {
        fn content_hash(&self) -> WidgetHash {
            hash_i32(0, 0x7A_4E)
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

    /// One frame of `Stack { Taker, sheet }` — the shape a screen has, with the screen underneath
    /// standing in for a whole `Screen` — pressed at `key`.
    fn stack_press(slots: &mut SlotTable, seen: &Rc<StdRefCell<Vec<Key>>>, key: Key) -> Handled {
        slots.begin_frame();
        let sheet = DetailSheet::<Msg>::new(slots, "Launcher", "0.1.0")
            .row(ui::Row::pair("Installed", "0.1.0"))
            .action("Install");
        let stack = Stack::new(slots).child(Taker(Rc::clone(seen))).child(sheet);
        testing::with_theme(Palette::DARK, |theme| {
            let mut clip = symbian_ui::NoClipboard;
            let mut cx = KeyCtx::new(theme, &mut clip);
            stack.handle_key(KeyEvent::new(key), SCREEN, &mut cx)
        })
    }

    #[test]
    fn a_closed_sheet_takes_the_centre_key_before_anything_under_it_in_the_stack() {
        // The inverted softkey trap, measured: a `Screen` under this layer never hears `Select`, so
        // a screen carrying a detail sheet must leave its action slot unlabelled or its bar promises
        // a message that cannot be delivered.
        let seen = Rc::new(StdRefCell::new(Vec::<Key>::new()));
        let mut slots = SlotTable::new();

        // The negative control first, and it must fire — without it "the layer underneath saw
        // nothing" is satisfied by a stack that never dispatches at all.
        assert_eq!(stack_press(&mut slots, &seen, Key::Down), Handled::Consumed);
        assert_eq!(&*seen.borrow(), &[Key::Down], "the control did not fire");
        seen.borrow_mut().clear();

        assert_eq!(stack_press(&mut slots, &seen, Key::Select), Handled::Consumed);
        assert!(seen.borrow().is_empty(), "the screen under it answered: {:?}", seen.borrow());
    }

    #[test]
    fn an_open_sheet_does_not_let_the_screen_underneath_answer_anything() {
        let seen = Rc::new(StdRefCell::new(Vec::<Key>::new()));
        let mut slots = SlotTable::new();
        stack_press(&mut slots, &seen, Key::Select);
        seen.borrow_mut().clear();
        for key in [Key::Down, Key::Up, Key::Left, Key::Right, Key::Char('a'), Key::Backspace] {
            assert_eq!(stack_press(&mut slots, &seen, key), Handled::Consumed, "{key:?}");
            assert!(seen.borrow().is_empty(), "{key:?} reached the screen underneath");
        }
    }

    // ------------------------------------------------------------------ geometry

    #[test]
    fn the_layer_is_placed_at_the_whole_frame_whether_it_is_open_or_closed() {
        // The property that keeps the state underneath alive: a closed sheet is still measured and
        // still placed, so nothing is dropped out of the slot table when it closes.
        with_real_theme(|theme| {
            let mut rig = Rig::new();
            for open in [false, true] {
                if open {
                    rig.press(Key::Select);
                }
                let node = Node::leaf(rig.build());
                let mut cache = UiCache::with_capacity(node.slot_count());
                layout::place_frame(&node, SCREEN, &mut cache, theme);
                assert_eq!(cache.rect(0), Some(SCREEN), "open={open}");
                assert_eq!(cache.size(0), Some(Size::new(320, 240)), "open={open}");
            }
        });
    }

    #[test]
    fn a_rect_taller_than_the_sheet_measured_moves_its_bands_instead_of_stranding_them() {
        // The `Stretch` trap. `Sheet::draw` re-splits whatever rectangle it is handed, so a layer
        // placed at a rect it did not measure paints its title bar at that rect's top rather than at
        // a y it remembered. Asserted as a *shift* and not as "the ink is somewhere sensible":
        // painting straight into the canvas would also land in the right place on the first rect and
        // still be wrong on the second.
        with_real_theme(|theme| {
            let mut rig = Rig::new();
            rig.press(Key::Select);
            let top = inked_rows(theme, &paint(&mut rig, theme, SCREEN, 320, 240), 320);
            assert!(!top.is_empty(), "nothing was painted at all");

            let lower = Rect { x0: 0, y0: 20, x1: 320, y1: 240 };
            let moved = inked_rows(theme, &paint(&mut rig, theme, lower, 320, 240), 320);
            assert!(!moved.is_empty());
            assert_eq!(*moved.first().unwrap(), top.first().unwrap() + 20, "the top band stayed put");
            assert!(
                moved.iter().all(|&y| y >= lower.y0 && y < lower.y1),
                "ink at rows {moved:?} escaped the rect {lower:?}"
            );
        });
    }

    #[test]
    fn an_open_sheet_draws_its_own_title_bar_and_softkey_bar_over_the_whole_frame() {
        // Which is why this layer goes above a `Screen` and not inside its content band: handed a
        // band it would draw a second title bar inside the first screen's.
        with_real_theme(|theme| {
            let bg = theme.palette.bg.mid().to_rgb565().0;
            let mut rig = Rig::new();
            rig.press(Key::Select);
            let buf = paint(&mut rig, theme, SCREEN, 320, 240);
            let f = chrome::Frame::split(SCREEN, theme, true, true);
            assert!(f.title.height() > 0 && f.softkeys.height() > 0, "no bands to look at");
            let band_has_ink = |r: Rect| {
                (r.y0..r.y1).any(|y| (r.x0..r.x1).any(|x| buf[(y * 320 + x) as usize] != bg))
            };
            assert!(band_has_ink(f.title), "no title bar");
            assert!(band_has_ink(f.softkeys), "no softkey bar");
            assert!(band_has_ink(f.content), "no facts");
        });
    }

    #[test]
    fn the_digest_is_not_zero_and_does_not_move_with_the_facts() {
        // Constant because the size is a function of the offer alone — the rows decide what is
        // painted, not what is measured. Not zero because zero means "re-measure me every frame",
        // and a volatile layer makes the whole `Stack` above it volatile too.
        let mut rig = Rig::new();
        let a = rig.build().content_hash();
        assert_ne!(a, 0);
        rig.available = "9.9.9";
        rig.actions = &["Install", "Pin", "Remove"];
        assert_eq!(rig.build().content_hash(), a);
        rig.press(Key::Select);
        assert_eq!(rig.build().content_hash(), a, "opening re-measured the whole screen");
    }

    #[test]
    fn it_measures_everything_it_is_offered_in_both_states() {
        with_real_theme(|theme| {
            let mut rig = Rig::new();
            assert_eq!(rig.build().measure(Constraints::tight(320, 240), theme), Size::new(320, 240));
            assert_eq!(rig.build().measure(Constraints::loose(320, 240), theme), Size::new(320, 240));
            rig.press(Key::Select);
            assert_eq!(rig.build().measure(Constraints::loose(320, 240), theme), Size::new(320, 240));
        });
    }

    #[test]
    fn the_real_atlas_paints_the_sheet_so_the_pixel_tests_above_can_fail() {
        // The negative control every pixel assertion in this file leans on.
        // `symbian_ui::testing::with_theme` loads an atlas holding exactly one glyph — lowercase
        // 'a' — so "Installed", "Available" and "Pin" paint nothing at all under it. The fills and
        // rules of a sheet do survive it, which is worse than a blank screen: a test looking only
        // for "something was drawn" would pass while every label on the sheet was missing.
        // A value that differs only in its digits, because digits are what a sheet is mostly made
        // of and the test atlas has none of them. Comparing two whole sheets by ink *count* would
        // not do it: the band split moves with the fonts, so the two atlases disagree about where
        // the facts are before anything is drawn into them.
        let painted = |theme: &Theme<'_>, available: &'static str| {
            let mut rig = Rig::new();
            rig.available = available;
            rig.press(Key::Select);
            paint(&mut rig, theme, SCREEN, 320, 240)
        };
        with_real_theme(|theme| {
            let bg = theme.palette.bg.mid().to_rgb565().0;
            let one = painted(theme, "0.1.0");
            assert!(one.iter().any(|&p| p != bg), "the real atlas painted nothing");
            assert_ne!(one, painted(theme, "9.9.9"), "the fact never reached the canvas");
        });
        // The other half, so a future reader does not have to take the paragraph above on trust:
        // under the test atlas the two sheets are the same pixels, because neither "0.1.0" nor
        // "9.9.9" contains the one glyph it has.
        testing::with_theme(Palette::DARK, |t| {
            let bg = t.palette.bg.mid().to_rgb565().0;
            let one = painted(t, "0.1.0");
            assert!(one.iter().any(|&p| p != bg), "not even the fills arrived");
            assert_eq!(
                one,
                painted(t, "9.9.9"),
                "the test atlas grew a font, and the comparison above is no longer a control"
            );
        });
    }
}
