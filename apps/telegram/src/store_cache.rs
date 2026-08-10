//! The chat list and the tail of each conversation, kept on disk between launches.
//!
//! The reason is the same one behind [`symbian::cache`], only worse: there is no push in
//! this client, so every launch starts by asking `messages.getDialogs` over GPRS and the
//! screen is empty until the answer lands — tens of seconds of a chat application that shows
//! no chats. Opening a conversation repeats it for `messages.getHistory`. None of those bytes
//! were new; they were the same list, re-paid for.
//!
//! With this, a second launch draws the list it drew last time and the network becomes an
//! update rather than a prerequisite.
//!
//! # Everything lives in the data cage
//!
//! `C:\private\<UID3>\`, like the session and the media, because it is the one place an
//! unsigned application can write to with no capability. Files are flat: `chats.bin` for the
//! list, `c{peer_id}.bin` for one conversation's tail.
//!
//! # The format, and why it is hand-rolled
//!
//! Magic, version, then length-prefixed fields — the same shape as
//! [`crate::session_store`], for the same reason: there is no serde in this workspace, the
//! bytes have to stay readable across builds, and a format with a real parser is a parser
//! that can half-succeed on a truncated file.
//!
//! Decoding is **all-or-nothing**. Any inconsistency — short buffer, wrong magic, a length
//! prefix pointing past the end, a version this build does not know — returns `None` and the
//! whole file is treated as a miss. A half-loaded chat list is worse than no chat list: it
//! looks like data, and the user cannot tell which half is missing.
//!
//! # What is deliberately not stored
//!
//! Inline preview bytes. Those already have a home under the `p` prefix of
//! [`symbian::cache`], and duplicating a JPEG per message here would make the list file
//! the largest thing in the cage.
//!
//! Media is stored as its kind, its ids and its `file_reference` — enough to name the
//! attachment in a row and to attempt a download. A `file_reference` expires, but that is
//! true of a freshly parsed one too, and the refresh path (`TAG_REFRESH`) already exists to
//! renew it.

use alloc::string::String;
use alloc::vec::Vec;

use symbian::fs::{self, Fs, Utf16Path};

use crate::model::{Chat, Delivery, Media, Message, PeerRef, Store};

/// "tgC1" — a marker that says these bytes were meant for this reader, so a foreign file of
/// the right length is refused rather than decoded into nonsense.
const MAGIC: u32 = 0x7467_4331;

/// Bumped when a field is added or reordered. An unknown version is a miss, which costs one
/// network round trip and never a wrong screen — so there is no migration code here and does
/// not need to be.
const VERSION: u8 = 1;

/// The file the chat list goes in.
const LIST_NAME: &str = "chats.bin";

/// How many of a conversation's newest messages are written to disk.
///
/// Not the full [`crate::model::CHAT_WINDOW`]: what this has to do is fill the screen the
/// moment it opens, and twenty is a comfortable two screens of it. Everything above that is
/// one `getHistory` away and costs disk in every conversation the user ever opened.
pub const DISK_TAIL: usize = 20;

/// A ceiling on any length prefix, so a corrupt file cannot ask for a gigabyte allocation
/// before it fails. Larger than any name or message this client keeps.
const MAX_FIELD: usize = 8 * 1024;

/// And on the counts, for the same reason.
const MAX_ITEMS: usize = 4096;

// ---------------------------------------------------------------------------
// Primitives
// ---------------------------------------------------------------------------

fn put_u32(v: &mut Vec<u8>, n: u32) {
    v.extend_from_slice(&n.to_be_bytes());
}

fn put_i32(v: &mut Vec<u8>, n: i32) {
    v.extend_from_slice(&n.to_be_bytes());
}

fn put_i64(v: &mut Vec<u8>, n: i64) {
    v.extend_from_slice(&n.to_be_bytes());
}

fn put_bytes(v: &mut Vec<u8>, b: &[u8]) {
    put_u32(v, b.len() as u32);
    v.extend_from_slice(b);
}

fn put_str(v: &mut Vec<u8>, s: &str) {
    put_bytes(v, s.as_bytes());
}

/// A cursor that only ever moves forward and refuses to read past the end.
///
/// Every accessor returns `Option`, and one `None` anywhere aborts the whole decode — which
/// is the property that makes "all-or-nothing" true rather than aspirational.
struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, at: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        // Checked add: `at + n` on a length prefix read out of a corrupt file is exactly
        // where an overflow would wrap the bounds check into passing.
        let end = self.at.checked_add(n)?;
        let s = self.b.get(self.at..end)?;
        self.at = end;
        Some(s)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }

    fn i32(&mut self) -> Option<i32> {
        Some(i32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }

    fn i64(&mut self) -> Option<i64> {
        Some(i64::from_be_bytes(self.take(8)?.try_into().ok()?))
    }

    fn bool(&mut self) -> Option<bool> {
        Some(self.u8()? != 0)
    }

    fn bytes(&mut self) -> Option<Vec<u8>> {
        let n = self.u32()? as usize;
        if n > MAX_FIELD {
            return None;
        }
        Some(self.take(n)?.to_vec())
    }

    /// A length in a header, checked against [`MAX_ITEMS`] before anything is reserved.
    fn count(&mut self) -> Option<usize> {
        let n = self.u32()? as usize;
        if n > MAX_ITEMS {
            return None;
        }
        Some(n)
    }

    fn string(&mut self) -> Option<String> {
        // Invalid UTF-8 fails the whole decode rather than being replaced: it means the file
        // is not what it claims, and lossy conversion would put replacement characters in a
        // contact's name and call it a success.
        String::from_utf8(self.bytes()?).ok()
    }
}

// ---------------------------------------------------------------------------
// Enum tags
//
// Explicit numbers rather than `as u8` over the enum, so reordering a variant in `model.rs`
// or `tg_proto` cannot silently turn every stored photo into a voice note.
// ---------------------------------------------------------------------------

fn kind_tag(k: tg_proto::chats::Kind) -> u8 {
    match k {
        tg_proto::chats::Kind::User => 1,
        tg_proto::chats::Kind::Chat => 2,
        tg_proto::chats::Kind::Channel => 3,
    }
}

fn kind_of(tag: u8) -> Option<tg_proto::chats::Kind> {
    match tag {
        1 => Some(tg_proto::chats::Kind::User),
        2 => Some(tg_proto::chats::Kind::Chat),
        3 => Some(tg_proto::chats::Kind::Channel),
        _ => None,
    }
}

fn delivery_tag(d: Delivery) -> u8 {
    match d {
        Delivery::Pending => 1,
        Delivery::Sent => 2,
        Delivery::Read => 3,
        Delivery::Failed => 4,
    }
}

fn delivery_of(tag: u8) -> Option<Delivery> {
    match tag {
        // A message that was still Pending when the app closed never reached the server, and
        // nothing in this client retries. Restoring it as Pending would show a message
        // waiting on a send that will never happen; Failed is what actually became of it.
        1 => Some(Delivery::Failed),
        2 => Some(Delivery::Sent),
        3 => Some(Delivery::Read),
        4 => Some(Delivery::Failed),
        _ => None,
    }
}

fn put_peer(v: &mut Vec<u8>, p: Option<PeerRef>) {
    match p {
        Some(p) => {
            v.push(kind_tag(p.kind));
            put_i64(v, p.id);
            put_i64(v, p.access_hash);
        }
        // Zero means "no peer": the mock and the preview have none, and a chat without one
        // cannot be refreshed anyway.
        None => {
            v.push(0);
            put_i64(v, 0);
            put_i64(v, 0);
        }
    }
}

fn read_peer(r: &mut Reader<'_>) -> Option<Option<PeerRef>> {
    let tag = r.u8()?;
    let id = r.i64()?;
    let access_hash = r.i64()?;
    if tag == 0 {
        return Some(None);
    }
    Some(Some(PeerRef { kind: kind_of(tag)?, id, access_hash }))
}

// ---------------------------------------------------------------------------
// Media
// ---------------------------------------------------------------------------

fn put_media(v: &mut Vec<u8>, m: Option<&Media>) {
    let Some(m) = m else {
        v.push(0);
        return;
    };
    match m {
        Media::Photo { id, access_hash, file_reference, dc_id, thumb_size, size, .. } => {
            v.push(1);
            put_i64(v, *id);
            put_i64(v, *access_hash);
            put_bytes(v, file_reference);
            put_i32(v, *dc_id);
            put_str(v, thumb_size);
            put_i64(v, *size);
        }
        Media::Voice { id, access_hash, file_reference, dc_id, duration, waveform, size } => {
            v.push(2);
            put_i64(v, *id);
            put_i64(v, *access_hash);
            put_bytes(v, file_reference);
            put_i32(v, *dc_id);
            put_i32(v, *duration);
            put_bytes(v, waveform.as_deref().unwrap_or(&[]));
            put_i64(v, *size);
        }
        Media::Audio { id, access_hash, file_reference, dc_id, filename, duration, size } => {
            v.push(3);
            put_i64(v, *id);
            put_i64(v, *access_hash);
            put_bytes(v, file_reference);
            put_i32(v, *dc_id);
            put_str(v, filename);
            put_i32(v, *duration);
            put_i64(v, *size);
        }
        Media::File { id, access_hash, file_reference, dc_id, filename, size } => {
            v.push(4);
            put_i64(v, *id);
            put_i64(v, *access_hash);
            put_bytes(v, file_reference);
            put_i32(v, *dc_id);
            put_str(v, filename);
            put_i64(v, *size);
        }
        Media::Sticker { id, access_hash, file_reference, dc_id, alt, .. } => {
            v.push(5);
            put_i64(v, *id);
            put_i64(v, *access_hash);
            put_bytes(v, file_reference);
            put_i32(v, *dc_id);
            put_str(v, alt);
        }
        Media::Unknown => v.push(6),
    }
}

fn read_media(r: &mut Reader<'_>) -> Option<Option<Media>> {
    let tag = r.u8()?;
    if tag == 0 {
        return Some(None);
    }
    if tag == 6 {
        return Some(Some(Media::Unknown));
    }
    let id = r.i64()?;
    let access_hash = r.i64()?;
    let file_reference = r.bytes()?;
    let dc_id = r.i32()?;
    let m = match tag {
        1 => Media::Photo {
            id,
            access_hash,
            file_reference,
            dc_id,
            thumb_size: r.string()?,
            size: r.i64()?,
            // Restored empty; the spilled copy comes back through
            // `model::window_previews` when the message is near the selection.
            preview: None,
        },
        2 => {
            let duration = r.i32()?;
            let waveform = r.bytes()?;
            Media::Voice {
                id,
                access_hash,
                file_reference,
                dc_id,
                duration,
                waveform: if waveform.is_empty() { None } else { Some(waveform) },
                size: r.i64()?,
            }
        }
        3 => Media::Audio {
            id,
            access_hash,
            file_reference,
            dc_id,
            filename: r.string()?,
            duration: r.i32()?,
            size: r.i64()?,
        },
        4 => Media::File {
            id,
            access_hash,
            file_reference,
            dc_id,
            filename: r.string()?,
            size: r.i64()?,
        },
        5 => Media::Sticker {
            id,
            access_hash,
            file_reference,
            dc_id,
            alt: r.string()?,
            preview: None,
        },
        _ => return None,
    };
    Some(Some(m))
}

fn put_message(v: &mut Vec<u8>, m: &Message) {
    put_i32(v, m.id);
    put_str(v, &m.text);
    v.push(m.outgoing as u8);
    put_str(v, &m.time);
    v.push(delivery_tag(m.state));
    put_media(v, m.media.as_ref());
}

fn read_message(r: &mut Reader<'_>) -> Option<Message> {
    Some(Message {
        id: r.i32()?,
        text: r.string()?,
        outgoing: r.bool()?,
        time: r.string()?,
        state: delivery_of(r.u8()?)?,
        media: read_media(r)?,
    })
}

// ---------------------------------------------------------------------------
// The chat list
// ---------------------------------------------------------------------------

/// Encode the list: one row per chat, plus the single message each row previews.
pub fn encode_list(store: &Store) -> Vec<u8> {
    let mut v = Vec::new();
    put_u32(&mut v, MAGIC);
    v.push(VERSION);
    let n = store.chats.len().min(MAX_ITEMS);
    put_u32(&mut v, n as u32);
    for c in store.chats.iter().take(n) {
        put_peer(&mut v, c.peer);
        put_str(&mut v, &c.name);
        put_str(&mut v, &c.time);
        put_u32(&mut v, c.unread);
        v.push(c.last_outgoing as u8);
        put_i32(&mut v, c.oldest);
        // Only the previewed message: the rest of a conversation lives in its own file, and
        // writing it twice would make this file grow with every chat ever opened.
        match c.messages.last() {
            Some(m) => {
                v.push(1);
                put_message(&mut v, m);
            }
            None => v.push(0),
        }
    }
    v
}

/// Decode it, or `None` if anything at all is off.
pub fn decode_list(bytes: &[u8]) -> Option<Vec<Chat>> {
    let mut r = Reader::new(bytes);
    if r.u32()? != MAGIC || r.u8()? != VERSION {
        return None;
    }
    let n = r.count()?;
    let mut chats = Vec::with_capacity(n.min(crate::model::DIALOG_LIMIT));
    for _ in 0..n {
        let peer = read_peer(&mut r)?;
        let name = r.string()?;
        let time = r.string()?;
        let unread = r.u32()?;
        let last_outgoing = r.bool()?;
        let oldest = r.i32()?;
        let messages = match r.u8()? {
            0 => Vec::new(),
            1 => alloc::vec![read_message(&mut r)?],
            _ => return None,
        };
        chats.push(Chat {
            peer,
            oldest,
            // Never restored as complete or windowed: what is on disk is a preview row, not
            // a conversation, and marking it finished would stop the first scroll from ever
            // asking for the history it is missing.
            complete: false,
            windowed: false,
            loading: false,
            name,
            time,
            unread,
            last_outgoing,
            messages,
        });
    }
    Some(chats)
}

fn list_path<F: Fs>(fs: &mut F) -> Option<Utf16Path> {
    let dir = fs::private_path(fs).ok()?;
    Utf16Path::join(dir.as_units(), LIST_NAME).ok()
}

/// The chat list from the last launch, or `None`.
pub fn load_list<F: Fs>(fs: &mut F) -> Option<Vec<Chat>> {
    let p = list_path(fs)?;
    decode_list(&fs::read(fs, &p).ok()??)
}

/// Write the chat list. Failure is ignored by design: the list is already on screen, and the
/// only consequence is that the next launch is as slow as today's.
pub fn save_list<F: Fs>(fs: &mut F, store: &Store) {
    if let Some(p) = list_path(fs) {
        let _ = fs::write_atomic(fs, &p, &encode_list(store));
    }
}

// ---------------------------------------------------------------------------
// One conversation's tail
// ---------------------------------------------------------------------------

/// Encode the newest [`DISK_TAIL`] messages of a chat.
pub fn encode_tail(chat: &Chat) -> Vec<u8> {
    let start = chat.messages.len().saturating_sub(DISK_TAIL);
    let tail = &chat.messages[start..];
    let mut v = Vec::new();
    put_u32(&mut v, MAGIC);
    v.push(VERSION);
    put_i32(&mut v, chat.oldest);
    put_u32(&mut v, tail.len() as u32);
    for m in tail {
        put_message(&mut v, m);
    }
    v
}

/// Decode it into the messages and the pagination cursor that belongs with them.
pub fn decode_tail(bytes: &[u8]) -> Option<(Vec<Message>, i32)> {
    let mut r = Reader::new(bytes);
    if r.u32()? != MAGIC || r.u8()? != VERSION {
        return None;
    }
    let oldest = r.i32()?;
    let n = r.count()?;
    let mut out = Vec::with_capacity(n.min(DISK_TAIL));
    for _ in 0..n {
        out.push(read_message(&mut r)?);
    }
    Some((out, oldest))
}

fn tail_path<F: Fs>(fs: &mut F, peer: PeerRef) -> Option<Utf16Path> {
    let dir = fs::private_path(fs).ok()?;
    // Unsigned hex for the same reason as the media cache: a negative id would put a '-' at
    // the start of a filename. The kind is in the name too, because a user and a chat can
    // share an id and they are different conversations.
    let name = alloc::format!("c{}{:016x}.bin", kind_tag(peer.kind), peer.id as u64);
    Utf16Path::join(dir.as_units(), &name).ok()
}

/// Restore a conversation's tail into `chat`, returning whether anything was.
///
/// Refuses to overwrite messages already in memory: what is on disk is by definition older
/// than a live `getHistory` reply, and putting it on top of one would replace fresh
/// delivery states with stale ones.
pub fn load_tail<F: Fs>(fs: &mut F, chat: &mut Chat) -> bool {
    if chat.messages.len() > 1 {
        return false;
    }
    let Some(peer) = chat.peer else { return false };
    let Some(p) = tail_path(fs, peer) else { return false };
    let Some(Some(bytes)) = fs::read(fs, &p).ok() else { return false };
    let Some((messages, oldest)) = decode_tail(&bytes) else { return false };
    if messages.is_empty() {
        return false;
    }
    chat.oldest = oldest;
    chat.messages = messages;
    true
}

/// Write a conversation's tail. Failure ignored, as with the list.
pub fn save_tail<F: Fs>(fs: &mut F, chat: &Chat) {
    let Some(peer) = chat.peer else { return };
    if chat.messages.is_empty() {
        return;
    }
    if let Some(p) = tail_path(fs, peer) {
        let _ = fs::write_atomic(fs, &p, &encode_tail(chat));
    }
}

/// Whether a name in the private directory is one of ours to delete.
///
/// Deliberately narrow. The directory also holds `session.bin` and `iap.bin`, which belong
/// to other code, and a wipe that took everything would delete whatever is added there next
/// without anyone noticing until it was needed.
fn is_cache_name(name: &str) -> bool {
    // `.tmp` as well as `.bin`: `symbian::fs::write_atomic` writes beside the target and
    // renames, so a write interrupted by the battery leaves one behind — with the same
    // message content in it as the file it was going to become.
    let name = name.strip_suffix(".tmp").unwrap_or(name);
    if name == LIST_NAME {
        return true;
    }
    // `c<kind><16 hex>.bin` for a conversation, `p<16 hex>.bin` for a spilled preview, and
    // `m<16 hex>.bin` for a downloaded file — all three are this account's content.
    let ours = name.starts_with('c') || name.starts_with('p') || name.starts_with('m');
    ours && name.ends_with(".bin") && name.len() >= 18
}

/// Forget the list, every stored conversation, and the media that went with them.
///
/// Called when a session ends in a way that means the account is gone — logged out,
/// revoked, unregistered. Without it the next account to log in on this handset opens
/// showing the previous one's contacts and messages, which is not a stale cache: it is one
/// person's conversations shown to another.
///
/// Driven by listing the directory rather than by walking the chat list, because a
/// conversation whose chat has since dropped off the list still has a file, and because the
/// list itself may never have loaded.
pub fn clear<F: Fs>(fs: &mut F) {
    let Ok(dir) = fs::private_path(fs) else { return };
    let mut buf = [0u16; 1024];
    // Repeated passes: one listing holds a bounded number of names and each pass deletes
    // what it saw, so a directory larger than the buffer drains over several rounds. The
    // bound on rounds is there so a delete that always fails cannot spin forever.
    for _ in 0..64 {
        let Ok(n) = fs.list_dir(dir.as_units(), &mut buf) else { return };
        if n == 0 {
            return;
        }
        let mut removed = 0;
        for units in buf.split(|&u| u == 0).take(n) {
            // Ours are ASCII by construction; anything else is not, and is left alone.
            let mut name = String::new();
            if units.iter().any(|&u| u == 0 || u > 0x7F) {
                continue;
            }
            for u in units {
                name.push(*u as u8 as char);
            }
            if !is_cache_name(&name) {
                continue;
            }
            if let Ok(p) = Utf16Path::join(dir.as_units(), &name) {
                if fs.delete(p.as_units()).is_ok() {
                    removed += 1;
                }
            }
        }
        if removed == 0 {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use symbian::fs::MemFs;

    fn peer(id: i64) -> PeerRef {
        PeerRef { kind: tg_proto::chats::Kind::User, id, access_hash: 42 }
    }

    fn msg(id: i32, text: &str, media: Option<Media>) -> Message {
        Message {
            id,
            text: text.to_string(),
            outgoing: false,
            time: "10:00".to_string(),
            state: Delivery::Read,
            media,
        }
    }

    fn chat(id: i64, name: &str, n: usize) -> Chat {
        Chat {
            peer: Some(peer(id)),
            oldest: 1,
            name: name.to_string(),
            time: "10:00".to_string(),
            unread: 3,
            last_outgoing: true,
            messages: (1..=n as i32).map(|i| msg(i, "oi", None)).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_chat_list_survives_a_round_trip() {
        let store = Store {
            chats: alloc::vec![chat(1, "Ana Paula", 3), chat(-2, "Grupo", 1)],
            ..Default::default()
        };
        let back = decode_list(&encode_list(&store)).expect("decodes");
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].name, "Ana Paula");
        assert_eq!(back[0].peer, Some(peer(1)));
        assert_eq!(back[0].unread, 3);
        assert!(back[0].last_outgoing);
        assert_eq!(back[0].messages.len(), 1, "the list stores only the previewed message");
        assert_eq!(back[0].messages[0].id, 3, "and it is the newest one");
        assert_eq!(back[1].peer, Some(peer(-2)));
    }

    #[test]
    fn a_restored_chat_is_never_marked_complete() {
        // Otherwise the first scroll up in a restored conversation would decide there is
        // nothing above the single preview message and never fetch the history.
        let store = Store { chats: alloc::vec![chat(1, "Ana", 3)], ..Default::default() };
        let back = decode_list(&encode_list(&store)).unwrap();
        assert!(!back[0].complete);
        assert!(!back[0].windowed);
        assert!(!back[0].loading);
    }

    #[test]
    fn a_name_with_accents_and_emoji_comes_back_unchanged() {
        let store = Store { chats: alloc::vec![chat(1, "Márcia 👍 Ção", 1)], ..Default::default() };
        let back = decode_list(&encode_list(&store)).unwrap();
        assert_eq!(back[0].name, "Márcia 👍 Ção");
    }

    #[test]
    fn every_media_arm_round_trips_without_its_preview_bytes() {
        let media = alloc::vec![
            Media::Photo {
                id: 7,
                access_hash: 8,
                file_reference: alloc::vec![1, 2, 3],
                dc_id: 4,
                thumb_size: "m".to_string(),
                size: 900,
                preview: Some(alloc::vec![0xFF, 0xD8, 0xAA]),
            },
            Media::Voice {
                id: 9,
                access_hash: 10,
                file_reference: Vec::new(),
                dc_id: 2,
                duration: 7,
                waveform: Some(alloc::vec![5, 6]),
                size: 100,
            },
            Media::Audio {
                id: 11,
                access_hash: 12,
                file_reference: alloc::vec![9],
                dc_id: 1,
                filename: "song.mp3".to_string(),
                duration: 200,
                size: 3000,
            },
            Media::File {
                id: 13,
                access_hash: 14,
                file_reference: Vec::new(),
                dc_id: 5,
                filename: "notas.txt".to_string(),
                size: 12,
            },
            Media::Sticker {
                id: 15,
                access_hash: 16,
                file_reference: Vec::new(),
                dc_id: 2,
                alt: "😀".to_string(),
                preview: Some(alloc::vec![0xFF, 0xD8]),
            },
            Media::Unknown,
        ];
        let mut c = chat(1, "x", 0);
        for (i, m) in media.iter().enumerate() {
            c.messages.push(msg(i as i32 + 1, "", Some(m.clone())));
        }
        let (back, _) = decode_tail(&encode_tail(&c)).expect("decodes");
        assert_eq!(back.len(), media.len());
        for (got, want) in back.iter().zip(media.iter()) {
            let got = got.media.as_ref().unwrap();
            assert_eq!(got.file_id(), want.file_id());
            assert_eq!(got.dc_id(), want.dc_id());
            assert_eq!(got.kind(), want.kind(), "the arm itself, not just its numbers");
            // Preview bytes are the media cache's job; storing them here would put a JPEG
            // per message in this file.
            assert!(got.preview().is_none());
        }
        assert_eq!(
            back[2].media.as_ref().map(|m| m.file_id()),
            Some(11),
            "and they did not shuffle"
        );
    }

    #[test]
    fn only_the_newest_messages_reach_the_disk() {
        let c = chat(1, "x", DISK_TAIL + 30);
        let (back, _) = decode_tail(&encode_tail(&c)).unwrap();
        assert_eq!(back.len(), DISK_TAIL);
        assert_eq!(back[0].id, 31, "the tail, not the head");
        assert_eq!(back[DISK_TAIL - 1].id, (DISK_TAIL + 30) as i32);
    }

    #[test]
    fn a_pending_message_comes_back_as_failed() {
        // Nothing retries a send across launches, so restoring it as Pending would leave a
        // message waiting forever on something that is not going to happen.
        let mut c = chat(1, "x", 0);
        c.messages.push(Message { state: Delivery::Pending, ..msg(1, "oi", None) });
        let (back, _) = decode_tail(&encode_tail(&c)).unwrap();
        assert_eq!(back[0].state, Delivery::Failed);
    }

    #[test]
    fn a_truncated_file_is_a_miss_rather_than_half_a_chat_list() {
        let store = Store { chats: alloc::vec![chat(1, "Ana", 2), chat(2, "Bia", 2)], ..Default::default() };
        let full = encode_list(&store);
        for cut in 1..full.len() {
            assert!(
                decode_list(&full[..cut]).is_none(),
                "a file cut at {cut} must not decode into a partial list"
            );
        }
    }

    #[test]
    fn a_foreign_or_future_file_is_refused() {
        let store = Store { chats: alloc::vec![chat(1, "Ana", 1)], ..Default::default() };
        let mut bytes = encode_list(&store);
        assert!(decode_list(&bytes).is_some());

        let mut wrong_magic = bytes.clone();
        wrong_magic[0] ^= 0xFF;
        assert!(decode_list(&wrong_magic).is_none());

        bytes[4] = VERSION + 1;
        assert!(decode_list(&bytes).is_none(), "a version this build does not know is a miss");

        assert!(decode_list(&[]).is_none());
        assert!(decode_list(&[0, 0, 0]).is_none());
    }

    #[test]
    fn a_lying_length_prefix_cannot_read_past_the_end() {
        // The failure this guards is not a wrong answer, it is a panic on a slice index —
        // which on the device is the whole application closing.
        let store = Store { chats: alloc::vec![chat(1, "Ana", 1)], ..Default::default() };
        let mut bytes = encode_list(&store);
        // The name's length prefix sits after magic(4) + version(1) + count(4) + peer(17).
        let at = 4 + 1 + 4 + 17;
        bytes[at..at + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(decode_list(&bytes).is_none());

        let mut huge_count = encode_list(&store);
        huge_count[5..9].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(decode_list(&huge_count).is_none(), "and a count cannot reserve a gigabyte");
    }

    #[test]
    fn invalid_utf8_in_a_name_fails_the_whole_decode() {
        let store = Store { chats: alloc::vec![chat(1, "Ana", 1)], ..Default::default() };
        let mut bytes = encode_list(&store);
        let at = 4 + 1 + 4 + 17 + 4; // first byte of the name
        bytes[at] = 0xFF;
        assert!(decode_list(&bytes).is_none());
    }

    #[test]
    fn a_saved_list_is_the_one_that_loads() {
        let mut fs = MemFs::new();
        assert!(load_list(&mut fs).is_none(), "nothing cached yet");
        let store = Store { chats: alloc::vec![chat(1, "Ana", 2)], ..Default::default() };
        save_list(&mut fs, &store);
        let back = load_list(&mut fs).expect("loads");
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].name, "Ana");
    }

    #[test]
    fn a_saved_tail_refills_an_empty_conversation() {
        let mut fs = MemFs::new();
        let c = chat(1, "Ana", 5);
        save_tail(&mut fs, &c);

        let mut restored = Chat { peer: Some(peer(1)), ..Default::default() };
        assert!(load_tail(&mut fs, &mut restored));
        assert_eq!(restored.messages.len(), 5);
        assert_eq!(restored.oldest, 1);
    }

    #[test]
    fn a_tail_never_overwrites_messages_already_in_memory() {
        // Disk is by definition older than a live reply. Restoring over one would put stale
        // delivery ticks back on messages the server has since acknowledged.
        let mut fs = MemFs::new();
        save_tail(&mut fs, &chat(1, "Ana", 5));

        let mut live = chat(1, "Ana", 3);
        live.messages[0].text = "fresco".to_string();
        assert!(!load_tail(&mut fs, &mut live));
        assert_eq!(live.messages.len(), 3);
        assert_eq!(live.messages[0].text, "fresco");
    }

    #[test]
    fn a_user_and_a_chat_sharing_an_id_do_not_share_a_file() {
        let mut fs = MemFs::new();
        let mut as_user = chat(1234, "pessoa", 2);
        let mut as_chat = chat(1234, "grupo", 4);
        as_chat.peer = Some(PeerRef { kind: tg_proto::chats::Kind::Chat, id: 1234, access_hash: 0 });
        save_tail(&mut fs, &as_user);
        save_tail(&mut fs, &as_chat);

        as_user.messages.clear();
        as_chat.messages.clear();
        assert!(load_tail(&mut fs, &mut as_user));
        assert!(load_tail(&mut fs, &mut as_chat));
        assert_eq!(as_user.messages.len(), 2);
        assert_eq!(as_chat.messages.len(), 4);
    }

    #[test]
    fn logging_out_leaves_nothing_of_the_previous_account() {
        let mut fs = MemFs::new();
        let chats = alloc::vec![chat(1, "Ana", 2), chat(2, "Bia", 2)];
        let store = Store { chats: chats.clone(), ..Default::default() };
        save_list(&mut fs, &store);
        for c in &chats {
            save_tail(&mut fs, c);
        }
        assert!(load_list(&mut fs).is_some());

        clear(&mut fs);
        assert!(load_list(&mut fs).is_none());
        for c in &chats {
            let mut empty = Chat { peer: c.peer, ..Default::default() };
            assert!(!load_tail(&mut fs, &mut empty), "no conversation left behind");
        }
    }

    #[test]
    fn a_wipe_leaves_the_files_that_belong_to_other_code() {
        // `session.bin` is cleared by `session_store` on its own terms, and `iap.bin` is the
        // access point, which is the handset's setting rather than the account's. A wipe
        // that took the directory would take both.
        let mut fs = MemFs::new();
        let chats = alloc::vec![chat(1, "Ana", 2)];
        save_list(&mut fs, &Store { chats: chats.clone(), ..Default::default() });
        save_tail(&mut fs, &chats[0]);
        let dir = fs::private_path(&mut fs).unwrap();
        for other in ["session.bin", "iap.bin"] {
            let p = Utf16Path::join(dir.as_units(), other).unwrap();
            fs::write_atomic(&mut fs, &p, &[1, 2, 3]).unwrap();
        }

        clear(&mut fs);

        for other in ["session.bin", "iap.bin"] {
            let p = Utf16Path::join(dir.as_units(), other).unwrap();
            assert!(
                matches!(fs::read(&mut fs, &p), Ok(Some(_))),
                "{other} is not this module's to delete"
            );
        }
        assert!(load_list(&mut fs).is_none(), "and ours did go");
    }

    #[test]
    fn a_wipe_also_takes_a_temporary_left_by_an_interrupted_write() {
        // `write_atomic` writes `<name>.tmp` and renames. A battery pull between the two
        // leaves the temporary, holding the same messages as the file it was becoming.
        let mut fs = MemFs::new();
        let dir = fs::private_path(&mut fs).unwrap();
        let leftover = Utf16Path::join(dir.as_units(), "chats.bin.tmp").unwrap();
        fs::write_atomic(&mut fs, &leftover, &[1, 2, 3]).unwrap();

        clear(&mut fs);
        assert!(matches!(fs::read(&mut fs, &leftover), Ok(None)));
    }
}
