//! The clipboard, as the toolkit sees it: two methods and no idea where the text goes.
//!
//! # Why this is a trait and not a call
//!
//! Copying is a *device* operation — on this platform a file write into a stream store — and this
//! crate must not learn about the device: it draws, and it compiles and tests on the host. The
//! obvious dodge is a global the device layer fills in at start-up, which is how `symbian` registers
//! the log sink. It is not available here: `symbian-ui` is `#![forbid(unsafe_code)]`, and a mutable
//! global needs `unsafe` to read.
//!
//! So the clipboard arrives as an argument. That turns out to be the better shape anyway:
//!
//! - **Nothing to forget.** [`crate::TextField::handle_key`] takes one, so every field in every app
//!   handles Ctrl+C/X/V — a caller cannot accidentally ship a field that ignores paste, because
//!   there is no signature that lets them.
//! - **It tests.** A [`MemClipboard`] makes copy-and-paste an ordinary host unit test, rather than
//!   something only provable by pressing keys on a handset.
//! - **It stays honest about cost.** A `set` is a file write. Passing the thing that does it makes
//!   that visible at the call site instead of hiding it behind a static.
//!
//! The device implementation lives in `symbian-app`, which is the crate that already knows both
//! this toolkit and the platform.
//!
//! # Replacing the default, at whatever level it is wrong
//!
//! What the SDK ships is a *default*, not a policy. An application that disagrees does not fork the
//! editing code — it overrides at one of three levels, in rising order of how much it takes over:
//!
//! **1. A different clipboard, per app or per screen.** Implement [`Clipboard`] and pass it. It is
//! an argument to every call, so one screen can hold something the rest of the app does not — a
//! composer that pastes through a sanitiser, a form that pastes from a scratch buffer of its own:
//!
//! ```
//! use symbian_ui::{Clipboard, NoClipboard};
//! # use alloc::string::String;
//! # extern crate alloc;
//!
//! /// Paste as one line, whatever was copied. A composer is a single-line field, and a pasted
//! /// newline used to arrive as a character nothing would ever draw.
//! struct OneLine<C>(C);
//!
//! impl<C: Clipboard> Clipboard for OneLine<C> {
//!     fn get(&mut self) -> Option<String> {
//!         self.0.get().map(|t| t.replace(['\n', '\r'], " "))
//!     }
//!     fn set(&mut self, text: &str) -> bool {
//!         self.0.set(text)
//!     }
//! }
//!
//! let mut clip = OneLine(NoClipboard);
//! assert_eq!(clip.get(), None);
//! ```
//!
//! An app that wants one clipboard *everywhere* holds it as a field and hands out `&mut` from a
//! single accessor, rather than naming a type at each call site — which is also the seam that lets
//! its tests run the whole app against a [`MemClipboard`].
//!
//! **2. A different meaning for a chord, per screen.** Offer the key to the field first and act on
//! what comes back. A chord the field could not honour answers `Ignored` — precisely so a screen
//! can put its own behaviour underneath without racing the default:
//!
//! ```ignore
//! // Ctrl+C copies the field's selection; with nothing selected and nothing copied, this screen
//! // copies the message the cursor is on instead.
//! self.composer.handle_key(ev, clip).or_else(|| self.copy_highlighted_message(clip))
//! ```
//!
//! Or take the key before the field ever sees it, which is what a screen does when the chord means
//! something else entirely there.
//!
//! **3. Different editing altogether.** [`crate::TextField`]'s pieces are all public —
//! `paste`, `copy`, `cut`, `select_all`, `selection`, `insert_str`. An app that wants its own
//! bindings skips `handle_key` and calls them, and still does not reimplement caret arithmetic on
//! a `char` boundary, which is the part that panics when it is wrong.

use alloc::string::{String, ToString};

/// Somewhere to put text, and somewhere to get it back.
///
/// `get` answering `None` covers every uninteresting case at once — nothing was ever copied, the
/// clipboard holds an image, the platform refused — because a text field can do exactly the same
/// thing about all of them: nothing at all, silently, the way the platform's own Paste does.
pub trait Clipboard {
    /// The plain text on the clipboard, if there is any.
    fn get(&mut self) -> Option<String>;

    /// Put `text` on the clipboard. `false` if it could not be done, which callers may show but
    /// must not depend on — a copy that failed leaves whatever was there before.
    fn set(&mut self, text: &str) -> bool;
}

/// A clipboard for a build that has none.
///
/// Not an error case: an app compiled without the platform's clipboard support, a preview harness,
/// a widget test that is about something else. Paste does nothing and copy reports that it did
/// nothing, which is the same behaviour as a device whose clipboard is empty.
#[derive(Copy, Clone, Debug, Default)]
pub struct NoClipboard;

impl Clipboard for NoClipboard {
    fn get(&mut self) -> Option<String> {
        None
    }

    fn set(&mut self, _text: &str) -> bool {
        false
    }
}

/// A clipboard that is just a `String`.
///
/// For tests and for the simulator, where there is no platform clipboard to talk to but copy and
/// paste should still work between two fields on the same screen — which is enough to exercise
/// every line of the editing logic without a handset.
#[derive(Clone, Debug, Default)]
pub struct MemClipboard {
    text: Option<String>,
}

impl MemClipboard {
    pub const fn new() -> Self {
        Self { text: None }
    }

    /// Pre-load it, so a test can paste without copying first.
    pub fn with_text(text: &str) -> Self {
        Self { text: Some(text.to_string()) }
    }
}

impl Clipboard for MemClipboard {
    fn get(&mut self) -> Option<String> {
        self.text.clone()
    }

    fn set(&mut self, text: &str) -> bool {
        self.text = Some(text.to_string());
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_clipboard_answers_none_rather_than_an_empty_string() {
        // The distinction the field depends on: nothing to paste is not "paste nothing", which
        // would still count as an edit and still claim a redraw.
        assert_eq!(MemClipboard::new().get(), None);
        assert_eq!(NoClipboard.get(), None);
    }

    #[test]
    fn what_goes_in_comes_out() {
        let mut clip = MemClipboard::new();
        assert!(clip.set("hello"));
        assert_eq!(clip.get().as_deref(), Some("hello"));
    }

    #[test]
    fn the_absent_clipboard_refuses_a_copy_instead_of_pretending() {
        // A caller that shows "Copied" must be told the truth, or it lies on every build that did
        // not compile the platform's clipboard in.
        assert!(!NoClipboard.set("hello"));
    }
}
