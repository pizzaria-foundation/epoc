//! A question any screen can ask, in three lines.
//!
//! [`Prompt`](crate::prompt::Prompt) is the panel — it draws and it moves a cursor, and the caller
//! supplies the text on every frame. That is the right shape for a widget and the wrong shape for a
//! *use*: every caller ends up keeping the labels somewhere, keeping the meanings somewhere else,
//! and mapping an index between them. Two parallel lists, and the one that drifts is the one that
//! opens something when the user asked to copy.
//!
//! This is the whole thing in one value. A choice is a **label and what it means, written together**
//! — the same principle as [`chrome::Softkeys`](crate::chrome::Softkeys) carrying its message, and
//! for the same reason: you cannot write one without the other, so they cannot disagree.
//!
//! ```ignore
//! enum Msg { CopyOpen, Open, Copy }
//!
//! // Ask.
//! self.modal = Some(Modal::new("Abrir link", &url)
//!     .choice("Copiar e abrir", Msg::CopyOpen)
//!     .choice("Apenas abrir",  Msg::Open)
//!     .choice("Copiar link",   Msg::Copy));
//!
//! // Route. Modal means modal: while one is up it takes every key.
//! if let Some(m) = &mut self.modal {
//!     match m.handle_key(ev) {
//!         Some(Answer::Chosen(msg)) => { self.modal = None; self.act(msg) }
//!         Some(Answer::Cancelled)   => { self.modal = None }
//!         None => {}
//!     }
//!     return Handled::Consumed;
//! }
//!
//! // Draw, last, over everything.
//! if let Some(m) = &mut self.modal { m.draw(c, theme) }
//! ```
//!
//! The screen behind is *not* redrawn or dismantled — `draw` paints over whatever is already in the
//! buffer. That is what makes this a dialog rather than a navigation: nothing about the screen
//! underneath changes, so nothing has to be rebuilt when the question is answered. A modal that
//! pushed a screen would have to restore one, and restoring is where state gets lost.

use alloc::string::String;
use alloc::vec::Vec;

use symbian_gfx::{Canvas, Rect};

use crate::chrome;
use crate::input::{Handled, KeyEvent};
use crate::prompt::{Prompt, PromptAction};
use crate::theme::Theme;

/// What the user did with a modal.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Answer<T> {
    /// They picked this. The value is the caller's own — usually a message enum.
    Chosen(T),
    /// They backed out. Nothing was chosen and nothing should happen.
    Cancelled,
}

/// A question with typed answers.
///
/// `T` is whatever the caller wants back: a message, an index, a `()` for a bare acknowledgement.
/// It is cloned on the way out, so keep it small — an enum, not a document.
pub struct Modal<T> {
    title: String,
    body: String,
    choices: Vec<(String, T)>,
    prompt: Prompt,
    /// The label for the middle softkey. The action slot, per the SDK convention.
    action_label: String,
    /// The label for the right softkey. Always a way out — see [`Modal::back_label`].
    back_label: String,
}

impl<T: Clone> Modal<T> {
    /// A modal with a title and a body.
    ///
    /// The body is what the question is *about* — the URL, the filename, the error — and it wraps.
    /// The title is a heading and does not. Putting the long thing in the title is the mistake this
    /// split exists to prevent: it would be truncated, and the truncated part is the part that
    /// matters.
    pub fn new(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            body: body.into(),
            choices: Vec::new(),
            prompt: Prompt::new(),
            action_label: String::from(crate::strings::select()),
            back_label: String::from(crate::strings::back()),
        }
    }

    /// The default softkey labels come from the phone's language.
    ///
    /// They used to be Portuguese constants — `Escolher` and `Voltar` — written that way because
    /// this widget was built for `tg`, and documented at length because the consequence was already
    /// understood: *a shared widget that carries a language hands every later caller a decision it
    /// never asked about, and the symptom is one Portuguese word in an otherwise English screen*.
    /// The boot manager's first confirmation dialog looked exactly like that.
    ///
    /// `crate::strings` answers instead, so the decision belongs to the only party that knows it.
    /// In Portuguese the labels are the same two words, which is why nothing needed re-checking when
    /// this changed: what `tg` ships is unchanged and every other screen stopped being wrong.
    ///
    /// [`action_label`](Self::action_label) and [`back_label`](Self::back_label) are still there for
    /// a dialog whose buttons are about its own subject — `Remove`/`Keep` rather than
    /// `Select`/`Back`.
    ///
    /// Add a choice: what it reads as, and what it means.
    pub fn choice(mut self, label: impl Into<String>, value: T) -> Self {
        self.choices.push((label.into(), value));
        self
    }

    /// Start with the cursor on this choice, for a question with a sensible default.
    pub fn default_choice(mut self, index: usize) -> Self {
        self.prompt.select(index);
        self
    }

    /// Rename the middle softkey. It is the action slot and it always chooses; only the word
    /// changes.
    pub fn action_label(mut self, label: impl Into<String>) -> Self {
        self.action_label = label.into();
        self
    }

    /// Rename the right softkey.
    ///
    /// It cannot be *removed*, and that is deliberate: the right softkey is the one key a user
    /// presses without reading, and a modal with no way out is a phone that needs restarting. A
    /// caller who wants a decision made can still treat [`Answer::Cancelled`] as one.
    pub fn back_label(mut self, label: impl Into<String>) -> Self {
        self.back_label = label.into();
        self
    }

    /// The panel's cursor, whole — for a caller that rebuilds the modal on every frame.
    ///
    /// [`default_choice`](Self::default_choice) can seed the cursor and nothing could read it back,
    /// which is fine for the imperative use this was written for: the caller keeps one `Modal` in a
    /// field for as long as the question is up, so the cursor never has to leave it.
    ///
    /// A declarative caller cannot do that. Its view is rebuilt every frame, so its `Modal` is built
    /// every frame too, and without this pair the cursor is reset to the default by the very act of
    /// redrawing — `Down` moves the highlight, the next frame puts it back, and the dialog answers
    /// the first choice whatever the user pointed at.
    ///
    /// The whole [`Prompt`] and not just its index, because the index is not all of it: `Prompt`
    /// also records the row height and viewport it was last drawn at, and scrolling a dialog with
    /// more choices than fit is computed from those. Handing back only `selected()` would have made
    /// a long dialog scroll against a one-pixel viewport — the defect
    /// `Select::set_popup_metrics` exists to prevent, arrived at from the other side.
    pub fn cursor(&self) -> &Prompt {
        &self.prompt
    }

    /// Start from a cursor taken out of an earlier frame's modal. See [`Modal::cursor`].
    ///
    /// After [`default_choice`](Self::default_choice), not before: this restores where the user
    /// actually is, and a default is only where they started.
    pub fn with_cursor(mut self, cursor: Prompt) -> Self {
        self.prompt = cursor;
        self
    }

    /// How many choices there are, for a caller that builds them conditionally.
    pub fn len(&self) -> usize {
        self.choices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.choices.is_empty()
    }

    /// Feed it a key. `None` while the question is still open.
    ///
    /// Every key is consumed whether or not it did anything — a modal covers a screen the user can
    /// no longer see, so a key that leaked would act on something invisible.
    pub fn handle_key(&mut self, ev: KeyEvent) -> Option<Answer<T>> {
        let labels: Vec<&str> = self.choices.iter().map(|(l, _)| l.as_str()).collect();
        match self.prompt.handle_key(ev, &labels).1 {
            PromptAction::Chosen(i) => self.choices.get(i).map(|(_, v)| Answer::Chosen(v.clone())),
            PromptAction::Cancelled => Some(Answer::Cancelled),
            PromptAction::None => None,
        }
    }

    /// Paint over whatever is already on the canvas.
    ///
    /// Including the softkey bar, which is replaced for as long as the modal is up: the bar belongs
    /// to whatever has the keys, and while a modal is up that is the modal. Restoring it is not the
    /// caller's job either — the next frame the caller draws without the modal restores it.
    pub fn draw(&mut self, c: &mut Canvas<'_>, theme: &Theme<'_>) {
        let screen = Rect::from_size(c.size());

        // The scrim: everything behind, dimmed.
        //
        // This is what was missing and what made the first version look broken. The panel was
        // drawn straight onto a chat transcript — bubbles, avatars and text all still at full
        // strength around it — so nothing said which layer was in front, and the eye read the whole
        // screen as one damaged picture rather than as a dialog over a conversation.
        //
        // Done with the canvas's own alpha blend rather than a dither or a checkerboard: `fill_rect`
        // already blends into RGB565, so a translucent black is one call and leaves text underneath
        // legible-but-receded, which is the point. A checkerboard would strobe against the text
        // behind it at this size.
        c.fill_rect(screen, symbian_gfx::Color::rgb(0, 0, 0).with_alpha(0x90));

        let frame = chrome::Frame::split(screen, theme, false, true);
        let labels: Vec<&str> = self.choices.iter().map(|(l, _)| l.as_str()).collect();
        self.prompt.draw(c, frame.content, theme, &self.title, &self.body, &labels);
        chrome::softkey_bar(
            c,
            frame.softkeys,
            theme,
            chrome::Softkeys::new(None, Some(&self.action_label), Some(&self.back_label)),
        );
    }
}

/// Route a key to an optional modal, clearing it when answered.
///
/// The three lines every caller would otherwise write, and the one place to get the *order* right:
/// the modal must see the key before the screen behind it does.
///
/// ```ignore
/// if let Some(answer) = modal::route(&mut self.modal, ev) {
///     match answer { .. }
///     return Handled::Consumed;
/// }
/// if self.modal.is_some() { return Handled::Consumed }   // still open, key eaten
/// ```
///
/// Returns `Some` only on the press that answers it.
pub fn route<T: Clone>(slot: &mut Option<Modal<T>>, ev: KeyEvent) -> Option<Answer<T>> {
    let m = slot.as_mut()?;
    let answer = m.handle_key(ev)?;
    *slot = None;
    Some(answer)
}

/// Whether a modal is up and therefore owns the keyboard.
///
/// For the line after [`route`]: a key that did not answer the question must still not reach the
/// screen underneath.
pub fn owns_keys<T>(slot: &Option<Modal<T>>) -> Handled {
    if slot.is_some() {
        Handled::Consumed
    } else {
        Handled::Ignored
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{Key, Softkey};
    use crate::{testing, Palette};

    /// The defaults follow the phone, and nothing in this file knows how.
    ///
    /// This is the whole point of the change that removed `String::from("Escolher")` from `new`:
    /// a widget shared by every application stopped carrying one application's language. It reads
    /// as a small test because the mechanism is small — a static, a table, and a branch — and the
    /// bug it replaces shipped for months as one Portuguese word in an English dialog.
    #[test]
    fn the_default_labels_are_the_phones_language() {
        use symbian_sys::Lang;

        crate::lang::set(Lang::En);
        let en = Modal::<u8>::new("t", "b");
        assert_eq!(en.action_label, "Select");
        assert_eq!(en.back_label, "Back");

        crate::lang::set(Lang::Pt);
        let pt = Modal::<u8>::new("t", "b");
        assert_eq!(pt.action_label, "Escolher", "the words tg has always shipped");
        assert_eq!(pt.back_label, "Voltar");

        crate::lang::set(Lang::En);
    }

    /// And an override still wins, because a dialog about its own subject wants its own verbs.
    #[test]
    fn an_explicit_label_beats_the_language() {
        use symbian_sys::Lang;
        crate::lang::set(Lang::Pt);
        let m = Modal::<u8>::new("t", "b").action_label("Remove").back_label("Keep");
        assert_eq!(m.action_label, "Remove");
        assert_eq!(m.back_label, "Keep");
        crate::lang::set(Lang::En);
    }
    use symbian_gfx::Size;

    #[derive(Clone, PartialEq, Eq, Debug)]
    enum Msg {
        CopyOpen,
        Open,
        Copy,
    }

    fn modal() -> Modal<Msg> {
        Modal::new("Abrir link", "https://exemplo.com")
            .choice("Copiar e abrir", Msg::CopyOpen)
            .choice("Apenas abrir", Msg::Open)
            .choice("Copiar link", Msg::Copy)
    }

    fn ev(k: Key) -> KeyEvent {
        KeyEvent::new(k)
    }

    #[test]
    fn a_choice_returns_the_value_written_beside_it() {
        // The whole point: the label and the meaning are one declaration, so an index can never map
        // to the wrong action. The launcher had them as two lists and this is what replaced it.
        let mut m = modal();
        m.handle_key(ev(Key::Down));
        assert_eq!(m.handle_key(ev(Key::Select)), Some(Answer::Chosen(Msg::Open)));
    }

    #[test]
    fn the_default_choice_is_where_the_cursor_starts() {
        let mut m = modal().default_choice(2);
        assert_eq!(m.handle_key(ev(Key::Select)), Some(Answer::Chosen(Msg::Copy)));
    }

    #[test]
    fn back_cancels_and_chooses_nothing() {
        let mut m = modal();
        assert_eq!(m.handle_key(ev(Key::Softkey(Softkey::Right))), Some(Answer::Cancelled));
    }

    #[test]
    fn an_unanswered_key_is_still_swallowed() {
        // Modal means modal. `handle_key` answering `None` is "the question is still open", not
        // "pass it on" — and `owns_keys` is what says so to the caller.
        let mut m = modal();
        assert_eq!(m.handle_key(ev(Key::Char('x'))), None);
        let slot = Some(m);
        assert_eq!(owns_keys(&slot), Handled::Consumed);
        assert_eq!(owns_keys(&None::<Modal<Msg>>), Handled::Ignored);
    }

    #[test]
    fn route_clears_the_slot_exactly_when_it_answers() {
        // The bug this helper exists to prevent: a caller that forgets to clear leaves a modal that
        // answers the same question for ever, and one that clears too early loses the answer.
        let mut slot = Some(modal());
        assert_eq!(route(&mut slot, ev(Key::Down)), None);
        assert!(slot.is_some(), "still open after a key that only moved the cursor");
        assert_eq!(route(&mut slot, ev(Key::Select)), Some(Answer::Chosen(Msg::Open)));
        assert!(slot.is_none(), "cleared by the press that answered it");
        assert_eq!(route(&mut slot, ev(Key::Select)), None, "and stays cleared");
    }

    #[test]
    fn a_modal_with_no_choices_can_still_be_dismissed() {
        // An acknowledgement — "this went wrong" with nothing to decide. It must not be a screen
        // with no way out.
        let mut m: Modal<()> = Modal::new("Erro", "nao deu");
        assert!(m.is_empty());
        assert_eq!(m.handle_key(ev(Key::Select)), None);
        assert_eq!(m.handle_key(ev(Key::Softkey(Softkey::Right))), Some(Answer::Cancelled));
    }

    #[test]
    fn the_screen_behind_is_dimmed_and_not_erased() {
        // Both halves matter and they pull against each other. Erased, the modal is a navigation
        // and the caller has to rebuild a screen. Left at full strength, the panel sits on a chat
        // transcript with nothing saying which layer is in front — which is exactly what the first
        // version looked like, and it read as one damaged picture rather than as a dialog.
        //
        // So: the ink behind must still be there, and must be darker than it was.
        testing::with_theme(Palette::DARK, |t| {
            let (_, px) = testing::with_canvas(Size::new(320, 240), |c| {
                c.fill_rect(Rect::from_xywh(0, 0, 320, 240), symbian_gfx::Color::rgb(255, 0, 0));
                modal().draw(c, t);
            });
            // The top-left corner is outside the panel: the scrim is the only thing that touched it.
            let corner = px[0];
            let full_red = symbian_gfx::Color::rgb(255, 0, 0).to_rgb565().0;
            assert_ne!(corner, full_red, "the scrim did not dim anything");
            assert_ne!(corner, 0, "the screen behind was erased, not dimmed");
            // Still recognisably red: the red channel survives, the others stay empty.
            assert!(corner >> 11 > 0, "the colour behind was lost, not dimmed: {corner:#06x}");
            assert!(corner >> 11 < full_red >> 11, "not actually darker");
        });
    }
}
