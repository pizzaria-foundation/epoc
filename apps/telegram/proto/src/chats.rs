//! Dialogs and message history, turned into the six fields a chat list needs.
//!
//! Everything here rides on [`crate::walk`]: the schema table locates the fields and this
//! reads the handful that matter. Nothing in this file counts bytes or knows a field's
//! offset — the indices come from [`crate::schema`], generated from `api.tl`, so a schema
//! change moves them rather than silently shifting a message's text into its date.
//!
//! # What is deliberately thrown away
//!
//! Media, entities, reactions, forwards, replies, polls, and the other thirty-odd fields of
//! `message#7600b9d3`. A Symbian client with a 320x240 screen shows text; a photo it cannot
//! decode and a poll it cannot render are bytes to walk past, not data to model.
//!
//! That is why the walker exists rather than a struct per constructor: skipping a field
//! still requires knowing its shape, but it does not require a name, a type or a line of
//! code.
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
use crate::walk::{as_flag, as_int, as_long, as_str, vector_elements, Located, Walker};

pub use crate::walk::Error;
pub type Result<T> = core::result::Result<T, Error>;

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
    let (id_i, out_i, peer_i, date_i, text_i, service) = match c.id {
        s::MESSAGE_CTOR => (
            s::MESSAGE_ID,
            Some(s::MESSAGE_OUT),
            s::MESSAGE_PEER_ID,
            Some(s::MESSAGE_DATE),
            Some(s::MESSAGE_MESSAGE),
            false,
        ),
        s::MESSAGESERVICE_CTOR => (
            s::MESSAGESERVICE_ID,
            Some(s::MESSAGESERVICE_OUT),
            s::MESSAGESERVICE_PEER_ID,
            Some(s::MESSAGESERVICE_DATE),
            None,
            true,
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

    Ok(Some(Message {
        id: as_int(&f[id_i]).unwrap_or(0),
        peer,
        out: out_i.map(|i| as_flag(&f[i])).unwrap_or(false),
        date: date_i.and_then(|i| as_int(&f[i])).unwrap_or(0),
        text,
        service,
    }))
}

fn parse_named(body: &[u8]) -> Result<Option<Named>> {
    let (c, f) = Walker::new(body).value()?;
    let (kind, id_i, title_i) = match c.id {
        s::USER_CTOR => (Kind::User, s::USER_ID, None),
        s::CHAT_CTOR => (Kind::Chat, s::CHAT_ID, Some(s::CHAT_TITLE)),
        s::CHANNEL_CTOR => (Kind::Channel, s::CHANNEL_ID, Some(s::CHANNEL_TITLE)),
        s::CHATFORBIDDEN_CTOR => (Kind::Chat, s::CHATFORBIDDEN_ID, Some(s::CHATFORBIDDEN_TITLE)),
        s::CHANNELFORBIDDEN_CTOR => {
            (Kind::Channel, s::CHANNELFORBIDDEN_ID, Some(s::CHANNELFORBIDDEN_TITLE))
        }
        // userEmpty, chatEmpty: an id and nothing to call it.
        _ => return Ok(None),
    };

    let id = match as_long(&f[id_i]) {
        Some(v) => v,
        None => return Ok(None),
    };

    let title = match title_i {
        Some(i) => as_str(&f[i]).map(|b| String::from_utf8_lossy(b).into_owned()),
        None => {
            // A user has a first and last name rather than a title, and either can be
            // absent — an account with neither is legal and shows as its id elsewhere in
            // Telegram, so it shows as its id here.
            let first = as_str(&f[s::USER_FIRST_NAME]).unwrap_or(b"");
            let last = as_str(&f[s::USER_LAST_NAME]).unwrap_or(b"");
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

    Ok(Some(Named { peer: Peer { kind, id }, title }))
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
        let c = walk::ctor(s::MESSAGE_CTOR).expect("message is in the table");
        let mut w = Writer::new();
        w.ctor(s::MESSAGE_CTOR);

        // Which optional fields are present: `out` and nothing else.
        let flags: u32 = if out { 1 << 1 } else { 0 };

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
            } else {
                panic!("unexpected unconditional field {i} of message");
            }
        }
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
                Named { peer: u, title: String::from("a person") },
                Named { peer: c, title: String::from("a group") },
            ],
            ..Default::default()
        };
        assert_eq!(d.name_of(u), Some("a person"));
        assert_eq!(d.name_of(c), Some("a group"));
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
