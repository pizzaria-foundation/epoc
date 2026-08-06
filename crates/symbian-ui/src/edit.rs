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

use alloc::string::String;

use crate::input::{Handled, Key, KeyEvent};

#[derive(Clone, Debug, Default)]
pub struct TextField {
    text: String,
    cursor: usize,
    /// Cap in `char`s, not bytes — a limit in bytes would behave differently for
    /// Cyrillic than for Latin, which users would experience as a bug.
    max_chars: Option<usize>,
    /// When true, [`text`] returns `*` repeated for every character instead of the
    /// real contents. The underlying buffer is still the password and is cleared by
    /// [`take`], so a shadow copy in the screen is never needed and the only place
    /// the password lives is here.
    masked: bool,
}

impl TextField {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_limit(max_chars: usize) -> Self {
        Self { max_chars: Some(max_chars), ..Self::default() }
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
            core::iter::repeat('*').take(self.text.chars().count()).collect()
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

    pub fn set_text(&mut self, s: &str) {
        self.text.clear();
        self.text.push_str(s);
        self.cursor = self.text.len();
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// Take the contents, leaving the field empty. What a composer does on send.
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        core::mem::take(&mut self.text)
    }

    pub fn insert(&mut self, ch: char) -> bool {
        if let Some(max) = self.max_chars {
            if self.char_count() >= max {
                return false;
            }
        }
        self.text.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
        true
    }

    pub fn insert_str(&mut self, s: &str) {
        for ch in s.chars() {
            if !self.insert(ch) {
                break;
            }
        }
    }

    /// Delete the `char` before the caret.
    pub fn backspace(&mut self) -> bool {
        match self.text[..self.cursor].chars().next_back() {
            None => false,
            Some(ch) => {
                self.cursor -= ch.len_utf8();
                self.text.remove(self.cursor);
                true
            }
        }
    }

    /// Delete the `char` at the caret.
    pub fn delete(&mut self) -> bool {
        if self.cursor >= self.text.len() {
            return false;
        }
        self.text.remove(self.cursor);
        true
    }

    pub fn left(&mut self) -> bool {
        match self.text[..self.cursor].chars().next_back() {
            None => false,
            Some(ch) => {
                self.cursor -= ch.len_utf8();
                true
            }
        }
    }

    pub fn right(&mut self) -> bool {
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
    }

    pub fn end(&mut self) {
        self.cursor = self.text.len();
    }

    /// Standard editing bindings.
    ///
    /// Up/Down are deliberately *not* consumed: in a composer they should move
    /// focus back into the transcript, and that decision belongs to the screen.
    pub fn handle_key(&mut self, ev: KeyEvent) -> Handled {
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
            Key::Left => {
                self.left();
                Handled::Consumed
            }
            Key::Right => {
                self.right();
                Handled::Consumed
            }
            _ => Handled::Ignored,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(k: Key) -> KeyEvent {
        KeyEvent::new(k)
    }

    #[test]
    fn typing_appends_and_tracks_the_caret() {
        let mut f = TextField::new();
        for ch in "abc".chars() {
            f.handle_key(ev(Key::Char(ch)));
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
        assert_eq!(f.handle_key(ev(Key::Up)), Handled::Ignored);
        assert_eq!(f.handle_key(ev(Key::Down)), Handled::Ignored);
        assert_eq!(f.handle_key(ev(Key::Select)), Handled::Ignored);
        assert_eq!(f.handle_key(ev(Key::Softkey(crate::input::Softkey::Left))), Handled::Ignored);
    }

    #[test]
    fn control_characters_are_not_inserted() {
        let mut f = TextField::new();
        assert_eq!(f.handle_key(ev(Key::Char('\n'))), Handled::Ignored);
        assert_eq!(f.handle_key(ev(Key::Char('\t'))), Handled::Ignored);
        assert!(f.is_empty());
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
}
