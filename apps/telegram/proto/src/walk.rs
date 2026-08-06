//! A TL walker: reads any value in the schema without knowing what it is.
//!
//! # The problem it solves
//!
//! TL is not self-describing. A field carries no tag and no length, so a parser cannot skip
//! one without knowing its type — and `message#7600b9d3` has forty fields, several of them
//! nested constructor families, in front of the six a chat list actually needs.
//! `closure.py` counts what `messages.Dialogs` reaches: **495 constructors**.
//!
//! Hand-writing those is most of a megabyte of source. Hand-writing *some* of them is
//! worse: a parser that guesses at a field it does not know is off by however many bytes
//! that field was, and every value after it is plausible garbage.
//!
//! So the shapes live in [`crate::schema`], generated from `api.tl`, and this interprets
//! them. It knows how long each kind of field is and nothing about what any of them mean.
//!
//! # What that buys
//!
//! A boxed field is read by reading its constructor id and looking it up, so the walker
//! never assumes a field's type. A value that is not what the schema says produces
//! [`Error::Unknown`] with the id, not a misparse — which matters, because a misparse in
//! TL is silent and total.
//!
//! ```no_run
//! # use tg_proto::walk::{Walker, as_str};
//! # use tg_proto::schema as s;
//! # let bytes = &[][..];
//! // Reads the constructor id, then locates every field without interpreting any.
//! let (ctor, fields) = Walker::new(bytes).value()?;
//! if ctor.id == s::MESSAGE_CTOR {
//!     // Forty fields walked; one read.
//!     let text = as_str(&fields[s::MESSAGE_MESSAGE]);
//! }
//! # Ok::<(), tg_proto::walk::Error>(())
//! ```

use alloc::vec::Vec;

use crate::schema::CTORS;
use crate::tl::{self, Reader};

/// One field's shape, as the generator emits it.
#[derive(Copy, Clone, Debug)]
pub struct Field {
    /// `-1` unconditional, otherwise `(flags word index) << 8 | bit`.
    ///
    /// The word index matters: `message` has both `flags` and `flags2`, and reading the
    /// second through the first gates thirty fields on the wrong bits.
    pub f: i16,
    /// Kind, from the generator's table.
    pub k: u8,
    /// Element kind, for vectors.
    pub i: u8,
}

/// One constructor.
#[derive(Copy, Clone, Debug)]
pub struct Ctor {
    pub id: u32,
    /// Only with the `names` feature, which `std` turns on.
    ///
    /// Worth about 15 KB of a 41 KB table — a pointer, a length and the bytes, times 532.
    /// On the host it turns `Unknown(0x7600b9d3)` into `message`, which is most of the value
    /// of an error while a parser is being written. On the handset the id is a number to
    /// look up in `api.tl` and 15 KB is a tenth of the image.
    #[cfg(feature = "names")]
    pub name: &'static str,
    pub fields: &'static [Field],
}

impl Ctor {
    /// The constructor's name where there is one, and its id where there is not.
    pub fn label(&self) -> &'static str {
        #[cfg(feature = "names")]
        {
            self.name
        }
        #[cfg(not(feature = "names"))]
        {
            "?"
        }
    }
}

pub const K_INT: u8 = 0;
pub const K_LONG: u8 = 1;
pub const K_DOUBLE: u8 = 2;
pub const K_STRING: u8 = 3;
pub const K_INT128: u8 = 4;
pub const K_INT256: u8 = 5;
pub const K_BOOL: u8 = 6;
pub const K_TRUE: u8 = 7;
pub const K_BOXED: u8 = 8;
pub const K_VECTOR: u8 = 9;
pub const K_FLAGS: u8 = 10;
pub const K_BARE_VECTOR: u8 = 11;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Error {
    Tl(tl::Error),
    /// A constructor id not in the table. Either a type this build does not carry, or the
    /// stream is off by some number of bytes and this is not a constructor id at all.
    Unknown(u32),
    /// A vector count that does not fit the bytes remaining.
    BadCount,
    /// Nesting past [`MAX_DEPTH`].
    TooDeep,
}

impl From<tl::Error> for Error {
    fn from(e: tl::Error) -> Self {
        Error::Tl(e)
    }
}

pub type Result<T> = core::result::Result<T, Error>;

/// How deep the walker will recurse.
///
/// The type graph has cycles — a `Message` can hold a `MessageFwdHeader` that holds a
/// `Peer`, and a `MessageMedia` can hold another `Message`. Nothing the server sends nests
/// anywhere near this far, and without a bound a malformed reply is a stack overflow on a
/// device whose stack is 32 KB and whose overflow is a reboot.
pub const MAX_DEPTH: u32 = 24;

/// Look a constructor up by id.
pub fn ctor(id: u32) -> Option<&'static Ctor> {
    CTORS.binary_search_by_key(&id, |c| c.id).ok().map(|i| &CTORS[i])
}

/// A cursor over one TL value.
pub struct Walker<'a> {
    r: Reader<'a>,
}

/// One field's bytes, located but not interpreted.
#[derive(Copy, Clone, Debug)]
pub struct Located<'a> {
    pub kind: u8,
    /// Absent when the flag gating it was clear.
    pub bytes: Option<&'a [u8]>,
}

impl<'a> Walker<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Walker { r: Reader::new(buf) }
    }

    pub fn remaining(&self) -> usize {
        self.r.remaining()
    }

    /// Read a boxed value's constructor and locate every field, without interpreting any.
    ///
    /// The returned slices point into the input, so a caller reads the two fields it wants
    /// and pays nothing for the other thirty-eight.
    pub fn value(&mut self) -> Result<(&'static Ctor, Vec<Located<'a>>)> {
        let id = self.r.ctor()?;
        let c = ctor(id).ok_or(Error::Unknown(id))?;
        let fields = self.body(c, 0)?;
        Ok((c, fields))
    }

    /// The fields of a constructor whose id has already been read.
    fn body(&mut self, c: &'static Ctor, depth: u32) -> Result<Vec<Located<'a>>> {
        if depth > MAX_DEPTH {
            return Err(Error::TooDeep);
        }
        let mut out = Vec::with_capacity(c.fields.len());
        // Up to two flags words, in the order they appear. `message` is the constructor
        // that needs the second; everything else has one or none.
        let mut words = [0u32; 2];
        let mut nwords = 0usize;

        for f in c.fields {
            if f.k == K_FLAGS {
                let v = self.r.uint()?;
                if nwords < 2 {
                    words[nwords] = v;
                }
                nwords += 1;
                out.push(Located { kind: K_FLAGS, bytes: None });
                continue;
            }

            if f.f >= 0 {
                let widx = ((f.f as u16) >> 8) as usize;
                let bit = (f.f as u16 & 0xff) as u32;
                let word = words.get(widx).copied().unwrap_or(0);
                if word & (1 << bit) == 0 {
                    out.push(Located { kind: f.k, bytes: None });
                    continue;
                }
                // `true` is the absence of bytes: the flag *is* the value. Reading anything
                // here consumes the next field.
                if f.k == K_TRUE {
                    out.push(Located { kind: K_TRUE, bytes: Some(&[]) });
                    continue;
                }
            } else if f.k == K_TRUE {
                out.push(Located { kind: K_TRUE, bytes: Some(&[]) });
                continue;
            }

            let bytes = self.take_field(f, depth)?;
            out.push(Located { kind: f.k, bytes: Some(bytes) });
        }
        Ok(out)
    }

    /// Consume one field and return the bytes it occupied.
    fn take_field(&mut self, f: &Field, depth: u32) -> Result<&'a [u8]> {
        let start = self.r.pos();
        match f.k {
            K_INT | K_BOOL => {
                self.r.raw(4)?;
            }
            K_LONG | K_DOUBLE => {
                self.r.raw(8)?;
            }
            K_INT128 => {
                self.r.raw(16)?;
            }
            K_INT256 => {
                self.r.raw(32)?;
            }
            K_STRING => {
                self.r.bytes()?;
            }
            K_BOXED => {
                self.skip_boxed(depth + 1)?;
            }
            K_VECTOR | K_BARE_VECTOR => {
                if f.k == K_VECTOR {
                    self.r.expect(tl::VECTOR)?;
                }
                let n = self.r.uint()? as usize;
                // Every element is at least four bytes, so a count larger than a quarter of
                // what is left is a corrupt length rather than a big list.
                if n.saturating_mul(4) > self.r.remaining() {
                    return Err(Error::BadCount);
                }
                for _ in 0..n {
                    let e = Field { f: -1, k: f.i, i: 0 };
                    self.take_field(&e, depth + 1)?;
                }
            }
            K_TRUE | K_FLAGS => {}
            _ => return Err(Error::Unknown(0)),
        }
        Ok(self.r.slice(start))
    }

    /// Read a boxed value's id and consume its body.
    fn skip_boxed(&mut self, depth: u32) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err(Error::TooDeep);
        }
        let id = self.r.ctor()?;
        // Bool is boxed but not in the table: its two constructors have no fields, and
        // including them would mean the generator emitting types it filtered out.
        if id == tl::BOOL_TRUE || id == tl::BOOL_FALSE {
            return Ok(());
        }
        let c = ctor(id).ok_or(Error::Unknown(id))?;
        self.body(c, depth)?;
        Ok(())
    }
}

/// Read a field's bytes as the scalar it is.
///
/// Free functions rather than methods on `Located`, because a caller that asks for the
/// wrong one should get `None` rather than a number made of the wrong bytes.
pub fn as_int(l: &Located<'_>) -> Option<i32> {
    let b = l.bytes?;
    if l.kind != K_INT || b.len() != 4 {
        return None;
    }
    Some(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

pub fn as_long(l: &Located<'_>) -> Option<i64> {
    let b = l.bytes?;
    if l.kind != K_LONG || b.len() != 8 {
        return None;
    }
    let mut a = [0u8; 8];
    a.copy_from_slice(b);
    Some(i64::from_le_bytes(a))
}

/// A TL string's contents, with the length prefix and padding removed.
pub fn as_str<'a>(l: &Located<'a>) -> Option<&'a [u8]> {
    let b = l.bytes?;
    if l.kind != K_STRING {
        return None;
    }
    let mut r = Reader::new(b);
    r.bytes().ok()
}

/// Whether a `flags.n?true` field was present.
pub fn as_flag(l: &Located<'_>) -> bool {
    l.kind == K_TRUE && l.bytes.is_some()
}

/// The elements of a vector field, each still boxed.
///
/// Returns the raw slices; the caller runs a [`Walker`] over each. Splitting it this way
/// means a hundred-message vector is located once and interpreted only as far as the
/// caller reads.
pub fn vector_elements<'a>(l: &Located<'a>) -> Result<Vec<&'a [u8]>> {
    let b = l.bytes.ok_or(Error::BadCount)?;
    if l.kind != K_VECTOR && l.kind != K_BARE_VECTOR {
        return Err(Error::BadCount);
    }
    let mut r = Reader::new(b);
    if l.kind == K_VECTOR {
        r.expect(tl::VECTOR)?;
    }
    let n = r.uint()? as usize;
    let mut out = Vec::with_capacity(n.min(1024));
    for _ in 0..n {
        let start = r.pos();
        // Every element of a boxed vector is a boxed value; walk it to find its end.
        let mut w = Walker { r: Reader::new(&b[start..]) };
        w.value()?;
        let len = b[start..].len() - w.remaining();
        out.push(&b[start..start + len]);
        r.raw(len)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tl::Writer;

    #[test]
    fn the_table_is_sorted_for_binary_search() {
        // `ctor` binary-searches. An unsorted table finds some constructors and not others,
        // which is the worst kind of wrong: it works for most of the schema.
        for pair in CTORS.windows(2) {
            assert!(pair[0].id < pair[1].id, "{:#010x} then {:#010x}", pair[0].id, pair[1].id);
        }
    }

    #[test]
    fn the_table_holds_what_a_chat_list_needs() {
        // By id, from the generated accessor constants, rather than by name. Names are
        // behind a feature the device does not build with, and the id is the stronger
        // check anyway: it is what the walker actually looks up.
        //
        // If the generator's roots ever stop reaching one of these, a chat list stops
        // parsing and the failure is an unknown constructor at run time on a phone.
        use crate::schema as s;
        for id in [
            s::MESSAGES_DIALOGS_CTOR, s::MESSAGES_DIALOGSSLICE_CTOR, s::DIALOG_CTOR,
            s::MESSAGE_CTOR, s::MESSAGESERVICE_CTOR, s::MESSAGEEMPTY_CTOR, s::USER_CTOR,
            s::CHAT_CTOR, s::CHANNEL_CTOR, s::PEERUSER_CTOR, s::PEERCHAT_CTOR,
            s::PEERCHANNEL_CTOR, s::MESSAGES_MESSAGES_CTOR, s::AUTH_SENTCODE_CTOR,
            s::ACCOUNT_PASSWORD_CTOR,
        ] {
            assert!(ctor(id).is_some(), "{id:#010x} is missing from the generated table");
        }
    }

    #[test]
    fn a_flat_constructor_walks() {
        // peerUser#59511722 user_id:long = Peer;
        let mut w = Writer::new();
        w.ctor(0x5951_1722).ulong(4242);
        let buf = w.finish();
        let mut walk = Walker::new(&buf);
        let (c, fields) = walk.value().unwrap();
        assert_eq!(c.id, crate::schema::PEERUSER_CTOR);
        assert_eq!(as_long(&fields[0]), Some(4242));
        assert_eq!(walk.remaining(), 0);
    }

    #[test]
    fn a_clear_flag_consumes_nothing() {
        // The property the whole walker rests on: an absent optional field must not move
        // the cursor. One byte of drift makes every value after it garbage that still
        // parses.
        //
        // messageEmpty#90a6ca84 flags:# id:int peer_id:flags.0?Peer = Message;
        let mut w = Writer::new();
        w.ctor(0x90a6_ca84).uint(0).int(7);
        let buf = w.finish();
        let mut walk = Walker::new(&buf);
        let (c, fields) = walk.value().unwrap();
        assert_eq!(c.id, crate::schema::MESSAGEEMPTY_CTOR);
        assert_eq!(as_int(&fields[1]), Some(7));
        assert!(fields[2].bytes.is_none(), "the absent peer_id consumed bytes");
        assert_eq!(walk.remaining(), 0);
    }

    #[test]
    fn a_set_flag_consumes_its_field() {
        let mut w = Writer::new();
        w.ctor(0x90a6_ca84).uint(1).int(7).ctor(0x5951_1722).ulong(99);
        let buf = w.finish();
        let mut walk = Walker::new(&buf);
        let (_, fields) = walk.value().unwrap();
        assert!(fields[2].bytes.is_some());
        assert_eq!(walk.remaining(), 0);
    }

    #[test]
    fn an_unknown_constructor_is_named_rather_than_guessed() {
        let mut w = Writer::new();
        w.ctor(0xdead_beef);
        let buf = w.finish();
        // Compared on the error rather than the whole Result: Ctor holds a &'static str
        // and deriving PartialEq on it would make the table comparable, which is a
        // capability nothing needs and one more thing to keep true.
        match Walker::new(&buf).value() {
            Err(e) => assert_eq!(e, Error::Unknown(0xdead_beef)),
            Ok((c, _)) => panic!("0xdeadbeef resolved to {:#010x}", c.id),
        }
    }

    #[test]
    fn a_vector_of_boxed_values_splits_into_elements() {
        let mut inner = Writer::new();
        inner.ctor(tl::VECTOR).uint(3);
        for id in [1u64, 2, 3] {
            inner.ctor(0x5951_1722).ulong(id);
        }
        let bytes = inner.finish();
        let l = Located { kind: K_VECTOR, bytes: Some(&bytes) };
        let els = vector_elements(&l).unwrap();
        assert_eq!(els.len(), 3);
        for (i, e) in els.iter().enumerate() {
            let (c, f) = Walker::new(e).value().unwrap();
            assert_eq!(c.id, crate::schema::PEERUSER_CTOR);
            assert_eq!(as_long(&f[0]), Some(i as i64 + 1));
        }
    }

    #[test]
    fn an_absurd_vector_count_is_refused_before_allocating() {
        let mut w = Writer::new();
        w.ctor(tl::VECTOR).uint(1_000_000);
        let bytes = w.finish();
        let l = Located { kind: K_VECTOR, bytes: Some(&bytes) };
        assert!(vector_elements(&l).is_err());
    }

    #[test]
    fn a_truncated_value_is_an_error_not_a_panic() {
        // The walker indexes into a slice for every field. A short buffer must come back as
        // Truncated rather than as a panic, because on the device a panic is the
        // application closing.
        let mut w = Writer::new();
        w.ctor(0x5951_1722).ulong(1);
        let full = w.finish();
        for cut in 0..full.len() {
            let r = Walker::new(&full[..cut]).value();
            assert!(r.is_err(), "reading {cut} of {} bytes succeeded", full.len());
        }
    }
}
