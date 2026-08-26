//! Asking the launcher to open something — a URL — from another application.
//!
//! # Why the contract is here and not in either party
//!
//! The chat client sends the request and the launcher answers it. They live in different
//! repositories and neither depends on the other; the only place they can agree on a category, a
//! key and a file path is the SDK they both already depend on. Written twice instead, the pair
//! would drift the first time one side changed a number, and the symptom would be a link that does
//! nothing at all — no error, no log, because a request nobody is listening for looks exactly like
//! a request that was never sent.
//!
//! # Why a file and a counter, and not just the property
//!
//! Publish & Subscribe on this platform carries an `i32` and this SDK's wrapper exposes only that
//! (see [`crate::prop`]). A URL does not fit in a machine word. So the payload goes in a file and
//! the *signal* goes through P&S — which is exactly the shape [`iconsvc`] already uses in the
//! launcher, and the one place in this codebase where cross-process wake-up has been debugged on
//! real hardware.
//!
//! ```text
//!   caller                                    launcher (resident)
//!     write REQUEST_FILE            ──►
//!     prop::set(CATEGORY, KEY, n+1) ──►        SHIM_EV_PROP wakes it
//!                                              take_request() reads and DELETES
//!                                              resolve the scheme, launch
//! ```
//!
//! # The request is consumed, not left lying about
//!
//! [`take_request`] deletes the file as it reads it. A request is an *event*: a file that survived
//! would be re-read on the next boot and open a link the user asked for yesterday. The delete
//! happening before the launch is deliberate for the same reason — if opening the URL takes the
//! launcher down, the request must not be waiting to take it down again.
//!
//! # What this is not
//!
//! Not a general intent system. There is one verb, and adding a second is a decision about a wire
//! format rather than another constant — the file is a bare UTF-8 URL precisely so that the day a
//! second verb exists, the format has to be changed deliberately and both sides notice.

use alloc::string::String;

use crate::error::{Error, Result};
use crate::fs::{self, Fs, Utf16Path};

/// The launcher's P&S category — its UID3.
///
/// The launcher defines its keys `define_public` so a process with a different SID can reach them;
/// see the activity key it already publishes for the home-screen daemons.
pub const CATEGORY: u32 = 0xE0AA_0000;

/// The launcher's UID3, for a caller that needs to *start* it.
///
/// The same number as [`CATEGORY`], and named separately because the two are different facts that
/// happen to coincide: a P&S category is a UID by convention, and the launcher publishes under its
/// own. A caller launching the app should say so rather than passing a category to `apps::launch`.
pub const LAUNCHER_UID: u32 = CATEGORY;

/// The key bumped to say "there is a request waiting".
///
/// The *value* carries nothing but change: it is a wrapping counter, and a subscriber learns only
/// that it moved. Everything else is in the file. Key 100 is the launcher's foreground-activity
/// signal; this is the next one.
pub const OPEN_URL_KEY: u32 = 101;

/// The key bumped to say "there is a notice waiting to be delivered".
///
/// The return lane, and it exists because of a rule this codebase already paid for: the launcher
/// must not link `msgs.dll`. Its own manifest says so — the unread count comes from the bundled
/// `notifd` daemon precisely so a broken ordinal costs the count and not the home screen. So the
/// launcher cannot post an Inbox message, and the daemon that already can does it instead.
///
/// The first attempt put the text on the home screen's notification line, reusing the row that
/// belongs to Messages. That was wrong twice over: it borrows a control that means something else,
/// and it reports to a screen the user is not looking at — the launcher is in the background when
/// a request arrives, which is the whole reason the request exists. An Inbox entry waits.
pub const NOTICE_KEY: u32 = 102;

/// Where a notice waits until the daemon delivers it.
pub const NOTICE_FILE: &str = "C:\\Data\\launcher\\notice.dat";

/// Where the payload is written.
///
/// Under `C:\Data`, which needs no capability to write — the caller may be an application with
/// nothing but `WriteUserData`, and a channel that required a capability would be a channel most
/// applications could not use.
pub const REQUEST_FILE: &str = "C:\\Data\\launcher\\openurl.dat";

/// The directory holding it, which may not exist yet.
///
/// Created blind before every write. `RFs::MkDirAll` drops the last path component when there is no
/// trailing separator — the trap that once left this exact directory missing and killed a whole
/// pipeline silently, so the separator here is load-bearing.
const REQUEST_DIR: &str = "C:\\Data\\launcher\\";

/// The longest URL that will be carried.
///
/// Not a protocol limit — a sanity limit. A megabyte of "URL" in a message is not a link anybody
/// meant to open, and the launcher reads this file into a fixed buffer.
pub const MAX_URL: usize = 1024;

/// Ask the launcher to open `url`.
///
/// Writes the payload, then bumps the counter. That order matters and is not obvious: the counter
/// is what wakes the launcher, so bumping first would race — the launcher can be scheduled between
/// the two calls and find a file that is missing or half written.
///
/// `Ok(())` means the request was *posted*, not that anything opened. Whether a handler exists is
/// the launcher's question, and it is answered on the launcher's screen — this call has no way to
/// wait for it and deliberately does not try.
///
/// Which is exactly why it also calls [`yield_screen`]: the answer appears on a screen the caller
/// is standing in front of. The launcher is woken by a property, and a property does not raise a
/// window group — so its question is drawn behind the application that asked, and the user sees a
/// link that did nothing.
pub fn request_open<F: Fs>(fs: &mut F, url: &str) -> Result<()> {
    write_request(fs, url)?;
    signal()?;
    yield_screen();
    Ok(())
}

/// Get out of the way of the launcher's answer.
///
/// A caller that drives the channel by hand — [`write_request`] then [`signal`], because it has a
/// fallback to run when the bell goes unheard — has to do this itself once it knows the launcher
/// took the request. [`request_open`] does it for everyone else.
///
/// No `Result`, deliberately. Whether we managed to leave says nothing about whether the request
/// will be answered, there is nothing a caller could do differently, and off the device it always
/// fails — so a return value here would only be an error nobody can act on, in the one place a
/// caller is least able to react.
///
/// The mechanism is [`crate::apps::to_background`], and the reason it is the right one is written
/// down twice already on this platform: `shim_net.cpp` steps aside before the CommsDat access-point
/// dialog for the same reason, having learned on the handset that S60 3rd Edition draws it behind
/// the foreground application. Stepping aside also happens to reveal the launcher itself, since on
/// this phone it *is* what is behind everything.
pub fn yield_screen() {
    let _ = crate::apps::to_background();
}

/// Write the payload, without ringing the bell.
///
/// Split from [`signal`] so the half that is pure filesystem can be tested on the host: P&S needs a
/// kernel and answers `NotReady` off the device, which would have made the whole channel untestable
/// and left the file format — the part most likely to be got wrong — covered by nothing.
///
/// A caller wanting the channel wants [`request_open`]; this alone posts a request nobody is woken
/// for, which the launcher would find only the next time it happened to look.
pub fn write_request<F: Fs>(fs: &mut F, url: &str) -> Result<()> {
    if url.is_empty() || url.len() > MAX_URL {
        return Err(Error::Argument);
    }
    let dir: alloc::vec::Vec<u16> = REQUEST_DIR.encode_utf16().collect();
    // Blind: an existing directory is success, and the only interesting failure is one that the
    // write below will report anyway.
    let _ = fs.mkdir(&dir);
    fs::write_atomic(fs, &Utf16Path::new(REQUEST_FILE)?, url.as_bytes())
}

/// Bump the counter that wakes the launcher.
///
/// Called *after* the payload is written, and the order is not decoration: this is what schedules
/// the launcher, so ringing first lets it run between the two calls and find a file that is missing
/// or half written.
pub fn signal() -> Result<()> {
    // Wrapping, because the value means nothing: a subscriber sees only that it changed. Reading
    // first and ignoring a read failure keeps a fresh property (never set) working — it starts at
    // zero and the bump makes it one.
    let now = crate::prop::get(CATEGORY, OPEN_URL_KEY).unwrap_or(0);
    crate::prop::set(CATEGORY, OPEN_URL_KEY, now.wrapping_add(1))
}

/// Take the pending request, if there is one, removing it.
///
/// `None` when there is nothing waiting — which is the normal answer, since the launcher also wakes
/// for its own reasons and has no way to tell which signal it was.
pub fn take_request<F: Fs>(fs: &mut F) -> Option<String> {
    let path = Utf16Path::new(REQUEST_FILE).ok()?;
    let bytes = fs::read(fs, &path).ok().flatten()?;
    // Deleted before the caller ever sees the URL. If opening it takes the process down, the
    // request must not still be here to do it again on the next start.
    let _ = fs.delete(path.as_units());
    if bytes.is_empty() || bytes.len() > MAX_URL {
        return None;
    }
    // Lossless or nothing: a mangled URL is worse than no URL, because it would launch something at
    // an address nobody wrote.
    String::from_utf8(bytes).ok()
}

/// Leave a line for the messaging daemon to deliver to the Inbox.
///
/// Same shape as [`write_request`] and for the same reason — the payload does not fit in a
/// property — but pointing the other way: the launcher writes, `notifd` reads.
pub fn post_notice<F: Fs>(fs: &mut F, text: &str) -> Result<()> {
    if text.is_empty() || text.len() > MAX_URL {
        return Err(Error::Argument);
    }
    let dir: alloc::vec::Vec<u16> = REQUEST_DIR.encode_utf16().collect();
    let _ = fs.mkdir(&dir);
    fs::write_atomic(fs, &Utf16Path::new(NOTICE_FILE)?, text.as_bytes())
}

/// Ring the bell for a notice. Split from [`post_notice`] for the same testability reason
/// [`signal`] is split from [`write_request`].
pub fn signal_notice() -> Result<()> {
    let now = crate::prop::get(CATEGORY, NOTICE_KEY).unwrap_or(0);
    crate::prop::set(CATEGORY, NOTICE_KEY, now.wrapping_add(1))
}

/// Take the pending notice, if any, removing it.
pub fn take_notice<F: Fs>(fs: &mut F) -> Option<String> {
    let path = Utf16Path::new(NOTICE_FILE).ok()?;
    let bytes = fs::read(fs, &path).ok().flatten()?;
    let _ = fs.delete(path.as_units());
    if bytes.is_empty() || bytes.len() > MAX_URL {
        return None;
    }
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::MemFs;

    fn path(s: &str) -> Utf16Path {
        Utf16Path::new(s).unwrap()
    }

    #[test]
    fn a_request_survives_the_trip_and_is_consumed_once() {
        let mut fs = MemFs::new();
        write_request(&mut fs, "https://exemplo.com/a?b=1").unwrap();

        assert_eq!(take_request(&mut fs).as_deref(), Some("https://exemplo.com/a?b=1"));
        // The second read finds nothing: a request is an event, and one left behind would reopen
        // itself on the next boot.
        assert_eq!(take_request(&mut fs), None);
    }

    #[test]
    fn a_notice_travels_the_other_way_and_is_also_consumed_once() {
        // The return lane. Same shape as the request, opposite direction: the launcher writes it
        // because it may not link msgs.dll, and the daemon that may delivers it.
        let mut fs = MemFs::new();
        post_notice(&mut fs, "sem app para http").unwrap();
        assert_eq!(take_notice(&mut fs).as_deref(), Some("sem app para http"));
        assert_eq!(take_notice(&mut fs), None);
    }

    #[test]
    fn a_notice_and_a_request_do_not_collide() {
        // Two files, two keys. Sharing either would make a notice consume a request or the reverse,
        // and the symptom would be a link that opens the wrong thing exactly once.
        let mut fs = MemFs::new();
        write_request(&mut fs, "https://a.com").unwrap();
        post_notice(&mut fs, "um aviso").unwrap();
        assert_eq!(take_request(&mut fs).as_deref(), Some("https://a.com"));
        assert_eq!(take_notice(&mut fs).as_deref(), Some("um aviso"));
    }

    #[test]
    fn nothing_pending_is_not_an_error() {
        // The launcher wakes for its own reasons too, so "no request" is the common answer and must
        // not read as a failure.
        let mut fs = MemFs::new();
        assert_eq!(take_request(&mut fs), None);
    }

    #[test]
    fn the_directory_is_created_rather_than_assumed() {
        // The trap this repo has already paid for once: the write goes to a directory that does not
        // exist yet, fails silently, and the whole channel is dead with nothing to see.
        let mut fs = MemFs::new();
        write_request(&mut fs, "https://a.com").unwrap();
        assert!(fs::read(&mut fs, &path(REQUEST_FILE)).unwrap().is_some());
    }

    #[test]
    fn a_second_request_replaces_the_first() {
        // Last writer wins, deliberately: two links pressed in a row should open the second one,
        // not both and not the older.
        let mut fs = MemFs::new();
        write_request(&mut fs, "https://um.com").unwrap();
        write_request(&mut fs, "https://dois.com").unwrap();
        assert_eq!(take_request(&mut fs).as_deref(), Some("https://dois.com"));
        assert_eq!(take_request(&mut fs), None);
    }

    #[test]
    fn an_empty_or_absurd_url_is_refused_at_the_door() {
        let mut fs = MemFs::new();
        assert!(write_request(&mut fs, "").is_err());
        let huge = alloc::string::String::from_utf8(alloc::vec![b'a'; MAX_URL + 1]).unwrap();
        assert!(write_request(&mut fs, &huge).is_err());
        // And neither left a file behind for the launcher to find.
        assert_eq!(take_request(&mut fs), None);
    }

    #[test]
    fn a_url_at_the_limit_still_goes_through() {
        // The boundary, because an off-by-one here is a link that works until it does not.
        let mut fs = MemFs::new();
        let long = alloc::string::String::from_utf8(alloc::vec![b'a'; MAX_URL]).unwrap();
        assert!(write_request(&mut fs, &long).is_ok());
        assert_eq!(take_request(&mut fs).map(|s| s.len()), Some(MAX_URL));
    }

    #[test]
    fn bytes_that_are_not_text_are_dropped_rather_than_mangled() {
        // A truncated write or a stale file from another version: launching something at a mangled
        // address is worse than not launching.
        let mut fs = MemFs::new();
        let _ = fs.mkdir(&REQUEST_DIR.encode_utf16().collect::<alloc::vec::Vec<u16>>());
        fs::write_atomic(&mut fs, &path(REQUEST_FILE), &[0xff, 0xfe, 0xfd]).unwrap();
        assert_eq!(take_request(&mut fs), None);
        // Still consumed, so a bad file cannot wedge the channel for ever.
        assert!(fs::read(&mut fs, &path(REQUEST_FILE)).unwrap().is_none());
    }
}
