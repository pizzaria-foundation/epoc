//! The data the UI draws.
//!
//! Deliberately protocol-free. These are the shapes the screens need, not the
//! shapes MTProto happens to return — so when the real client lands it fills these
//! in from `messages.getDialogs` / `messages.getHistory` and the UI does not move.
//! `mock()` is the stand-in until then.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Clone, Debug)]
pub struct Message {
    /// The server's message id, for looking up a specific message.
    pub id: i32,
    pub text: String,
    /// True when we sent it.
    pub outgoing: bool,
    /// Pre-formatted `HH:MM`. Formatting needs the device clock and the user's
    /// locale, neither of which belongs in the drawing path.
    pub time: String,
    pub state: Delivery,
    /// An attached photo, document, or voice message. `None` for plain text.
    pub media: Option<Media>,
}

/// What kind of media a message carries, with enough to show a placeholder and to fetch
/// the real thing when the user asks for it.
///
/// Every arm carries `dc_id`, because the bytes do not necessarily live on the data centre
/// this session is connected to — asking the wrong one answers `FILE_MIGRATE_x`, and
/// without the number there is nothing to route by.
#[derive(Clone, Debug)]
pub enum Media {
    Photo {
        id: i64,
        access_hash: i64,
        file_reference: Vec<u8>,
        dc_id: i32,
        /// The `photoSize.type` to ask for, chosen once when the message was parsed.
        /// Empty when the photo listed no downloadable size, which makes it unfetchable.
        thumb_size: String,
        size: i64,
        /// A complete JPEG that arrived inside the message. Costs no request.
        preview: Option<Vec<u8>>,
    },
    Voice {
        id: i64,
        access_hash: i64,
        file_reference: Vec<u8>,
        dc_id: i32,
        duration: i32,
        /// The 5-bit-packed envelope, for drawing the bar rather than guessing at one.
        waveform: Option<Vec<u8>>,
        size: i64,
    },
    Audio {
        id: i64,
        access_hash: i64,
        file_reference: Vec<u8>,
        dc_id: i32,
        filename: String,
        duration: i32,
        size: i64,
    },
    File {
        id: i64,
        access_hash: i64,
        file_reference: Vec<u8>,
        dc_id: i32,
        filename: String,
        size: i64,
    },
    /// A sticker, which on this handset is a picture it cannot decode.
    ///
    /// The file is WebP, or gzipped Lottie, or VP9 — all of them younger than the phone.
    /// So a sticker is never downloaded; what is drawn is `alt`, the emoji it stands for,
    /// or an inline JPEG preview on the occasions the server provides one.
    Sticker {
        id: i64,
        access_hash: i64,
        file_reference: Vec<u8>,
        dc_id: i32,
        alt: String,
        preview: Option<Vec<u8>>,
    },
    Unknown,
}

/// Which of the [`Media`] arms, without the payload.
///
/// Carried alongside a download in flight, because the reply is only bytes and the bytes do
/// not say what they are. Its absence is why every download used to be handed to the image
/// decoder: a voice note's Ogg was written to a file named `_dl.jpg` and the failure
/// surfaced as "decode falhou", which named the wrong thing entirely.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MediaKind {
    Photo,
    Voice,
    Audio,
    File,
}

impl Media {
    /// What a download of this would be, or `None` when there is nothing to fetch.
    ///
    /// `None` for a sticker on purpose, not as an oversight: fetching one would spend the
    /// user's data on a WebP no codec here can open.
    pub fn kind(&self) -> Option<MediaKind> {
        match self {
            Media::Photo { thumb_size, .. } if thumb_size.is_empty() => None,
            Media::Photo { .. } => Some(MediaKind::Photo),
            Media::Voice { .. } => Some(MediaKind::Voice),
            Media::Audio { .. } => Some(MediaKind::Audio),
            Media::File { .. } => Some(MediaKind::File),
            Media::Sticker { .. } | Media::Unknown => None,
        }
    }

    /// Whether pressing Select on this should fetch anything.
    ///
    /// Media is lazy on purpose: nothing downloads until the user asks for it, because the
    /// link is GPRS and the person holding the phone pays for it by the kilobyte. A row
    /// shows a placeholder and stays that way until it is chosen.
    pub fn is_fetchable(&self) -> bool {
        self.kind().is_some()
    }

    /// Bytes already in hand that a codec would accept, if any.
    pub fn preview(&self) -> Option<&[u8]> {
        match self {
            Media::Photo { preview, .. } | Media::Sticker { preview, .. } => preview.as_deref(),
            _ => None,
        }
    }

    /// The photo or document id, which is what the cache is keyed by.
    ///
    /// Stable and unique within an account, and the bytes behind one never change — an edited
    /// photo gets a new id. So a cache entry cannot go stale, which is why there is nothing
    /// to validate and no expiry.
    pub fn file_id(&self) -> i64 {
        match self {
            Media::Photo { id, .. }
            | Media::Voice { id, .. }
            | Media::Audio { id, .. }
            | Media::File { id, .. }
            | Media::Sticker { id, .. } => *id,
            Media::Unknown => 0,
        }
    }

    /// The slot the inline preview lives in, for the arms that have one.
    ///
    /// Exists so the eviction pass can take the bytes out and put them back without a
    /// `match` over every arm at each call site.
    pub fn preview_slot(&mut self) -> Option<&mut Option<Vec<u8>>> {
        match self {
            Media::Photo { preview, .. } | Media::Sticker { preview, .. } => Some(preview),
            _ => None,
        }
    }

    /// Which data centre holds the file.
    pub fn dc_id(&self) -> i32 {
        match self {
            Media::Photo { dc_id, .. }
            | Media::Voice { dc_id, .. }
            | Media::Audio { dc_id, .. }
            | Media::File { dc_id, .. }
            | Media::Sticker { dc_id, .. } => *dc_id,
            Media::Unknown => 0,
        }
    }
}

/// A peer in the form the protocol needs, kept on the row that shows it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PeerRef {
    pub kind: tg_proto::chats::Kind,
    pub id: i64,
    pub access_hash: i64,
}

/// Delivery state, shown as ticks on outgoing messages.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Delivery {
    /// Queued locally, not yet acknowledged by the server.
    Pending,
    Sent,
    Read,
    /// Never reached the server.
    Failed,
}

impl Delivery {
    pub fn glyph(self) -> &'static str {
        match self {
            Delivery::Pending => "\u{00B7}",
            Delivery::Sent => "\u{2713}",
            Delivery::Read => "\u{2713}\u{2713}",
            Delivery::Failed => "!",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Chat {
    /// Who this is, in the form a request takes. `None` for the mock and the preview,
    /// which have no server behind them.
    ///
    /// Carried on the row rather than looked up by name, because a name is not an identity:
    /// two people can share one, and Telegram lets them.
    pub peer: Option<PeerRef>,
    /// The oldest message id held, for asking for the page before it. Zero when the chat
    /// has never been opened.
    pub oldest: i32,
    /// Whether every message this chat has is already here, so scrolling up stops asking.
    pub complete: bool,
    /// Whether the window is full and older messages are no longer being retained.
    ///
    /// Distinct from `complete`: the server has more, we are choosing not to hold it. Read
    /// alongside `complete` wherever scrolling up asks for a page, because with a
    /// drop-oldest window the answer to that request would be trimmed away on arrival and
    /// the next keypress would ask for it again — a request loop that costs GPRS and shows
    /// the user nothing.
    pub windowed: bool,
    /// Set while a page is in flight, so scrolling up does not ask twice for the same one.
    pub loading: bool,
    pub name: String,
    pub time: String,
    pub unread: u32,
    /// Whether the last message in the preview is ours.
    pub last_outgoing: bool,
    pub messages: Vec<Message>,
}

/// How many messages of one conversation are held in memory at a time.
///
/// The heap ceiling is 4 MB (`app.conf`) and a single message can carry a complete inline
/// JPEG in `Media::preview`, so an unbounded transcript is an allocator failure waiting for
/// a long enough conversation. A hundred is roughly five screens of scrollback at this
/// font size — more than anyone reads back through on a 320x240 display, and small enough
/// that the layout in `conv::Transcript` stays cheap to rebuild.
///
/// The policy is drop-oldest: a chat holds its hundred *newest* messages. See
/// [`trim_window`].
pub const CHAT_WINDOW: usize = 100;

/// How many chats the list holds. "Carregar mais" is otherwise unbounded, and each row
/// carries a name, a formatted time and its top message.
pub const DIALOG_LIMIT: usize = 200;

/// Cut a chat down to the newest [`CHAT_WINDOW`] messages. Returns how many were dropped.
///
/// Also moves `chat.oldest` up to the oldest message actually retained. Leaving it pointing
/// at a message no longer held would make the next `getHistory` ask for a page above
/// something that is not on screen, and the gap would never be visible or fillable.
pub fn trim_window(chat: &mut Chat) -> usize {
    let n = chat.messages.len().saturating_sub(CHAT_WINDOW);
    if n == 0 {
        return 0;
    }
    chat.messages.drain(..n);
    chat.windowed = true;
    if let Some(first) = chat.messages.first() {
        chat.oldest = first.id;
    }
    n
}

/// How many bubbles either side of the selected one keep their inline preview in memory.
///
/// The transcript shows perhaps four at a time, so twenty is far enough that scrolling with
/// the D-pad never waits on the disk, and near enough that a hundred-message conversation
/// full of photos holds a handful of JPEGs rather than a hundred.
pub const PREVIEW_BAND: usize = 20;

/// Free the inline previews outside the band around `selected`, spilling them to disk
/// first, and bring back the ones inside it. Returns how many were freed.
///
/// The spill is not an optimisation, it is what makes the eviction safe: an inline preview
/// arrives inside the `getHistory` reply and there is no request that fetches one on its
/// own, so bytes dropped without being written are gone until the whole conversation is
/// re-fetched. A preview whose write fails is therefore kept in memory.
pub fn window_previews<F: symbian::fs::Fs>(chat: &mut Chat, selected: usize, fs: &mut F) -> usize {
    let lo = selected.saturating_sub(PREVIEW_BAND);
    let hi = selected.saturating_add(PREVIEW_BAND);
    let mut freed = 0;
    for (i, m) in chat.messages.iter_mut().enumerate() {
        let Some(media) = m.media.as_mut() else { continue };
        // Keyed by file id, so media without one has nowhere to be spilled to and stays.
        let id = media.file_id();
        if id == 0 {
            continue;
        }
        let Some(slot) = media.preview_slot() else { continue };
        if i >= lo && i <= hi {
            if slot.is_none() {
                *slot = symbian::cache::get_preview(fs, id);
            }
        } else if let Some(bytes) = slot.as_deref() {
            if symbian::cache::put_preview(fs, id, bytes) {
                *slot = None;
                freed += 1;
            }
        }
    }
    freed
}

/// Append a message and hold the window. Returns how many fell off the front.
///
/// Callers that track a position by index — a selected bubble, a scroll offset — must
/// subtract the return value, or the screen silently shifts by one at the moment the
/// window fills.
pub fn push_message(chat: &mut Chat, m: Message) -> usize {
    chat.messages.push(m);
    trim_window(chat)
}

impl Chat {
    /// One or two initials for the avatar, taken from word starts so "Ana Paula"
    /// gives "AP" rather than "An".
    pub fn initials(&self) -> String {
        let mut out = String::new();
        for word in self.name.split_whitespace().take(2) {
            if let Some(c) = word.chars().next() {
                out.extend(c.to_uppercase());
            }
        }
        if out.is_empty() {
            out.push('?');
        }
        out
    }

    /// Stable per-contact avatar tint. FNV-1a over the name, so it survives
    /// restarts and reordering without storing anything.
    pub fn color_seed(&self) -> u32 {
        let mut h: u32 = 0x811C_9DC5;
        for b in self.name.as_bytes() {
            h ^= *b as u32;
            h = h.wrapping_mul(0x0100_0193);
        }
        h
    }

    /// The one line of the last message the chat list shows under the name.
    ///
    /// A media message usually has no text, so this used to return an empty string and the
    /// row looked broken — a name, a time, and nothing between them. Saying what the
    /// attachment is is what every other client does, and it is the difference between "this
    /// conversation is idle" and "someone sent you a photo".
    ///
    /// Returns `&str` rather than a `String` on purpose: this is called once per visible row
    /// per frame, and the labels are literals, so there is nothing to allocate. That also
    /// rules out putting a sticker's emoji here — it would need formatting, and the list
    /// draws in `small`, which has no emoji fallback behind it anyway.
    pub fn preview(&self) -> &str {
        let Some(m) = self.messages.last() else {
            return "";
        };
        // A caption wins over the label: it is what the sender actually wrote.
        if !m.text.is_empty() {
            return &m.text;
        }
        match &m.media {
            Some(Media::Photo { .. }) => "Foto",
            Some(Media::Sticker { .. }) => "Sticker",
            Some(Media::Voice { .. }) => "Mensagem de voz",
            Some(Media::Audio { .. }) => "Audio",
            Some(Media::File { .. }) => "Arquivo",
            Some(Media::Unknown) => "Midia",
            None => "",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Store {
    pub chats: Vec<Chat>,
    /// What the title bar shows: connection state from the transport layer.
    pub status: String,
    /// Whether a page of dialogs is in flight, to avoid double requests.
    pub dialogs_loading: bool,
    /// Whether every dialog the account has is already here.
    pub dialogs_complete: bool,
    /// Pagination: the last dialog of the current page. Used as the offset for the
    /// next `messages.getDialogs` request — the server wants the peer that comes
    /// before the ones we already have.
    pub dialog_offset_date: i32,
    pub dialog_offset_id: i32,
    pub dialog_offset_peer: Option<PeerRef>,
}

impl Store {
    /// Sample data, sized to expose the layout problems that matter: names that
    /// overflow, a preview that must ellipsize, a message long enough to wrap
    /// several lines, one short enough to under-fill a bubble, and Cyrillic.
    pub fn mock() -> Self {
        fn msg(text: &str, outgoing: bool, time: &str, state: Delivery) -> Message {
            Message { id: 0, text: text.to_string(), outgoing, time: time.to_string(), state, media: None }
        }

        /// As `msg`, with an attachment. Media rows exist in the mock so the simulator can
        /// show one at all: `Screen::Viewer` and every media placeholder used to be
        /// unreachable on the host, because every mock message had `media: None` and the
        /// only way to see a photo bubble was to sign in to Telegram from a phone.
        fn media_msg(text: &str, outgoing: bool, time: &str, media: Media) -> Message {
            Message {
                id: 0,
                text: text.to_string(),
                outgoing,
                time: time.to_string(),
                state: Delivery::Read,
                media: Some(media),
            }
        }

        let mut chats = Vec::new();

        chats.push(Chat {
            name: "Ana Paula".to_string(),
            time: "14:32".to_string(),
            unread: 2,
            last_outgoing: false,
            messages: alloc::vec![
                msg("oi! conseguiu compilar aquilo?", false, "14:18", Delivery::Read),
                msg("consegui, o elf2e32 rodou de primeira em aarch64", true, "14:20", Delivery::Read),
                msg("sério? achei que ia precisar de wine pra tudo", false, "14:21", Delivery::Read),
                msg(
                    "não, o Martin Storsjö reescreveu as ferramentas todas em C++ nativo. \
                     makesis, signsis, rcomp, elf2e32 — tudo compila direto no Linux.",
                    true,
                    "14:24",
                    Delivery::Read,
                ),
                msg("isso muda tudo", false, "14:30", Delivery::Sent),
                msg("agora falta rodar no aparelho de verdade", false, "14:30", Delivery::Sent),
                // One of each kind that draws differently, so the transcript exercises
                // every media label — and the last one is media with no caption, so the
                // chat list's own media preview is exercised too. None can be opened
                // without a connection; pressing Select says so.
                media_msg(
                    "olha a tela",
                    false,
                    "14:31",
                    Media::Photo {
                        id: 1,
                        access_hash: 0,
                        file_reference: Vec::new(),
                        dc_id: 2,
                        thumb_size: "m".to_string(),
                        size: 47_104,
                        preview: None,
                    },
                ),
                media_msg(
                    "",
                    true,
                    "14:31",
                    Media::Voice {
                        id: 2,
                        access_hash: 0,
                        file_reference: Vec::new(),
                        dc_id: 2,
                        duration: 7,
                        waveform: None,
                        size: 4_096,
                    },
                ),
                // A thumbs-up: inside the emoji subset, so this row is also the check that
                // the fallback atlas is wired up. Outside it, the label reads "[Sticker]".
                media_msg(
                    "",
                    false,
                    "14:32",
                    Media::Sticker {
                        id: 3,
                        access_hash: 0,
                        file_reference: Vec::new(),
                        dc_id: 2,
                        alt: "\u{1F44D}".to_string(),
                        preview: None,
                    },
                ),
            ],
            ..Default::default()
        });

        chats.push(Chat {
            name: "Symbian Revive".to_string(),
            time: "13:07".to_string(),
            unread: 17,
            last_outgoing: false,
            messages: alloc::vec![
                msg("anyone tried GCC 15 for arm-none-symbianelf?", false, "12:55", Delivery::Read),
                msg("binutils dropped the triple, you have to patch config.bfd", true, "13:02", Delivery::Sent),
                msg("that worked, thanks", false, "13:07", Delivery::Sent),
            ],
            ..Default::default()
        });

        chats.push(Chat {
            name: "Дмитрий".to_string(),
            time: "11:48".to_string(),
            unread: 0,
            last_outgoing: true,
            messages: alloc::vec![
                msg("привет! как дела с телефоном?", false, "11:40", Delivery::Read),
                msg("всё работает, спасибо", true, "11:48", Delivery::Read),
            ],
            ..Default::default()
        });

        chats.push(Chat {
            name: "Um Nome Bem Comprido Que Não Cabe".to_string(),
            time: "ter".to_string(),
            unread: 0,
            last_outgoing: false,
            messages: alloc::vec![msg(
                "este preview é longo o suficiente para precisar de reticências no fim",
                false,
                "09:12",
                Delivery::Read,
            )],
            ..Default::default()
        });

        chats.push(Chat {
            name: "Build Bot".to_string(),
            time: "seg".to_string(),
            unread: 0,
            last_outgoing: false,
            messages: alloc::vec![
                msg("ok", false, "08:00", Delivery::Read),
                msg("falhou: libgcov não compila", false, "08:02", Delivery::Read),
            ],
            ..Default::default()
        });

        chats.push(Chat {
            name: "Notas".to_string(),
            time: "dom".to_string(),
            unread: 0,
            last_outgoing: true,
            messages: alloc::vec![msg("EPOCSTACKSIZE 0x8000", true, "22:10", Delivery::Pending)],
            ..Default::default()
        });

        chats.push(Chat {
            name: "Marina".to_string(),
            time: "sáb".to_string(),
            unread: 1,
            last_outgoing: false,
            messages: alloc::vec![msg("👍", false, "19:30", Delivery::Sent)],
            ..Default::default()
        });

        Self { chats, status: "conectado".to_string(), ..Default::default() }
    }
}

/// Convert a protocol-level media type to the model's simplified one.
fn media_from_proto(m: &tg_proto::chats::Media) -> Media {
    use tg_proto::chats::Media as P;
    match m {
        P::Photo { id, access_hash, file_reference, dc_id, sizes } => {
            // Which size to fetch is decided here rather than at download time, so the
            // choice is made once against the whole list and the request only has to carry
            // a string. See `pick_size`.
            let chosen = pick_size(sizes, SCREEN_W, SCREEN_H);
            Media::Photo {
                id: *id,
                access_hash: *access_hash,
                file_reference: file_reference.clone(),
                dc_id: *dc_id,
                thumb_size: chosen.as_ref().map(|s| s.kind.clone()).unwrap_or_default(),
                size: chosen.as_ref().map(|s| s.size as i64).unwrap_or(0),
                preview: inline_preview(sizes),
            }
        }
        P::Sticker { id, access_hash, file_reference, dc_id, alt, thumbs, .. } => Media::Sticker {
            id: *id,
            access_hash: *access_hash,
            file_reference: file_reference.clone(),
            dc_id: *dc_id,
            alt: alt.clone(),
            preview: inline_preview(thumbs),
        },
        P::Document {
            id, access_hash, file_reference, dc_id, filename, size, duration, is_voice, waveform, ..
        } => {
            let common = (*id, *access_hash, file_reference.clone(), *dc_id);
            // The voice flag, not the presence of a duration. A music track has a duration
            // too, and treating it as a voice note drew a waveform for an album.
            if *is_voice {
                Media::Voice {
                    id: common.0,
                    access_hash: common.1,
                    file_reference: common.2,
                    dc_id: common.3,
                    duration: duration.unwrap_or(0),
                    waveform: waveform.clone(),
                    size: *size,
                }
            } else if duration.is_some() {
                Media::Audio {
                    id: common.0,
                    access_hash: common.1,
                    file_reference: common.2,
                    dc_id: common.3,
                    filename: filename.clone().unwrap_or_default(),
                    duration: duration.unwrap_or(0),
                    size: *size,
                }
            } else {
                Media::File {
                    id: common.0,
                    access_hash: common.1,
                    file_reference: common.2,
                    dc_id: common.3,
                    filename: filename.clone().unwrap_or_default(),
                    size: *size,
                }
            }
        }
        P::Unsupported => Media::Unknown,
    }
}

/// The E72's screen, which is the box a downloaded photo has to fit.
const SCREEN_W: i32 = 320;
const SCREEN_H: i32 = 240;

/// Choose which of a photo's sizes to download.
///
/// The smallest one that still covers the screen — not the largest, which is what asking
/// for the original amounts to. A Telegram photo's `y` size is commonly 1280 pixels wide
/// and several hundred kilobytes; on a 320-pixel screen every one of those pixels is
/// thrown away after being paid for over GPRS. `x` is 800 and `m` is 320, so `m` usually
/// wins outright.
///
/// Sizes that carry their bytes inline are skipped: they are previews, not the picture,
/// and they are already available without a request.
fn pick_size(
    sizes: &[tg_proto::chats::SizeOption],
    max_w: i32,
    max_h: i32,
) -> Option<tg_proto::chats::SizeOption> {
    let downloadable = || sizes.iter().filter(|s| s.inline.is_none() && !s.kind.is_empty());

    // Baseline before progressive, whatever the sizes say.
    //
    // `photoSizeProgressive` bytes are a progressive JPEG: a different scan structure that
    // an ICL plugin from 2008 is under no obligation to implement, and a codec handed a
    // format it does not support is under no obligation to fail politely. A slightly wrong
    // size that decodes beats a perfect one that does not.
    //
    // Expressed as a sort key rather than a filter, because a photo whose only downloadable
    // entries are progressive should still be attempted — refusing would mean showing
    // nothing at all.
    let rank = |s: &&tg_proto::chats::SizeOption| (s.progressive, (s.w as i64) * (s.h as i64));

    // Covers the screen in at least one axis, and among those the smallest.
    let covering = downloadable().filter(|s| s.w >= max_w || s.h >= max_h).min_by_key(rank);
    if let Some(s) = covering {
        return Some(s.clone());
    }
    // Nothing that big: the largest there is, still preferring baseline.
    downloadable()
        .max_by_key(|s| (!s.progressive, (s.w as i64) * (s.h as i64)))
        .cloned()
}

/// The best inline preview, if the message brought one.
///
/// Free pixels: `photoCachedSize` embeds a whole JPEG in the message itself, so it can be
/// drawn without a single request. That matters because downloads here are on demand — a
/// transcript full of photos costs nothing to scroll, and still shows something.
///
/// `photoStrippedSize` is excluded even though it is more common, because its JPEG has the
/// header stripped and needs a fixed one prepended before any codec will take it. Doing
/// that is worth a follow-up; handing the bytes to the decoder as they are is not.
fn inline_preview(sizes: &[tg_proto::chats::SizeOption]) -> Option<Vec<u8>> {
    sizes
        .iter()
        .filter(|s| s.inline_is_complete())
        .max_by_key(|s| (s.w as i64) * (s.h as i64))
        .and_then(|s| s.inline.clone())
}

/// Turn a parsed `messages.Dialogs` into what the chat list draws.
///
/// The two shapes are deliberately different. `tg_proto::chats` holds what Telegram sent —
/// peers, ids, unix seconds — and this holds what a 320x240 screen needs, which is a name,
/// a preview and `HH:MM` already formatted. Formatting in the drawing path would mean doing
/// it once per frame on a 600 MHz core for rows that have not changed.
///
/// `now` is the local clock. Dialogs older than today keep the same field, so the caller can
/// decide later whether to show a date instead; today's show the time.
pub fn store_from_dialogs(d: &tg_proto::chats::Dialogs, now: i64) -> Store {
    let mut chats = Vec::new();
    for dialog in &d.dialogs {
        if chats.len() >= DIALOG_LIMIT {
            break;
        }
        let name = d
            .names
            .iter()
            .find(|n| n.peer == dialog.peer)
            .map(|n| n.title.clone())
            .unwrap_or_else(|| String::from("(sem nome)"));

        let top = d.top_of(dialog);
        let (text, outgoing, date) = match top {
            Some(m) => (m.text.clone(), m.out, m.date),
            None => (String::new(), false, 0),
        };

        chats.push(Chat {
            peer: Some(PeerRef {
                kind: dialog.peer.kind,
                id: dialog.peer.id,
                access_hash: d.hash_of(dialog.peer),
            }),
            // The preview message is the newest one, so it is also the oldest one held —
            // the page above starts below it.
            oldest: dialog.top_message,
            name,
            time: hhmm(date as i64),
            unread: dialog.unread.max(0) as u32,
            last_outgoing: outgoing,
            messages: if top.is_some() {
                alloc::vec![Message {
                    id: top.map(|m| m.id).unwrap_or(0),
                    text,
                    outgoing,
                    time: hhmm(date as i64),
                    state: if outgoing { Delivery::Sent } else { Delivery::Read },
                    media: top.and_then(|m| m.media.as_ref()).map(media_from_proto),
                }]
            } else {
                Vec::new()
            },
            ..Default::default()
        });
    }
    let _ = now;
    // At the cap the list is as complete as it is going to get, whatever the server says
    // the total is.
    let dialog_complete = d.total.is_none() || chats.len() >= DIALOG_LIMIT;
    let (offset_date, offset_id, offset_peer) = d.dialogs.last()
        .map(|last| {
            let date = d.top_of(last).map(|m| m.date).unwrap_or(0);
            let hash = d.hash_of(last.peer);
            // Always Some — a Chat has no access_hash (zero), but inputPeerChat only needs the id.
            (date, last.top_message, Some(PeerRef { kind: last.peer.kind, id: last.peer.id, access_hash: hash }))
        })
        .unwrap_or((0, 0, None));
    let store = Store {
        chats,
        dialogs_complete: dialog_complete,
        dialog_offset_date: offset_date,
        dialog_offset_id: offset_id,
        dialog_offset_peer: offset_peer,
        ..Store::default()
    };
    store
}

/// Fold a page of history into a chat, in order and without losing or repeating anything.
///
/// A page is placed by message id rather than assumed to be older than everything held. The
/// assumption used to hold, because the only caller was "scroll up"; it stopped holding the
/// moment two other callers appeared. `refresh_conversation` asks from offset zero, and a
/// conversation restored from disk already holds its newest messages — in both cases the
/// reply overlaps what is on screen, and a blind prepend put a second copy of every message
/// above the first.
///
/// Returns how many were **prepended and retained**: the number the caller shifts its
/// selection by, so the bubble the user was reading stays under the cursor. Messages
/// appended at the bottom shift nothing and are not counted.
pub fn merge_history(chat: &mut Chat, page: &tg_proto::chats::Dialogs, peer: PeerRef) -> usize {
    let incoming = || page.messages.iter().filter(|m| m.peer.id == peer.id);

    // An empty page — not an empty *retained* count — is what means there is nothing above
    // this. Deriving it from what survived deduplication would mark a conversation complete
    // the first time a refresh returned only messages already on screen.
    if incoming().next().is_none() {
        chat.loading = false;
        chat.complete = true;
        return 0;
    }

    // A locally sent message has id 0 until the server answers, so it is no bound on
    // anything and is excluded from both ends.
    let held_lo = chat.messages.iter().map(|m| m.id).filter(|&id| id != 0).min();
    let held_hi = chat.messages.iter().map(|m| m.id).filter(|&id| id != 0).max();
    let full = chat.messages.len() >= CHAT_WINDOW;

    let mut older = Vec::new();
    let mut newer = Vec::new();
    for m in incoming() {
        if chat.messages.iter().any(|held| held.id == m.id && m.id != 0) {
            continue;
        }
        let built = Message {
            id: m.id,
            text: m.text.clone(),
            outgoing: m.out,
            time: hhmm(m.date as i64),
            state: if m.out { Delivery::Sent } else { Delivery::Read },
            media: m.media.as_ref().map(media_from_proto),
        };
        match (held_lo, held_hi) {
            // Above what is held. A full chat has nowhere to put these: they would go in
            // front and `trim_window` would take them straight back off, so they are
            // refused and `windowed` stops the screen asking again on the next keypress.
            (Some(lo), Some(_)) if m.id < lo => {
                if full {
                    chat.windowed = true;
                } else {
                    older.push(built);
                }
            }
            // Below it: what a refresh brings back.
            (Some(_), Some(hi)) if m.id > hi => newer.push(built),
            // Inside the range and not an exact id match: a hole in what is held, which
            // this client cannot splice into the middle without reordering the transcript.
            // Dropped rather than misplaced.
            (Some(_), Some(_)) => {}
            // Nothing held with a server id yet — the first load of a conversation.
            _ => newer.push(built),
        }
    }

    // Telegram answers newest first and the screen reads downwards in time.
    older.reverse();
    newer.reverse();

    let prepended = older.len();
    if prepended > 0 {
        older.append(&mut chat.messages);
        chat.messages = older;
    }
    chat.messages.append(&mut newer);

    chat.oldest = chat.messages.iter().map(|m| m.id).filter(|&id| id != 0).min().unwrap_or(0);
    chat.loading = false;

    // A page can overshoot the window; the overshoot goes off the front, which is the
    // oldest of what is now held. `trim_window` fixes `oldest` up afterwards, so the cursor
    // names a message that is still here.
    let dropped = trim_window(chat);
    prepended.saturating_sub(dropped)
}

/// Append a page of dialogs to the existing chat list.
///
/// Called when a second (or later) `messages.getDialogs` reply arrives. Unlike
/// [`store_from_dialogs`], which replaces the store entirely, this appends new chats and
/// updates the pagination offsets from the last dialog of the incoming page.
///
/// Returns how many new chats were added.
pub fn merge_dialogs(store: &mut Store, d: &tg_proto::chats::Dialogs, now: i64) -> usize {
    let mut added = 0;
    for dialog in &d.dialogs {
        if store.chats.len() >= DIALOG_LIMIT {
            // Stop asking as well as stop storing: a list that refuses the page it just
            // paid for would request the same one every time the user reaches the bottom.
            store.dialogs_complete = true;
            break;
        }

        // A dialog already held is skipped, not appended.
        //
        // This is not belt-and-braces, it is the load-bearing check. `messages.getDialogs`
        // returns the page *after* the offset triple, and that triple is only as good as the
        // last page's data: `offset_date` comes from the last dialog's top message, and a
        // dialog whose top message is not in the reply's `messages` vector yields zero —
        // which the server reads as "from the beginning". The next page is then the first page
        // again, and without this the list grew a second copy of every conversation each time
        // the user reached the bottom.
        //
        // Keyed by peer rather than by name: two people can share a name, and the same person
        // must never appear twice.
        let held = store
            .chats
            .iter()
            .any(|c| c.peer.is_some_and(|p| p.kind == dialog.peer.kind && p.id == dialog.peer.id));
        if held {
            continue;
        }

        let name = d
            .names
            .iter()
            .find(|n| n.peer == dialog.peer)
            .map(|n| n.title.clone())
            .unwrap_or_else(|| String::from("(sem nome)"));

        let top = d.top_of(dialog);
        let (text, outgoing, date) = match top {
            Some(m) => (m.text.clone(), m.out, m.date),
            None => (String::new(), false, 0),
        };

        store.chats.push(Chat {
            peer: Some(PeerRef {
                kind: dialog.peer.kind,
                id: dialog.peer.id,
                access_hash: d.hash_of(dialog.peer),
            }),
            oldest: dialog.top_message,
            name,
            time: hhmm(date as i64),
            unread: dialog.unread.max(0) as u32,
            last_outgoing: outgoing,
            messages: if top.is_some() {
                alloc::vec![Message {
                    id: top.map(|m| m.id).unwrap_or(0),
                    text,
                    outgoing,
                    time: hhmm(date as i64),
                    state: if outgoing { Delivery::Sent } else { Delivery::Read },
                    media: top.and_then(|m| m.media.as_ref()).map(media_from_proto),
                }]
            } else {
                Vec::new()
            },
            ..Default::default()
        });
        added += 1;
    }

    store.dialogs_loading = false;

    // The end of the list is "this page told us nothing new", not "this page was empty".
    //
    // An empty page is the polite ending and cannot be relied on: when the offset fails to
    // advance, the server keeps answering with a full page of dialogs that are all already
    // held. Counting *new* arrivals catches both, so the list stops at the real end either
    // way instead of asking again for every keypress.
    if added == 0 {
        store.dialogs_complete = true;
    }
    if let Some(last) = d.dialogs.last() {
        store.dialog_offset_date = d.top_of(last).map(|m| m.date).unwrap_or(0);
        store.dialog_offset_id = last.top_message;
        let hash = d.hash_of(last.peer);
        store.dialog_offset_peer = Some(PeerRef {
            kind: last.peer.kind,
            id: last.peer.id,
            access_hash: hash, // zero for Chat, which is correct — inputPeerChat ignores it
        });
    }
    let _ = now;
    added
}

/// Replace the media field of a specific message in a chat with a fresh one from
/// a `messages.getMessages` reply. Used when a `file_reference` expires — re-fetching
/// the single message gives a valid reference without reloading the whole history.
///
/// Returns `true` if the message was found and updated.
pub fn refresh_media(chat: &mut Chat, msg_id: i32, fresh: &tg_proto::chats::Message) -> bool {
    for m in chat.messages.iter_mut() {
        if m.id == msg_id {
            if let Some(ref media) = fresh.media {
                m.media = Some(media_from_proto(media));
                return true;
            }
        }
    }
    false
}

/// `HH:MM` in the handset's local time, from UTC unix seconds.
///
/// The offset is read once per call from the device's locale setting —
/// `User::UTCOffset()` on the shim side — and added to the UTC timestamp.
/// Zero renders as empty rather than as midnight — a dialog with no preview
/// message should show nothing, not `00:00`.
pub(crate) fn hhmm(unix: i64) -> String {
    let mut s = String::new();
    if unix <= 0 {
        return s;
    }
    let local = (unix + symbian::utc_offset() as i64).rem_euclid(86_400);
    let (h, m) = (local / 3600, (local % 3600) / 60);
    fn push2(s: &mut String, v: i64) {
        s.push((b'0' + (v / 10) as u8) as char);
        s.push((b'0' + (v % 10) as u8) as char);
    }
    push2(&mut s, h);
    s.push(':');
    push2(&mut s, m);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A chat holding `n` messages with ids 1..=n, oldest first, as the screen reads them.
    fn filled(n: usize) -> Chat {
        let mut c = Chat { name: "x".to_string(), ..Default::default() };
        for i in 1..=n {
            c.messages.push(Message {
                id: i as i32,
                text: alloc::format!("m{i}"),
                outgoing: false,
                time: "10:00".to_string(),
                state: Delivery::Read,
                media: None,
            });
        }
        c.oldest = 1;
        c
    }

    /// A `getDialogs` page listing `ids` as user peers, with a top message for each.
    fn dialog_page(ids: &[i64]) -> tg_proto::chats::Dialogs {
        use tg_proto::chats as tc;
        let mut dialogs = Vec::new();
        let mut messages = Vec::new();
        let mut names = Vec::new();
        for (n, &id) in ids.iter().enumerate() {
            let peer = tc::Peer { kind: tc::Kind::User, id };
            let top = 1000 + n as i32;
            dialogs.push(tc::Dialog { peer, top_message: top, unread: 0 });
            messages.push(tc::Message {
                id: top,
                peer,
                out: false,
                date: 1_700_000_000 + n as i32,
                text: alloc::format!("oi {id}"),
                service: false,
                media: None,
            });
            names.push(tc::Named {
                peer,
                title: alloc::format!("Contato {id}"),
                access_hash: id * 7,
            });
        }
        tc::Dialogs { dialogs, messages, names, total: Some(ids.len() as i32) }
    }

    /// The same page twice must not produce the list twice.
    ///
    /// The reported symptom: reaching the bottom of the chat list and pressing Down grew a
    /// second copy of every contact. `messages.getDialogs` returns the page *after* an offset
    /// triple, and that triple is only as good as the previous page's data — `offset_date`
    /// comes from the last dialog's top message, and a dialog whose top message is absent from
    /// the reply yields zero, which the server reads as "from the beginning". So the second
    /// page can legitimately be the first, and the merge has to be the thing that notices.
    #[test]
    fn the_same_page_arriving_twice_adds_nothing_the_second_time() {
        let page = dialog_page(&[1, 2, 3]);
        let mut store = Store::default();

        assert_eq!(merge_dialogs(&mut store, &page, 0), 3);
        assert_eq!(store.chats.len(), 3);

        assert_eq!(merge_dialogs(&mut store, &page, 0), 0, "every dialog is already held");
        assert_eq!(store.chats.len(), 3, "the list must not double");
    }

    /// And it stops asking, which is what "hit the end" means.
    ///
    /// The end of the list is "this page told us nothing new", not "this page was empty": a
    /// server whose offset never advanced keeps answering with a full page of dialogs that are
    /// all already held, and an emptiness check would never fire.
    #[test]
    fn a_page_of_only_known_dialogs_ends_the_list() {
        let page = dialog_page(&[1, 2]);
        let mut store = Store::default();
        merge_dialogs(&mut store, &page, 0);
        assert!(!store.dialogs_complete, "a full first page is not the end");

        merge_dialogs(&mut store, &page, 0);
        assert!(store.dialogs_complete, "a page with nothing new is the end");
        assert!(!store.dialogs_loading);
    }

    /// A genuinely new page still appends, so the dedup cannot be a list that never grows.
    #[test]
    fn a_page_of_new_dialogs_still_appends() {
        let mut store = Store::default();
        merge_dialogs(&mut store, &dialog_page(&[1, 2]), 0);
        assert_eq!(merge_dialogs(&mut store, &dialog_page(&[3, 4]), 0), 2);
        assert_eq!(store.chats.len(), 4);
        assert!(!store.dialogs_complete);
    }

    /// A page that overlaps by one — the off-by-one an exclusive offset produces — keeps the
    /// new dialogs and drops only the repeat.
    #[test]
    fn an_overlapping_page_keeps_only_what_is_new() {
        let mut store = Store::default();
        merge_dialogs(&mut store, &dialog_page(&[1, 2, 3]), 0);
        assert_eq!(merge_dialogs(&mut store, &dialog_page(&[3, 4, 5]), 0), 2);
        assert_eq!(store.chats.len(), 5);
        let ids: Vec<i64> = store.chats.iter().filter_map(|c| c.peer.map(|p| p.id)).collect();
        assert_eq!(ids, alloc::vec![1, 2, 3, 4, 5]);
    }

    /// A `getHistory` page for `peer` carrying ids `from` down to `to`, newest first,
    /// which is the order the server answers in.
    fn page(peer: PeerRef, from: i32, to: i32) -> tg_proto::chats::Dialogs {
        let p = tg_proto::chats::Peer { kind: peer.kind, id: peer.id };
        let messages = (to..=from)
            .rev()
            .map(|id| tg_proto::chats::Message {
                id,
                peer: p,
                out: false,
                date: 0,
                text: alloc::format!("m{id}"),
                service: false,
                media: None,
            })
            .collect();
        tg_proto::chats::Dialogs { messages, ..Default::default() }
    }

    fn peer() -> PeerRef {
        PeerRef { kind: tg_proto::chats::Kind::User, id: 7, access_hash: 9 }
    }

    #[test]
    fn the_hundred_and_first_message_pushes_the_oldest_out() {
        let mut c = filled(CHAT_WINDOW);
        let dropped = push_message(
            &mut c,
            Message {
                id: 999,
                text: "novo".to_string(),
                outgoing: true,
                time: "10:01".to_string(),
                state: Delivery::Pending,
                media: None,
            },
        );
        assert_eq!(dropped, 1, "one out for one in");
        assert_eq!(c.messages.len(), CHAT_WINDOW, "and the window did not grow");
        assert_eq!(c.messages.first().map(|m| m.id), Some(2), "the oldest is the one that left");
        assert_eq!(c.messages.last().map(|m| m.id), Some(999));
    }

    #[test]
    fn trimming_moves_the_pagination_cursor_to_a_message_still_held() {
        // `oldest` is what the next getHistory asks above. Left pointing at a message that
        // was just dropped, the reply would be a page nobody can see the join to.
        let mut c = filled(CHAT_WINDOW + 5);
        assert_eq!(c.oldest, 1);
        let dropped = trim_window(&mut c);
        assert_eq!(dropped, 5);
        assert_eq!(c.messages.len(), CHAT_WINDOW);
        assert_eq!(c.oldest, 6, "the oldest retained, not the oldest ever seen");
        assert!(c.windowed);
    }

    #[test]
    fn a_page_of_history_below_the_window_is_kept_whole() {
        let mut c = filled(10);
        c.oldest = 1;
        let n = merge_history(&mut c, &page(peer(), 0, -9), peer());
        assert_eq!(n, 10, "all ten retained");
        assert_eq!(c.messages.len(), 20);
        assert_eq!(c.messages.first().map(|m| m.id), Some(-9), "oldest first on screen");
        assert_eq!(c.oldest, -9);
        assert!(!c.windowed);
    }

    #[test]
    fn a_full_chat_refuses_a_page_of_older_messages_instead_of_looping() {
        // The trap this guards: prepend then trim would discard exactly what arrived, the
        // screen would look unchanged, and the next Up would ask for the same page again.
        let mut c = filled(CHAT_WINDOW);
        c.loading = true;
        let n = merge_history(&mut c, &page(peer(), 0, -19), peer());
        assert_eq!(n, 0);
        assert!(c.windowed, "so the screen stops asking");
        assert!(!c.complete, "the server still has more — this is our limit, not its end");
        assert!(!c.loading);
        assert_eq!(c.messages.len(), CHAT_WINDOW);
        assert_eq!(c.messages.first().map(|m| m.id), Some(1), "nothing was displaced");
    }

    #[test]
    fn a_page_that_overshoots_the_window_reports_only_what_was_retained() {
        // The caller shifts the selected bubble by this number. Reporting the arrival count
        // rather than the retained one would scroll the user past the message they were on.
        let mut c = filled(CHAT_WINDOW - 5);
        c.oldest = 1;
        let n = merge_history(&mut c, &page(peer(), 0, -19), peer());
        assert_eq!(c.messages.len(), CHAT_WINDOW);
        assert_eq!(n, 5, "twenty arrived, five fit");
        assert_eq!(c.messages.first().map(|m| m.id), Some(-4));
        assert_eq!(c.oldest, -4);
    }

    #[test]
    fn a_refresh_appends_only_what_is_new_and_never_a_second_copy() {
        // The bug this pins: `refresh_conversation` asks from offset zero, so its reply
        // overlaps what is on screen. Prepending it blindly put a duplicate of every
        // message above the originals, and the transcript read as if the conversation had
        // happened twice.
        let mut c = filled(5);
        let n = merge_history(&mut c, &page(peer(), 7, 3), peer());
        assert_eq!(n, 0, "nothing was prepended, so the selection does not move");
        let ids: alloc::vec::Vec<i32> = c.messages.iter().map(|m| m.id).collect();
        assert_eq!(ids, alloc::vec![1, 2, 3, 4, 5, 6, 7], "in order, each exactly once");
    }

    #[test]
    fn a_page_that_is_entirely_already_held_changes_nothing() {
        let mut c = filled(5);
        let n = merge_history(&mut c, &page(peer(), 5, 1), peer());
        assert_eq!(n, 0);
        assert_eq!(c.messages.len(), 5);
        assert!(!c.complete, "the page was not empty — the server has more, we had it already");
    }

    #[test]
    fn a_full_chat_still_accepts_messages_that_are_new() {
        // `windowed` blocks history *above* the window. A message that arrives below it is
        // the point of the application, and drops the oldest to make room.
        let mut c = filled(CHAT_WINDOW);
        let n = merge_history(&mut c, &page(peer(), CHAT_WINDOW as i32 + 2, CHAT_WINDOW as i32 + 1), peer());
        assert_eq!(n, 0, "appended, not prepended");
        assert_eq!(c.messages.len(), CHAT_WINDOW);
        assert_eq!(c.messages.last().map(|m| m.id), Some(CHAT_WINDOW as i32 + 2));
        assert_eq!(c.messages.first().map(|m| m.id), Some(3), "two off the front for two on");
    }

    #[test]
    fn the_first_load_of_a_conversation_fills_it_in_reading_order() {
        let mut c = Chat { name: "x".to_string(), ..Default::default() };
        let n = merge_history(&mut c, &page(peer(), 5, 1), peer());
        assert_eq!(n, 0, "nothing was displaced, so nothing shifts");
        let ids: alloc::vec::Vec<i32> = c.messages.iter().map(|m| m.id).collect();
        assert_eq!(ids, alloc::vec![1, 2, 3, 4, 5], "oldest at the top");
        assert_eq!(c.oldest, 1);
    }

    #[test]
    fn a_pending_message_does_not_become_the_pagination_cursor() {
        // A locally sent message has id 0 until the server answers. Counted as the oldest
        // id held, it would make the next getHistory ask for everything above zero.
        let mut c = filled(3);
        push_message(&mut c, Message {
            id: 0,
            text: "enviando".to_string(),
            outgoing: true,
            time: "10:01".to_string(),
            state: Delivery::Pending,
            media: None,
        });
        merge_history(&mut c, &page(peer(), 6, 4), peer());
        assert_eq!(c.oldest, 1, "the oldest real id, not the pending zero");
    }

    #[test]
    fn an_empty_page_still_ends_the_conversation() {
        let mut c = filled(3);
        let n = merge_history(&mut c, &page(peer(), 0, 1), peer());
        assert_eq!(n, 0);
        assert!(c.complete, "nothing above this");
    }

    #[test]
    fn the_dialog_list_stops_growing_at_the_cap() {
        let mut store = Store { chats: (0..DIALOG_LIMIT).map(|_| Chat::default()).collect(), ..Default::default() };
        let d = tg_proto::chats::Dialogs {
            dialogs: alloc::vec![tg_proto::chats::Dialog {
                peer: tg_proto::chats::Peer { kind: tg_proto::chats::Kind::User, id: 1 },
                top_message: 1,
                unread: 0,
            }],
            ..Default::default()
        };
        assert_eq!(merge_dialogs(&mut store, &d, 0), 0);
        assert_eq!(store.chats.len(), DIALOG_LIMIT);
        assert!(store.dialogs_complete, "and it stops asking for the page it would refuse");
    }

    /// A chat of `n` photo messages, each carrying its own inline JPEG.
    fn with_previews(n: usize) -> Chat {
        let mut c = filled(n);
        for (i, m) in c.messages.iter_mut().enumerate() {
            m.media = Some(Media::Photo {
                id: i as i64 + 1,
                access_hash: 0,
                file_reference: Vec::new(),
                dc_id: 2,
                thumb_size: "m".to_string(),
                size: 10,
                preview: Some(alloc::vec![0xFF, 0xD8, i as u8]),
            });
        }
        c
    }

    fn held(c: &Chat) -> usize {
        c.messages.iter().filter(|m| m.media.as_ref().and_then(|x| x.preview()).is_some()).count()
    }

    #[test]
    fn previews_far_from_the_selection_are_freed_and_the_near_ones_kept() {
        let mut fs = symbian::fs::MemFs::new();
        let mut c = with_previews(CHAT_WINDOW);
        assert_eq!(held(&c), CHAT_WINDOW);

        let freed = window_previews(&mut c, 0, &mut fs);
        assert_eq!(freed, CHAT_WINDOW - (PREVIEW_BAND + 1));
        assert_eq!(held(&c), PREVIEW_BAND + 1, "the selection and the band below it");
        assert!(c.messages[0].media.as_ref().unwrap().preview().is_some());
        assert!(c.messages[CHAT_WINDOW - 1].media.as_ref().unwrap().preview().is_none());
    }

    #[test]
    fn a_freed_preview_comes_back_when_the_selection_reaches_it_again() {
        // The point of spilling rather than dropping: the bytes came inside the message and
        // no request fetches one on its own, so scrolling back to a photo must not show a
        // hole where its thumbnail was.
        let mut fs = symbian::fs::MemFs::new();
        let mut c = with_previews(CHAT_WINDOW);
        window_previews(&mut c, 0, &mut fs);
        let last = CHAT_WINDOW - 1;
        assert!(c.messages[last].media.as_ref().unwrap().preview().is_none());

        window_previews(&mut c, last, &mut fs);
        assert_eq!(
            c.messages[last].media.as_ref().unwrap().preview(),
            Some(&[0xFFu8, 0xD8, last as u8][..]),
            "the same bytes, not someone else's"
        );
        assert!(c.messages[0].media.as_ref().unwrap().preview().is_none(), "and the far end went");
    }

    #[test]
    fn a_preview_whose_write_fails_stays_in_memory() {
        // MemFs with no private directory stands in for a full or unwritable cage. Dropping
        // the bytes anyway would lose them for good.
        struct NoFs;
        impl symbian::fs::Fs for NoFs {
            fn open(&mut self, _: &[u16], _: symbian::fs::OpenMode) -> symbian::Result<i32> {
                Err(symbian::Error::NotFound)
            }
            fn close(&mut self, _: i32) {}
            fn read(&mut self, _: i32, _: &mut [u8]) -> symbian::Result<usize> {
                Err(symbian::Error::NotFound)
            }
            fn write(&mut self, _: i32, _: &[u8]) -> symbian::Result<usize> {
                Err(symbian::Error::NotFound)
            }
            fn size(&mut self, _: i32) -> symbian::Result<u64> {
                Err(symbian::Error::NotFound)
            }
            fn seek(&mut self, _: i32, _: u64) -> symbian::Result<()> {
                Err(symbian::Error::NotFound)
            }
            fn list_dir(&mut self, _: &[u16], _: &mut [u16]) -> symbian::Result<usize> {
                Err(symbian::Error::NotFound)
            }
            fn delete(&mut self, _: &[u16]) -> symbian::Result<()> {
                Err(symbian::Error::NotFound)
            }
            fn rename(&mut self, _: &[u16], _: &[u16]) -> symbian::Result<()> {
                Err(symbian::Error::NotFound)
            }
            fn private_path(&mut self, _: &mut [u16]) -> symbian::Result<usize> {
                Err(symbian::Error::NotFound)
            }
        }
        let mut c = with_previews(CHAT_WINDOW);
        let freed = window_previews(&mut c, 0, &mut NoFs);
        assert_eq!(freed, 0);
        assert_eq!(held(&c), CHAT_WINDOW, "nothing was lost to a disk that would not take it");
    }

    #[test]
    fn a_dialog_with_no_preview_message_shows_no_time() {
        // `top_of` finds nothing when the message list does not carry the dialog's top
        // message, which Telegram does whenever that message was deleted. Rendering the
        // epoch as 00:00 reads as a real timestamp on a real chat.
        assert_eq!(hhmm(0), "");
        assert_eq!(hhmm(-1), "");
    }

    fn photo(size: i64) -> Media {
        Media::Photo {
            id: 1,
            access_hash: 2,
            file_reference: Vec::new(),
            dc_id: 2,
            thumb_size: "m".to_string(),
            size,
            preview: None,
        }
    }

    fn with_last(text: &str, media: Option<Media>) -> Chat {
        Chat {
            name: "x".to_string(),
            messages: alloc::vec![Message {
                id: 1,
                text: text.to_string(),
                outgoing: false,
                time: "10:00".to_string(),
                state: Delivery::Read,
                media,
            }],
            ..Default::default()
        }
    }

    fn size_of(kind: &str, w: i32, h: i32, progressive: bool) -> tg_proto::chats::SizeOption {
        tg_proto::chats::SizeOption {
            kind: kind.to_string(),
            w,
            h,
            size: w * h / 10,
            inline: None,
            progressive,
        }
    }

    #[test]
    fn a_baseline_size_beats_a_progressive_one_that_fits_better() {
        // Progressive JPEG is a different scan structure, and the 2008 ICL plugin is under
        // no obligation to implement it — nor to fail politely when handed one. A slightly
        // wrong size that decodes beats a perfect one that does not.
        let sizes = [
            size_of("x", 800, 600, true),
            size_of("m", 320, 240, false),
            size_of("s", 90, 67, false),
        ];
        let picked = pick_size(&sizes, 320, 240).unwrap();
        assert_eq!(picked.kind, "m");
        assert!(!picked.progressive);
    }

    #[test]
    fn a_progressive_size_is_still_tried_when_it_is_the_only_one() {
        // Refusing outright would mean showing nothing at all, which is worse than
        // attempting a format the codec may yet handle.
        let sizes = [size_of("y", 1280, 960, true)];
        let picked = pick_size(&sizes, 320, 240).unwrap();
        assert_eq!(picked.kind, "y");
        assert!(picked.progressive);
    }

    #[test]
    fn the_smallest_size_that_still_covers_the_screen_wins() {
        // Not the largest: a 1280-wide original is several hundred kilobytes over GPRS and
        // every pixel past 320 is paid for and thrown away.
        let sizes = [
            size_of("s", 90, 67, false),
            size_of("m", 320, 240, false),
            size_of("x", 800, 600, false),
            size_of("y", 1280, 960, false),
        ];
        assert_eq!(pick_size(&sizes, 320, 240).unwrap().kind, "m");
    }

    #[test]
    fn a_photo_smaller_than_the_screen_still_picks_its_largest() {
        let sizes = [size_of("s", 90, 67, false), size_of("m", 160, 120, false)];
        assert_eq!(pick_size(&sizes, 320, 240).unwrap().kind, "m");
    }

    #[test]
    fn a_truncated_download_is_recognisable_before_the_codec_sees_it() {
        // The diagnosis this exists for: a decoder handed a JPEG with no end marker is
        // entitled to sit waiting for the rest, which looks exactly like a codec that has
        // hung. Distinguishing the two from the app is the difference between fixing the
        // transport and blaming the plugin.
        let whole = [0xFFu8, 0xD8, 0x11, 0x22, 0xFF, 0xD9];
        assert!(crate::App::describe_bytes(&whole).contains("SOI+EOI"));

        let cut = [0xFFu8, 0xD8, 0x11, 0x22];
        let s = crate::App::describe_bytes(&cut);
        assert!(s.contains("SOI"));
        assert!(s.contains("NO-EOI"), "{s}");

        // And something that is not a JPEG at all points back at the transport.
        assert!(crate::App::describe_bytes(&[1, 2, 3]).contains("no-SOI"));
        // Including the empty case, which must not panic on the two-byte reads.
        assert!(crate::App::describe_bytes(&[]).contains("no-SOI"));
    }

    #[test]
    fn a_media_only_chat_says_what_the_attachment_is() {
        // It used to return "" — a row with a name, a time and nothing between them, which
        // reads as a broken list rather than as "someone sent you a photo".
        assert_eq!(with_last("", Some(photo(0))).preview(), "Foto");
        assert_eq!(
            with_last("", Some(Media::Voice {
                id: 1, access_hash: 0, file_reference: Vec::new(), dc_id: 2,
                duration: 7, waveform: None, size: 0,
            })).preview(),
            "Mensagem de voz"
        );
        assert_eq!(
            with_last("", Some(Media::Sticker {
                id: 1, access_hash: 0, file_reference: Vec::new(), dc_id: 2,
                alt: "\u{1F44D}".to_string(), preview: None,
            })).preview(),
            // The list draws in `small`, which has no emoji fallback behind it, so the label
            // is a word rather than the alt.
            "Sticker"
        );
    }

    #[test]
    fn a_caption_wins_over_the_attachment_label() {
        // What the sender actually wrote is more informative than its kind.
        assert_eq!(with_last("olha isso", Some(photo(0))).preview(), "olha isso");
    }

    #[test]
    fn a_chat_with_no_messages_still_has_no_preview() {
        assert_eq!(Chat::default().preview(), "");
        assert_eq!(with_last("", None).preview(), "");
    }

    #[test]
    fn the_cache_key_is_the_file_id_for_every_kind() {
        // Keyed on the id and nothing else, so the same file opened from two different
        // messages — a forward, say — is downloaded once.
        assert_eq!(photo(0).file_id(), 1);
        assert_eq!(
            Media::File {
                id: 99, access_hash: 0, file_reference: Vec::new(), dc_id: 2,
                filename: String::new(), size: 0,
            }
            .file_id(),
            99
        );
        assert_eq!(Media::Unknown.file_id(), 0);
    }

    #[test]
    fn a_sticker_is_never_fetchable_and_a_sizeless_photo_is_not_either() {
        // A sticker would spend data to arrive at the same placeholder; a photo that listed
        // no downloadable size has no `thumb_size` to ask with, and sending an empty one is
        // what the server answers LOCATION_INVALID to.
        assert!(!Media::Sticker {
            id: 1, access_hash: 0, file_reference: Vec::new(), dc_id: 2,
            alt: String::new(), preview: None,
        }
        .is_fetchable());

        let mut p = photo(0);
        if let Media::Photo { thumb_size, .. } = &mut p {
            thumb_size.clear();
        }
        assert!(!p.is_fetchable(), "no size type means nothing to ask for");
        assert!(photo(0).is_fetchable());
    }

    #[test]
    fn times_render_as_two_digits_each() {
        // 09:05, not 9:5. The chat list aligns on the colon.
        assert_eq!(hhmm(9 * 3600 + 5 * 60), "09:05");
        assert_eq!(hhmm(23 * 3600 + 59 * 60), "23:59");
        // And a date far from the epoch still shows the time of day, not the day.
        assert_eq!(hhmm(1_786_044_000), hhmm(1_786_044_000 % 86_400));
    }
}
