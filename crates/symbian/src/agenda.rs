//! The queue of reminders waiting to fire, shared between an application and the daemon that
//! delivers them.
//!
//! # Why the contract is here and not in either party
//!
//! Exactly the argument [`crate::intent`] makes. The calendar writes the queue and `notifd`
//! delivers from it; they live in different repositories and neither depends on the other, so the
//! only place they can agree on a path, a key and a byte layout is the SDK they both already have.
//! Written twice, they would drift the first time one side changed a field — and the symptom would
//! be a reminder that never arrives, which looks exactly like a reminder nobody set.
//!
//! # Why a file and not the calendar's own database
//!
//! The obvious design is for the daemon to open the calendar's SQLite and ask it what is due. It
//! would couple a binary that has to come up at boot to a schema that will change: a migration in
//! the application would break the daemon, and the daemon is the half the user cannot see failing.
//!
//! It would also put `sqldb` in the daemon's image, which is the opposite of why `notifd` exists —
//! it is a separate binary precisely so that one risky import costs the notification count and not
//! the home screen.
//!
//! ```text
//!   calendar                                    notifd (running since boot)
//!     derive the next N reminders
//!     write AGENDA_FILE            ──►
//!     prop::set(CATEGORY, KEY, n+1)──►           wakes, and caps its next sleep at next_due()
//!                                                take_due(now) → fires each as a notice,
//!                                                and rewrites the file without them
//! ```
//!
//! # The queue is a cache; the database is the truth
//!
//! This file holds only the **next few** reminders, denormalised — an instant and a line of text.
//! It is derived, and the calendar rewrites it whenever anything with a reminder changes.
//!
//! Two consequences worth knowing. Reminders keep arriving for weeks with the application never
//! opened, because the queue was filled ahead. And if the two ever disagree, the file is the one
//! that is wrong; the fix is to open the calendar, not to repair the file.
//!
//! # Why the daemon rewrites rather than deletes
//!
//! [`crate::intent::take_notice`] consumes its whole file, because a request is one event. A
//! reminder that has fired is one entry out of many, and the rest must survive — so `take_due`
//! writes the remainder back. An entry that fired and stayed would fire again on every poll, which
//! on a five-second cadence is a phone that will not stop buzzing.

use alloc::string::String;
use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::fs::{self, Fs, Utf16Path};

/// The P&S category — the launcher's, the same one [`crate::intent`] uses.
///
/// One category for everything on this channel, because `notifd` already subscribes to it and a
/// second would be a second subscription for no gain. Its policies are open in both directions
/// (see `shim_prop_define_public`), which is what lets an application in another SID write here.
pub const CATEGORY: u32 = crate::intent::CATEGORY;

/// The key bumped to say "the queue changed".
///
/// Keys 100–102 are the launcher's activity signal, the open-URL request and the notice lane. This
/// is the next one. The *value* carries nothing but change, as with the others.
pub const AGENDA_KEY: u32 = 103;

/// Where the queue lives.
///
/// Beside the other cross-process files, under `C:\Data`, which needs no capability to write — an
/// application with nothing but `WriteUserData` has to be able to use this channel.
pub const AGENDA_FILE: &str = "C:\\Data\\launcher\\agenda.dat";

/// The directory holding it, which may not exist yet.
///
/// The trailing separator is load-bearing: `RFs::MkDirAll` drops the last path component when there
/// is none, which once left this exact directory missing and killed a whole pipeline silently.
const AGENDA_DIR: &str = "C:\\Data\\launcher\\";

/// File magic. Four bytes, so a file that is not this is rejected rather than parsed into nonsense.
pub const MAGIC: [u8; 4] = *b"CALA";

/// Format version. A reader that does not know a version answers empty rather than guessing.
pub const VERSION: u16 = 1;

/// How many reminders the queue carries.
///
/// Enough that a phone left alone for a fortnight still buzzes, and small enough that the whole
/// file is one page. A queue that held everything would be a second copy of the database.
pub const MAX_ENTRIES: usize = 32;

/// The longest reminder line, in bytes.
///
/// A sanity limit, not a protocol one — the text becomes the subject of an Inbox entry, which is
/// read at a glance in a list 320 pixels wide.
pub const MAX_TEXT: usize = 96;

/// The fixed part of an entry: `due`, `id`, and the text's length.
const ENTRY_HEADER: usize = 8 + 4 + 2;

/// The file header: magic, version, count.
const FILE_HEADER: usize = 4 + 2 + 2;

/// One waiting reminder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reminder {
    /// When to fire, UTC seconds.
    pub due: i64,
    /// The writer's own identifier for what this is about, echoed back untouched.
    ///
    /// Opaque here on purpose: the daemon has no business knowing whether it is an event id, a task
    /// id or a hash. It exists so the *writer* can tell its own entries apart in a log.
    pub id: u32,
    /// What the notice says.
    pub text: String,
}

impl Reminder {
    pub fn new(due: i64, id: u32, text: impl Into<String>) -> Self {
        Reminder { due, id, text: text.into() }
    }
}

/// Serialise a queue.
///
/// Little-endian, because the only two things that read this are an ARM handset and a host test on
/// a machine that is also little-endian — and a format that said "native" would be a format that
/// silently changed meaning the day somebody cross-compiled it somewhere else.
///
/// Entries past [`MAX_ENTRIES`] are dropped and text past [`MAX_TEXT`] is truncated **on a
/// character boundary**, so the file can never carry bytes that are not valid UTF-8.
pub fn encode(entries: &[Reminder]) -> Vec<u8> {
    let kept = &entries[..entries.len().min(MAX_ENTRIES)];
    let mut out = Vec::with_capacity(FILE_HEADER + kept.len() * (ENTRY_HEADER + 24));
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(kept.len() as u16).to_le_bytes());
    for e in kept {
        let text = truncate_on_boundary(&e.text, MAX_TEXT);
        out.extend_from_slice(&e.due.to_le_bytes());
        out.extend_from_slice(&e.id.to_le_bytes());
        out.extend_from_slice(&(text.len() as u16).to_le_bytes());
        out.extend_from_slice(text.as_bytes());
    }
    out
}

/// Parse a queue, or `None` if this is not one.
///
/// Every failure is `None` rather than an error, and every one of them is a file that should not
/// exist: wrong magic, a version from the future, a length that runs off the end. There is nothing
/// a caller could do with the distinction, and a daemon that refused to start over a corrupt cache
/// would be a daemon a single bad write could disable for good.
pub fn decode(bytes: &[u8]) -> Option<Vec<Reminder>> {
    if bytes.len() < FILE_HEADER || bytes[..4] != MAGIC {
        return None;
    }
    let version = u16::from_le_bytes([bytes[4], bytes[5]]);
    if version != VERSION {
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
        let due = i64::from_le_bytes(bytes[at..at + 8].try_into().ok()?);
        let id = u32::from_le_bytes(bytes[at + 8..at + 12].try_into().ok()?);
        let len = u16::from_le_bytes([bytes[at + 12], bytes[at + 13]]) as usize;
        at += ENTRY_HEADER;
        if len > MAX_TEXT || at + len > bytes.len() {
            return None;
        }
        // Lossless or nothing: a mangled line becomes the subject of a message the user reads, and
        // half a character is worse than no reminder.
        let text = core::str::from_utf8(&bytes[at..at + len]).ok()?;
        out.push(Reminder { due, id, text: String::from(text) });
        at += len;
    }
    Some(out)
}

/// Write the queue, without ringing the bell.
///
/// Split from [`signal`] for the reason [`crate::intent::write_request`] is: P&S needs a kernel and
/// answers `NotReady` off the device, so the half that is pure filesystem — the format, which is
/// the part most likely to be got wrong — stays testable on a host.
pub fn write<F: Fs>(fs: &mut F, entries: &[Reminder]) -> Result<()> {
    let dir: Vec<u16> = AGENDA_DIR.encode_utf16().collect();
    // Blind: an existing directory is success, and the only interesting failure is one the write
    // below reports anyway.
    let _ = fs.mkdir(&dir);
    fs::write_atomic(fs, &Utf16Path::new(AGENDA_FILE)?, &encode(entries))
}

/// Bump the counter that wakes the daemon.
///
/// Called *after* the queue is written. Ringing first lets the daemon run between the two calls and
/// read a file that is missing or half written — the same ordering rule the intent channel has.
pub fn signal() -> Result<()> {
    // Defined blind first, so whichever side runs first is the one that creates it. Idempotent:
    // `RProperty::Define` answering `KErrAlreadyExists` is success here.
    let _ = crate::prop::define_public(CATEGORY, AGENDA_KEY);
    let now = crate::prop::get(CATEGORY, AGENDA_KEY).unwrap_or(0);
    crate::prop::set(CATEGORY, AGENDA_KEY, now.wrapping_add(1))
}

/// Write the queue and ring the bell. What a calendar calls after anything changed.
pub fn publish<F: Fs>(fs: &mut F, entries: &[Reminder]) -> Result<()> {
    write(fs, entries)?;
    signal()
}

/// The queue as it stands. Empty when there is no file, or the file is not one of ours.
pub fn read<F: Fs>(fs: &mut F) -> Vec<Reminder> {
    let Ok(path) = Utf16Path::new(AGENDA_FILE) else {
        return Vec::new();
    };
    match fs::read(fs, &path) {
        Ok(Some(bytes)) => decode(&bytes).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// When the next reminder is due, or `None` if the queue is empty.
///
/// This is what a poller caps its sleep at. Without it a reminder waits for whatever the daemon's
/// backoff happens to be — five minutes at `notifd`'s idle ceiling, which for a meeting reminder is
/// the difference between useful and decorative.
pub fn next_due<F: Fs>(fs: &mut F) -> Option<i64> {
    read(fs).into_iter().map(|r| r.due).min()
}

/// Take everything due at or before `now`, leaving the rest.
///
/// Rewrites the file with the remainder before returning, so an entry cannot fire twice even if the
/// caller dies while delivering it. The trade is the other way round — an entry can be *lost* if
/// the process dies between the rewrite and the delivery — and that is the right way round: a
/// missed reminder is a disappointment, and a reminder that arrives every fifteen seconds until the
/// battery dies is a phone the user turns off.
///
/// The remainder is written even when nothing was due, which costs a write per poll and is
/// deliberately not optimised away here: the caller that cares checks `is_empty` first, and
/// [`take_due`] having one path is worth more than a branch.
pub fn take_due<F: Fs>(fs: &mut F, now: i64) -> Vec<Reminder> {
    let all = read(fs);
    if all.is_empty() {
        return Vec::new();
    }
    let (due, rest): (Vec<Reminder>, Vec<Reminder>) = all.into_iter().partition(|r| r.due <= now);
    if !due.is_empty() {
        let _ = write(fs, &rest);
    }
    due
}

/// Cut a string to at most `max` bytes without splitting a character.
///
/// `&s[..max]` panics on a boundary, and on this device a panic in the calendar's save path is a
/// dialog with a number in it. Portuguese makes this reachable rather than theoretical: "reunião"
/// has a two-byte character in it, and a limit landing between its halves is an ordinary title.
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

/// The error a caller gets for a queue it cannot even attempt to write.
///
/// Exposed so the calendar can tell "the disk is full" from "I built a bad list": the only
/// argument error this module produces is a path that will not fit, which is a build-time mistake.
pub fn path() -> Result<Utf16Path> {
    Utf16Path::new(AGENDA_FILE).map_err(|_| Error::Argument)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::MemFs;
    use alloc::vec;

    fn three() -> Vec<Reminder> {
        vec![
            Reminder::new(1_000, 1, "Dentista às 14:00"),
            Reminder::new(2_000, 2, "Reunião"),
            Reminder::new(3_000, 3, "Jantar"),
        ]
    }

    #[test]
    fn a_queue_survives_the_round_trip_exactly() {
        let entries = three();
        assert_eq!(decode(&encode(&entries)).unwrap(), entries);
    }

    #[test]
    fn an_empty_queue_is_a_legal_file_and_not_a_missing_one() {
        // The difference matters: the daemon rewrites the file with the remainder, and a queue that
        // emptied has to be distinguishable from a queue nobody ever wrote — otherwise the last
        // entry fires for ever, because deleting it looks like "no file yet".
        let bytes = encode(&[]);
        assert_eq!(decode(&bytes), Some(vec![]));
        assert!(bytes.len() >= FILE_HEADER);
    }

    #[test]
    fn a_file_that_is_not_ours_is_refused_rather_than_parsed() {
        assert_eq!(decode(b""), None);
        assert_eq!(decode(b"not a queue at all"), None);
        // Right magic, wrong version: a file from a build that knows more than this one.
        let mut future = encode(&three());
        future[4] = 99;
        assert_eq!(decode(&future), None);
    }

    #[test]
    fn a_truncated_file_answers_none_rather_than_half_a_queue() {
        // A write interrupted by a flat battery. Half a queue would fire half a reminder — the
        // entry whose text ran off the end would carry whatever followed it in the buffer.
        let full = encode(&three());
        for cut in [FILE_HEADER + 1, FILE_HEADER + ENTRY_HEADER, full.len() - 1] {
            assert_eq!(decode(&full[..cut]), None, "accepted a file cut at {cut}");
        }
    }

    #[test]
    fn a_count_larger_than_the_file_can_hold_is_refused() {
        // The field a corrupt file is most likely to lie about, and the one that would otherwise
        // drive a loop over memory that is not there.
        let mut bad = encode(&three());
        bad[6] = MAX_ENTRIES as u8;
        bad[7] = 0;
        assert_eq!(decode(&bad), None);
    }

    #[test]
    fn the_queue_is_capped_and_the_extra_entries_are_dropped_not_wrapped() {
        let many: Vec<Reminder> =
            (0..MAX_ENTRIES + 10).map(|i| Reminder::new(i as i64, i as u32, "x")).collect();
        let got = decode(&encode(&many)).unwrap();
        assert_eq!(got.len(), MAX_ENTRIES);
        // The *first* ones, which are the soonest when the caller sorted — dropping the front
        // would silently discard the reminder about to fire.
        assert_eq!(got[0].id, 0);
        assert_eq!(got[MAX_ENTRIES - 1].id, MAX_ENTRIES as u32 - 1);
    }

    #[test]
    fn a_long_title_is_cut_on_a_character_boundary() {
        // Portuguese makes this reachable rather than theoretical: a two-byte character straddling
        // the limit is an ordinary title, and `&s[..max]` on one is a panic.
        let long = "ç".repeat(MAX_TEXT); // two bytes each, so twice the limit
        let cut = decode(&encode(&[Reminder::new(0, 1, long)])).unwrap();
        assert!(cut[0].text.len() <= MAX_TEXT);
        assert!(cut[0].text.chars().all(|c| c == 'ç'), "a character was split in half");
    }

    #[test]
    fn writing_and_reading_through_a_filesystem_gives_the_queue_back() {
        let mut fs = MemFs::new();
        write(&mut fs, &three()).unwrap();
        assert_eq!(read(&mut fs), three());
    }

    #[test]
    fn reading_a_queue_that_was_never_written_is_empty_rather_than_an_error() {
        // The normal state of a phone with no calendar installed, and the daemon has to keep
        // running through it.
        let mut fs = MemFs::new();
        assert!(read(&mut fs).is_empty());
        assert_eq!(next_due(&mut fs), None);
        assert!(take_due(&mut fs, i64::MAX).is_empty());
    }

    #[test]
    fn only_what_is_due_is_taken_and_the_rest_stays() {
        let mut fs = MemFs::new();
        write(&mut fs, &three()).unwrap();
        let fired = take_due(&mut fs, 2_000);
        assert_eq!(fired.len(), 2);
        assert_eq!(fired[0].id, 1);
        // Due *at* now counts as due: a reminder set for 14:00 has to fire at 14:00.
        assert_eq!(fired[1].id, 2);
        let left = read(&mut fs);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, 3);
    }

    #[test]
    fn an_entry_that_fired_does_not_fire_again() {
        // The failure this design exists to prevent: on a five-second poll, an entry that stayed
        // would buzz twelve times a minute until the battery died.
        let mut fs = MemFs::new();
        write(&mut fs, &three()).unwrap();
        assert_eq!(take_due(&mut fs, 5_000).len(), 3);
        assert!(take_due(&mut fs, 5_000).is_empty());
        assert!(read(&mut fs).is_empty());
    }

    #[test]
    fn nothing_due_leaves_the_file_alone() {
        let mut fs = MemFs::new();
        write(&mut fs, &three()).unwrap();
        assert!(take_due(&mut fs, 999).is_empty());
        assert_eq!(read(&mut fs), three());
    }

    #[test]
    fn the_next_due_is_the_soonest_whatever_order_it_was_written_in() {
        // This is what a poller caps its sleep at; taking the first entry instead of the smallest
        // would let an unsorted queue sleep straight past a reminder.
        let mut fs = MemFs::new();
        write(
            &mut fs,
            &[
                Reminder::new(3_000, 3, "c"),
                Reminder::new(1_000, 1, "a"),
                Reminder::new(2_000, 2, "b"),
            ],
        )
        .unwrap();
        assert_eq!(next_due(&mut fs), Some(1_000));
    }

    #[test]
    fn a_reminder_in_the_past_is_due_now_rather_than_lost() {
        // The phone was off when it should have fired. Better late than never: the user still
        // wants to know they missed the dentist.
        let mut fs = MemFs::new();
        write(&mut fs, &[Reminder::new(-1, 7, "ontem")]).unwrap();
        assert_eq!(take_due(&mut fs, 0).len(), 1);
    }
}
