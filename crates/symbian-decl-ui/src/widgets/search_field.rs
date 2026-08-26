//! A search box: a one-line field, a magnifier, and a matcher the screen applies to its own items.
//!
//! # This file contains no matching arithmetic and no editing arithmetic
//!
//! The rule for "does this row pass what the user typed" is
//! [`symbian_ui::match_filter`], extracted out of the app picker for this widget; the
//! caret and every key that moves it are [`symbian_ui::edit::TextField`]. Neither is reimplemented
//! here, and both refusals prevent a named defect:
//!
//! * A second matcher agrees with the first on the day it is written and then one of them learns
//!   about accents. Three letters would then find different rows in the launcher's picker than in a
//!   search box, and nothing on screen says which one answered.
//! * A second caret does not misbehave on Cyrillic, it *panics*, on a device whose entire failure
//!   report is a dialog with a number in it.
//!
//! What is left is placement, a magnifier, and the question of where the typed text lives.
//!
//! # Where the query lives: with the caret, not in the model
//!
//! [`ScrollList`](super::ScrollList) splits a list in two — the *selection* is the model's, because
//! `update` is what pushes a detail screen for row 4; the *scroll offset* is the slot table's,
//! because it cannot be computed without a viewport height the model does not have. The same
//! question is asked here and comes out the other way round from what "filtering changes what the
//! screen shows" suggests, so it is worth being explicit.
//!
//! **The query lives where the caret lives: in one [`edit::TextField`] in the slot table.** Not
//! because the query is uninteresting to the model — it is very interesting — but because a query in
//! the model would be a *second copy of a string whose caret is in the first*. `update` would write
//! `model.query`, `view` would push it into the editor, and the caret would land wherever that
//! assignment left it: at zero, mid-word, on every keystroke. That is not a theoretical risk, it is
//! what [`TextField`](super::TextField)'s docs describe as the shape a caret forces, and there is no
//! version of it where the two agree.
//!
//! The model still learns everything it needs, because **every change is announced**: see
//! [`SearchField::on_change`]. The message carries the new query as a `String`, so `update` can do
//! the one thing only it can do — reset the selection to the top, because a narrowed list leaves the
//! old index pointing at a row that is no longer there.
//!
//! ## What breaks if this choice is wrong
//!
//! Two things, both real:
//!
//! * **A slot forgets.** The query is gone when the field leaves the screen — pushing a detail
//!   screen and coming back reopens an empty search. For a search box that is usually right (S60's
//!   own pickers open empty). When it is not — a saved search, a query that must survive a screen
//!   push, a query `update` needs to *set* rather than read — use
//!   [`with_buffer`](SearchField::with_buffer) and let the model hold the `Rc`. That is the same
//!   escape hatch, for the same reason, that [`TextField::with_buffer`](super::TextField::with_buffer)
//!   exists for on the login screen.
//! * **`update` cannot read the query except from a message.** A softkey is answered by
//!   [`DeclarativeApp::on_key`](crate::app::DeclarativeApp::on_key), where there is no widget and no
//!   slot table in hand. So a screen whose *action* softkey means "search now" must have kept the
//!   last `on_change` payload in its model — the message is not a nicety, it is the only channel.
//!   Again, `with_buffer` is the alternative.
//!
//! # Who owns the filtered indices: the caller, always
//!
//! This widget never sees the items. It could not: only the screen knows what a row is, how many
//! there are, and what text a row is searched by. So it offers the query and the matcher —
//! [`SearchField::matches`] from a `view` that holds the widget, and the free
//! [`matching`] from an `update` that holds only the message — and the screen maps indices to rows.
//!
//! Anything else would mean this widget holding a `Vec<usize>` derived from a list it does not own,
//! which is a cache with no way to know when its input changed.
//!
//! # Backspace
//!
//! `Backspace` is consumed by a focused field **even when the query is empty**, which is
//! [`edit::TextField`]'s deliberate behaviour and the right one here: the resolution order is softkey
//! bar → [`OnKey`](super::OnKey) hatches innermost-first → [`FocusScope`](super::FocusScope) → the
//! focused widget, so a `Backspace` that fell through would be caught by whatever hatch encloses the
//! screen and would navigate away from a field the user is typing into.
//!
//! That is a deliberate divergence from [`AppPicker`](symbian_ui::AppPicker), where Backspace on an
//! empty filter cancels the drawer. The drawer is modal and owns every key; this is one widget on a
//! screen that has a softkey bar, and "the way out" is the Back softkey — the key S60 has trained its
//! user to press without reading.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::icon::{self, Icon as Glyph};
use symbian_ui::{chrome, edit, paint, Handled, KeyEvent, Theme};

use crate::constraints::Constraints;
use crate::outbox::Outbox;
use crate::slot::SlotTable;
use crate::spacing::Gap;
use crate::widget::{hash_str, KeyCtx, Widget, WidgetHash};
use crate::widgets::{IconSize, Ink, TextField};

/// The positions in `labels` that pass `query`, in the caller's own order.
///
/// The same function [`SearchField::matches`] calls, exposed as a free one because
/// [`update`](crate::app::DeclarativeApp::update) is where a filter is usually applied and `update`
/// has a message, not a widget. Re-exported rather than rewritten: see the module note on two
/// matchers.
pub use symbian_ui::match_filter::matching_indices as matching;

/// Where a changed query goes: the queue, and the app's own name for "the query became this".
///
/// A named pair rather than the tuple written out, because it is written twice — the field and the
/// [`on_change`](SearchField::on_change) setter — and a boxed closure spelled twice is a boxed
/// closure that can be changed in one place only.
type ChangeSink<M> = (Outbox<M>, Box<dyn Fn(String) -> M>);

/// A one-line query field with a magnifier, announcing every change.
pub struct SearchField<M> {
    /// The query *and* its caret, in one editor — see the module note on why they are not two
    /// things. Shared with the slot table, or with the model when the caller brought its own.
    state: Rc<RefCell<edit::TextField>>,
    focused: bool,
    placeholder: Option<String>,
    /// Where a changed query goes, and how it is spelled in the app's own message type. A closure
    /// and not a bare message, because the payload *is* the query: a `Msg::Search(String)` that
    /// arrived without the string would leave `update` unable to do the one thing it is being told
    /// about.
    on_change: Option<ChangeSink<M>>,
}

impl<M> SearchField<M> {
    /// A field whose query lives in the slot table and is forgotten when the field leaves the screen.
    ///
    /// The ordinary case, and the one S60's own pickers behave like: a search box opens empty.
    pub fn new(slots: &mut SlotTable) -> Self {
        let state = slots.use_state_with(|| Rc::new(RefCell::new(edit::TextField::new()))).clone();
        Self::over(state)
    }

    /// A field over a query the caller keeps.
    ///
    /// For the two cases a slot cannot serve: a query that must survive the field leaving the screen,
    /// and a query `update` must be able to *set* (a "clear search" softkey) rather than only hear
    /// about. The model holds the `Rc` and there is still exactly one buffer and one caret — a handle,
    /// not a copy, which is the whole point.
    pub fn with_buffer(buffer: Rc<RefCell<edit::TextField>>) -> Self {
        Self::over(buffer)
    }

    fn over(state: Rc<RefCell<edit::TextField>>) -> Self {
        Self { state, focused: false, placeholder: None, on_change: None }
    }

    /// Whether this field has the keyboard. Only a focused field edits, shows a caret, or answers a
    /// key — two search boxes on one screen must not both take the same keystroke.
    pub fn focused(mut self, on: bool) -> Self {
        self.focused = on;
        self
    }

    /// Dimmed text shown while the query is empty. The crate ships no English: the caller's word.
    pub fn placeholder(mut self, s: impl Into<String>) -> Self {
        self.placeholder = Some(s.into());
        self
    }

    /// What a changed query means, and where the message goes.
    ///
    /// Fires **only when the text actually changed** — a `Left` that moved the caret is not a new
    /// query. That is not an optimisation: an `update` that re-filtered and reset the selection to
    /// the top on every arrow key would drag the highlight back to row one while the user was trying
    /// to inspect the caret.
    ///
    /// One method for the queue and the message, as [`Switch::out`](super::Switch::out) is: a message
    /// with nowhere to go is a widget that consumes keys and reports nothing.
    pub fn on_change(mut self, out: Outbox<M>, msg: impl Fn(String) -> M + 'static) -> Self {
        self.on_change = Some((out, Box::new(msg)));
        self
    }

    /// The query as typed.
    ///
    /// Allocates, because the buffer lives behind a `RefCell` and cannot be lent past the borrow.
    /// Call it once per frame in `view`, not per row — [`matches`](Self::matches) already does.
    pub fn query(&self) -> String {
        String::from(self.state.borrow().text())
    }

    pub fn is_empty(&self) -> bool {
        self.state.borrow().is_empty()
    }

    /// Byte offset of the caret, always on a `char` boundary.
    pub fn cursor(&self) -> usize {
        self.state.borrow().cursor()
    }

    /// A handle on the query itself, to keep — see [`with_buffer`](Self::with_buffer).
    pub fn buffer(&self) -> Rc<RefCell<edit::TextField>> {
        self.state.clone()
    }

    /// The positions in `labels` that the current query keeps, in the caller's order.
    ///
    /// The screen passes the text a row is searched by and gets back indices into its own list. An
    /// empty query keeps everything, which is what a search box must do on the frame it appears: the
    /// alternative shows an empty list before anything has been typed and reads as "there is nothing
    /// here".
    pub fn matches<'a, I>(&self, labels: I) -> Vec<usize>
    where
        I: IntoIterator<Item = &'a str>,
    {
        matching(self.state.borrow().text(), labels)
    }

    /// The inner field, rebuilt for this pass.
    ///
    /// Composition rather than a second field: the box, the placeholder, the selection and the caret
    /// are [`chrome::text_field`] through [`TextField`](super::TextField), so a search box and a form
    /// field cannot end up being two drawings of one control — the defect that kept the declarative
    /// login screen from being comparable with the one it replaced.
    fn inner(&self) -> TextField {
        let mut f = TextField::with_buffer(self.state.clone()).focused(self.focused);
        if let Some(ph) = &self.placeholder {
            f = f.placeholder(ph.clone());
        }
        f
    }

    /// The band the field occupies inside `rect`, and the magnifier's slot beside it.
    ///
    /// One function for both because `draw` is not the only caller that must agree with it: a rect
    /// taller than the measurement — every `CrossAlign::Stretch` row — has to centre the same band
    /// both times, and two copies of that arithmetic is how a magnifier ends up three pixels above
    /// the text it belongs to.
    fn bands(rect: Rect, theme: &Theme<'_>) -> (Rect, Rect, Rect) {
        let h = chrome::text_field_height(theme).min(rect.height());
        let band = Rect::from_xywh(rect.x0, rect.y0 + (rect.height() - h) / 2, rect.width(), h);
        let iw = icon::width_for(Glyph::Search, IconSize::Medium.resolve(theme));
        // Roles, not pixels: the lead-in is a row's own side margin and the space between a glyph
        // and what it labels is the gap named for exactly that.
        let lead = Gap::Base.resolve(theme);
        let slot = Rect::from_xywh(band.x0 + lead, band.y0, iw, band.height());
        // Clamped into the band, so a field squeezed narrower than its own magnifier draws a short
        // field rather than an inverted rect.
        let text_x = (slot.x1 + Gap::Tight.resolve(theme)).min(band.x1);
        (band, slot, Rect { x0: text_x, ..band })
    }
}

impl<M: 'static> Widget for SearchField<M> {
    fn focus_state(&self) -> Option<bool> {
        Some(self.focused)
    }

    fn content_hash(&self) -> WidgetHash {
        // A constant, and everything is excluded — the query, the caret, the placeholder, the focus,
        // the outbox. None of them can change the box: the field is one line of the body font tall
        // whatever is in it, and as wide as the parent offers. A digest that folded the query in
        // would re-measure the whole row on every keystroke to arrive at the same two numbers, which
        // is precisely the work the cache exists to avoid on the one widget that is typed into.
        //
        // Deliberately *not* what `TextField` does — it folds its text in, on the argument that a
        // parent may have measured around it. A search box's parent cannot: `measure` below never
        // looks at the text.
        //
        // Not zero: zero means "re-measure me every frame", which would put a search screen's
        // layout on the slow path for ever.
        hash_str(0, "search_field")
    }

    fn measure(&self, constraints: Constraints, theme: &Theme<'_>) -> Size {
        // The inner field's own answer, so the two can never disagree about how tall a one-line
        // editor is. The magnifier sits *inside* that height rather than adding to it — a search box
        // that were taller than the form field below it would read as a different control.
        self.inner().measure(constraints, theme)
    }

    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        let (band, slot, field) = Self::bands(rect, theme);
        if band.is_empty() {
            return;
        }
        // The band is painted across the whole width first: the inner field paints only its own
        // part, and without this the magnifier's gutter would show whatever was behind the screen.
        paint::band(c, band, &theme.palette.chrome);
        if slot.x1 <= band.x1 {
            // Accent while the field holds the keyboard, dim otherwise. The affordance has to be a
            // *shape and a colour* rather than a word, because the one thing a 320x240 screen cannot
            // spare is a line of text explaining that a text box is for searching.
            let ink = if self.focused { Ink::Accent } else { Ink::Dim };
            icon::draw(c, slot, Glyph::Search, ink.resolve(theme));
        }
        if !field.is_empty() {
            self.inner().draw(c, field, theme);
        }
    }

    fn handle_key(&self, ev: KeyEvent, rect: Rect, cx: &mut KeyCtx<'_>) -> Handled {
        // `edit` refuses when unfocused, so an unfocused field cannot type and cannot announce
        // anything either — both fields on a two-field screen are handed every key.
        let text_before: Option<String> =
            self.on_change.as_ref().map(|_| String::from(self.state.borrow().text()));
        let (_, _, field) = Self::bands(rect, cx.theme);
        let handled = self.inner().handle_key(ev, field, cx);
        if let (Some((out, msg)), Some(was)) = (&self.on_change, text_before) {
            // Compared as strings and pushed as one: the borrow has to end before `push` runs,
            // because a message this widget sends could reach an `update` that reads the buffer back.
            let now = String::from(self.state.borrow().text());
            if now != was {
                // Only a real change wakes `update`. A caret that moved is not a new query, and an
                // `update` that reset the selection anyway would pull the highlight back to the top
                // row under the user's thumb.
                out.push(msg(now));
            }
        }
        handled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::with_key_ctx;
    use symbian_gfx::Size as GSize;
    use symbian_ui::{testing, Key, Palette};

    const W: i32 = 200;
    const H: i32 = 40;
    const RECT: Rect = Rect { x0: 0, y0: 0, x1: W, y1: H };

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Msg {
        Query(String),
    }

    const APPS: [&str; 6] = ["Calculator", "Calendar", "Camera", "Maps", "Messaging", "Web"];

    fn labels() -> impl Iterator<Item = &'static str> {
        APPS.iter().copied()
    }

    fn press(f: &SearchField<Msg>, key: Key) -> Handled {
        with_key_ctx(|cx| f.handle_key(KeyEvent::new(key), RECT, cx))
    }

    fn type_str(f: &SearchField<Msg>, s: &str) {
        for ch in s.chars() {
            press(f, Key::Char(ch));
        }
    }

    /// Draw it the way a frame does. Also the only way to catch a bad caret offset: the panic
    /// happens where the string is sliced, which is in the draw and not in the key handler.
    fn frame(f: &SearchField<Msg>) -> Vec<u16> {
        let (_, px) = testing::with_canvas(GSize::new(W, H), |c| {
            testing::with_theme(Palette::DARK, |t| f.draw(c, RECT, t));
        });
        px
    }

    #[test]
    fn typing_changes_the_query_and_the_query_survives_the_tree_being_rebuilt() {
        // Every keystroke invalidates the view, so a mid-word rebuild is the normal case. A query
        // held in this struct would be back to empty on the next frame and the box would type
        // backwards.
        let mut slots = SlotTable::new();

        slots.begin_frame();
        let f: SearchField<Msg> = SearchField::new(&mut slots).focused(true);
        type_str(&f, "cal");
        assert_eq!(f.query(), "cal");
        drop(f);

        slots.begin_frame();
        let f: SearchField<Msg> = SearchField::new(&mut slots).focused(true);
        assert_eq!(f.query(), "cal", "a rebuilt field must not forget what was typed");
        assert_eq!(f.cursor(), 3, "nor where the caret was");
    }

    #[test]
    fn backspace_deletes_and_never_escapes_to_the_enclosing_scope() {
        // The resolution order puts the focused widget last, so a `Backspace` this field handed back
        // would be caught by the screen's own hatch and navigate away from a box being typed into.
        // It must be consumed even with nothing left to delete.
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let f: SearchField<Msg> = SearchField::new(&mut slots).focused(true);
        type_str(&f, "cam");

        assert_eq!(press(&f, Key::Backspace), Handled::Consumed);
        assert_eq!(f.query(), "ca");
        press(&f, Key::Backspace);
        press(&f, Key::Backspace);
        assert!(f.is_empty());
        assert_eq!(
            press(&f, Key::Backspace),
            Handled::Consumed,
            "an empty search box still owns Backspace"
        );
    }

    #[test]
    fn the_filter_answers_indices_into_the_callers_own_list() {
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let f: SearchField<Msg> = SearchField::new(&mut slots).focused(true);

        // Empty query: everything, in the caller's order.
        assert_eq!(f.matches(labels()), alloc::vec![0, 1, 2, 3, 4, 5]);

        type_str(&f, "cal");
        assert_eq!(f.matches(labels()), alloc::vec![0, 1], "Calculator and Calendar, not Camera");

        // Substring and case-blind, which is the picker's rule and must stay one rule. A fresh box,
        // because the point is what a query finds and not how it was erased.
        let mut slots2 = SlotTable::new();
        slots2.begin_frame();
        let g: SearchField<Msg> = SearchField::new(&mut slots2).focused(true);
        type_str(&g, "AME");
        assert_eq!(g.matches(labels()), alloc::vec![2], "Camera, matched in the middle");

        // No matches is an empty answer, not the whole list.
        type_str(&g, "zzz");
        assert!(g.matches(labels()).is_empty());
    }

    #[test]
    fn the_widget_and_the_toolkit_never_disagree_about_a_match() {
        // The point of reusing `match_filter`: the picker and this box answer the same three letters
        // the same way. If they ever diverge, a user sees different apps in two lists and nothing on
        // screen says which was right.
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let f: SearchField<Msg> = SearchField::new(&mut slots).focused(true);
        type_str(&f, "ca");
        assert_eq!(f.matches(labels()), matching("ca", labels()));
        assert_eq!(f.matches(labels()), matching(&f.query(), labels()));
    }

    #[test]
    fn an_unfocused_field_leaves_the_keys_alone() {
        // Two search boxes on one screen, one keystroke. Without the flag both would type.
        let out: Outbox<Msg> = Outbox::new();
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let f = SearchField::new(&mut slots).on_change(out.clone(), Msg::Query);

        assert_eq!(press(&f, Key::Char('c')), Handled::Ignored);
        assert_eq!(press(&f, Key::Backspace), Handled::Ignored);
        assert!(f.is_empty());
        assert!(out.is_empty(), "and it announces nothing it did not do");
    }

    #[test]
    fn the_navigation_keys_a_screen_needs_fall_through() {
        // What lets the cursor leave the box and the action softkey fire. A search field that
        // consumed `Down` would trap the user in it with a full list underneath.
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let f: SearchField<Msg> = SearchField::new(&mut slots).focused(true);
        type_str(&f, "ca");

        for key in [Key::Up, Key::Down, Key::Select, Key::Enter] {
            assert_eq!(press(&f, key), Handled::Ignored, "{key:?}");
        }
        assert_eq!(f.query(), "ca", "and none of them edited it");
    }

    #[test]
    fn every_real_change_tells_update_what_the_query_became() {
        // The only channel the model has: `on_key` and `update` cannot reach a slot, so a screen
        // that must reset its selection when the list narrows learns about it here or not at all.
        let out: Outbox<Msg> = Outbox::new();
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let f = SearchField::new(&mut slots).focused(true).on_change(out.clone(), Msg::Query);

        type_str(&f, "ca");
        press(&f, Key::Backspace);
        assert_eq!(
            out.take(),
            alloc::vec![
                Msg::Query(String::from("c")),
                Msg::Query(String::from("ca")),
                Msg::Query(String::from("c")),
            ]
        );
        assert_eq!(out.dropped(), 0);
    }

    #[test]
    fn a_key_that_did_not_change_the_query_does_not_wake_update() {
        // An `update` that re-filtered on every arrow key would reset the selection to the top row
        // while the user was only moving the caret.
        let out: Outbox<Msg> = Outbox::new();
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let f = SearchField::new(&mut slots).focused(true).on_change(out.clone(), Msg::Query);
        type_str(&f, "ca");
        let _ = out.take();

        press(&f, Key::Left);
        press(&f, Key::Right);
        press(&f, Key::Up);
        assert!(out.is_empty(), "the caret moved; the query did not");
        assert_eq!(f.query(), "ca");
    }

    #[test]
    fn a_query_the_model_owns_outlives_the_field() {
        // The escape hatch the module docs name: a slot forgets when the field leaves the screen,
        // and a saved search must not.
        let buffer = Rc::new(RefCell::new(edit::TextField::new()));
        {
            let f: SearchField<Msg> = SearchField::with_buffer(buffer.clone()).focused(true);
            type_str(&f, "cam");
        }
        assert_eq!(buffer.borrow().text(), "cam", "no field on screen and the query is still there");

        // And it is a handle, not a copy: what `update` writes is what the next field shows.
        buffer.borrow_mut().set_text("web");
        let again: SearchField<Msg> = SearchField::with_buffer(buffer.clone()).focused(true);
        assert_eq!(again.query(), "web");
        assert_eq!(again.matches(labels()), alloc::vec![5]);
    }

    #[test]
    fn typing_and_deleting_accented_text_never_splits_a_char() {
        // The hazard that panics rather than misbehaving. Every step is drawn, because the draw is
        // where a caret left off a boundary actually slices the string.
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let f: SearchField<Msg> = SearchField::new(&mut slots).focused(true);

        type_str(&f, "Ação");
        assert_eq!(f.query(), "Ação");
        assert_eq!(f.cursor(), 6, "ç and ã are two bytes each");
        frame(&f);

        press(&f, Key::Backspace);
        assert_eq!(f.query(), "Açã", "one whole char, not one byte of one");
        assert_eq!(f.cursor(), 5, "A + ç + ã is five bytes");
        frame(&f);

        let mut slots2 = SlotTable::new();
        slots2.begin_frame();
        let g: SearchField<Msg> = SearchField::new(&mut slots2).focused(true);
        type_str(&g, "Привет");
        assert_eq!(g.cursor(), 12);
        frame(&g);
        // Walk the caret to the start and back out again, drawing at every stop.
        for _ in 0..8 {
            press(&g, Key::Left);
            frame(&g);
        }
        assert_eq!(g.cursor(), 0);
        for _ in 0..8 {
            press(&g, Key::Right);
            frame(&g);
        }
        assert_eq!(g.cursor(), 12, "walking right stops at the end rather than running past it");
        assert_eq!(g.matches(["Привет", "Web"].into_iter()), alloc::vec![0]);
    }

    #[test]
    fn the_digest_is_constant_and_not_zero() {
        // Constant because nothing about a search box changes its box: one line tall, as wide as it
        // is offered. Not zero, because zero means "re-measure me every frame" — on the one widget
        // that gets a key event per character.
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let empty: SearchField<Msg> = SearchField::new(&mut slots);
        let typed: SearchField<Msg> = SearchField::new(&mut slots).focused(true);
        type_str(&typed, "a much longer query than fits in the box");

        assert_eq!(empty.content_hash(), typed.content_hash());
        assert_ne!(empty.content_hash(), 0);
    }

    #[test]
    fn it_is_one_line_tall_whatever_is_in_it_and_as_wide_as_it_is_offered() {
        // A box that grew with its query would reflow the list underneath it while the user typed.
        testing::with_theme(Palette::DARK, |t| {
            let mut slots = SlotTable::new();
            slots.begin_frame();
            let f: SearchField<Msg> = SearchField::new(&mut slots).focused(true);
            let empty = f.measure(Constraints::loose(W, 400), t);
            assert_eq!(empty, Size::new(W, chrome::text_field_height(t)));
            type_str(&f, "a much longer query than fits in the box");
            assert_eq!(f.measure(Constraints::loose(W, 400), t), empty);
        });
    }

    #[test]
    fn the_magnifier_is_actually_painted() {
        // The test theme's atlas has one glyph in it, so no pixel test here can see typography. The
        // magnifier is geometry — `symbian_ui::icon` draws it with lines, no font — so it *is*
        // visible, and the negative control is the same field with the same query drawn by the plain
        // `TextField` over the same rect. If the two ever match, the icon stopped being drawn.
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let f: SearchField<Msg> = SearchField::new(&mut slots).focused(true);

        let mine = frame(&f);
        let (_, plain) = testing::with_canvas(GSize::new(W, H), |c| {
            testing::with_theme(Palette::DARK, |t| {
                TextField::with_buffer(f.buffer()).focused(true).draw(c, RECT, t)
            });
        });
        assert_ne!(mine, plain, "a search box without a magnifier is just a text field");

        // And the control's control: the same draw twice is the same pixels, so the assertion above
        // is about the icon and not about draw being unstable.
        assert_eq!(mine, frame(&f));
    }

    #[test]
    fn focus_is_visible_without_a_single_glyph() {
        // The atlas cannot show a caret made of letters, but the caret and the magnifier's ink are
        // both geometry. A focused box must look different from an unfocused one, or the user cannot
        // tell where the keyboard is.
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let on: SearchField<Msg> = SearchField::new(&mut slots).focused(true);
        let mut slots2 = SlotTable::new();
        slots2.begin_frame();
        let off: SearchField<Msg> = SearchField::new(&mut slots2).focused(false);
        assert_ne!(frame(&on), frame(&off));
    }

    #[test]
    fn a_taller_rect_does_not_stretch_the_field() {
        // `CrossAlign::Stretch` hands this widget the whole band a column leaves it, not the one line
        // it measured. Drawing into the rect directly would give a 40-pixel-tall search box with its
        // magnifier floating in the middle of it.
        let field_h = testing::with_theme(Palette::DARK, chrome::text_field_height);
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let f: SearchField<Msg> = SearchField::new(&mut slots).focused(true);

        let (_, px) = testing::with_canvas(GSize::new(W, H), |c| {
            testing::with_theme(Palette::DARK, |t| {
                c.clear(Palette::DARK.bg.mid());
                f.draw(c, RECT, t);
            });
        });
        let bg = Palette::DARK.bg.mid().to_rgb565().0;
        let painted: Vec<i32> =
            (0..H).filter(|&y| (0..W).any(|x| px[(y * W + x) as usize] != bg)).collect();
        let top = (H - field_h) / 2;
        assert!(!painted.is_empty(), "something has to be drawn");
        // Containment rather than an exact row count: the band is shaded, so its topmost row can
        // legitimately be the same colour as the page behind it. What must not happen is ink outside
        // the one-line band — that is what a stretched field looks like.
        assert!(
            painted.iter().all(|&y| y >= top && y < top + field_h),
            "ink outside the centred band: rows {painted:?}, band {top}..{}",
            top + field_h
        );
        assert!(
            *painted.last().unwrap() - painted[0] >= field_h - 3,
            "and it fills that band rather than sitting in a corner of it"
        );
    }

    #[test]
    fn it_draws_in_every_palette_and_in_a_rect_too_narrow_for_itself() {
        // The narrow case is not decorative: a search box inside a `Row` with a flexed sibling can be
        // handed fewer pixels than its own magnifier, and the clamped text rect is what keeps that
        // from being an inverted rectangle.
        for (name, palette) in Palette::ALL {
            let mut slots = SlotTable::new();
            slots.begin_frame();
            let f: SearchField<Msg> = SearchField::new(&mut slots).focused(true);
            type_str(&f, "ca");
            let (_, px) = testing::with_canvas(GSize::new(W, H), |c| {
                testing::with_theme(palette, |t| f.draw(c, RECT, t));
            });
            assert!(px.iter().any(|&v| v != 0), "{name}: nothing was painted");

            testing::with_canvas(GSize::new(W, H), |c| {
                testing::with_theme(palette, |t| {
                    f.draw(c, Rect::from_xywh(0, 0, 4, H), t);
                    f.draw(c, Rect::from_xywh(0, 0, W, 1), t);
                    f.draw(c, Rect::from_xywh(0, 0, 0, 0), t);
                });
            });
        }
    }
}
