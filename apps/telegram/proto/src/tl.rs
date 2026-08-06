//! TL serialisation: the wire format everything in MTProto is written in.
//!
//! # The format in one paragraph
//!
//! Everything is little-endian and everything is padded to four bytes. Scalars are their
//! natural width; `int128` and `int256` are raw byte blocks. A `string` (and `bytes`, which
//! is the same thing) is length-prefixed: one byte for lengths under 254, otherwise `0xFE`
//! and three bytes of length, then the data, then zeros up to a multiple of four. A boxed
//! value is a 32-bit constructor id followed by the fields.
//!
//! # Why this is hand-written
//!
//! `api.tl` describes several thousand constructors and a generator is the obvious answer.
//! It is not the answer here: login needs about fifteen of them, the generator would be
//! more code than the fifteen, and every constructor it emitted would be unread code in a
//! 150 KB image on a phone with 45 MB of RAM. Constructors are added as they are needed and
//! each one carries the line from `api.tl` it came from, so the source is checkable.
//!
//! # The two bugs this module exists to prevent
//!
//! **Padding read as data.** A string of length 5 occupies 8 bytes. A reader that advances
//! by 6 is off by two for the rest of the message, and the failure surfaces as a nonsense
//! constructor id several fields later — nowhere near the cause.
//!
//! **A length byte of 254 misread.** 0xFE is the escape, not a length. Getting it backwards
//! works perfectly for every short string and fails on the first long one, which in practice
//! means it works through the whole handshake and fails at the first real message.
//!
//! Both are covered by round-trip tests across every boundary length.

use alloc::vec::Vec;

/// What went wrong reading a message.
///
/// Deliberately not a single `Malformed`: a truncated buffer and an unexpected constructor
/// are different problems — one is a short read that may complete, the other is a protocol
/// mismatch that never will — and the caller reacts differently to each.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Error {
    /// The buffer ended mid-value. May simply mean more bytes are on the way.
    Truncated,
    /// A boxed value carried a constructor id this build does not know.
    UnknownConstructor(u32),
    /// A constructor id was read where a specific different one was required.
    Unexpected { want: u32, got: u32 },
    /// A length field that cannot be honoured — negative, or past any sane message size.
    BadLength,
}

pub type Result<T> = core::result::Result<T, Error>;

/// Longest string this reader will accept.
///
/// MTProto's `string` can encode 16 MB. Nothing in a login flow is larger than a few
/// kilobytes, and a corrupt length field otherwise asks a phone with 45 MB of RAM for a
/// 16 MB allocation — which fails, but only after the allocator has tried. A bound here
/// turns that into an error at the point the nonsense was read.
pub const MAX_STRING: usize = 1 << 20;

// --------------------------------------------------------------------------- writing --

/// Appends TL values to a byte buffer.
///
/// A `Vec` rather than a fixed buffer: handshake messages are small but the inner data
/// blocks are built by nesting, and the sizes are not known until the nesting is done.
#[derive(Default)]
pub struct Writer {
    pub buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Writer { buf: Vec::new() }
    }

    pub fn with_capacity(n: usize) -> Self {
        Writer { buf: Vec::with_capacity(n) }
    }

    pub fn int(&mut self, v: i32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn uint(&mut self, v: u32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn long(&mut self, v: i64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn ulong(&mut self, v: u64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    /// A constructor id. Same encoding as `uint`; a separate name because a reader looking
    /// for where a message starts should be able to find it.
    pub fn ctor(&mut self, id: u32) -> &mut Self {
        self.uint(id)
    }

    /// `int128` / `int256` and other fixed blocks: raw, no length, no padding.
    ///
    /// The nonces are these. Writing one through `bytes` instead would prepend a length
    /// byte, and the server would reject the message with no explanation — MTProto's error
    /// reporting during the handshake is a closed connection.
    pub fn raw(&mut self, b: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(b);
        self
    }

    /// A TL `string` or `bytes`: length-prefixed and padded to four.
    pub fn bytes(&mut self, b: &[u8]) -> &mut Self {
        let start = self.buf.len();
        if b.len() < 254 {
            self.buf.push(b.len() as u8);
        } else {
            self.buf.push(254);
            let n = b.len() as u32;
            self.buf.extend_from_slice(&n.to_le_bytes()[..3]);
        }
        self.buf.extend_from_slice(b);
        // Padding is measured from the start of the *string*, not from the start of the
        // message: a string beginning at an unaligned offset would otherwise pad to the
        // wrong boundary. In practice every string starts aligned, which is exactly why
        // this would go unnoticed.
        while (self.buf.len() - start) % 4 != 0 {
            self.buf.push(0);
        }
        self
    }

    pub fn string(&mut self, s: &str) -> &mut Self {
        self.bytes(s.as_bytes())
    }

    /// `boolTrue` / `boolFalse`, which are boxed constructors rather than a bit.
    pub fn boolean(&mut self, v: bool) -> &mut Self {
        self.ctor(if v { BOOL_TRUE } else { BOOL_FALSE })
    }

    pub fn finish(self) -> Vec<u8> {
        self.buf
    }
}

// --------------------------------------------------------------------------- reading --

/// Reads TL values from a byte slice, tracking position.
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.remaining() < n {
            return Err(Error::Truncated);
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    pub fn int(&mut self) -> Result<i32> {
        let b = self.take(4)?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn uint(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn long(&mut self) -> Result<i64> {
        Ok(self.ulong()? as i64)
    }

    pub fn ulong(&mut self) -> Result<u64> {
        let b = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_le_bytes(a))
    }

    pub fn ctor(&mut self) -> Result<u32> {
        self.uint()
    }

    /// Read a constructor id and require it to be `want`.
    ///
    /// The error carries both ids. During a handshake the server answers a wrong request
    /// with a different constructor rather than an error message, so "expected resPQ, got
    /// 0xb5890dba" *is* the error message — `0xb5890dba` being `rpc_error`, and the pair is
    /// what tells you which.
    pub fn expect(&mut self, want: u32) -> Result<()> {
        let got = self.ctor()?;
        if got == want {
            Ok(())
        } else {
            Err(Error::Unexpected { want, got })
        }
    }

    pub fn raw(&mut self, n: usize) -> Result<&'a [u8]> {
        self.take(n)
    }

    pub fn int128(&mut self) -> Result<[u8; 16]> {
        let mut a = [0u8; 16];
        a.copy_from_slice(self.take(16)?);
        Ok(a)
    }

    pub fn int256(&mut self) -> Result<[u8; 32]> {
        let mut a = [0u8; 32];
        a.copy_from_slice(self.take(32)?);
        Ok(a)
    }

    /// A TL `string` / `bytes`, padding consumed.
    pub fn bytes(&mut self) -> Result<&'a [u8]> {
        let start = self.pos;
        let first = self.take(1)?[0];
        let len = if first == 254 {
            let b = self.take(3)?;
            u32::from_le_bytes([b[0], b[1], b[2], 0]) as usize
        } else if first == 255 {
            // 255 is not a valid length prefix in any TL dialect. Rejecting it explicitly
            // rather than treating it as a 255-byte string means a desynchronised stream
            // fails here instead of consuming 255 bytes of someone else's message.
            return Err(Error::BadLength);
        } else {
            first as usize
        };
        if len > MAX_STRING {
            return Err(Error::BadLength);
        }
        let data = self.take(len)?;
        let consumed = self.pos - start;
        let pad = (4 - consumed % 4) % 4;
        self.take(pad)?;
        Ok(data)
    }

    pub fn boolean(&mut self) -> Result<bool> {
        match self.ctor()? {
            BOOL_TRUE => Ok(true),
            BOOL_FALSE => Ok(false),
            other => Err(Error::UnknownConstructor(other)),
        }
    }

    /// A `Vector<long>`, which the handshake needs for RSA key fingerprints.
    pub fn vector_long(&mut self) -> Result<Vec<u64>> {
        self.expect(VECTOR)?;
        let n = self.uint()? as usize;
        // A count is four bytes and a `long` is eight, so a message cannot contain more
        // elements than a quarter of its remaining length. Checking before reserving turns
        // a corrupt count into an error rather than an allocation the phone cannot make.
        if n * 8 > self.remaining() {
            return Err(Error::BadLength);
        }
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(self.ulong()?);
        }
        Ok(v)
    }
}

// ----------------------------------------------------------------- constructor ids --

/* Every id below is the CRC32 of its own definition line in api.tl or mtproto.tl with
 * whitespace normalised, which is how TL assigns them. They are written as literals rather
 * than computed, because computing one requires the exact source line and getting *that*
 * wrong produces the same wrong number silently. The line each came from is quoted so it
 * can be checked against the .tl files in vendor/research/mtproto.
 *
 * (Written without a path glob on purpose: Rust block comments nest, so a stray slash-star
 * inside one opens a second comment that never closes, and the error lands at the end of
 * the file rather than at the text that caused it.) */

/// `boolFalse#bc799737 = Bool;`
pub const BOOL_FALSE: u32 = 0xbc79_9737;
/// `boolTrue#997275b5 = Bool;`
pub const BOOL_TRUE: u32 = 0x9972_75b5;
/// `vector#1cb5c415 {t:Type} # [ t ] = Vector t;`
pub const VECTOR: u32 = 0x1cb5_c415;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalars_round_trip() {
        let mut w = Writer::new();
        w.int(-1).uint(0xdead_beef).long(-2).ulong(0x0123_4567_89ab_cdef);
        let buf = w.finish();
        let mut r = Reader::new(&buf);
        assert_eq!(r.int().unwrap(), -1);
        assert_eq!(r.uint().unwrap(), 0xdead_beef);
        assert_eq!(r.long().unwrap(), -2);
        assert_eq!(r.ulong().unwrap(), 0x0123_4567_89ab_cdef);
        assert!(r.is_empty());
    }

    #[test]
    fn everything_is_little_endian() {
        // Not a tautology against the reader: the byte order is checked against literal
        // bytes, because a reader and writer that are both big-endian round-trip perfectly
        // and talk to nobody.
        let mut w = Writer::new();
        w.uint(0x0102_0304);
        assert_eq!(w.buf, [0x04, 0x03, 0x02, 0x01]);
        let mut w = Writer::new();
        w.ulong(0x0102_0304_0506_0708);
        assert_eq!(w.buf, [0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]);
    }

    #[test]
    fn strings_round_trip_at_every_padding_boundary() {
        // 0..8 covers all four padding cases twice; 253..256 covers the 0xFE escape on both
        // sides. 253 is the last short length and 254 is the first long one, and getting
        // that comparison backwards is invisible until the first long string.
        for len in [0usize, 1, 2, 3, 4, 5, 6, 7, 8, 252, 253, 254, 255, 256, 1000] {
            let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let mut w = Writer::new();
            w.bytes(&data);
            let buf = w.finish();
            assert_eq!(buf.len() % 4, 0, "len {len} was not padded to four");
            let mut r = Reader::new(&buf);
            assert_eq!(r.bytes().unwrap(), &data[..], "len {len}");
            assert!(r.is_empty(), "len {len} left {} bytes of padding", r.remaining());
        }
    }

    #[test]
    fn the_length_escape_is_at_254_not_253() {
        let mut w = Writer::new();
        w.bytes(&[0u8; 253]);
        assert_eq!(w.buf[0], 253, "253 bytes must use the short form");
        let mut w = Writer::new();
        w.bytes(&[0u8; 254]);
        assert_eq!(w.buf[0], 254, "254 bytes must use the escape");
    }

    #[test]
    fn a_string_after_other_fields_still_pads_correctly() {
        // The padding is relative to the string's own start. This is the case that would
        // hide a bug measuring from the message start, since every real message happens to
        // begin its strings at an aligned offset.
        let mut w = Writer::new();
        w.int(1).bytes(b"abc").int(2);
        let buf = w.finish();
        assert_eq!(buf.len(), 4 + 4 + 4);
        let mut r = Reader::new(&buf);
        assert_eq!(r.int().unwrap(), 1);
        assert_eq!(r.bytes().unwrap(), b"abc");
        assert_eq!(r.int().unwrap(), 2);
    }

    #[test]
    fn truncation_is_reported_rather_than_read_past() {
        let mut w = Writer::new();
        w.int(7).bytes(b"hello");
        let full = w.finish();
        for cut in 0..full.len() {
            let mut r = Reader::new(&full[..cut]);
            let outcome = r.int().and_then(|_| r.bytes().map(|_| ()));
            assert!(outcome.is_err(), "reading {cut} of {} bytes succeeded", full.len());
        }
    }

    #[test]
    fn a_255_length_prefix_is_rejected() {
        let mut r = Reader::new(&[255u8, 0, 0, 0]);
        assert_eq!(r.bytes(), Err(Error::BadLength));
    }

    #[test]
    fn an_absurd_length_is_rejected_before_allocating() {
        // 0xFE followed by 0xFFFFFF: a 16 MB string. Valid TL, and not something a login
        // flow ever sends, so it is corruption and treated as such.
        let mut r = Reader::new(&[254u8, 0xff, 0xff, 0xff]);
        assert_eq!(r.bytes(), Err(Error::BadLength));
    }

    #[test]
    fn expect_reports_both_ids() {
        let mut w = Writer::new();
        w.ctor(0xb589_0dba);
        let buf = w.finish();
        let mut r = Reader::new(&buf);
        assert_eq!(
            r.expect(0x0516_2463),
            Err(Error::Unexpected { want: 0x0516_2463, got: 0xb589_0dba })
        );
    }

    #[test]
    fn vectors_round_trip_and_reject_absurd_counts() {
        let mut w = Writer::new();
        w.ctor(VECTOR).uint(3).ulong(1).ulong(2).ulong(3);
        let buf = w.finish();
        let mut r = Reader::new(&buf);
        assert_eq!(r.vector_long().unwrap(), alloc::vec![1, 2, 3]);

        let mut w = Writer::new();
        w.ctor(VECTOR).uint(1_000_000);
        let buf = w.finish();
        let mut r = Reader::new(&buf);
        assert_eq!(r.vector_long(), Err(Error::BadLength));
    }

    #[test]
    fn booleans_are_boxed() {
        let mut w = Writer::new();
        w.boolean(true).boolean(false);
        let buf = w.finish();
        assert_eq!(buf.len(), 8);
        let mut r = Reader::new(&buf);
        assert!(r.boolean().unwrap());
        assert!(!r.boolean().unwrap());
    }

    #[test]
    fn raw_blocks_carry_no_length() {
        let nonce = [0xAAu8; 16];
        let mut w = Writer::new();
        w.raw(&nonce);
        assert_eq!(w.buf.len(), 16, "a nonce must not be length-prefixed");
    }
}
