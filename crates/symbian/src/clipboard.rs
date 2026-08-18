//! The system clipboard, both directions.
//!
//! Writing came first, for one case: an application that will not take a URL on its command line can
//! still be handed one, if the user pastes — which works for *every* application on the phone,
//! unlike a command line, which works for the few that parse one.
//!
//! Reading is the half that makes our own applications ordinary citizens of the phone. Without it a
//! phone number copied in Contacts has to be retyped in ours, and the user is the one carrying data
//! between two programs that both know how to do it.
//!
//! # It is a file, not a variable
//!
//! Symbian's clipboard is a stream store on disk with a stream dictionary keyed by content type,
//! not a string in a global. Putting text on it means building the same `CPlainText` document an
//! editor would have built and serialising it under the UID every Avkon editor looks for. Text
//! written any other way produces a clipboard that Paste cannot read — and reports nothing at all,
//! because there is no error to report: the paste simply does nothing.
//!
//! All of that is in the shim. What is worth knowing here is that a copy is a **file write**, so it
//! is not free and it is not something to do on every frame.
//!
//! # Only for a binary built with `USE_CLIPBOARD=1`
//!
//! It imports `bafl` and `etext`, which nothing else in this SDK needs, and an import that does not
//! resolve makes the whole image fail to load with no panic and no log. Every other build links a
//! stub that answers [`Error::NotSupported`] — a quiet no, which is the honest answer for a binary
//! that did not ask for the path.

use alloc::string::String;
use alloc::vec::Vec;

use symbian_sys as sys;

use crate::error::{Error, Result};

/// How much of a long clipboard one paste delivers, in UTF-16 units.
///
/// A fixed buffer rather than an ask-then-read pair of calls: the clipboard is a file, so asking
/// for the length first means opening and parsing the store twice, and the answer can change
/// between the two reads. 4096 units is longer than any field this SDK draws.
///
/// It is allocated on the **heap**, deliberately: 8 KB of `u16` is the whole default stack of a
/// Symbian thread, and a stack overflow here would look like a random panic in whatever ran next.
const MAX_UNITS: usize = 4096;

/// Put `text` on the system clipboard, so any application's Paste can take it.
///
/// ```ignore
/// // The link could not be handed to the browser on its command line, so hand it to the user.
/// let _ = symbian::clipboard::set_text(url);
/// ```
///
/// Empty text is refused rather than silently clearing the clipboard: losing whatever the user had
/// copied is a side effect nobody asked for, and "copy nothing" is not a request anybody makes on
/// purpose.
pub fn set_text(text: &str) -> Result<()> {
    if text.is_empty() {
        return Err(Error::Argument);
    }
    let units: Vec<u16> = text.encode_utf16().collect();
    // SAFETY: the pointer and length describe `units`, which outlives the call; the shim copies
    // into a descriptor before anything that can leave.
    Error::check(unsafe { sys::shim_clip_set_text(units.as_ptr(), units.len() as i32) })
}

/// Whatever plain text is on the system clipboard.
///
/// ```ignore
/// // Ctrl+V in a text field: what the user copied, wherever they copied it.
/// if let Ok(text) = symbian::clipboard::get_text() {
///     field.insert_str(&text);
/// }
/// ```
///
/// [`Error::NotFound`] means there is nothing to paste — an empty clipboard, or one holding
/// something that is not text (an image, a contact). That is a state, not a failure: a caller
/// should do nothing and say nothing, exactly as the platform's own Paste does.
///
/// Text beyond [`MAX_UNITS`] is truncated rather than refused, and a clipboard holding unpaired
/// surrogates is read lossily — half a character is still better than dropping the paste, and the
/// alternative is an error nobody can act on.
pub fn get_text() -> Result<String> {
    let mut buf = alloc::vec![0u16; MAX_UNITS];
    let mut len = 0i32;
    // SAFETY: `buf` holds MAX_UNITS units and outlives the call; the shim writes at most `cap` of
    // them and reports how many through `len`.
    Error::check(unsafe { sys::shim_clip_get_text(buf.as_mut_ptr(), MAX_UNITS as i32, &mut len) })?;
    let n = (len.max(0) as usize).min(MAX_UNITS);
    Ok(String::from_utf16_lossy(&buf[..n]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_is_refused_rather_than_clearing_the_clipboard() {
        // Off the device every call answers NotReady, so this is the one behaviour testable on the
        // host — and it is the one worth pinning, because the alternative silently destroys
        // something the user put there.
        assert!(matches!(set_text(""), Err(Error::Argument)));
    }

    #[test]
    fn reading_off_the_device_is_an_error_and_never_an_empty_string() {
        // The distinction a caller depends on: "could not read the clipboard" must not arrive as
        // "the clipboard is empty", or a paste would look like it worked and delivered nothing.
        assert!(get_text().is_err());
    }
}
