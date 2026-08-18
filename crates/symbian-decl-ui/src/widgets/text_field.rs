//! A single-line editor, declared.
//!
//! # This file contains no editing arithmetic either
//!
//! [`symbian_ui::edit::TextField`] already holds a caret that is always on a `char` boundary, and
//! every movement it offers steps by whole `char`s so that no operation can panic on a slice. That
//! is not a detail to reproduce: get it wrong and typing in Cyrillic or emoji does not misbehave,
//! it *panics*, on a device whose entire failure report is a dialog with a number in it. This
//! widget owns the caret's lifetime and its pixels, and forwards every edit.
//!
//! # The caret is the slot table's second reason to exist
//!
//! [`crate::slot`] names two things that must not be in the app model: a list's scroll offset and
//! a text field's caret. A caret is not application state — it is a consequence of having drawn a
//! field here last frame — and yet rebuilding the tree must not send it back to zero mid-word.
//! So the editor lives in a slot and this struct holds a handle to it.
//!
//! # Why the character limit is a constructor and the mask is a setter
//!
//! [`edit::TextField`] takes its limit at construction and has no way to change it afterwards,
//! which is right: a limit that could move would let a field hold more than it promises and then
//! refuse to let the user fix it. So a limited field is [`TextField::with_limit`], applied once
//! when the slot is first filled. Masking *does* have a setter, and is re-applied every frame,
//! because a login screen legitimately toggles "show password".

use alloc::rc::Rc;
use alloc::string::String;
use core::cell::RefCell;

use symbian_gfx::{Canvas, Rect, Size};
use symbian_ui::{chrome, edit, Clipboard, Handled, KeyEvent, Theme};

use crate::constraints::Constraints;
use crate::slot::SlotTable;
use crate::widget::{hash_i32, hash_str, KeyCtx, Widget, WidgetHash};

/// A one-line text input backed by [`edit::TextField`].
pub struct TextField {
    /// Shared with the slot table. `RefCell` and not `Cell` here — unlike a list's state, an
    /// editor owns a `String` and is not `Copy`, so it cannot be swapped out by value.
    state: Rc<RefCell<edit::TextField>>,
    focused: bool,
    masked: bool,
    placeholder: Option<String>,
    /// Drawn before the text and not part of it. See [`TextField::prefix`].
    prefix: Option<String>,
}

impl TextField {
    /// An unlimited field.
    pub fn new(slots: &mut SlotTable) -> Self {
        Self::from_slot(slots, edit::TextField::new)
    }

    /// A field that will hold at most `max_chars` characters.
    ///
    /// Counted in characters rather than bytes, which is [`edit::TextField`]'s own decision and
    /// the right one: a byte limit behaves differently for Cyrillic than for Latin, which a user
    /// experiences as the field being broken rather than as being full.
    pub fn with_limit(slots: &mut SlotTable, max_chars: usize) -> Self {
        Self::from_slot(slots, move || edit::TextField::with_limit(max_chars))
    }

    /// A field over a buffer the caller already owns.
    ///
    /// # When a slot is not enough
    ///
    /// A caret belongs in the slot table — [`crate::slot`] argues that at length and it is right for
    /// every field whose text nobody outside asks about. A login screen is the exception, and the
    /// reason is the *submit key*: it is a softkey, so it is answered by
    /// [`DeclarativeApp::on_key`](crate::app::DeclarativeApp::on_key) and turned into a message, and
    /// by the time `update` runs there is no widget in hand and no `view` to read one from. A slot
    /// cannot be reached from `update`; an `Rc` the model holds can.
    ///
    /// So the app creates the buffer, keeps the handle, and hands a clone here. There is still one
    /// buffer and one caret — this is a handle, not a copy, which is what stops the two from drifting
    /// the way a model holding a `String` beside a field would.
    ///
    /// The trade is lifetime: a slot forgets when the field leaves the screen and this does not. For
    /// a login form that is the behaviour you want — the number survives the transition to the code
    /// screen and back. For a search box that should start empty each time, use
    /// [`new`](Self::new) and let the slot do its job.
    pub fn with_buffer(buffer: Rc<RefCell<edit::TextField>>) -> Self {
        let masked = buffer.borrow().is_masked();
        Self { state: buffer, focused: false, masked, placeholder: None, prefix: None }
    }

    fn from_slot(slots: &mut SlotTable, initial: impl FnOnce() -> edit::TextField) -> Self {
        let state = slots
            .use_state_with(|| Rc::new(RefCell::new(initial())))
            .clone();
        let masked = state.borrow().is_masked();
        Self { state, focused: false, masked, placeholder: None, prefix: None }
    }

    /// Whether this field has the keyboard. Only a focused field edits or shows a caret.
    pub fn focused(mut self, on: bool) -> Self {
        self.focused = on;
        self
    }

    /// Show `*` instead of the characters. Re-applied every frame, so a "show password" toggle in
    /// the model reaches the editor without the field being rebuilt from scratch.
    pub fn masked(mut self, on: bool) -> Self {
        self.masked = on;
        self.state.borrow_mut().set_masked(on);
        self
    }

    /// Dimmed text shown while the field is empty.
    pub fn placeholder(mut self, s: impl Into<String>) -> Self {
        self.placeholder = Some(s.into());
        self
    }

    /// Dimmed text drawn before the contents and *not* part of them.
    ///
    /// The fixed `+` of a phone number is the case this exists for, and the distinction matters: the
    /// login screen draws it and does not store it, so a pasted `+55 21 99999-0000` leaves the digits
    /// alone rather than arriving with a second plus in front of it.
    pub fn prefix(mut self, s: impl Into<String>) -> Self {
        self.prefix = Some(s.into());
        self
    }

    /// The real contents, mask or no mask.
    ///
    /// Allocates, because the buffer lives behind a `RefCell` and cannot be lent out past the
    /// borrow. Call it when the app submits, not once per frame.
    pub fn text(&self) -> String {
        String::from(self.state.borrow().text())
    }

    /// Take the contents and leave the field empty — what a composer does on send.
    ///
    /// Goes through [`edit::TextField::take`] rather than reading and clearing, because on a
    /// masked field that is the call that actually clears the password out of the buffer.
    pub fn take(&self) -> String {
        self.state.borrow_mut().take()
    }

    pub fn is_empty(&self) -> bool {
        self.state.borrow().is_empty()
    }

    /// Byte offset of the caret, always on a `char` boundary.
    pub fn cursor(&self) -> usize {
        self.state.borrow().cursor()
    }

    /// A handle on the buffer itself, to keep.
    ///
    /// The way an app reads what was typed *without* the tree being rebuilt. A login screen needs
    /// the number when the user presses Send, and by then no `view` has run since the last
    /// keystroke — the buffer lives in the slot table and the widget in hand is one frame old.
    /// Holding the `Rc` from the frame that created it answers at any time.
    ///
    /// This is the shape a caret forces. The text is not application state: it is a consequence of
    /// keys arriving at a field that is on screen, it must survive the tree being rebuilt, and it
    /// must *not* survive the field going away — which is exactly what a slot is. The model holding
    /// a copy instead would be two buffers with one caret between them, and the copy would be the
    /// stale one every time.
    pub fn buffer(&self) -> Rc<RefCell<edit::TextField>> {
        self.state.clone()
    }

    /// Offer a key to the editor. Ignored unless focused, so two fields on one screen do not both
    /// consume the same keystroke.
    ///
    /// The clipboard comes from the caller, as it does for the buffer underneath — see
    /// [`symbian_ui::clip`] for why the toolkit will not hold one itself. Pass
    /// `&mut symbian_app::SystemClipboard` on a device and `&mut NoClipboard` where there is none.
    pub fn edit(&self, ev: KeyEvent, clip: &mut dyn Clipboard) -> Handled {
        if !self.focused {
            return Handled::Ignored;
        }
        self.state.borrow_mut().handle_key(ev, clip)
    }
}

impl Widget for TextField {
    fn content_hash(&self) -> WidgetHash {
        // The text is part of the digest because the field's width does not change with it but a
        // parent that measured around it may care; the caret is not, because moving it never
        // resizes anything and re-measuring on every arrow key is exactly the cost the cache is
        // there to avoid.
        let f = self.state.borrow();
        let h = hash_str(0, f.text());
        hash_i32(h, self.masked as i32)
    }

    fn measure(&self, constraints: Constraints, theme: &Theme<'_>) -> Size {
        // One line of the body font plus the vertical padding either side, asked of the toolkit so
        // that the height measured here and the height drawn there cannot disagree. The width is the
        // parent's to give: a field that asked for its text width would grow as you typed and shrink
        // as you deleted, which is not how a form behaves.
        constraints.constrain(Size::new(constraints.max_w, chrome::text_field_height(theme)))
    }

    /// Nothing of its own: the box, the prefix, the text or its mask, the selection and the caret are
    /// [`chrome::text_field`].
    ///
    /// This used to be drawn here — a stroked rectangle, no prefix, no selection, a caret centred in
    /// the box — and `tg`'s login screens drew a different field by hand. Both were reasonable, and
    /// the two could never agree, so the declarative login screen could never have been compared
    /// against the one it replaces. The pixels moved into the toolkit and this widget places them.
    fn draw(&self, c: &mut Canvas<'_>, rect: Rect, theme: &Theme<'_>) {
        let f = self.state.borrow();
        chrome::text_field(
            c,
            rect,
            theme,
            &f,
            chrome::FieldStyle {
                prefix: self.prefix.as_deref(),
                placeholder: self.placeholder.as_deref(),
                focused: self.focused,
            },
        );
    }

    fn handle_key(&self, ev: KeyEvent, _rect: Rect, cx: &mut KeyCtx<'_>) -> Handled {
        // The clipboard rides in the context, so this path pastes exactly like the imperative one.
        // It used to hand over `NoClipboard` — the trait carried nowhere to get one from — which
        // meant a declarative field was the one field on the phone that could not paste.
        self.edit(ev, cx.clip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_ui::NoClipboard;
    use symbian_gfx::Size as GSize;
    use symbian_ui::{testing, Key, Palette};

    const W: i32 = 200;
    const H: i32 = 30;

    fn press(k: Key) -> KeyEvent {
        KeyEvent::new(k)
    }

    fn type_str(f: &TextField, s: &str) {
        for ch in s.chars() {
            f.edit(press(Key::Char(ch)), &mut NoClipboard);
        }
    }

    /// Draw it the way a frame does, which is also the only way to catch a panic in the caret
    /// arithmetic — the multi-byte hazard shows up when the draw slices the string.
    fn frame(f: &TextField) {
        testing::with_theme(Palette::DARK, |t| {
            let mut buf = alloc::vec![0u16; (W * H) as usize];
            let mut c = Canvas::from_slice(&mut buf, GSize::new(W, H));
            f.draw(&mut c, Rect::from_xywh(0, 0, W, H), t);
        });
    }

    #[test]
    fn the_caret_and_the_text_survive_the_tree_being_rebuilt() {
        // Mid-word rebuilds are the normal case, not an edge one: every keystroke invalidates the
        // view. If the caret lived in this struct it would return to zero on the next frame and
        // the field would type backwards.
        let mut slots = SlotTable::new();

        slots.begin_frame();
        let f = TextField::new(&mut slots).focused(true);
        type_str(&f, "hel");
        assert_eq!(f.cursor(), 3);
        drop(f);

        slots.begin_frame();
        let f = TextField::new(&mut slots).focused(true);
        assert_eq!(f.text(), "hel", "a rebuilt field must not forget what was typed");
        assert_eq!(f.cursor(), 3, "nor where the caret was");
        type_str(&f, "lo");
        assert_eq!(f.text(), "hello");
    }

    #[test]
    fn typing_at_a_multibyte_boundary_never_splits_a_char() {
        // edit.rs's own hazard case, driven through the widget and then drawn — the draw is where
        // a bad offset would actually slice the string and panic.
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let f = TextField::new(&mut slots).focused(true);

        type_str(&f, "Привет");
        assert_eq!(f.cursor(), 12, "Cyrillic is two bytes per char");
        frame(&f);

        f.edit(press(Key::Backspace), &mut NoClipboard);
        assert_eq!(f.text(), "Приве");
        assert_eq!(f.cursor(), 10);
        frame(&f);

        // Walk the caret to the start and back out again, drawing at every stop. A caret left off
        // a boundary panics in the draw, not here, which is why every step is drawn.
        for _ in 0..8 {
            f.edit(press(Key::Left), &mut NoClipboard);
            frame(&f);
        }
        assert_eq!(f.cursor(), 0);
        for _ in 0..8 {
            f.edit(press(Key::Right), &mut NoClipboard);
            frame(&f);
        }
        assert_eq!(f.cursor(), 10, "walking right stops at the end rather than running past it");
    }

    #[test]
    fn a_buffer_the_app_owns_survives_the_field_leaving_the_screen() {
        // The login screen's case. The app holds the `Rc`, so it can read what was typed from
        // `update` — where there is no widget and no slot table — and the text survives a transition
        // that a slot would have reclaimed.
        let buffer = Rc::new(RefCell::new(edit::TextField::with_limit(16)));
        {
            let f = TextField::with_buffer(buffer.clone()).focused(true);
            type_str(&f, "5521");
            assert_eq!(f.text(), "5521");
        }
        // No field on screen at all for a frame, and the digits are still there.
        assert_eq!(buffer.borrow().text(), "5521");

        let again = TextField::with_buffer(buffer.clone()).focused(true);
        type_str(&again, "9");
        assert_eq!(again.text(), "55219", "the second field typed into the same buffer");
        // And it is a handle, not a copy: the app's view of it moved too.
        assert_eq!(buffer.borrow().text(), "55219");
    }

    #[test]
    fn a_buffer_that_is_already_masked_says_so_without_being_told() {
        // The mask lives on the editor, so a field built over an existing buffer must not silently
        // reveal a password by defaulting to unmasked.
        let buffer = Rc::new(RefCell::new(edit::TextField::new()));
        buffer.borrow_mut().set_masked(true);
        buffer.borrow_mut().insert_str("hunter2");
        let f = TextField::with_buffer(buffer.clone());
        assert_eq!(buffer.borrow().display(), "*******");
        assert_eq!(f.text(), "hunter2");
        frame(&f);
    }

    #[test]
    fn two_fields_on_one_screen_keep_their_own_text() {
        // Positional slot identity, and the failure it prevents: a login screen whose password
        // box echoes the phone number.
        let mut slots = SlotTable::new();

        slots.begin_frame();
        let phone = TextField::new(&mut slots).focused(true);
        let code = TextField::new(&mut slots).focused(true);
        type_str(&phone, "555");
        type_str(&code, "12");

        assert_eq!(phone.text(), "555");
        assert_eq!(code.text(), "12");
        drop((phone, code));

        // And they stay apart across a rebuild, in the same order.
        slots.begin_frame();
        let phone = TextField::new(&mut slots);
        let code = TextField::new(&mut slots);
        assert_eq!(phone.text(), "555");
        assert_eq!(code.text(), "12");
        assert_eq!(slots.type_mismatches(), 0);
    }

    #[test]
    fn only_the_focused_field_takes_the_keystroke() {
        // Both fields are on screen and both are handed the key; exactly one may act on it.
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let a = TextField::new(&mut slots).focused(true);
        let b = TextField::new(&mut slots).focused(false);

        assert_eq!(a.edit(press(Key::Char('x')), &mut NoClipboard), Handled::Consumed);
        assert_eq!(b.edit(press(Key::Char('x')), &mut NoClipboard), Handled::Ignored);
        assert_eq!(a.text(), "x");
        assert!(b.is_empty());
    }

    #[test]
    fn the_keys_a_screen_needs_are_left_alone() {
        // edit.rs deliberately ignores Up/Down/Select so a composer can move focus back into the
        // transcript and the action softkey can send. A widget that consumed them would trap the
        // user in the field.
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let f = TextField::new(&mut slots).focused(true);

        assert_eq!(f.edit(press(Key::Up), &mut NoClipboard), Handled::Ignored);
        assert_eq!(f.edit(press(Key::Down), &mut NoClipboard), Handled::Ignored);
        assert_eq!(f.edit(press(Key::Select), &mut NoClipboard), Handled::Ignored);
    }

    #[test]
    fn a_masked_field_draws_stars_and_still_submits_the_password() {
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let f = TextField::new(&mut slots).focused(true).masked(true);
        type_str(&f, "hunter2");

        assert_eq!(f.text(), "hunter2", "the app must still be able to submit it");
        frame(&f);

        // And `take` clears the real buffer, not just the mask — so the only copy of the password
        // is gone once the app has it.
        assert_eq!(f.take(), "hunter2");
        assert!(f.is_empty());
    }

    #[test]
    fn a_mask_toggled_from_the_model_reaches_the_editor() {
        // "Show password" flips a field that already has text in it, so the mask must be applied
        // on rebuild rather than only when the slot is first filled.
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let f = TextField::new(&mut slots).focused(true).masked(true);
        type_str(&f, "abc");
        drop(f);

        slots.begin_frame();
        let f = TextField::new(&mut slots).focused(true).masked(false);
        assert_eq!(f.text(), "abc");
        frame(&f);
    }

    #[test]
    fn a_limit_is_counted_in_characters_and_set_once() {
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let f = TextField::with_limit(&mut slots, 3).focused(true);
        type_str(&f, "ЖЖЖЖЖ");
        assert_eq!(f.text(), "ЖЖЖ", "a limit in bytes would cut Cyrillic at a different place");
        drop(f);

        // The limit is part of the slot's initial value, so it survives without being re-applied.
        slots.begin_frame();
        let f = TextField::with_limit(&mut slots, 3).focused(true);
        type_str(&f, "x");
        assert_eq!(f.text(), "ЖЖЖ", "still full after a rebuild");
    }

    #[test]
    fn an_empty_field_shows_its_placeholder_and_a_full_one_does_not() {
        let mut slots = SlotTable::new();
        slots.begin_frame();
        let f = TextField::new(&mut slots).placeholder("Phone number");
        assert!(f.is_empty());
        frame(&f);

        let f = TextField::new(&mut slots).focused(true).placeholder("Phone number");
        type_str(&f, "5");
        frame(&f);
        assert_eq!(f.text(), "5");
    }

    #[test]
    fn a_field_is_one_line_tall_whatever_is_in_it() {
        // A field that grew with its text would reflow the form under the user's thumb.
        testing::with_theme(Palette::DARK, |t| {
            let mut slots = SlotTable::new();
            slots.begin_frame();
            let f = TextField::new(&mut slots).focused(true);
            let empty = f.measure(Constraints::loose(W, 400), t);
            type_str(&f, "a much longer piece of text than fits");
            assert_eq!(f.measure(Constraints::loose(W, 400), t), empty);
        });
    }
}
