//! Ask for a line of text, and get an answer or a no.
//!
//! [`Modal`] asks a question with fixed choices. [`crate::edit::TextField`] edits text. Nothing put
//! the two together, so every screen that needed a value typed had to lay out a field, decide where
//! the OK went, route the keys, and remember to handle Back — four decisions, taken again each time
//! and differently.
//!
//! The value that needed asking for here is `owner/repo`, and it is the worst case on purpose: it is
//! long, it is punctuated, and it is exactly the sort of thing somebody has in their clipboard rather
//! than in their head. So paste is not an extra — `Ctrl+V` works on this handset (the shim was fixed
//! for it) and `TextField` already routes it, which means this dialog gets it by composing rather
//! than by implementing.
//!
//! ## Why the field is pre-selected when it opens with a value
//!
//! The same reason the browser's address bar is, and the reason is written down there: it serves both
//! gestures at once. The first keystroke replaces — for somebody entering a different repository —
//! and an arrow key drops the selection and keeps the text, for somebody fixing a typo in this one.
//! One state, two intentions, no mode to choose.

use alloc::string::String;

use crate::clip::Clipboard;
use crate::edit::TextField;
use crate::input::{Key, KeyEvent, Softkey};
use crate::theme::Theme;
use crate::{chrome, paint};
use symbian_gfx::{Align, Canvas, Point, Rect};

/// What the user did with the prompt.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TextAnswer {
    /// They committed this. Trimmed, and never empty — an empty commit is treated as a cancel,
    /// because "OK" on a blank field is not an answer anybody meant.
    Entered(String),
    /// They backed out.
    Cancelled,
}

/// A one-line question.
pub struct TextPrompt {
    title: String,
    hint: String,
    field: TextField,
    /// A word under the field: what went wrong last time, or what the format is.
    note: String,
}

impl TextPrompt {
    /// A prompt with a title and a placeholder.
    pub fn new(title: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            hint: hint.into(),
            field: TextField::with_limit(160),
            note: String::new(),
        }
    }

    /// Open with a value already in it, selected whole.
    pub fn with_value(mut self, value: &str) -> Self {
        for ch in value.chars() {
            self.field.insert(ch);
        }
        self.field.select_all();
        self
    }

    /// A line under the field. Used for the format when the prompt opens, and for the reason when an
    /// answer was refused — so a rejected value is corrected in place rather than retyped into a
    /// dialog that has forgotten it.
    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.note = note.into();
        self
    }

    /// Replace the note and keep everything else, for a caller that refused the value.
    pub fn set_note(&mut self, note: impl Into<String>) {
        self.note = note.into();
    }

    pub fn text(&self) -> &str {
        self.field.text()
    }

    /// Route a key. `None` while the dialog is still open.
    ///
    /// The field sees the key first, and that ordering is the whole of the design: a prompt that
    /// grabbed Select for OK would make it impossible to type a character the field maps to Select,
    /// and a prompt that grabbed Back would make it impossible to delete. So only the two softkeys
    /// are the dialog's, and everything else belongs to the text.
    pub fn handle_key(&mut self, ev: KeyEvent, clip: &mut dyn Clipboard) -> Option<TextAnswer> {
        match ev.key {
            Key::Softkey(Softkey::Right) => return Some(TextAnswer::Cancelled),
            Key::Softkey(Softkey::Left) | Key::Select => {
                let text = self.field.text().trim();
                // An empty OK is a cancel rather than an empty answer: nobody means "yes, nothing".
                return Some(if text.is_empty() {
                    TextAnswer::Cancelled
                } else {
                    TextAnswer::Entered(String::from(text))
                });
            }
            _ => {}
        }
        self.field.handle_key(ev, clip);
        None
    }

    /// The height this wants: title, field, note.
    pub fn height(&self, theme: &Theme<'_>) -> i32 {
        let f = theme.fonts.body;
        let mut h = f.line_height() + chrome::text_field_height(theme) + theme.metrics.pad * 3;
        h += f.line_height();
        if !self.note.is_empty() {
            h += theme.fonts.small.line_height();
        }
        h
    }

    /// Draw centred over `screen`, as a panel.
    ///
    /// Over, not instead of: the list behind stays visible, which is what tells somebody that
    /// dismissing this returns them to where they were.
    pub fn draw(&mut self, c: &mut Canvas<'_>, screen: Rect, theme: &Theme<'_>) {
        let p = &theme.palette;
        let pad = theme.metrics.pad;
        let h = self.height(theme).min(screen.height());
        let w = screen.width() - pad * 2;
        let panel = Rect::from_xywh(screen.x0 + pad, screen.y0 + (screen.height() - h) / 2, w, h);

        paint::band_round(c, panel, &p.chrome, theme.metrics.radius);
        c.stroke_rect(panel, p.divider);

        let inner = panel.inset(pad);
        let body = theme.fonts.body;
        let (title_row, rest) = inner.split_top(body.line_height());
        c.draw_text_in(title_row, &self.title, theme.fonts.strong, p.chrome_text, Align::Start);

        let (_, rest) = rest.split_top(pad);
        let (field_row, rest) = rest.split_top(chrome::text_field_height(theme));
        chrome::text_field(
            c,
            field_row,
            theme,
            &self.field,
            chrome::FieldStyle {
                focused: true,
                placeholder: Some(&self.hint),
                prefix: None,
            },
        );

        if !self.note.is_empty() {
            let (_, rest) = rest.split_top(2);
            let (note_row, _) = rest.split_top(theme.fonts.small.line_height());
            c.draw_text(
                Point::new(note_row.x0, note_row.y0 + theme.fonts.small.ascent()),
                &self.note,
                theme.fonts.small,
                p.dim,
            );
        }
    }

    /// The softkeys this dialog wants while it is open.
    ///
    /// Named here rather than in the caller so every prompt in the project reads the same way. It is
    /// also why `Select` commits: on this keypad the centre key is where a thumb already is, and
    /// making somebody reach for a softkey to accept what they just typed is a step for nothing.
    pub fn softkeys(&self) -> [Option<&'static str>; 3] {
        [Some("OK"), None, Some("Cancel")]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clip::{MemClipboard, NoClipboard};
    use crate::testing::{with_canvas, with_theme, SCREEN};
    use crate::Palette;
    use symbian_gfx::Size;

    fn press(p: &mut TextPrompt, k: Key) -> Option<TextAnswer> {
        p.handle_key(KeyEvent::new(k), &mut NoClipboard)
    }

    fn type_in(p: &mut TextPrompt, s: &str) {
        for ch in s.chars() {
            press(p, Key::Char(ch));
        }
    }

    #[test]
    fn a_typed_value_comes_back_trimmed() {
        let mut p = TextPrompt::new("Add repository", "owner/repo");
        type_in(&mut p, "  pizzaria-foundation/home  ");
        assert_eq!(
            press(&mut p, Key::Select),
            Some(TextAnswer::Entered(String::from("pizzaria-foundation/home")))
        );
    }

    #[test]
    fn ok_on_a_blank_field_is_a_cancel_rather_than_an_empty_answer() {
        // Nobody means "yes, nothing".
        let mut p = TextPrompt::new("Add repository", "owner/repo");
        assert_eq!(press(&mut p, Key::Select), Some(TextAnswer::Cancelled));
        type_in(&mut p, "   ");
        assert_eq!(press(&mut p, Key::Softkey(Softkey::Left)), Some(TextAnswer::Cancelled));
    }

    #[test]
    fn back_cancels_and_leaves_nothing_behind() {
        let mut p = TextPrompt::new("Add repository", "owner/repo");
        type_in(&mut p, "abc/def");
        assert_eq!(press(&mut p, Key::Softkey(Softkey::Right)), Some(TextAnswer::Cancelled));
    }

    #[test]
    fn the_field_gets_every_key_the_dialog_does_not_need() {
        // A prompt that grabbed more would make it impossible to type or to delete.
        let mut p = TextPrompt::new("t", "h");
        type_in(&mut p, "abc");
        assert_eq!(p.text(), "abc");
        assert_eq!(press(&mut p, Key::Backspace), None, "still open");
        assert_eq!(p.text(), "ab");
        press(&mut p, Key::Left);
        press(&mut p, Key::Char('X'));
        assert_eq!(p.text(), "aXb", "the caret moved, so the insert landed inside");
    }

    #[test]
    fn a_pasted_value_arrives_whole() {
        // The value this exists to ask for is a URL, and a URL lives in a clipboard rather than in
        // somebody's head. Ctrl+V works on this handset and `TextField` already routes it — this
        // dialog gets it by composing rather than by implementing.
        let mut clip = MemClipboard::default();
        clip.set("https://github.com/pizzaria-foundation/home");
        let mut p = TextPrompt::new("Add repository", "owner/repo");
        // `Key::Ctrl('v')` and not `Char('v')` with a modifier: the chord is its own key here,
        // which is what `TextField` matches on.
        p.handle_key(KeyEvent::new(Key::Ctrl('v')), &mut clip);
        assert_eq!(p.text(), "https://github.com/pizzaria-foundation/home");
    }

    #[test]
    fn opening_with_a_value_selects_it_whole() {
        // Serves both gestures: the first keystroke replaces (a different repository), an arrow drops
        // the selection and keeps the text (fix a typo in this one). The browser's address bar took
        // the same decision for the same reason.
        let mut p = TextPrompt::new("Edit", "owner/repo").with_value("a/b");
        assert_eq!(p.text(), "a/b");
        type_in(&mut p, "c");
        assert_eq!(p.text(), "c", "the selection was replaced");

        let mut q = TextPrompt::new("Edit", "owner/repo").with_value("a/b");
        press(&mut q, Key::Right);
        type_in(&mut q, "c");
        assert_eq!(q.text(), "a/bc", "and an arrow keeps it");
    }

    #[test]
    fn a_refused_value_is_corrected_in_place() {
        // Rather than retyped into a dialog that has forgotten it.
        let mut p = TextPrompt::new("Add repository", "owner/repo");
        type_in(&mut p, "nonsense");
        let TextAnswer::Entered(v) = press(&mut p, Key::Select).unwrap() else { panic!() };
        assert_eq!(v, "nonsense");
        p.set_note("not an owner/repo");
        assert_eq!(p.text(), "nonsense", "still there to be fixed");
    }

    #[test]
    fn it_draws_in_both_palettes_and_with_a_note() {
        for palette in [Palette::DARK, Palette::LIGHT] {
            let mut p = TextPrompt::new("Add repository", "owner/repo").note("e.g. owner/repo");
            type_in(&mut p, "pizzaria-foundation/home");
            let (_, px) = with_canvas(Size::new(SCREEN.width(), SCREEN.height()), |c| {
                with_theme(palette, |t| p.draw(c, SCREEN, t));
            });
            let blank = with_canvas(Size::new(SCREEN.width(), SCREEN.height()), |_| {}).1;
            assert_ne!(px, blank);
        }
    }

    #[test]
    fn a_screen_too_small_for_the_panel_does_not_panic() {
        let mut p = TextPrompt::new("t", "h").note("n");
        with_canvas(Size::new(40, 20), |c| {
            with_theme(Palette::DARK, |t| p.draw(c, Rect::from_xywh(0, 0, 40, 20), t));
        });
    }

    #[test]
    fn the_softkeys_are_the_dialogs_and_are_named_once() {
        assert_eq!(TextPrompt::new("t", "h").softkeys(), [Some("OK"), None, Some("Cancel")]);
    }
}
