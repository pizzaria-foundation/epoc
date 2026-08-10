//! Dialogs and message history, turned into the six fields a chat list needs.
//!
//! Everything here rides on [`crate::walk`]: the schema table locates the fields and this
//! reads the handful that matter. Nothing in this file counts bytes or knows a field's
//! offset — the indices come from [`crate::schema`], generated from `api.tl`, so a schema
//! change moves them rather than silently shifting a message's text into its date.
//!
//! # What is deliberately thrown away
//!
//! Entities, reactions, forwards, replies, polls, and the other thirty-odd fields of
//! `message#7600b9d3`. A poll this client cannot render is bytes to walk past, not data to
//! model.
//!
//! Media is **not** in that list, though this comment used to say it was. Photos, voice
//! messages and stickers are modelled, because the handset can show a photo through its own
//! JPEG codec — see `crates/symbian/src/image.rs` — and because what it cannot show it can
//! at least name.
//!
//! That is why the walker exists rather than a struct per constructor: skipping a field
//! still requires knowing its shape, but it does not require a name, a type or a line of
//! code.
//!
//! # Field indices are positional, and that is a real hazard
//!
//! The walker addresses fields by position, so an index off by one reads a different field
//! and usually still parses. Two of the three bugs that kept media from working here were
//! exactly that: `messageMediaDocument`'s document read at index 0 instead of 6, which is
//! `flags` and never has bytes, so no document was ever built. Every index below therefore
//! carries the `api.tl` line it came from, and the generated constants in
//! [`crate::schema`] are the place to put new ones.
//!
//! # Peers, and the two id spaces
//!
//! A dialog names a peer; the name lives in a separate `users` or `chats` vector. Both are
//! keyed by id and **the two spaces overlap** — user 1234 and chat 1234 are different
//! entities — so [`Peer`] carries which kind it is and a lookup that ignores that finds the
//! wrong name roughly whenever a small account and a small group share a number.

use alloc::string::String;
use alloc::vec::Vec;

use crate::schema as s;
use crate::walk::{as_flag, as_int, as_long, as_str, field, vector_elements, Located, Walker};

pub use crate::walk::Error;
pub type Result<T> = core::result::Result<T, Error>;

// The empty variants, and the two PhotoSizes that carry nothing drawable. Local because
// they have no fields worth naming, so there is nothing for the generator to emit: this
// file only ever compares their ids. Everything with a field it reads comes from
// [`crate::schema`] instead — see the note about positional indices above.
const MESSAGEMEDIAEMPTY_CTOR: u32 = 0x3ded6320;
const PHOTOEMPTY_CTOR: u32 = 0x2331b22d;
const DOCUMENTEMPTY_CTOR: u32 = 0x36f8c871;
/// A compressed vector outline, not an image: it sketches a sticker's shape while the
/// real file loads. Skipped, because there is no path rasteriser here to draw it with.
const PHOTOPATHSIZE_CTOR: u32 = 0xd8214d41;
const PHOTOSIZEEMPTY_CTOR: u32 = 0x0e17e23c;

/// Which id space an id belongs to.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Kind {
    User,
    Chat,
    Channel,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Peer {
    pub kind: Kind,
    pub id: i64,
}

/// One message, reduced to what a 320x240 screen shows.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Message {
    pub id: i32,
    pub peer: Peer,
    /// True when we sent it.
    pub out: bool,
    /// Unix seconds. Formatting needs a locale and a clock, neither of which belongs here.
    pub date: i32,
    pub text: String,
    /// A service message — someone joined, the title changed. Carried rather than dropped
    /// because a dialog whose latest event is one shows a blank preview otherwise, which
    /// reads as a bug.
    pub service: bool,
    /// An attached photo, document, voice message, etc. `None` for plain-text messages
    /// and `messageMediaEmpty`.
    pub media: Option<Media>,
}

/// One entry of a photo's or document's size vector.
///
/// The `kind` string is the point of this type. `upload.getFile` on a photo needs
/// `inputPhotoFileLocation.thumb_size`, and it must be the `type` of one of these — an
/// empty string is refused with `LOCATION_INVALID`. So a client that never reads `sizes`
/// cannot download a photo at all, whatever else it gets right.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SizeOption {
    /// The `type` field: `"s"`, `"m"`, `"x"`, `"y"`, `"w"`, or `"i"`/`"j"` for the two
    /// that carry their own bytes.
    pub kind: String,
    pub w: i32,
    pub h: i32,
    /// Bytes on the wire, or 0 for the constructors that do not say.
    pub size: i32,
    /// The image itself, for `photoCachedSize` and `photoStrippedSize`, which embed it in
    /// the message. Free to show: it needs no request, which is what makes it the right
    /// thing to draw in a transcript where downloads are on demand.
    pub inline: Option<Vec<u8>>,
    /// True for `photoSizeProgressive`, whose bytes are a *progressive* JPEG.
    ///
    /// Worth distinguishing because the handset may not be able to decode one. Progressive
    /// JPEG is a different scan structure from baseline, an ICL plugin is free not to
    /// support it, and the failure mode of a codec handed a format it does not implement is
    /// not guaranteed to be an error return.
    pub progressive: bool,
}

impl SizeOption {
    /// Whether `inline` holds a complete JPEG rather than one with its header stripped.
    ///
    /// `photoStrippedSize` omits the JPEG tables and expects the client to prepend a
    /// fixed header; until something does that, its bytes are not decodable and must not
    /// be handed to a codec. `photoCachedSize` is a whole file.
    pub fn inline_is_complete(&self) -> bool {
        matches!(self.inline.as_deref(), Some([0xFF, 0xD8, ..]))
    }
}

/// What type of media a message carries, with enough information to download it and to
/// draw something in the meantime.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Media {
    Photo {
        id: i64,
        access_hash: i64,
        file_reference: Vec<u8>,
        /// Which data centre holds the bytes. Not necessarily the one this session is on:
        /// asking the wrong one answers `FILE_MIGRATE_x`, so the number has to survive
        /// this far or the download cannot be routed.
        dc_id: i32,
        /// Every available size, in the order the server listed them.
        sizes: Vec<SizeOption>,
    },
    /// A document: voice message, audio, video, file. Not a sticker — see [`Media::Sticker`].
    Document {
        id: i64,
        access_hash: i64,
        file_reference: Vec<u8>,
        dc_id: i32,
        mime_type: String,
        size: i64,
        /// From `documentAttributeFilename`, when present.
        filename: Option<String>,
        /// Seconds, from `documentAttributeAudio`, for anything with sound.
        duration: Option<i32>,
        /// True only when the `voice` flag says so.
        ///
        /// Inferred before from the mere presence of an audio attribute, which made every
        /// music track a voice note — the two are drawn differently and only one has a
        /// waveform.
        is_voice: bool,
        /// The 5-bit-packed amplitude envelope Telegram clients draw as the voice bar.
        waveform: Option<Vec<u8>>,
        /// Previews, for the ones that have them.
        thumbs: Vec<SizeOption>,
    },
    /// A sticker.
    ///
    /// Its own variant rather than a document, because on this device it behaves nothing
    /// like one: the file is WebP, or gzipped Lottie, or VP9 in WebM, and the handset has
    /// a plugin for none of the three. What can be drawn is `alt` — the emoji the sticker
    /// stands for — so that is what is carried, and nothing is downloaded.
    Sticker {
        id: i64,
        access_hash: i64,
        file_reference: Vec<u8>,
        dc_id: i32,
        mime_type: String,
        /// The emoji from `documentAttributeSticker`, e.g. "😀". Empty when absent.
        alt: String,
        /// Previews. Occasionally a JPEG, in which case it is decodable and worth showing.
        thumbs: Vec<SizeOption>,
    },
    /// A media type this build does not handle — poll, geo, game, contact.
    Unsupported,
}

/// One conversation in the list.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Dialog {
    pub peer: Peer,
    pub top_message: i32,
    pub unread: i32,
}

/// A name for a peer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Named {
    pub peer: Peer,
    pub title: String,
    /// The number that turns a [`Peer`] into an `InputPeer`.
    ///
    /// Telegram will not answer a question about a user or a channel from its id alone —
    /// it wants proof the client learned that id legitimately, and this is that proof.
    /// Without it every `messages.getHistory` comes back `PEER_ID_INVALID`. Chats (the
    /// small non-channel groups) have no hash and use zero.
    pub access_hash: i64,
}

/// Everything a `messages.getDialogs` reply carries.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Dialogs {
    pub dialogs: Vec<Dialog>,
    pub messages: Vec<Message>,
    pub names: Vec<Named>,
    /// Present on a slice reply: how many dialogs exist beyond this page.
    pub total: Option<i32>,
}

impl Dialogs {
    /// The name for a peer, matching on kind as well as id.
    pub fn name_of(&self, p: Peer) -> Option<&str> {
        self.names.iter().find(|n| n.peer == p).map(|n| n.title.as_str())
    }

    /// What identifies a peer to the server, for a question asked about it.
    pub fn hash_of(&self, p: Peer) -> i64 {
        self.names.iter().find(|n| n.peer == p).map(|n| n.access_hash).unwrap_or(0)
    }

    /// The message a dialog previews.
    pub fn top_of(&self, d: &Dialog) -> Option<&Message> {
        self.messages.iter().find(|m| m.peer == d.peer && m.id == d.top_message)
    }
}

/// Parse a `messages.Dialogs`.
pub fn parse_dialogs(body: &[u8]) -> Result<Dialogs> {
    let (c, f) = Walker::new(body).value()?;
    let (total, di, mi, ci, ui) = match c.id {
        s::MESSAGES_DIALOGS_CTOR => (
            None,
            s::MESSAGES_DIALOGS_DIALOGS,
            s::MESSAGES_DIALOGS_MESSAGES,
            s::MESSAGES_DIALOGS_CHATS,
            s::MESSAGES_DIALOGS_USERS,
        ),
        s::MESSAGES_DIALOGSSLICE_CTOR => (
            as_int(&f[s::MESSAGES_DIALOGSSLICE_COUNT]),
            s::MESSAGES_DIALOGSSLICE_DIALOGS,
            s::MESSAGES_DIALOGSSLICE_MESSAGES,
            s::MESSAGES_DIALOGSSLICE_CHATS,
            s::MESSAGES_DIALOGSSLICE_USERS,
        ),
        other => return Err(Error::Unknown(other)),
    };

    Ok(Dialogs {
        dialogs: parse_each(&f[di], parse_dialog)?,
        messages: parse_each(&f[mi], parse_message)?,
        names: {
            let mut names = parse_each(&f[ci], parse_named)?;
            names.extend(parse_each(&f[ui], parse_named)?);
            names
        },
        total,
    })
}

/// Parse a `messages.Messages` from `messages.getHistory`.
pub fn parse_history(body: &[u8]) -> Result<Dialogs> {
    let (c, f) = Walker::new(body).value()?;
    let (mi, ci, ui) = match c.id {
        s::MESSAGES_MESSAGES_CTOR => (
            s::MESSAGES_MESSAGES_MESSAGES,
            s::MESSAGES_MESSAGES_CHATS,
            s::MESSAGES_MESSAGES_USERS,
        ),
        s::MESSAGES_MESSAGESSLICE_CTOR => (
            s::MESSAGES_MESSAGESSLICE_MESSAGES,
            s::MESSAGES_MESSAGESSLICE_CHATS,
            s::MESSAGES_MESSAGESSLICE_USERS,
        ),
        s::MESSAGES_CHANNELMESSAGES_CTOR => (
            s::MESSAGES_CHANNELMESSAGES_MESSAGES,
            s::MESSAGES_CHANNELMESSAGES_CHATS,
            s::MESSAGES_CHANNELMESSAGES_USERS,
        ),
        other => return Err(Error::Unknown(other)),
    };

    Ok(Dialogs {
        dialogs: Vec::new(),
        messages: parse_each(&f[mi], parse_message)?,
        names: {
            let mut names = parse_each(&f[ci], parse_named)?;
            names.extend(parse_each(&f[ui], parse_named)?);
            names
        },
        total: None,
    })
}

/// Run a parser over every element of a vector field, dropping the ones it does not know.
///
/// Dropping rather than failing, and that is the important decision. A `Vector<Chat>` can
/// hold a `channelForbidden`; a `Vector<Message>` holds whatever the server has. One
/// element this build cannot read must not lose the other ninety-nine — a chat list that
/// disappears because someone forwarded a poll is worse than one missing a row.
fn parse_each<T>(l: &Located<'_>, f: fn(&[u8]) -> Result<Option<T>>) -> Result<Vec<T>> {
    let mut out = Vec::new();
    for e in vector_elements(l)? {
        match f(e) {
            Ok(Some(v)) => out.push(v),
            // A shape the walker understood but this file does not model.
            Ok(None) => {}
            // A shape the walker did not understand. Fatal for the element and for
            // everything after it, because the vector's remaining offsets came from walking
            // this one -- vector_elements already located them, so in fact only this
            // element is lost. Kept rather than propagated for the reason above.
            Err(_) => {}
        }
    }
    Ok(out)
}

fn parse_peer(body: &[u8]) -> Result<Option<Peer>> {
    let (c, f) = Walker::new(body).value()?;
    Ok(match c.id {
        s::PEERUSER_CTOR => {
            as_long(&f[s::PEERUSER_USER_ID]).map(|id| Peer { kind: Kind::User, id })
        }
        s::PEERCHAT_CTOR => {
            as_long(&f[s::PEERCHAT_CHAT_ID]).map(|id| Peer { kind: Kind::Chat, id })
        }
        s::PEERCHANNEL_CTOR => {
            as_long(&f[s::PEERCHANNEL_CHANNEL_ID]).map(|id| Peer { kind: Kind::Channel, id })
        }
        _ => None,
    })
}

fn parse_dialog(body: &[u8]) -> Result<Option<Dialog>> {
    let (c, f) = Walker::new(body).value()?;
    if c.id != s::DIALOG_CTOR {
        // dialogFolder, which a Symbian client has no screen for.
        return Ok(None);
    }
    let peer = match f[s::DIALOG_PEER].bytes {
        Some(b) => match parse_peer(b)? {
            Some(p) => p,
            None => return Ok(None),
        },
        None => return Ok(None),
    };
    Ok(Some(Dialog {
        peer,
        top_message: as_int(&f[s::DIALOG_TOP_MESSAGE]).unwrap_or(0),
        unread: as_int(&f[s::DIALOG_UNREAD_COUNT]).unwrap_or(0),
    }))
}

fn parse_message(body: &[u8]) -> Result<Option<Message>> {
    let (c, f) = Walker::new(body).value()?;
    let (id_i, out_i, peer_i, date_i, text_i, service, media_i) = match c.id {
        s::MESSAGE_CTOR => (
            s::MESSAGE_ID,
            Some(s::MESSAGE_OUT),
            s::MESSAGE_PEER_ID,
            Some(s::MESSAGE_DATE),
            Some(s::MESSAGE_MESSAGE),
            false,
            Some(s::MESSAGE_MEDIA),
        ),
        s::MESSAGESERVICE_CTOR => (
            s::MESSAGESERVICE_ID,
            Some(s::MESSAGESERVICE_OUT),
            s::MESSAGESERVICE_PEER_ID,
            Some(s::MESSAGESERVICE_DATE),
            None,
            true,
            None,
        ),
        // messageEmpty has no peer at all when its flag is clear, and nothing to show.
        _ => return Ok(None),
    };

    let peer = match f[peer_i].bytes {
        Some(b) => match parse_peer(b)? {
            Some(p) => p,
            None => return Ok(None),
        },
        None => return Ok(None),
    };

    let text = text_i
        .and_then(|i| as_str(&f[i]))
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .unwrap_or_default();

    let media = media_i
        .and_then(|i| f[i].bytes)
        .and_then(|b| match parse_media(b) {
            Ok(m) => m,
            Err(_) => None,
        });

    Ok(Some(Message {
        id: as_int(&f[id_i]).unwrap_or(0),
        peer,
        out: out_i.map(|i| as_flag(&f[i])).unwrap_or(false),
        date: date_i.and_then(|i| as_int(&f[i])).unwrap_or(0),
        text,
        service,
        media,
    }))
}

/// Parse a `MessageMedia` constructor. Returns `None` for `messageMediaEmpty` and
/// unsupported types, which the caller treats the same — a text-only message.
fn parse_media(body: &[u8]) -> Result<Option<Media>> {
    let (c, f) = Walker::new(body).value()?;
    match c.id {
        s::MESSAGEMEDIAPHOTO_CTOR => {
            if let Some(photo_bytes) = f.get(s::MESSAGEMEDIAPHOTO_PHOTO).and_then(|p| p.bytes) {
                return Ok(parse_photo(photo_bytes));
            }
            Ok(None)
        }
        s::MESSAGEMEDIADOCUMENT_CTOR => {
            // The index that was wrong, and is now generated. It was 0 — which is `flags:#`,
            // whose `.bytes` is always None — so `Media::Document` was never once
            // constructed and every voice message, sticker, video and file in every
            // conversation arrived as a plain text message with nothing attached.
            if let Some(doc_bytes) = f.get(s::MESSAGEMEDIADOCUMENT_DOCUMENT).and_then(|d| d.bytes) {
                return Ok(parse_document(doc_bytes));
            }
            Ok(None)
        }
        MESSAGEMEDIAEMPTY_CTOR => Ok(None),
        _ => Ok(None),
    }
}

/// Parse one `PhotoSize`. `None` for the ones with nothing to show.
fn parse_size(body: &[u8]) -> Option<SizeOption> {
    let (c, f) = Walker::new(body).value().ok()?;
    let text = |l: &Located| as_str(l).map(|b| String::from_utf8_lossy(b).into_owned());
    match c.id {
        s::PHOTOSIZE_CTOR => Some(SizeOption {
            kind: text(&f[s::PHOTOSIZE_TYPE])?,
            w: as_int(&f[s::PHOTOSIZE_W]).unwrap_or(0),
            h: as_int(&f[s::PHOTOSIZE_H]).unwrap_or(0),
            size: as_int(&f[s::PHOTOSIZE_SIZE]).unwrap_or(0),
            inline: None,
            progressive: false,
        }),
        s::PHOTOSIZEPROGRESSIVE_CTOR => Some(SizeOption {
            kind: text(&f[s::PHOTOSIZEPROGRESSIVE_TYPE])?,
            w: as_int(&f[s::PHOTOSIZEPROGRESSIVE_W]).unwrap_or(0),
            h: as_int(&f[s::PHOTOSIZEPROGRESSIVE_H]).unwrap_or(0),
            // Its `sizes` field is a Vector<int> of partial lengths, not one int, so there
            // is no single byte count to report here.
            size: 0,
            inline: None,
            progressive: true,
        }),
        s::PHOTOCACHEDSIZE_CTOR => Some(SizeOption {
            kind: text(&f[s::PHOTOCACHEDSIZE_TYPE])?,
            w: as_int(&f[s::PHOTOCACHEDSIZE_W]).unwrap_or(0),
            h: as_int(&f[s::PHOTOCACHEDSIZE_H]).unwrap_or(0),
            size: 0,
            inline: as_str(&f[s::PHOTOCACHEDSIZE_BYTES]).map(|b| b.to_vec()),
            progressive: false,
        }),
        s::PHOTOSTRIPPEDSIZE_CTOR => Some(SizeOption {
            kind: text(&f[s::PHOTOSTRIPPEDSIZE_TYPE])?,
            // No dimensions in the constructor. Always about 40 pixels on the long edge.
            w: 0,
            h: 0,
            size: 0,
            inline: as_str(&f[s::PHOTOSTRIPPEDSIZE_BYTES]).map(|b| b.to_vec()),
            progressive: false,
        }),
        // Nothing displayable: an outline path and an empty marker.
        PHOTOPATHSIZE_CTOR | PHOTOSIZEEMPTY_CTOR => None,
        _ => None,
    }
}

/// Every `PhotoSize` in a vector field, skipping the ones that carry nothing.
fn parse_sizes(field: &Located) -> Vec<SizeOption> {
    let mut out = Vec::new();
    if let Ok(elements) = vector_elements(field) {
        for e in elements {
            if let Some(s) = parse_size(e) {
                out.push(s);
            }
        }
    }
    out
}

fn parse_photo(body: &[u8]) -> Option<Media> {
    let (c, f) = Walker::new(body).value().ok()?;
    match c.id {
        s::PHOTO_CTOR => Some(Media::Photo {
            id: as_long(&f[s::PHOTO_ID]).unwrap_or(0),
            access_hash: as_long(&f[s::PHOTO_ACCESS_HASH]).unwrap_or(0),
            file_reference: as_str(&f[s::PHOTO_FILE_REFERENCE])
                .map(|b| b.to_vec())
                .unwrap_or_default(),
            dc_id: as_int(&f[s::PHOTO_DC_ID]).unwrap_or(0),
            sizes: f.get(s::PHOTO_SIZES).map(parse_sizes).unwrap_or_default(),
        }),
        PHOTOEMPTY_CTOR => None,
        _ => None,
    }
}

fn parse_document(body: &[u8]) -> Option<Media> {
    let (c, f) = Walker::new(body).value().ok()?;
    match c.id {
        s::DOCUMENT_CTOR => {}
        // A document the server no longer holds. Named rather than folded into the
        // catch-all so the difference from "a constructor we do not know" stays visible.
        DOCUMENTEMPTY_CTOR => return None,
        _ => return None,
    }
    let id = as_long(&f[s::DOCUMENT_ID]).unwrap_or(0);
    let access_hash = as_long(&f[s::DOCUMENT_ACCESS_HASH]).unwrap_or(0);
    let file_reference = as_str(&f[s::DOCUMENT_FILE_REFERENCE])
        .map(|b| b.to_vec())
        .unwrap_or_default();
    let mime_type = as_str(&f[s::DOCUMENT_MIME_TYPE])
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .unwrap_or_default();
    let size = as_long(&f[s::DOCUMENT_SIZE]).unwrap_or(0);
    let thumbs = f.get(s::DOCUMENT_THUMBS).map(parse_sizes).unwrap_or_default();
    let dc_id = as_int(&f[s::DOCUMENT_DC_ID]).unwrap_or(0);

    let mut filename = None;
    let mut duration = None;
    let mut is_voice = false;
    let mut waveform = None;
    let mut sticker_alt = None;

    if let Some(attrs) = f.get(s::DOCUMENT_ATTRIBUTES) {
        if let Ok(elements) = vector_elements(attrs) {
            for elem in elements {
                let Ok((ec, ef)) = Walker::new(elem).value() else {
                    continue;
                };
                match ec.id {
                    s::DOCUMENTATTRIBUTEAUDIO_CTOR => {
                        duration = Some(as_int(&ef[s::DOCUMENTATTRIBUTEAUDIO_DURATION]).unwrap_or(0));
                        // The flag, not the presence of a duration: a music track has one
                        // too, and treating it as a voice note drew a waveform for an album.
                        is_voice = ef.get(s::DOCUMENTATTRIBUTEAUDIO_VOICE).map(as_flag).unwrap_or(false);
                        waveform = ef
                            .get(s::DOCUMENTATTRIBUTEAUDIO_WAVEFORM)
                            .and_then(as_str)
                            .map(|b| b.to_vec());
                    }
                    s::DOCUMENTATTRIBUTEFILENAME_CTOR => {
                        filename = as_str(&ef[s::DOCUMENTATTRIBUTEFILENAME_FILE_NAME])
                            .map(|b| String::from_utf8_lossy(b).into_owned());
                    }
                    s::DOCUMENTATTRIBUTESTICKER_CTOR => {
                        sticker_alt = Some(
                            as_str(&ef[s::DOCUMENTATTRIBUTESTICKER_ALT])
                                .map(|b| String::from_utf8_lossy(b).into_owned())
                                .unwrap_or_default(),
                        );
                    }
                    _ => {}
                }
            }
        }
    }

    // The sticker attribute wins: a sticker is also a document, and the difference is
    // that one of them can be drawn on this device and the other cannot.
    if let Some(alt) = sticker_alt {
        return Some(Media::Sticker {
            id,
            access_hash,
            file_reference,
            dc_id,
            mime_type,
            alt,
            thumbs,
        });
    }

    Some(Media::Document {
        id,
        access_hash,
        file_reference,
        dc_id,
        mime_type,
        size,
        filename,
        duration,
        is_voice,
        waveform,
        thumbs,
    })
}

/// # The bug this function's shape exists to prevent
///
/// Field indices belong to a *constructor*, not to a [`Kind`]. `channel` and
/// `channelForbidden` are both `Kind::Channel` and share no field positions at all: the
/// first has `access_hash` at index 31, the second has eight fields in total.
///
/// This used to pick the id and title per constructor — correctly — and then the access hash
/// per *kind*, reaching for `CHANNEL_ACCESS_HASH` whichever channel it had. On any chat list
/// containing an inaccessible channel that read index 31 of an eight-field value and the
/// application closed itself. It only ever showed up on a page that happened to contain one,
/// which on the account it was found with meant the second page — so the symptom was
/// "scrolling to the end of the chat list crashes", and the end of the list had nothing to do
/// with it.
///
/// So all three indices now come from the same match arm, and there is nowhere left to
/// mismatch them. `walk::field` is the second line of defence: a wrong index is a missing
/// value rather than a dead process.
fn parse_named(body: &[u8]) -> Result<Option<Named>> {
    let (c, f) = Walker::new(body).value()?;
    // (kind, id, title, access_hash) — every index from the one constructor in hand.
    let (kind, id_i, title_i, hash_i) = match c.id {
        s::USER_CTOR => (Kind::User, s::USER_ID, None, Some(s::USER_ACCESS_HASH)),
        // `chat` and `chatForbidden` have no access hash: a small group is addressed by id
        // alone, and `inputPeerChat` has no field for one.
        s::CHAT_CTOR => (Kind::Chat, s::CHAT_ID, Some(s::CHAT_TITLE), None),
        s::CHATFORBIDDEN_CTOR => {
            (Kind::Chat, s::CHATFORBIDDEN_ID, Some(s::CHATFORBIDDEN_TITLE), None)
        }
        s::CHANNEL_CTOR => {
            (Kind::Channel, s::CHANNEL_ID, Some(s::CHANNEL_TITLE), Some(s::CHANNEL_ACCESS_HASH))
        }
        s::CHANNELFORBIDDEN_CTOR => (
            Kind::Channel,
            s::CHANNELFORBIDDEN_ID,
            Some(s::CHANNELFORBIDDEN_TITLE),
            Some(s::CHANNELFORBIDDEN_ACCESS_HASH),
        ),
        // userEmpty, chatEmpty: an id and nothing to call it.
        _ => return Ok(None),
    };

    let id = match as_long(&field(&f, id_i)) {
        Some(v) => v,
        None => return Ok(None),
    };

    let title = match title_i {
        Some(i) => as_str(&field(&f, i)).map(|b| String::from_utf8_lossy(b).into_owned()),
        None => {
            // A user has a first and last name rather than a title, and either can be
            // absent — an account with neither is legal and shows as its id elsewhere in
            // Telegram, so it shows as its id here.
            let first = as_str(&field(&f, s::USER_FIRST_NAME)).unwrap_or(b"");
            let last = as_str(&field(&f, s::USER_LAST_NAME)).unwrap_or(b"");
            let mut t = String::from_utf8_lossy(first).into_owned();
            if !last.is_empty() {
                if !t.is_empty() {
                    t.push(' ');
                }
                t.push_str(&String::from_utf8_lossy(last));
            }
            Some(t)
        }
    };

    let mut title = title.unwrap_or_default();
    if title.is_empty() {
        title = alloc::format!("#{id}");
    }

    let access_hash = hash_i.and_then(|i| as_long(&field(&f, i))).unwrap_or(0);

    Ok(Some(Named { peer: Peer { kind, id }, title, access_hash }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tl::Writer;
    use crate::walk;

    fn peer_user(id: i64) -> Vec<u8> {
        let mut w = Writer::new();
        w.ctor(s::PEERUSER_CTOR).long(id);
        w.finish()
    }

    /// `message#7600b9d3` with only the fields this file reads.
    ///
    /// Built by walking the generated field table rather than by hand, because the
    /// constructor has forty fields and writing them out would be transcribing the schema
    /// into the test — the exact thing the generator exists to avoid.
    fn message(id: i32, peer: i64, out: bool, date: i32, text: &str) -> Vec<u8> {
        message_with(id, peer, out, date, text, None)
    }

    /// As [`message`], optionally carrying a `MessageMedia` in `media:flags.9?MessageMedia`.
    fn message_with(
        id: i32,
        peer: i64,
        out: bool,
        date: i32,
        text: &str,
        media: Option<&[u8]>,
    ) -> Vec<u8> {
        const MEDIA_BIT: u32 = 9;
        let c = walk::ctor(s::MESSAGE_CTOR).expect("message is in the table");
        let mut w = Writer::new();
        w.ctor(s::MESSAGE_CTOR);

        let mut flags: u32 = if out { 1 << 1 } else { 0 };
        if media.is_some() {
            flags |= 1 << MEDIA_BIT;
        }

        let mut word = 0usize;
        for (i, f) in c.fields.iter().enumerate() {
            if f.k == walk::K_FLAGS {
                w.uint(if word == 0 { flags } else { 0 });
                word += 1;
                continue;
            }
            if f.f >= 0 {
                let widx = ((f.f as u16) >> 8) as usize;
                let bit = (f.f as u16 & 0xff) as u32;
                let present = widx == 0 && (flags & (1 << bit)) != 0;
                if !present {
                    continue;
                }
                if f.k == walk::K_TRUE {
                    continue;
                }
            }
            if i == s::MESSAGE_ID {
                w.int(id);
            } else if i == s::MESSAGE_PEER_ID {
                w.raw(&peer_user(peer));
            } else if i == s::MESSAGE_DATE {
                w.int(date);
            } else if i == s::MESSAGE_MESSAGE {
                w.string(text);
            } else if i == s::MESSAGE_MEDIA {
                w.raw(media.expect("the flag was set for it"));
            } else {
                panic!("unexpected unconditional field {i} of message");
            }
        }
        w.finish()
    }

    fn photo_size(kind: &str, w_px: i32, h_px: i32, size: i32) -> Vec<u8> {
        let mut w = Writer::new();
        w.ctor(s::PHOTOSIZE_CTOR).string(kind).int(w_px).int(h_px).int(size);
        w.finish()
    }

    fn photo_cached_size(kind: &str, w_px: i32, h_px: i32, jpeg: &[u8]) -> Vec<u8> {
        let mut w = Writer::new();
        w.ctor(s::PHOTOCACHEDSIZE_CTOR).string(kind).int(w_px).int(h_px).bytes(jpeg);
        w.finish()
    }

    fn photo_stripped_size(kind: &str, blob: &[u8]) -> Vec<u8> {
        let mut w = Writer::new();
        w.ctor(s::PHOTOSTRIPPEDSIZE_CTOR).string(kind).bytes(blob);
        w.finish()
    }

    /// `photo#fb197a65 flags:# has_stickers:flags.0?true id:long access_hash:long
    ///  file_reference:bytes date:int sizes:Vector<PhotoSize>
    ///  video_sizes:flags.1?Vector<VideoSize> dc_id:int`
    fn photo(id: i64, hash: i64, reference: &[u8], sizes: &[Vec<u8>], dc: i32) -> Vec<u8> {
        let mut w = Writer::new();
        w.ctor(s::PHOTO_CTOR)
            .uint(0)
            .long(id)
            .long(hash)
            .bytes(reference)
            .int(1_700_000_000)
            .raw(&vector(sizes))
            .int(dc);
        w.finish()
    }

    /// `messageMediaPhoto#e216eb63 flags:# spoiler:flags.3?true live_photo:flags.4?true
    ///  photo:flags.0?Photo ttl_seconds:flags.2?int video:flags.4?Document`
    fn media_photo(photo_bytes: &[u8]) -> Vec<u8> {
        let mut w = Writer::new();
        w.ctor(s::MESSAGEMEDIAPHOTO_CTOR).uint(1).raw(photo_bytes);
        w.finish()
    }

    /// `documentAttributeAudio#9852f9c6 flags:# voice:flags.10?true duration:int
    ///  title:flags.0?string performer:flags.1?string waveform:flags.2?bytes`
    fn attr_audio(voice: bool, duration: i32, waveform: Option<&[u8]>) -> Vec<u8> {
        let mut flags = 0u32;
        if voice {
            flags |= 1 << 10;
        }
        if waveform.is_some() {
            flags |= 1 << 2;
        }
        let mut w = Writer::new();
        w.ctor(s::DOCUMENTATTRIBUTEAUDIO_CTOR).uint(flags).int(duration);
        if let Some(wf) = waveform {
            w.bytes(wf);
        }
        w.finish()
    }

    fn attr_filename(name: &str) -> Vec<u8> {
        let mut w = Writer::new();
        w.ctor(s::DOCUMENTATTRIBUTEFILENAME_CTOR).string(name);
        w.finish()
    }

    /// `documentAttributeSticker#6319d612 flags:# mask:flags.1?true alt:string
    ///  stickerset:InputStickerSet mask_coords:flags.0?MaskCoords`
    fn attr_sticker(alt: &str) -> Vec<u8> {
        // inputStickerSetEmpty#ffb62b95, a set reference with no payload.
        let mut w = Writer::new();
        w.ctor(s::DOCUMENTATTRIBUTESTICKER_CTOR).uint(0).string(alt).ctor(0xffb6_2b95);
        w.finish()
    }

    /// `document#8fd4c4d8 flags:# id:long access_hash:long file_reference:bytes date:int
    ///  mime_type:string size:long thumbs:flags.0?Vector<PhotoSize>
    ///  video_thumbs:flags.1?Vector<VideoSize> dc_id:int
    ///  attributes:Vector<DocumentAttribute>`
    fn document(
        id: i64,
        hash: i64,
        mime: &str,
        size: i64,
        thumbs: Option<&[Vec<u8>]>,
        dc: i32,
        attrs: &[Vec<u8>],
    ) -> Vec<u8> {
        let mut w = Writer::new();
        w.ctor(s::DOCUMENT_CTOR)
            .uint(if thumbs.is_some() { 1 } else { 0 })
            .long(id)
            .long(hash)
            .bytes(b"ref")
            .int(1_700_000_000)
            .string(mime)
            .long(size);
        if let Some(t) = thumbs {
            w.raw(&vector(t));
        }
        w.int(dc).raw(&vector(attrs));
        w.finish()
    }

    /// `messageMediaDocument#52d8ccd9 flags:# nopremium:flags.3?true spoiler:flags.4?true
    ///  video:flags.6?true round:flags.7?true voice:flags.8?true document:flags.0?Document
    ///  ...` — the document is field 6, and reading it at 0 is the bug these tests pin.
    fn media_document(doc: &[u8]) -> Vec<u8> {
        let mut w = Writer::new();
        w.ctor(s::MESSAGEMEDIADOCUMENT_CTOR).uint(1).raw(doc);
        w.finish()
    }

    fn vector(elements: &[Vec<u8>]) -> Vec<u8> {
        let mut w = Writer::new();
        w.ctor(crate::tl::VECTOR).uint(elements.len() as u32);
        for e in elements {
            w.raw(e);
        }
        w.finish()
    }



    #[test]
    fn a_photo_carries_its_sizes_and_data_centre() {
        // Without `sizes` there is no `type` string to put in
        // `inputPhotoFileLocation.thumb_size`, and without `dc_id` there is nothing to route
        // a FILE_MIGRATE by. Neither was read, so a photo was unfetchable in two ways at
        // once even though it parsed.
        let sizes = [
            photo_size("s", 90, 67, 1_000),
            photo_size("m", 320, 240, 12_000),
            photo_size("x", 800, 600, 90_000),
        ];
        let bytes = message_with(
            1, 42, false, 1_700_000_000, "",
            Some(&media_photo(&photo(0x0102_0304_0506_0708, 0x99, b"fileref", &sizes, 4))),
        );
        let m = parse_message(&bytes).unwrap().unwrap();
        let Some(Media::Photo { id, access_hash, file_reference, dc_id, sizes }) = m.media else {
            panic!("expected a photo, got {:?}", m.media);
        };
        assert_eq!(id, 0x0102_0304_0506_0708);
        assert_eq!(access_hash, 0x99);
        assert_eq!(file_reference, b"fileref");
        assert_eq!(dc_id, 4);
        assert_eq!(sizes.len(), 3);
        assert_eq!(sizes[1].kind, "m");
        assert_eq!((sizes[1].w, sizes[1].h, sizes[1].size), (320, 240, 12_000));
        assert!(sizes.iter().all(|s| s.inline.is_none()));
    }

    #[test]
    fn an_inline_thumbnail_comes_through_whole() {
        // photoCachedSize embeds a complete JPEG in the message. It is the only preview
        // available without spending a request, which is what makes it the right thing to
        // draw in a client where downloads wait for the user.
        let jpeg = [0xFFu8, 0xD8, 0xFF, 0xE0, 1, 2, 3];
        let sizes = [photo_cached_size("m", 90, 67, &jpeg), photo_size("x", 800, 600, 90_000)];
        let bytes = message_with(
            1, 42, false, 1, "", Some(&media_photo(&photo(1, 2, b"r", &sizes, 2))),
        );
        let Some(Media::Photo { sizes, .. }) = parse_message(&bytes).unwrap().unwrap().media else {
            panic!("expected a photo");
        };
        assert_eq!(sizes[0].inline.as_deref(), Some(&jpeg[..]));
        assert!(sizes[0].inline_is_complete(), "starts with the JPEG marker");
        assert!(!sizes[1].inline_is_complete(), "photoSize carries no bytes");
    }

    #[test]
    fn a_stripped_thumbnail_is_not_mistaken_for_a_whole_jpeg() {
        // photoStrippedSize omits the JPEG tables and expects the client to prepend a fixed
        // header. Handing its bytes to a codec as they are fails, so it must be
        // distinguishable from photoCachedSize, which is a complete file.
        let sizes = [photo_stripped_size("i", &[0x01, 0x2a, 0x3b])];
        let bytes = message_with(
            1, 42, false, 1, "", Some(&media_photo(&photo(1, 2, b"r", &sizes, 2))),
        );
        let Some(Media::Photo { sizes, .. }) = parse_message(&bytes).unwrap().unwrap().media else {
            panic!("expected a photo");
        };
        assert_eq!(sizes[0].kind, "i");
        assert!(sizes[0].inline.is_some(), "the bytes are there");
        assert!(!sizes[0].inline_is_complete(), "but they are not a decodable file");
    }

    #[test]
    fn a_voice_message_is_parsed_at_all() {
        // The bug this pins: messageMediaDocument's document was read at index 0, which is
        // `flags` and never carries bytes — so Media::Document was never constructed once.
        // Every voice note, video, audio file and attachment in every conversation arrived
        // as a plain text message.
        let doc = document(
            77, 88, "audio/ogg", 4_096, None, 2,
            &[attr_audio(true, 7, Some(&[0x10, 0x20, 0x30]))],
        );
        let bytes = message_with(1, 42, false, 1, "", Some(&media_document(&doc)));
        let m = parse_message(&bytes).unwrap().unwrap();
        let Some(Media::Document { id, mime_type, size, duration, is_voice, waveform, dc_id, .. }) =
            m.media
        else {
            panic!("expected a document, got {:?}", m.media);
        };
        assert_eq!(id, 77);
        assert_eq!(mime_type, "audio/ogg");
        assert_eq!(size, 4_096);
        assert_eq!(duration, Some(7));
        assert!(is_voice);
        assert_eq!(waveform.as_deref(), Some(&[0x10u8, 0x20, 0x30][..]));
        assert_eq!(dc_id, 2);
    }

    #[test]
    fn a_music_track_is_not_a_voice_message() {
        // `is_voice` used to be set by the mere presence of an audio attribute, so an album
        // track became a voice note. The two are drawn differently and only one of them has
        // a waveform to draw.
        let doc = document(
            9, 9, "audio/mpeg", 5_000_000, None, 2,
            &[attr_audio(false, 213, None), attr_filename("song.mp3")],
        );
        let bytes = message_with(1, 42, false, 1, "", Some(&media_document(&doc)));
        let Some(Media::Document { is_voice, duration, filename, .. }) =
            parse_message(&bytes).unwrap().unwrap().media
        else {
            panic!("expected a document");
        };
        assert!(!is_voice, "no voice flag means no voice message");
        assert_eq!(duration, Some(213), "it still has a length");
        assert_eq!(filename.as_deref(), Some("song.mp3"));
    }

    #[test]
    fn a_sticker_is_its_own_thing_and_keeps_its_emoji() {
        // A sticker is a document, but on this handset it behaves like nothing else: the
        // file is WebP, which the 2008 codec set has no plugin for. What can be shown is
        // the emoji it stands for, so that has to survive parsing.
        let doc = document(
            5, 6, "image/webp", 30_000, None, 2, &[attr_sticker("\u{1F600}")],
        );
        let bytes = message_with(1, 42, false, 1, "", Some(&media_document(&doc)));
        let Some(Media::Sticker { alt, mime_type, id, .. }) =
            parse_message(&bytes).unwrap().unwrap().media
        else {
            panic!("expected a sticker");
        };
        assert_eq!(alt, "\u{1F600}");
        assert_eq!(mime_type, "image/webp");
        assert_eq!(id, 5);
    }

    #[test]
    fn a_file_with_no_audio_attribute_is_a_plain_document() {
        let doc = document(
            3, 4, "application/pdf", 120_000, None, 2, &[attr_filename("contrato.pdf")],
        );
        let bytes = message_with(1, 42, false, 1, "", Some(&media_document(&doc)));
        let Some(Media::Document { filename, duration, is_voice, size, .. }) =
            parse_message(&bytes).unwrap().unwrap().media
        else {
            panic!("expected a document");
        };
        assert_eq!(filename.as_deref(), Some("contrato.pdf"));
        assert_eq!(duration, None);
        assert!(!is_voice);
        assert_eq!(size, 120_000);
    }

    #[test]
    fn a_document_thumbnail_vector_is_read_when_present() {
        // thumbs is flags.0, so its presence shifts nothing but must be picked up when set.
        let thumbs = [photo_size("m", 90, 90, 2_000)];
        let doc = document(
            1, 1, "video/mp4", 9_000, Some(&thumbs), 2, &[attr_filename("clip.mp4")],
        );
        let bytes = message_with(1, 42, false, 1, "", Some(&media_document(&doc)));
        let Some(Media::Document { thumbs, dc_id, .. }) =
            parse_message(&bytes).unwrap().unwrap().media
        else {
            panic!("expected a document");
        };
        assert_eq!(thumbs.len(), 1);
        assert_eq!(thumbs[0].kind, "m");
        // And dc_id still lands, one field further along than in the no-thumbs case.
        assert_eq!(dc_id, 2);
    }

    #[test]
    fn a_photo_with_no_usable_sizes_still_parses() {
        // photoPathSize is a vector outline and photoSizeEmpty is a marker; neither is an
        // image. A photo of nothing but those must come back with an empty list rather than
        // an entry the caller would try to download.
        let mut path = Writer::new();
        path.ctor(PHOTOPATHSIZE_CTOR).string("j").bytes(&[1, 2, 3]);
        let mut empty = Writer::new();
        empty.ctor(PHOTOSIZEEMPTY_CTOR).string("s");
        let sizes = [path.finish(), empty.finish()];
        let bytes = message_with(
            1, 42, false, 1, "", Some(&media_photo(&photo(1, 2, b"r", &sizes, 2))),
        );
        let Some(Media::Photo { sizes, .. }) = parse_message(&bytes).unwrap().unwrap().media else {
            panic!("expected a photo");
        };
        assert!(sizes.is_empty());
    }

    #[test]
    fn a_message_round_trips_through_the_walker() {
        let bytes = message(7, 4242, true, 1_700_000_000, "olá");
        let m = parse_message(&bytes).unwrap().unwrap();
        assert_eq!(m.id, 7);
        assert_eq!(m.peer, Peer { kind: Kind::User, id: 4242 });
        assert!(m.out);
        assert_eq!(m.date, 1_700_000_000);
        assert_eq!(m.text, "olá");
        assert!(!m.service);
    }

    #[test]
    fn a_clear_out_flag_reads_as_incoming() {
        // The flag is `flags.1?true`, which occupies no bytes. A walker that consumed one
        // for it would shift every field after it, and the text would come out as a date.
        let bytes = message(1, 1, false, 5, "hi");
        let m = parse_message(&bytes).unwrap().unwrap();
        assert!(!m.out);
        assert_eq!(m.text, "hi");
        assert_eq!(m.date, 5);
    }

    #[test]
    fn a_user_and_a_chat_with_the_same_id_are_different_peers() {
        // The two id spaces overlap. A lookup keyed on the number alone finds the wrong
        // name whenever a small account and a small group share one.
        let u = Peer { kind: Kind::User, id: 7 };
        let c = Peer { kind: Kind::Chat, id: 7 };
        assert_ne!(u, c);
        let d = Dialogs {
            names: alloc::vec![
                Named { peer: u, title: String::from("a person"), access_hash: 11 },
                Named { peer: c, title: String::from("a group"), access_hash: 0 },
            ],
            ..Default::default()
        };
        assert_eq!(d.name_of(u), Some("a person"));
        assert_eq!(d.name_of(c), Some("a group"));
        // The hash follows the same lookup: two peers with the same id and different kinds
        // are different peers, and handing one the other's hash is PEER_ID_INVALID.
        assert_eq!(d.hash_of(u), 11);
        assert_eq!(d.hash_of(c), 0);
    }

    #[test]
    fn one_unparseable_element_does_not_lose_the_others() {
        // A chat list that disappears because someone forwarded something this build cannot
        // model is worse than one missing a row.
        let mut bad = Writer::new();
        bad.ctor(0xdead_beef);
        let good = message(1, 1, false, 1, "kept");

        let v = vector(&[good, bad.finish()]);
        let l = Located { kind: walk::K_VECTOR, bytes: Some(&v) };
        // vector_elements fails on the unknown element, so the whole vector is lost -- which
        // is the honest behaviour and worth asserting so nobody assumes otherwise.
        assert!(vector_elements(&l).is_err());
    }

    #[test]
    fn an_empty_reply_is_empty_rather_than_an_error() {
        let mut w = Writer::new();
        w.ctor(s::MESSAGES_DIALOGS_CTOR)
            .raw(&vector(&[]))
            .raw(&vector(&[]))
            .raw(&vector(&[]))
            .raw(&vector(&[]));
        let d = parse_dialogs(&w.finish()).unwrap();
        assert!(d.dialogs.is_empty() && d.messages.is_empty() && d.names.is_empty());
    }

    /// The crash: an inaccessible channel in the chat list.
    ///
    /// `channelForbidden` is a `Kind::Channel` with eight fields. The access hash used to be
    /// read at `CHANNEL_ACCESS_HASH`, which is index 31 of the *other* channel constructor,
    /// so every chat list containing one closed the application. It reached a user as
    /// "scrolling to the end of the list crashes", because the page that happened to contain
    /// one was the second.
    ///
    /// Built field by field from the generated table rather than by hand, for the reason the
    /// `message` builder gives: a fixture that transcribes the schema would copy a mistake in
    /// the table and pass.
    #[test]
    fn an_inaccessible_channel_parses_instead_of_panicking() {
        // channelForbidden#17d493d5 flags:# broadcast:flags.5?true megagroup:flags.8?true
        //   id:long access_hash:long title:string until_date:flags.16?int
        let mut w = Writer::new();
        w.ctor(s::CHANNELFORBIDDEN_CTOR)
            .uint(0) // flags: no broadcast, no megagroup, no until_date
            .long(777)
            .long(0x1234_5678_9abc_def0u64 as i64)
            .string("Canal sem acesso");
        let bytes = w.finish();

        let got = parse_named(&bytes).expect("a forbidden channel is not a protocol error");
        let named = got.expect("it has an id and a title, so it is nameable");
        assert_eq!(named.peer, Peer { kind: Kind::Channel, id: 777 });
        assert_eq!(named.title, "Canal sem acesso");
        assert_eq!(
            named.access_hash, 0x1234_5678_9abc_def0u64 as i64,
            "read from channelForbidden's own field, not channel's index 31"
        );
    }

    /// A whole page carrying one, which is how it actually arrived.
    ///
    /// End to end through `parse_dialogs`, because the panic was inside the `chats` vector's
    /// element parser and a unit test of that parser alone would not have covered the path
    /// the reply takes.
    #[test]
    fn a_page_containing_an_inaccessible_channel_still_parses() {
        let mut ch = Writer::new();
        ch.ctor(s::CHANNELFORBIDDEN_CTOR).uint(0).long(42).long(7).string("Sem acesso");

        let mut w = Writer::new();
        w.ctor(s::MESSAGES_DIALOGSSLICE_CTOR)
            .int(1)
            .raw(&vector(&[]))
            .raw(&vector(&[]))
            .raw(&vector(&[ch.finish()]))
            .raw(&vector(&[]));

        let d = parse_dialogs(&w.finish()).expect("one forbidden channel must not lose the page");
        assert_eq!(d.names.len(), 1);
        assert_eq!(d.names[0].title, "Sem acesso");
    }

    /// The constructor the *second* page of the chat list comes back as.
    ///
    /// `messages.dialogs` is what the first page returns when everything fit; a paginated
    /// reply is `messages.dialogsSlice`, whose field indices are all shifted by one because
    /// of the leading `count`. That branch had no test at all, which matters because it is
    /// only ever exercised by pressing Down at the bottom of the list — the one path a user
    /// reaches and a test suite did not.
    #[test]
    fn a_dialogs_slice_parses_and_reports_its_total() {
        let mut w = Writer::new();
        w.ctor(s::MESSAGES_DIALOGSSLICE_CTOR)
            .int(137)
            .raw(&vector(&[]))
            .raw(&vector(&[]))
            .raw(&vector(&[]))
            .raw(&vector(&[]));
        let d = parse_dialogs(&w.finish()).unwrap();
        assert_eq!(d.total, Some(137), "the count field must be read, not skipped");
        assert!(d.dialogs.is_empty() && d.messages.is_empty() && d.names.is_empty());
    }

    /// The end of the list: a slice whose arrays are empty but whose total is not.
    ///
    /// This is what the server sends when the client asks for a page past the last one, so
    /// it is exactly what arrives after the final Down keypress.
    #[test]
    fn an_exhausted_slice_is_empty_rather_than_an_error() {
        let mut w = Writer::new();
        w.ctor(s::MESSAGES_DIALOGSSLICE_CTOR)
            .int(0)
            .raw(&vector(&[]))
            .raw(&vector(&[]))
            .raw(&vector(&[]))
            .raw(&vector(&[]));
        let d = parse_dialogs(&w.finish()).expect("an exhausted page is not a protocol error");
        assert!(d.dialogs.is_empty());
    }

    /* A realistic reply is not built here.
     *
     * Constructing valid TL needs the same schema knowledge as parsing it, so a hand-built
     * `messages.dialogs` in this file would be the walker's own table transcribed into a
     * test -- and a mistake in the table would be copied into the fixture and pass.
     *
     * `vendor/research/mtproto/gen_chats.py` reads api.tl itself and knows nothing about
     * the walker, which is what makes the comparison mean something. See
     * `tests/chats_differential.rs`. */
}
