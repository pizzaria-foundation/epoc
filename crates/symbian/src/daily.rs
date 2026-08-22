//! What is on the calendar in the next few days, for a home screen to show at a glance.
//!
//! # Why not [`crate::agenda`], which already connects these two
//!
//! The reminder queue looks like the answer and is not, for three reasons that are each fatal on
//! their own:
//!
//! * it carries only events that **have a reminder set** — most of a calendar does not;
//! * its `due` is the *reminder* instant, not the event's start, so "20 minutes before the 9 o'clock
//!   meeting" is stored as 08:40 and there is nothing in the file that says 09:00;
//! * `notifd` **consumes** entries as they fire ([`crate::agenda::take_due`] rewrites the file
//!   without them), so the 09:00 meeting leaves the file at 08:40 — which is exactly the hour a home
//!   screen most wants to show it.
//!
//! A reminder is a thing that happens once; this is a thing that is *true for a while*. Same two
//! processes, same directory, same rationale — a different file, because they are different facts.
//!
//! # Who writes it
//!
//! The calendar, on every change, exactly as it already publishes the reminder queue. Not a daemon
//! reading the calendar's database: [`crate::agenda`]'s header makes that argument and it holds
//! unchanged here — it would couple a binary that comes up at boot to a schema that will change, and
//! put `sqldb` in an image that exists to be small.
//!
//! ```text
//!   calendar                                        launcher (running since boot)
//!     occurrences(today .. +WINDOW_DAYS)
//!     daily::publish(entries)          ──►  file + a bumped key
//!                                           reads on the key and on its own tick,
//!                                           draws the ones that have not started yet
//! ```
//!
//! # The staleness, stated
//!
//! The file is only as fresh as the last time the calendar ran. That is why the window is a week
//! rather than a day: a phone whose owner has not opened the calendar since Monday still has the
//! right thing on the home screen on Thursday. And the reader skips entries whose start has passed,
//! so a file left over from last month shows **less** — never something wrong. The same trade the
//! reminder queue already makes, and the same fix: open the calendar.

use alloc::string::String;
use alloc::vec::Vec;

use crate::error::Result;
use crate::fs::{self, Fs, Utf16Path};

/// The P&S category — the launcher's, shared with [`crate::intent`] and [`crate::agenda`].
pub const CATEGORY: u32 = crate::intent::CATEGORY;

/// The key bumped to say "the day changed". 103 is the reminder queue's; this is the next.
pub const DAILY_KEY: u32 = 104;

/// Where the digest lives — beside the reminder queue, under `C:\Data`, which needs no capability.
pub const DAILY_FILE: &str = "C:\\Data\\launcher\\daily.dat";

/// The directory holding it. The trailing separator is load-bearing — see [`crate::agenda`].
const DAILY_DIR: &str = "C:\\Data\\launcher\\";

/// File magic, distinct from the queue's `CALA` so neither can be read as the other.
pub const MAGIC: [u8; 4] = *b"CALD";

/// Format version. A reader that does not know a version answers empty rather than guessing.
pub const VERSION: u16 = 1;

/// How many occurrences the digest carries. A home screen shows three; the rest are here so that
/// scrolling, or a day that is already over, still has something behind it.
pub const MAX_ENTRIES: usize = 32;

/// How far ahead the writer should look. Not enforced here — the file is whatever was written — but
/// it is the number both sides are built around, so it belongs in the contract rather than in one of
/// them.
pub const WINDOW_DAYS: i64 = 7;

/// The longest title, in bytes. It is read at a glance in half of a 320-pixel row.
pub const MAX_TITLE: usize = 64;

/// `start`, `end`, flags, and the title's length.
const ENTRY_HEADER: usize = 8 + 8 + 1 + 1;

/// Magic, version, count.
const FILE_HEADER: usize = 4 + 2 + 2;

/// Set when the occurrence covers whole days and has no meaningful clock time.
const FLAG_ALL_DAY: u8 = 1 << 0;

/// One occurrence, denormalised: when it runs and what it is called.
///
/// No id, no calendar, no colour. Those are the calendar's business, and a reader that had them
/// would grow features that depend on the *writer's* model — which is how a cache becomes a second
/// copy of a database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// When it starts, UTC seconds.
    pub start: i64,
    /// When it ends, UTC seconds. Equal to `start` for something with no duration.
    pub end: i64,
    /// Whole-day: show no clock time for it.
    pub all_day: bool,
    /// What it is called.
    pub title: String,
}

impl Entry {
    pub fn new(start: i64, end: i64, all_day: bool, title: impl Into<String>) -> Self {
        Entry { start, end, all_day, title: title.into() }
    }
}

/// Serialise a digest. Little-endian, entries past [`MAX_ENTRIES`] dropped, titles truncated on a
/// character boundary — so the file can never carry bytes that are not valid UTF-8.
pub fn encode(entries: &[Entry]) -> Vec<u8> {
    let kept = &entries[..entries.len().min(MAX_ENTRIES)];
    let mut out = Vec::with_capacity(FILE_HEADER + kept.len() * (ENTRY_HEADER + 24));
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(kept.len() as u16).to_le_bytes());
    for e in kept {
        let title = truncate_on_boundary(&e.title, MAX_TITLE);
        out.extend_from_slice(&e.start.to_le_bytes());
        out.extend_from_slice(&e.end.to_le_bytes());
        out.push(if e.all_day { FLAG_ALL_DAY } else { 0 });
        out.push(title.len() as u8);
        out.extend_from_slice(title.as_bytes());
    }
    out
}

/// Parse a digest, or `None` if this is not one. Every failure is `None`, for the reason
/// [`crate::agenda::decode`] gives: a home screen that refused to draw over a bad cache would be a
/// home screen one bad write could disable.
pub fn decode(bytes: &[u8]) -> Option<Vec<Entry>> {
    if bytes.len() < FILE_HEADER || bytes[..4] != MAGIC {
        return None;
    }
    if u16::from_le_bytes([bytes[4], bytes[5]]) != VERSION {
        return None;
    }
    let count = u16::from_le_bytes([bytes[6], bytes[7]]) as usize;
    if count > MAX_ENTRIES {
        return None;
    }
    let mut out = Vec::with_capacity(count);
    let mut at = FILE_HEADER;
    for _ in 0..count {
        if at + ENTRY_HEADER > bytes.len() {
            return None;
        }
        let start = i64::from_le_bytes(bytes[at..at + 8].try_into().ok()?);
        let end = i64::from_le_bytes(bytes[at + 8..at + 16].try_into().ok()?);
        let flags = bytes[at + 16];
        let len = bytes[at + 17] as usize;
        at += ENTRY_HEADER;
        if len > MAX_TITLE || at + len > bytes.len() {
            return None;
        }
        let title = core::str::from_utf8(&bytes[at..at + len]).ok()?;
        out.push(Entry {
            start,
            end,
            all_day: flags & FLAG_ALL_DAY != 0,
            title: String::from(title),
        });
        at += len;
    }
    Some(out)
}

/// Write the digest, without ringing the bell. Split from [`signal`] so the format stays testable
/// on a host, where P&S answers `NotReady`.
pub fn write<F: Fs>(fs: &mut F, entries: &[Entry]) -> Result<()> {
    let dir: Vec<u16> = DAILY_DIR.encode_utf16().collect();
    let _ = fs.mkdir(&dir);
    fs::write_atomic(fs, &Utf16Path::new(DAILY_FILE)?, &encode(entries))
}

/// Bump the counter that tells a reader the file changed. Called *after* the write, so nobody can
/// be woken to read a file that is half there.
pub fn signal() -> Result<()> {
    let _ = crate::prop::define_public(CATEGORY, DAILY_KEY);
    let now = crate::prop::get(CATEGORY, DAILY_KEY).unwrap_or(0);
    crate::prop::set(CATEGORY, DAILY_KEY, now.wrapping_add(1))
}

/// Write the digest and ring the bell. What a calendar calls after anything changed.
pub fn publish<F: Fs>(fs: &mut F, entries: &[Entry]) -> Result<()> {
    write(fs, entries)?;
    signal()
}

/// The digest as it stands. Empty when there is no file, or the file is not one of ours.
pub fn read<F: Fs>(fs: &mut F) -> Vec<Entry> {
    let Ok(path) = Utf16Path::new(DAILY_FILE) else {
        return Vec::new();
    };
    match fs::read(fs, &path) {
        Ok(Some(bytes)) => decode(&bytes).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// What is still ahead, soonest first, at most `limit` of them.
///
/// "Ahead" is by *end* rather than by start, so a meeting that began ten minutes ago is still on the
/// screen while it is happening — which is when a person is most likely to be looking for it. An
/// all-day entry counts as ahead until its day is over, for the same reason.
///
/// This is the whole of the reader's policy, in one pure function, because it is the part that will
/// look wrong on the phone before it looks wrong in a test.
pub fn upcoming<F: Fs>(fs: &mut F, now: i64, limit: usize) -> Vec<Entry> {
    let mut all: Vec<Entry> = read(fs).into_iter().filter(|e| e.end.max(e.start) > now).collect();
    all.sort_by_key(|e| (e.start, e.end));
    all.truncate(limit);
    all
}

/// Cut a string to at most `max` bytes without splitting a character. `&s[..max]` panics on a
/// boundary, and "reunião" is an ordinary title.
fn truncate_on_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::MemFs;

    fn entry(start: i64, title: &str) -> Entry {
        Entry::new(start, start + 3600, false, title)
    }

    #[test]
    fn a_digest_round_trips() {
        let entries = vec![
            entry(1_000, "Reunião de equipe"),
            Entry::new(86_400, 172_800, true, "Feriado"),
        ];
        let got = decode(&encode(&entries)).expect("our own bytes");
        assert_eq!(got, entries);
        assert!(got[1].all_day, "the flag survives");
    }

    /// The queue and the digest live in the same directory and must not be readable as each other.
    #[test]
    fn the_two_files_have_different_magic() {
        assert_ne!(MAGIC, crate::agenda::MAGIC);
        assert!(decode(&crate::agenda::encode(&[])).is_none(), "a queue is not a digest");
    }

    #[test]
    fn a_file_that_is_not_ours_is_no_entries_rather_than_an_error() {
        assert!(decode(b"").is_none());
        assert!(decode(b"CALDx").is_none(), "short header");
        let mut bytes = encode(&[entry(1, "x")]);
        bytes[4] = 9; // a version from the future
        assert!(decode(&bytes).is_none());
        let mut truncated = encode(&[entry(1, "hello")]);
        truncated.truncate(truncated.len() - 2);
        assert!(decode(&truncated).is_none(), "a title that runs off the end");
    }

    #[test]
    fn the_caps_hold() {
        let many: Vec<Entry> = (0..MAX_ENTRIES as i64 + 10).map(|i| entry(i, "e")).collect();
        assert_eq!(decode(&encode(&many)).unwrap().len(), MAX_ENTRIES);

        let long = "á".repeat(MAX_TITLE);
        let got = decode(&encode(&[entry(1, &long)])).unwrap();
        assert!(got[0].title.len() <= MAX_TITLE);
        assert!(long.starts_with(&got[0].title), "cut, not mangled");
    }

    #[test]
    fn what_is_ahead_includes_what_is_happening_now() {
        let mut fs = MemFs::new();
        let entries = vec![
            entry(1_000, "over"),          // ends at 4_600
            entry(5_000, "running"),       // 5_000..8_600, "now" is inside it
            entry(20_000, "later"),
            entry(10_000, "sooner"),
        ];
        write(&mut fs, &entries).unwrap();

        let got = upcoming(&mut fs, 6_000, 3);

        assert_eq!(
            got.iter().map(|e| e.title.as_str()).collect::<Vec<_>>(),
            ["running", "sooner", "later"],
            "sorted by start, and the one under way is still on screen"
        );
        assert_eq!(upcoming(&mut fs, 6_000, 1).len(), 1, "the limit holds");
        assert!(upcoming(&mut fs, 100_000, 3).is_empty(), "a stale file shows nothing");
    }

    #[test]
    fn no_file_is_no_entries() {
        let mut fs = MemFs::new();
        assert!(read(&mut fs).is_empty());
        assert!(upcoming(&mut fs, 0, 3).is_empty());
    }
}
