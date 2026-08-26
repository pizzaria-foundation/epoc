//! Single-line text editing.
//!
//! Worth having in the toolkit rather than leaning on Avkon's `CEikEdwin`: we
//! already own the pixels, and the window server hands us a fully translated
//! character in `TKeyEvent::iCode` with Shift, Caps Lock and the Fn layer already
//! applied. On a QWERTY device that stream *is* text input. What we give up is
//! predictive entry (irrelevant here) and the CJK front-end processor.
//!
//! The cursor is a byte offset that is always on a `char` boundary. Every
//! movement steps by whole `char`s, so no operation can panic on a slice.
//!
//! # Selection and the clipboard
//!
//! Shift with an arrow key selects, and `Ctrl+C`/`Ctrl+X`/`Ctrl+V`/`Ctrl+A` do what they do on
//! every other computer the user owns. That is the argument for having them: the phone's own
//! editors have had these bindings since before this SDK existed, so a field of ours without them
//! is the one that feels broken.
//!
//! They live *here*, in the buffer, rather than in each application, because there is exactly one
//! place every text field in every app funnels through — [`TextField::handle_key`] — and a default
//! that has to be re-implemented per screen is not a default. The clipboard itself arrives as an
//! argument; see [`crate::clip`] for why it is not a global.
//!
//! The selection is one extra field, [`TextField::anchor`]: where the selection started, with the
//! caret as its other end. Text is selected when the two differ, which means "no selection" and
//! "an empty selection" cannot drift apart the way a separate flag would let them.

use alloc::string::String;

use crate::clip::Clipboard;
use crate::input::{Handled, Key, KeyEvent};

#[derive(Clone, Debug, Default)]
pub struct TextField {
    text: String,
    cursor: usize,
    /// Where a Shift-selection started, as a byte offset on a `char` boundary. `None` means no
    /// selection; equal to [`Self::cursor`] means the user shrank one back to nothing, which is
    /// treated as none everywhere.
    anchor: Option<usize>,
    /// Cap in `char`s, not bytes — a limit in bytes would behave differently for
    /// Cyrillic than for Latin, which users would experience as a bug.
    max_chars: Option<usize>,
    /// Which characters this field takes at all, if it is fussy — a phone-number field wants
    /// digits and nothing else.
    ///
    /// It lives on the field rather than in the screen that owns it, and that is the whole point:
    /// the filter used to sit in the caller, in front of `handle_key`, where it only ever saw
    /// keystrokes. Pasted text does not arrive as keystrokes, so it walked straight past — a
    /// digits-only field would happily hold a pasted street address.
    accept: Option<fn(char) -> bool>,
    /// When true, [`text`] returns `*` repeated for every character instead of the
    /// real contents. The underlying buffer is still the password and is cleared by
    /// [`take`], so a shadow copy in the screen is never needed and the only place
    /// the password lives is here.
    masked: bool,
    /// When true the field is a multi-line text area: `\n` is a legal character, Enter inserts one,
    /// and Up/Down move the caret between lines. Default false keeps every existing field single-line.
    multiline: bool,
}

impl TextField {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_limit(max_chars: usize) -> Self {
        Self { max_chars: Some(max_chars), ..Self::default() }
    }

    /// Take only the characters `accept` approves — typed *or* pasted.
    ///
    /// ```ignore
    /// let mut phone = TextField::with_limit(20).accepting(|c| c.is_ascii_digit());
    /// ```
    pub fn accepting(mut self, accept: fn(char) -> bool) -> Self {
        self.accept = Some(accept);
        self
    }

    /// Make this a multi-line text area (Enter inserts a newline; Up/Down move between lines).
    pub fn multiline(mut self, on: bool) -> Self {
        self.multiline = on;
        self
    }

    /// Whether this field is a multi-line text area.
    pub fn is_multiline(&self) -> bool {
        self.multiline
    }

    /// The visible content. When masked, every character is replaced with `*`; the
    /// underlying buffer still holds the real text and [`take`] clears it.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// What to draw on the screen: the real text, or a row of `*` with the same length.
    ///
    /// A masked field never exposes the password in a draw call; the caller does not
    /// need a second branch.
    pub fn display(&self) -> alloc::string::String {
        if self.masked {
            core::iter::repeat_n('*', self.text.chars().count()).collect()
        } else {
            alloc::string::String::from(&self.text)
        }
    }

    pub fn set_masked(&mut self, masked: bool) {
        self.masked = masked;
    }

    pub fn is_masked(&self) -> bool {
        self.masked
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn char_count(&self) -> usize {
        self.text.chars().count()
    }

    /// Byte offset of the caret, always on a `char` boundary.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The selected range as byte offsets, low end first, or `None` when nothing is selected.
    pub fn selection(&self) -> Option<(usize, usize)> {
        match self.anchor {
            Some(a) if a != self.cursor => {
                Some((a.min(self.cursor), a.max(self.cursor)))
            }
            _ => None,
        }
    }

    /// The selected text, for a caller that wants to copy or highlight it.
    pub fn selected_text(&self) -> Option<&str> {
        self.selection().map(|(from, to)| &self.text[from..to])
    }

    /// Where a byte offset in the real text lands in [`Self::display`].
    ///
    /// The two differ on a masked field: every character becomes one `*`, one byte wide, so an
    /// offset into a password is not an offset into the stars that stand for it. A caller drawing
    /// a caret or a selection band measures the *displayed* string — it is what is on the screen —
    /// and this is the conversion that gets it to the right place.
    ///
    /// Doing it here rather than at each drawing site is deliberate: the arithmetic was already
    /// written out twice, in the login screen and in the declarative field, and a third copy is how
    /// a caret ends up in a different place in one screen than in another.
    pub fn display_offset(&self, at: usize) -> usize {
        let at = at.min(self.text.len());
        if self.masked {
            // One `*` per character, and `*` is one byte.
            self.text[..at].chars().count()
        } else {
            at
        }
    }

    /// Select everything, as `Ctrl+A` does.
    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.text.len();
    }

    /// Forget the selection, leaving the caret where it is.
    pub fn deselect(&mut self) {
        self.anchor = None;
    }

    /// Delete the selection, if there is one. Returns whether anything went.
    ///
    /// An *empty* selection — the anchor sitting exactly on the caret — is dropped here too, even
    /// though nothing is deleted. It has to be: `selection()` already reports an empty range as
    /// `None`, so an anchor left behind is invisible to every reader and yet becomes a real
    /// selection the moment the caret moves. That is not theoretical. `select_all` on an empty
    /// field anchors at 0, the first typed character moves the caret to 1, and the anchor turns the
    /// character just typed into a selection that the second character replaces — a field where the
    /// first letter vanishes as you type the second, found on the phone in a search box.
    pub fn delete_selection(&mut self) -> bool {
        match self.selection() {
            None => {
                self.anchor = None;
                false
            }
            Some((from, to)) => {
                self.text.replace_range(from..to, "");
                self.cursor = from;
                self.anchor = None;
                true
            }
        }
    }

    pub fn set_text(&mut self, s: &str) {
        self.text.clear();
        self.text.push_str(s);
        self.cursor = self.text.len();
        self.anchor = None;
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.anchor = None;
    }

    /// Take the contents, leaving the field empty. What a composer does on send.
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        self.anchor = None;
        core::mem::take(&mut self.text)
    }

    /// Insert one character at the caret, replacing the selection if there is one.
    ///
    /// `false` when the field refused it — the limit is reached, or the character is not one this
    /// field accepts. A refusal after a selection was replaced still counts as an edit, which is
    /// why the caller sees the return value rather than a "nothing happened".
    pub fn insert(&mut self, ch: char) -> bool {
        // A newline is a legal character only in a multi-line field; a single-line field drops it
        // so a pasted address with a trailing return does not smuggle one in.
        if ch == '\n' && !self.multiline {
            return false;
        }
        if let Some(accept) = self.accept {
            if !accept(ch) {
                return false;
            }
        }
        // Typing over a selection replaces it — the behaviour of every editor the user has met.
        self.delete_selection();
        if let Some(max) = self.max_chars {
            if self.char_count() >= max {
                return false;
            }
        }
        self.text.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
        true
    }

    /// Insert a run of text at the caret, replacing the selection. What a paste does.
    ///
    /// Characters the field does not accept are **skipped**, not fatal: pasting `+55 21 99999`
    /// into a digits-only field should leave the digits rather than nothing. The length cap still
    /// stops the run, since past it there is nowhere to put anything.
    pub fn insert_str(&mut self, s: &str) {
        self.delete_selection();
        for ch in s.chars() {
            if self.is_full() {
                break;
            }
            self.insert(ch);
        }
    }

    /// Whether the length cap leaves room for nothing more.
    fn is_full(&self) -> bool {
        matches!(self.max_chars, Some(max) if self.char_count() >= max)
    }

    /// Delete the selection, or the `char` before the caret.
    pub fn backspace(&mut self) -> bool {
        if self.delete_selection() {
            return true;
        }
        match self.text[..self.cursor].chars().next_back() {
            None => false,
            Some(ch) => {
                self.cursor -= ch.len_utf8();
                self.text.remove(self.cursor);
                true
            }
        }
    }

    /// Delete the selection, or the `char` at the caret.
    pub fn delete(&mut self) -> bool {
        if self.delete_selection() {
            return true;
        }
        if self.cursor >= self.text.len() {
            return false;
        }
        self.text.remove(self.cursor);
        true
    }

    pub fn left(&mut self) -> bool {
        self.anchor = None;
        self.step_left()
    }

    pub fn right(&mut self) -> bool {
        self.anchor = None;
        self.step_right()
    }

    /// Move left keeping (or starting) the selection, as Shift+Left does.
    pub fn select_left(&mut self) -> bool {
        self.anchor.get_or_insert(self.cursor);
        self.step_left()
    }

    /// Move right keeping (or starting) the selection, as Shift+Right does.
    pub fn select_right(&mut self) -> bool {
        self.anchor.get_or_insert(self.cursor);
        self.step_right()
    }

    fn step_left(&mut self) -> bool {
        match self.text[..self.cursor].chars().next_back() {
            None => false,
            Some(ch) => {
                self.cursor -= ch.len_utf8();
                true
            }
        }
    }

    fn step_right(&mut self) -> bool {
        match self.text[self.cursor..].chars().next() {
            None => false,
            Some(ch) => {
                self.cursor += ch.len_utf8();
                true
            }
        }
    }

    pub fn home(&mut self) {
        self.cursor = 0;
        self.anchor = None;
    }

    pub fn end(&mut self) {
        self.cursor = self.text.len();
        self.anchor = None;
    }

    // ---- word granularity -------------------------------------------------------------------

    /// The byte offset one word to the left of the caret: skip any whitespace, then the run of
    /// non-whitespace before it. Used by word-left and delete-word-back.
    fn prev_word_boundary(&self) -> usize {
        let mut pos = self.cursor;
        let head = &self.text[..pos];
        let mut chars = head.char_indices().rev().peekable();
        while let Some(&(i, c)) = chars.peek() {
            if c.is_whitespace() { pos = i; chars.next(); } else { break; }
        }
        while let Some(&(i, c)) = chars.peek() {
            if !c.is_whitespace() { pos = i; chars.next(); } else { break; }
        }
        pos
    }

    /// The byte offset one word to the right: skip the run of non-whitespace, then any whitespace.
    fn next_word_boundary(&self) -> usize {
        let tail = &self.text[self.cursor..];
        let mut pos = self.cursor;
        let mut chars = tail.char_indices().peekable();
        while let Some(&(i, c)) = chars.peek() {
            if !c.is_whitespace() { pos = self.cursor + i + c.len_utf8(); chars.next(); } else { break; }
        }
        while let Some(&(i, c)) = chars.peek() {
            if c.is_whitespace() { pos = self.cursor + i + c.len_utf8(); chars.next(); } else { break; }
        }
        pos
    }

    fn move_to(&mut self, pos: usize, keep_selection: bool) {
        if keep_selection { self.anchor.get_or_insert(self.cursor); } else { self.anchor = None; }
        self.cursor = pos;
    }

    /// Move the caret one word left (Ctrl+Left).
    pub fn word_left(&mut self) { let p = self.prev_word_boundary(); self.move_to(p, false); }
    /// Move the caret one word right (Ctrl+Right).
    pub fn word_right(&mut self) { let p = self.next_word_boundary(); self.move_to(p, false); }

    /// Delete the selection, or the word before the caret (Ctrl+Backspace).
    pub fn delete_word_back(&mut self) -> bool {
        if self.delete_selection() { return true; }
        let from = self.prev_word_boundary();
        if from == self.cursor { return false; }
        self.text.replace_range(from..self.cursor, "");
        self.cursor = from;
        true
    }

    /// Delete the selection, or the word after the caret (Ctrl+Delete).
    pub fn delete_word_forward(&mut self) -> bool {
        if self.delete_selection() { return true; }
        let to = self.next_word_boundary();
        if to == self.cursor { return false; }
        self.text.replace_range(self.cursor..to, "");
        true
    }

    // ---- line granularity (single-line: the whole field) ------------------------------------

    fn line_start_at(&self, pos: usize) -> usize {
        self.text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0)
    }
    fn line_end_at(&self, pos: usize) -> usize {
        self.text[pos..].find('\n').map(|i| pos + i).unwrap_or(self.text.len())
    }

    /// Move the caret to the start of the current line.
    pub fn line_start(&mut self) { let p = self.line_start_at(self.cursor); self.move_to(p, false); }
    /// Move the caret to the end of the current line.
    pub fn line_end(&mut self) { let p = self.line_end_at(self.cursor); self.move_to(p, false); }

    /// Delete the whole current line (and its trailing newline, if any). On a single-line field
    /// this clears the field. The one destructive shortcut, so it drops any selection first.
    pub fn delete_line(&mut self) -> bool {
        self.anchor = None;
        let start = self.line_start_at(self.cursor);
        let mut end = self.line_end_at(self.cursor);
        if end < self.text.len() && self.text[end..].starts_with('\n') {
            end += 1; // take the newline so the line truly disappears
        }
        if start == end { return false; }
        self.text.replace_range(start..end, "");
        self.cursor = start.min(self.text.len());
        true
    }

    // ---- vertical movement (multi-line only) ------------------------------------------------

    fn nth_col_offset(line: &str, col: usize) -> usize {
        line.char_indices().nth(col).map(|(i, _)| i).unwrap_or(line.len())
    }

    fn move_up(&mut self) -> bool {
        let ls = self.line_start_at(self.cursor);
        if ls == 0 { return false; }
        let col = self.text[ls..self.cursor].chars().count();
        let prev_end = ls - 1;
        let prev_start = self.line_start_at(prev_end);
        let prev_line = &self.text[prev_start..prev_end];
        self.cursor = prev_start + Self::nth_col_offset(prev_line, col);
        true
    }

    fn move_down(&mut self) -> bool {
        let le = self.line_end_at(self.cursor);
        if le == self.text.len() { return false; }
        let ls = self.line_start_at(self.cursor);
        let col = self.text[ls..self.cursor].chars().count();
        let next_start = le + 1;
        let next_end = self.line_end_at(next_start);
        let next_line = &self.text[next_start..next_end];
        self.cursor = next_start + Self::nth_col_offset(next_line, col);
        true
    }

    /// Move the caret up one line, keeping the column (multi-line).
    pub fn up(&mut self) -> bool { self.anchor = None; self.move_up() }
    /// Move the caret down one line, keeping the column (multi-line).
    pub fn down(&mut self) -> bool { self.anchor = None; self.move_down() }

    /// Move up one line extending the selection (Shift+Up).
    pub fn select_up(&mut self) -> bool { self.anchor.get_or_insert(self.cursor); self.move_up() }
    /// Move down one line extending the selection (Shift+Down).
    pub fn select_down(&mut self) -> bool { self.anchor.get_or_insert(self.cursor); self.move_down() }

    /// Put the selection — or the whole field, when nothing is selected — on the clipboard.
    ///
    /// A masked field refuses: a password copied here would sit in the phone's clipboard, readable
    /// by every application on it, long after the user forgot they pressed the key.
    pub fn copy(&self, clip: &mut dyn Clipboard) -> bool {
        if self.masked {
            return false;
        }
        let text = self.selected_text().unwrap_or(&self.text);
        !text.is_empty() && clip.set(text)
    }

    /// Copy, then delete what was copied. Nothing is deleted if the copy failed — losing the text
    /// on a clipboard that refused it is the one outcome with no way back.
    pub fn cut(&mut self, clip: &mut dyn Clipboard) -> bool {
        if !self.copy(clip) {
            return false;
        }
        if self.delete_selection() {
            return true;
        }
        self.clear();
        true
    }

    /// Insert the clipboard's text at the caret, replacing the selection. `false` when there was
    /// nothing to paste.
    pub fn paste(&mut self, clip: &mut dyn Clipboard) -> bool {
        match clip.get() {
            Some(text) if !text.is_empty() => {
                self.insert_str(&text);
                true
            }
            _ => false,
        }
    }

    /// Standard editing bindings, clipboard included.
    ///
    /// Up/Down are deliberately *not* consumed: in a composer they should move
    /// focus back into the transcript, and that decision belongs to the screen.
    ///
    /// The clipboard is a parameter because this crate must not know what a device is — pass
    /// `&mut NoClipboard` for a build that has none, and copy and paste become quiet no-ops rather
    /// than a compile error or a panic.
    ///
    /// # A default that gets out of the way
    ///
    /// A clipboard chord that **did nothing** answers [`Handled::Ignored`], not `Consumed`: an
    /// empty clipboard, a refused copy, a masked field. That is what lets a screen put its own
    /// behaviour underneath the default instead of having to pre-empt it — hand the key to the
    /// field first and act on what comes back, and the two never fight over who owns `Ctrl+C`.
    ///
    /// The editing keys are the other way round on purpose: `Backspace` at the start of a field
    /// consumes the key even though nothing moved, because the field is unambiguously the thing
    /// being typed into and letting that fall through would have Backspace navigate the screen.
    pub fn handle_key(&mut self, ev: KeyEvent, clip: &mut dyn Clipboard) -> Handled {
        match ev.key {
            Key::Char(ch) if !ch.is_control() => {
                self.insert(ch);
                Handled::Consumed
            }
            Key::Backspace => {
                self.backspace();
                Handled::Consumed
            }
            Key::Delete => {
                self.delete();
                Handled::Consumed
            }
            // Shift turns a movement into a selection; without it, an arrow drops the selection —
            // which is the standard behaviour and also the only way out of one on a phone with no
            // pointer to click somewhere else with.
            Key::Left => {
                if ev.mods.ctrl {
                    self.word_left();
                } else if ev.mods.shift {
                    self.select_left();
                } else {
                    self.left();
                }
                Handled::Consumed
            }
            Key::Right => {
                if ev.mods.ctrl {
                    self.word_right();
                } else if ev.mods.shift {
                    self.select_right();
                } else {
                    self.right();
                }
                Handled::Consumed
            }
            // Up/Down and Enter act only in a multi-line field; a single-line field leaves them
            // alone (a composer moves focus into the transcript with Up/Down).
            Key::Up if self.multiline => {
                if ev.mods.shift { self.select_up(); } else { self.up(); }
                Handled::Consumed
            }
            Key::Down if self.multiline => {
                if ev.mods.shift { self.select_down(); } else { self.down(); }
                Handled::Consumed
            }
            Key::Enter if self.multiline => {
                self.insert('\n');
                Handled::Consumed
            }
            // Ctrl+Backspace arrives as the backspace control char with the Ctrl bit, which
            // `ctrl_chord` renders as Ctrl('h') (0x08 + 0x60): delete the word before the caret.
            // Falls through when there was nothing to delete. (Verify the exact code on the E72.)
            Key::Ctrl('h') => Handled::from(self.delete_word_back()),
            // Each of these answers whether it actually did something, so a chord this field could
            // not honour — nothing on the clipboard, a masked field, no clipboard at all — falls
            // through to the screen rather than being swallowed. See the note above.
            Key::Ctrl('v') => Handled::from(self.paste(clip)),
            // A masked field refuses to copy at all: the phone's clipboard is readable by every
            // application on it, and a password left there outlives the keypress by a long way.
            Key::Ctrl('c') => Handled::from(!self.masked && self.copy(clip)),
            Key::Ctrl('x') => Handled::from(!self.masked && self.cut(clip)),
            Key::Ctrl('a') => {
                if self.text.is_empty() {
                    return Handled::Ignored;
                }
                self.select_all();
                Handled::Consumed
            }
            _ => Handled::Ignored,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clip::{MemClipboard, NoClipboard};
    use crate::input::Modifiers;

    fn ev(k: Key) -> KeyEvent {
        KeyEvent::new(k)
    }

    /// The same key with Shift held — how a selection is made.
    fn shifted(k: Key) -> KeyEvent {
        KeyEvent::with_mods(k, Modifiers { shift: true, ..Modifiers::default() })
    }

    /// A field driven with no clipboard, for the tests that are not about one.
    fn key(f: &mut TextField, k: Key) -> Handled {
        f.handle_key(ev(k), &mut NoClipboard)
    }

    #[test]
    fn typing_appends_and_tracks_the_caret() {
        let mut f = TextField::new();
        for ch in "abc".chars() {
            key(&mut f, Key::Char(ch));
        }
        assert_eq!(f.text(), "abc");
        assert_eq!(f.cursor(), 3);
    }

    #[test]
    fn insertion_happens_at_the_caret_not_the_end() {
        let mut f = TextField::new();
        f.set_text("ac");
        f.left();
        f.insert('b');
        assert_eq!(f.text(), "abc");
        assert_eq!(f.cursor(), 2);
    }

    #[test]
    fn multibyte_editing_never_splits_a_char() {
        let mut f = TextField::new();
        f.insert_str("Привет");
        assert_eq!(f.char_count(), 6);
        assert_eq!(f.cursor(), 12, "Cyrillic is two bytes per char");

        assert!(f.backspace());
        assert_eq!(f.text(), "Приве");
        assert_eq!(f.cursor(), 10);

        // Walk left across every char, then back right, checking boundaries hold.
        while f.left() {}
        assert_eq!(f.cursor(), 0);
        let mut steps = 0;
        while f.right() {
            steps += 1;
            // Slicing at a bad boundary would panic here.
            let _ = &f.text()[..f.cursor()];
        }
        assert_eq!(steps, 5);
    }

    #[test]
    fn backspace_and_delete_at_the_boundaries_are_no_ops() {
        let mut f = TextField::new();
        assert!(!f.backspace());
        assert!(!f.delete());
        f.set_text("a");
        f.home();
        assert!(!f.backspace(), "nothing before the caret");
        assert!(f.delete());
        assert_eq!(f.text(), "");
    }

    #[test]
    fn delete_removes_forward() {
        let mut f = TextField::new();
        f.set_text("abc");
        f.home();
        f.delete();
        assert_eq!(f.text(), "bc");
        assert_eq!(f.cursor(), 0);
    }

    #[test]
    fn limit_is_counted_in_chars_not_bytes() {
        let mut f = TextField::with_limit(3);
        f.insert_str("ЖЖЖЖЖ");
        assert_eq!(f.char_count(), 3, "limit must not depend on encoding width");
        assert_eq!(f.text(), "ЖЖЖ");
        assert!(!f.insert('x'));
    }

    #[test]
    fn take_empties_the_field_and_resets_the_caret() {
        let mut f = TextField::new();
        f.insert_str("hello");
        assert_eq!(f.take(), "hello");
        assert!(f.is_empty());
        assert_eq!(f.cursor(), 0);
    }

    #[test]
    fn navigation_keys_the_composer_needs_are_left_alone() {
        let mut f = TextField::new();
        assert_eq!(key(&mut f, Key::Up), Handled::Ignored);
        assert_eq!(key(&mut f, Key::Down), Handled::Ignored);
        assert_eq!(key(&mut f, Key::Select), Handled::Ignored);
        assert_eq!(key(&mut f, Key::Softkey(crate::input::Softkey::Left)), Handled::Ignored);
    }

    #[test]
    fn control_characters_are_not_inserted() {
        let mut f = TextField::new();
        assert_eq!(key(&mut f, Key::Char('\n')), Handled::Ignored);
        assert_eq!(key(&mut f, Key::Char('\t')), Handled::Ignored);
        assert!(f.is_empty());
    }

    #[test]
    fn shift_and_an_arrow_select_while_a_bare_arrow_drops_the_selection() {
        let mut f = TextField::new();
        f.set_text("hello");
        f.handle_key(shifted(Key::Left), &mut NoClipboard);
        f.handle_key(shifted(Key::Left), &mut NoClipboard);
        assert_eq!(f.selected_text(), Some("lo"));
        // The way out of a selection on a phone with no pointer: press an arrow.
        key(&mut f, Key::Left);
        assert_eq!(f.selection(), None);
    }

    #[test]
    fn a_selection_that_shrinks_back_to_nothing_is_no_selection() {
        // Anchor and caret land on the same offset. Reporting that as an empty selection would
        // make copy put "" on the clipboard and delete_selection claim it did something.
        let mut f = TextField::new();
        f.set_text("ab");
        f.handle_key(shifted(Key::Left), &mut NoClipboard);
        f.handle_key(shifted(Key::Right), &mut NoClipboard);
        assert_eq!(f.selection(), None);
        assert!(!f.delete_selection());
    }

    #[test]
    fn typing_and_backspace_replace_the_selection() {
        let mut f = TextField::new();
        f.set_text("hello");
        f.select_all();
        key(&mut f, Key::Char('x'));
        assert_eq!(f.text(), "x", "typing over a selection replaces it");

        f.set_text("hello");
        f.select_all();
        key(&mut f, Key::Backspace);
        assert!(f.is_empty(), "one Backspace deletes the whole selection");
    }

    #[test]
    fn paste_inserts_at_the_caret_and_replaces_a_selection() {
        let mut clip = MemClipboard::with_text("world");
        let mut f = TextField::new();
        f.set_text("hello ");
        f.handle_key(ev(Key::Ctrl('v')), &mut clip);
        assert_eq!(f.text(), "hello world");

        f.select_all();
        f.handle_key(ev(Key::Ctrl('v')), &mut clip);
        assert_eq!(f.text(), "world", "a paste over a selection replaces it");
    }

    #[test]
    fn paste_respects_the_length_cap_and_the_accepted_characters() {
        let mut clip = MemClipboard::with_text("+55 21 99999-0000");
        // The case the old caller-side filter got wrong: pasted text never passed through it.
        let mut phone = TextField::with_limit(6).accepting(|c| c.is_ascii_digit());
        phone.handle_key(ev(Key::Ctrl('v')), &mut clip);
        assert_eq!(phone.text(), "552199", "digits only, and no more than the cap");
    }

    #[test]
    fn pasting_nothing_changes_nothing() {
        let mut f = TextField::new();
        f.set_text("keep");
        f.handle_key(ev(Key::Ctrl('v')), &mut MemClipboard::new());
        f.handle_key(ev(Key::Ctrl('v')), &mut NoClipboard);
        assert_eq!(f.text(), "keep");
    }

    #[test]
    fn copy_takes_the_selection_and_otherwise_the_whole_field() {
        let mut clip = MemClipboard::new();
        let mut f = TextField::new();
        f.set_text("hello world");
        f.handle_key(ev(Key::Ctrl('c')), &mut clip);
        assert_eq!(clip.get().as_deref(), Some("hello world"), "no selection means all of it");

        f.handle_key(shifted(Key::Left), &mut clip);
        f.handle_key(shifted(Key::Left), &mut clip);
        f.handle_key(ev(Key::Ctrl('c')), &mut clip);
        assert_eq!(clip.get().as_deref(), Some("ld"));
        assert_eq!(f.text(), "hello world", "copy leaves the field alone");
    }

    #[test]
    fn cut_copies_before_it_deletes() {
        let mut clip = MemClipboard::new();
        let mut f = TextField::new();
        f.set_text("abc");
        f.select_all();
        f.handle_key(ev(Key::Ctrl('x')), &mut clip);
        assert_eq!(clip.get().as_deref(), Some("abc"));
        assert!(f.is_empty());
    }

    #[test]
    fn a_cut_that_could_not_copy_keeps_the_text() {
        // The one outcome with no way back: text deleted from the field and never put anywhere.
        let mut f = TextField::new();
        f.set_text("abc");
        f.select_all();
        f.handle_key(ev(Key::Ctrl('x')), &mut NoClipboard);
        assert_eq!(f.text(), "abc");
    }

    #[test]
    fn a_masked_field_refuses_to_put_the_password_on_the_clipboard() {
        let mut clip = MemClipboard::new();
        let mut f = TextField::new();
        f.set_masked(true);
        f.set_text("hunter2");
        assert_eq!(f.handle_key(ev(Key::Ctrl('c')), &mut clip), Handled::Ignored);
        assert_eq!(f.handle_key(ev(Key::Ctrl('x')), &mut clip), Handled::Ignored);
        assert_eq!(clip.get(), None, "the phone's clipboard is readable by every app on it");
        assert_eq!(f.text(), "hunter2");
        // Pasting *into* a password field is fine, and is how a password manager is used.
        let mut src = MemClipboard::with_text("s3cret");
        f.select_all();
        f.handle_key(ev(Key::Ctrl('v')), &mut src);
        assert_eq!(f.text(), "s3cret");
    }

    #[test]
    fn a_chord_that_did_nothing_falls_through_to_the_screen() {
        // What makes the SDK's behaviour a default rather than a policy: a screen can offer the key
        // to the field and, if the field had nothing to do with it, do its own thing — without
        // having to intercept the chord first and re-implement the part that does work.
        let mut f = TextField::new();
        assert_eq!(f.handle_key(ev(Key::Ctrl('v')), &mut MemClipboard::new()), Handled::Ignored);
        assert_eq!(f.handle_key(ev(Key::Ctrl('c')), &mut NoClipboard), Handled::Ignored);
        assert_eq!(f.handle_key(ev(Key::Ctrl('a')), &mut NoClipboard), Handled::Ignored);

        // And when it *does* do something, it says so, so the same screen does not act twice.
        f.set_text("hello");
        let mut clip = MemClipboard::new();
        assert_eq!(f.handle_key(ev(Key::Ctrl('a')), &mut clip), Handled::Consumed);
        assert_eq!(f.handle_key(ev(Key::Ctrl('c')), &mut clip), Handled::Consumed);
        assert_eq!(f.handle_key(ev(Key::Ctrl('v')), &mut clip), Handled::Consumed);
    }

    #[test]
    fn a_screen_can_wrap_the_clipboard_it_hands_over() {
        // The per-screen override, exercised rather than only described: a composer that pastes as
        // one line. The field knows nothing about it.
        struct OneLine(MemClipboard);
        impl crate::clip::Clipboard for OneLine {
            fn get(&mut self) -> Option<String> {
                self.0.get().map(|t| t.replace('\n', " "))
            }
            fn set(&mut self, text: &str) -> bool {
                self.0.set(text)
            }
        }

        let mut clip = OneLine(MemClipboard::with_text("duas\nlinhas"));
        let mut f = TextField::new();
        f.handle_key(ev(Key::Ctrl('v')), &mut clip);
        assert_eq!(f.text(), "duas linhas");
    }

    #[test]
    fn editing_keys_are_consumed_even_when_nothing_moved() {
        // The deliberate asymmetry with the chords above: a field being typed into owns Backspace
        // and the arrows outright, or a Backspace at the start of an empty field would navigate
        // the screen behind it.
        let mut f = TextField::new();
        assert_eq!(key(&mut f, Key::Backspace), Handled::Consumed);
        assert_eq!(key(&mut f, Key::Delete), Handled::Consumed);
        assert_eq!(key(&mut f, Key::Left), Handled::Consumed);
        assert_eq!(key(&mut f, Key::Right), Handled::Consumed);
    }

    #[test]
    fn typing_into_an_empty_field_that_was_select_alled_keeps_the_first_letter() {
        // The browser opens an in-page input with `select_all`, so typing replaces a prefilled
        // value. On an *empty* field that anchors at 0 with the caret at 0, and the first keystroke
        // used to leave the anchor behind — making the letter just typed a selection the next one
        // overwrote. Typing "go" produced "o".
        let mut f = TextField::new();
        f.select_all();
        f.insert('g');
        assert_eq!(f.selection(), None, "no selection may survive the first insert");
        f.insert('o');
        assert_eq!(f.text(), "go");
    }


    #[test]
    fn select_all_then_type_is_the_quick_way_to_replace_a_code() {
        let mut f = TextField::new();
        f.set_text("12345");
        f.handle_key(ev(Key::Ctrl('a')), &mut NoClipboard);
        assert_eq!(f.selected_text(), Some("12345"));
        key(&mut f, Key::Char('9'));
        assert_eq!(f.text(), "9");
    }

    #[test]
    fn a_selection_never_survives_the_text_being_replaced() {
        // set_text/clear/take leave the caret at a known place; an anchor left pointing into the
        // old text would be a byte offset into a string that no longer exists.
        let mut f = TextField::new();
        f.set_text("hello");
        f.select_all();
        f.set_text("hi");
        assert_eq!(f.selection(), None);
        f.select_all();
        f.take();
        assert_eq!(f.selection(), None);
        f.set_text("hello");
        f.select_all();
        f.clear();
        assert_eq!(f.selection(), None);
    }

    #[test]
    fn selection_offsets_stay_on_char_boundaries() {
        let mut f = TextField::new();
        f.set_text("Привет");
        f.handle_key(shifted(Key::Left), &mut NoClipboard);
        f.handle_key(shifted(Key::Left), &mut NoClipboard);
        // Slicing at a bad boundary would panic here rather than fail the assert.
        assert_eq!(f.selected_text(), Some("ет"));
        let (from, to) = f.selection().unwrap();
        assert_eq!((from, to), (8, 12), "Cyrillic is two bytes per char");
    }

    #[test]
    fn a_masked_field_never_leaks_the_password_through_display() {
        // The display hides characters; the real text is still in the buffer so it
        // can be submitted to the server, and take() clears it so a shadow copy in
        // the screen is never needed.
        let mut f = TextField::new();
        f.set_masked(true);
        f.insert_str("hunter2");
        assert_eq!(f.text(), "hunter2", "the real text must still be readable internally");
        assert_eq!(f.display(), "*******");
        assert_eq!(f.display().chars().count(), f.char_count());
        // Take must clear the underlying password, not just the mask.
        f.take();
        assert!(f.is_empty());
        assert_eq!(f.display(), "");
    }

    #[test]
    fn word_left_and_right_hop_whole_words() {
        let mut f = TextField::new();
        f.insert_str("foo bar baz");
        f.word_left();
        assert_eq!(f.cursor(), 8, "from end to start of 'baz'");
        f.word_left();
        assert_eq!(f.cursor(), 4, "to start of 'bar'");
        f.word_right();
        assert_eq!(f.cursor(), 8, "back to start of 'baz' (skips space then word)");
    }

    #[test]
    fn delete_word_back_removes_the_word_before_the_caret() {
        let mut f = TextField::new();
        f.insert_str("foo bar baz");
        assert!(f.delete_word_back());
        assert_eq!(f.text(), "foo bar ");
        assert!(f.delete_word_back());
        assert_eq!(f.text(), "foo ");
        assert!(f.delete_word_back());
        assert_eq!(f.text(), "");
        assert!(!f.delete_word_back(), "nothing left to delete");
    }

    #[test]
    fn delete_word_back_takes_the_selection_when_there_is_one() {
        let mut f = TextField::new();
        f.insert_str("hello world");
        f.select_all();
        assert!(f.delete_word_back());
        assert_eq!(f.text(), "", "a selection wins over word granularity");
    }

    #[test]
    fn delete_line_clears_a_single_line_field() {
        let mut f = TextField::new();
        f.insert_str("a long url");
        assert!(f.delete_line());
        assert_eq!(f.text(), "");
        assert!(!f.delete_line());
    }

    #[test]
    fn a_single_line_field_refuses_newlines() {
        let mut f = TextField::new();
        assert!(!f.insert('\n'));
        assert_eq!(f.text(), "");
    }

    #[test]
    fn a_multiline_field_takes_newlines_and_moves_between_lines() {
        let mut f = TextField::new().multiline(true);
        f.insert_str("abc");
        assert!(f.insert('\n'));
        f.insert_str("de");
        assert_eq!(f.text(), "abc\nde");
        // caret is after "de" (column 2 on line 2). Up keeps the column on the longer line 1.
        assert!(f.up());
        assert_eq!(f.cursor(), 2, "column 2 of 'abc'");
        assert!(!f.up(), "already on the first line");
        assert!(f.down());
        assert_eq!(f.cursor(), 6, "column 2 of 'de' is its end");
        assert!(!f.down(), "already on the last line");
    }

    #[test]
    fn up_clamps_the_column_to_a_shorter_line() {
        let mut f = TextField::new().multiline(true);
        f.insert_str("hi\nlonger");
        // caret at end of "longer" (column 6); line above "hi" has only 2 columns.
        assert!(f.up());
        assert_eq!(f.cursor(), 2, "clamped to the end of 'hi'");
    }

    #[test]
    fn delete_line_on_multiline_removes_the_line_and_its_newline() {
        let mut f = TextField::new().multiline(true);
        f.insert_str("one\ntwo\nthree");
        // put the caret on line 2 ("two")
        f.home();
        f.down();
        assert!(f.delete_line());
        assert_eq!(f.text(), "one\nthree");
    }
}
