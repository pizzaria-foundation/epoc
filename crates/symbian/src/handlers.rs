//! Which application opens which kind of link, as the user set it.
//!
//! # Why the format lives in the SDK
//!
//! The launcher *writes* this file, from its Settings screen. Any application may need to *read*
//! it — a chat client deciding what to do with a link is the case this was built for, and it must
//! not have to ask the launcher, because asking means bringing the launcher to the foreground and
//! that is a different screen appearing over the one the user was on.
//!
//! Two parties, neither depending on the other, so the format belongs to the SDK they share. A
//! second copy of the encoding in the reader would drift from the writer the first time either
//! changed, and the symptom would be a default that reads back as absent.
//!
//! # What is *not* here
//!
//! Which applications are worth offering, and how they are matched — that is a table about this
//! phone and this project's taste, and it stays in the launcher beside the screen that shows it.
//! This module knows only how a choice is written down.

use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::{self, Fs, Utf16Path};

/// A scheme the registry knows how to route.
///
/// A fixed list rather than "whatever a URL happens to carry": the Settings screen shows a row per
/// scheme, and a screen that grew a row the first time somebody was sent an `ftp://` link would be
/// a screen whose shape depends on message history.
pub const SCHEMES: [&str; 4] = ["http", "https", "mailto", "tg"];

/// The platform's own web browser.
///
/// A fact about this generation of Symbian rather than a policy about this project, which is why it
/// sits here and not in whichever application happens to want it. It is the answer to "what would
/// the phone do", and that is what an application falls back on when there is no registry to
/// consult — because no launcher is installed to keep one.
pub const NATIVE_BROWSER: u32 = 0x1000_8D39;

/// Where the launcher keeps it.
pub const FILE: &str = "C:\\Data\\launcher\\defaults.dat";

/// One record: four bytes of UID and a scheme, length-prefixed.
///
/// Text would be easier to read on the device and is not worth it here — the scheme is already
/// short and the launcher writes half a dozen other blobs in this shape, so a reader of this
/// codebase has seen it before.
const MAX_SCHEME: usize = 16;

/// Scheme-to-application, as the user set it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Defaults {
    entries: Vec<(String, u32)>,
}

impl Defaults {
    pub const fn new() -> Self {
        Self { entries: Vec::new() }
    }

    /// Which application opens `scheme`, if the user has said.
    ///
    /// The scheme is matched case-insensitively because [`symbian::url::scheme_of`] lowercases and
    /// a hand-edited file might not.
    pub fn get(&self, scheme: &str) -> Option<u32> {
        self.entries
            .iter()
            .find(|(s, _)| s.eq_ignore_ascii_case(scheme))
            .map(|(_, uid)| *uid)
    }

    /// Set, replace, or — with `None` — clear the handler for `scheme`.
    pub fn set(&mut self, scheme: &str, uid3: Option<u32>) {
        self.entries.retain(|(s, _)| !s.eq_ignore_ascii_case(scheme));
        if let Some(uid) = uid3 {
            self.entries.push((scheme.to_ascii_lowercase(), uid));
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Serialise for `C:\Data\launcher\defaults.dat`.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for (scheme, uid) in &self.entries {
            if scheme.is_empty() || scheme.len() > MAX_SCHEME {
                continue;
            }
            out.push(scheme.len() as u8);
            out.extend_from_slice(scheme.as_bytes());
            out.extend_from_slice(&uid.to_le_bytes());
        }
        out
    }

    /// Read a blob written by [`encode`].
    ///
    /// A truncated or corrupt tail is dropped rather than refused: this file is a convenience, and
    /// losing one mapping is better than a launcher that will not start because a settings file has
    /// a byte missing.
    pub fn decode(bytes: &[u8]) -> Self {
        let mut me = Self::new();
        let mut i = 0usize;
        while i < bytes.len() {
            let n = bytes[i] as usize;
            i += 1;
            if n == 0 || n > MAX_SCHEME || i + n + 4 > bytes.len() {
                break;
            }
            let Ok(scheme) = core::str::from_utf8(&bytes[i..i + n]) else {
                break;
            };
            i += n;
            let uid = u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
            i += 4;
            me.set(scheme, Some(uid));
        }
        me
    }
}


/// Read the registry from disk, or an empty one when it has never been written.
///
/// An absent file is the first run, not a failure — every scheme simply has no handler, and the
/// caller says so rather than reporting an error nobody can act on.
pub fn load<F: Fs>(f: &mut F) -> Defaults {
    let Ok(path) = Utf16Path::new(FILE) else {
        return Defaults::new();
    };
    match fs::read(f, &path) {
        Ok(Some(bytes)) => Defaults::decode(&bytes),
        _ => Defaults::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::MemFs;

    #[test]
    fn a_setting_survives_the_round_trip() {
        let mut d = Defaults::new();
        d.set("http", Some(0x1000_8D39));
        d.set("tg", Some(0xE123_4569));
        assert_eq!(Defaults::decode(&d.encode()), d);
    }

    #[test]
    fn setting_a_scheme_twice_replaces_rather_than_appends() {
        // Otherwise `get` would answer with whichever copy it met first, and the file would grow a
        // record every time the user changed their mind.
        let mut d = Defaults::new();
        d.set("http", Some(1));
        d.set("http", Some(2));
        assert_eq!(d.get("http"), Some(2));
        assert_eq!(d.encode().len(), Defaults::decode(&d.encode()).encode().len());
    }

    #[test]
    fn clearing_a_scheme_leaves_no_handler_rather_than_a_zero() {
        // A zero UID would launch nothing and report success, which is the failure mode this whole
        // registry is arranged to avoid.
        let mut d = Defaults::new();
        d.set("http", Some(7));
        d.set("http", None);
        assert_eq!(d.get("http"), None);
        assert!(d.is_empty());
    }

    #[test]
    fn schemes_match_without_regard_to_case() {
        let mut d = Defaults::new();
        d.set("HTTP", Some(7));
        assert_eq!(d.get("http"), Some(7));
        assert_eq!(Defaults::decode(&d.encode()).get("http"), Some(7));
    }

    #[test]
    fn a_truncated_file_loses_the_tail_and_not_the_reader() {
        let mut d = Defaults::new();
        d.set("http", Some(1));
        d.set("mailto", Some(2));
        let blob = d.encode();
        for cut in 1..blob.len() {
            let _ = Defaults::decode(&blob[..cut]);
        }
        assert_eq!(Defaults::decode(&blob), d);
    }

    #[test]
    fn rubbish_decodes_to_nothing_instead_of_panicking() {
        assert!(Defaults::decode(&[]).is_empty());
        assert!(Defaults::decode(&[0]).is_empty(), "a zero-length scheme");
        assert!(Defaults::decode(&[200, 1, 2, 3]).is_empty(), "a length past the end");
        assert!(Defaults::decode(&[2, 0xff, 0xfe, 1, 2, 3, 4]).is_empty(), "not text");
    }

    #[test]
    fn a_registry_that_was_never_written_reads_as_empty() {
        // First run. Every scheme has no handler, which the caller reports as such — an error here
        // would be one nobody can act on.
        let mut f = MemFs::new();
        assert!(load(&mut f).is_empty());
    }

    #[test]
    fn what_the_launcher_wrote_is_what_another_process_reads() {
        // The whole reason this module is in the SDK: two processes, one format. A second copy of
        // the encoding in the reader would drift and a default would read back as absent.
        let mut f = MemFs::new();
        let mut d = Defaults::new();
        d.set("https", Some(0x2000_1111));
        let path = Utf16Path::new(FILE).unwrap();
        let _ = f.mkdir(&"C:\\Data\\launcher\\".encode_utf16().collect::<Vec<u16>>());
        fs::write_atomic(&mut f, &path, &d.encode()).unwrap();
        assert_eq!(load(&mut f).get("https"), Some(0x2000_1111));
    }
}
