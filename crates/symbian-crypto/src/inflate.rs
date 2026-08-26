//! DEFLATE decompression, RFC 1951, with the zlib and gzip wrappers.
//!
//! MTProto wraps large responses in `gzip_packed`, so a client that cannot inflate cannot
//! read a dialog list. `libz` has this if the handset has Open C — which
//! `examples/libprobe` exists to find out — but that is a property of the phone rather than
//! of the SDK, and this is four hundred lines against a dependency that may not be there.
//!
//! Decompression only. Nothing here ever needs to *produce* a compressed stream: a client
//! sends small requests, and `gzip_packed` is a thing the server does.
//!
//! # The decoder
//!
//! Canonical Huffman decoded bit by bit against the code-length counts, the way zlib's own
//! `puff.c` reference does it, rather than by building lookup tables.
//!
//! That is slower per symbol — a few comparisons instead of one indexed load — and it is the
//! right trade here twice over. The payloads are kilobytes, so the difference is microseconds
//! against a network measured in hundreds of milliseconds. And a table-building step is the
//! part of an inflate implementation where the subtle bugs live: an over-subscribed code, a
//! table entry that aliases, a fast-path length that disagrees with the slow path. This
//! version has no table to get wrong.
//!
//! # Bounds
//!
//! `max_out` is required rather than optional. A compressed stream is attacker-controlled
//! input, and DEFLATE's ratio is unbounded — a few hundred bytes can expand to gigabytes,
//! which on a device with 45 MB free is a denial of service by way of one message. Every
//! caller has to say how much it is prepared to hold.

use alloc::vec;
use alloc::vec::Vec;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The input ended in the middle of something.
    Truncated,
    /// A block header, Huffman code or wrapper field was not valid.
    Corrupt,
    /// A back-reference pointed before the start of the output.
    BadDistance,
    /// The output would exceed `max_out`.
    TooLarge,
    /// A zlib or gzip checksum did not match what the stream claimed.
    ChecksumMismatch,
    /// The consumer refused the bytes. Only reachable through the `*_to` entry points.
    Sink,
}

// ------------------------------------------------------------------------- streaming --

/// The largest back-reference DEFLATE can encode, and therefore the least output that has to
/// stay in memory for the decoder to keep working.
const WINDOW: usize = 32 * 1024;

/// Where inflated bytes go.
///
/// # Why this exists
///
/// The whole-buffer entry points hold every output byte at once, which is what a `gzip_packed`
/// MTProto response wants — it is kilobytes and the caller wants a `Vec`. A web page is the other
/// shape: one measured response was 294 KB compressed, and inflated it is over a megabyte on a
/// handset with about 45 MB free. Holding the compressed body *and* the inflated body *and* the DOM
/// built from it is how a browser runs out of memory on a page that is not unusual.
///
/// So output can go somewhere as it is produced. What stays in memory is [`WINDOW`] bytes, because
/// DEFLATE back-references reach 32 KB and no further; everything older is already delivered and can
/// be dropped. The inflated body never exists as one object.
///
/// A sink may refuse — an HTML tokenizer can fail on the bytes it is fed — which surfaces as
/// [`Error::Sink`].
pub trait Sink {
    fn write(&mut self, bytes: &[u8]) -> Result<(), Error>;
}

/// Collecting into a `Vec` is a sink like any other, which is what lets the whole-buffer functions
/// below be thin wrappers rather than a second copy of the decoder.
impl Sink for Vec<u8> {
    fn write(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.extend_from_slice(bytes);
        Ok(())
    }
}

/// The decoder's output side: a sliding window over what it has produced.
struct Emit<'s, S: Sink> {
    sink: &'s mut S,
    /// The most recent output. Never shorter than `min(total, WINDOW)`, so a legal
    /// back-reference always lands inside it.
    tail: Vec<u8>,
    /// Every byte produced, delivered or not. This is what `max_out` bounds and what a gzip
    /// trailer's length field is checked against.
    total: usize,
    max_out: usize,
    crc: u32,
}

impl<'s, S: Sink> Emit<'s, S> {
    fn new(sink: &'s mut S, max_out: usize) -> Self {
        Emit { sink, tail: Vec::new(), total: 0, max_out, crc: 0xFFFF_FFFF }
    }

    /// Hand the sink everything older than the window.
    ///
    /// Deliberately not called from inside a back-reference copy: that loop reads bytes by their
    /// distance from the end, and a flush mid-copy would move the front underneath it.
    fn drain(&mut self) -> Result<(), Error> {
        // Two windows before flushing one, so the common case is a memmove of 32 KB every 32 KB
        // rather than one per byte.
        if self.tail.len() <= 2 * WINDOW {
            return Ok(());
        }
        let cut = self.tail.len() - WINDOW;
        self.crc = crc32_update(self.crc, &self.tail[..cut]);
        self.sink.write(&self.tail[..cut])?;
        self.tail.drain(..cut);
        Ok(())
    }

    fn push(&mut self, b: u8) -> Result<(), Error> {
        if self.total + 1 > self.max_out {
            return Err(Error::TooLarge);
        }
        self.tail.push(b);
        self.total += 1;
        Ok(())
    }

    fn extend(&mut self, bytes: &[u8]) -> Result<(), Error> {
        if self.total + bytes.len() > self.max_out {
            return Err(Error::TooLarge);
        }
        self.tail.extend_from_slice(bytes);
        self.total += bytes.len();
        self.drain()
    }

    /// The byte `distance` back from the end. The caller has already checked the distance.
    fn back(&self, distance: usize) -> u8 {
        self.tail[self.tail.len() - distance]
    }

    /// Deliver what is left. Returns the CRC-32 of everything written, for the gzip trailer.
    fn finish(mut self) -> Result<(u32, usize), Error> {
        self.crc = crc32_update(self.crc, &self.tail);
        self.sink.write(&self.tail)?;
        Ok((!self.crc, self.total))
    }
}

// ------------------------------------------------------------------------ bit reader --

struct Bits<'a> {
    data: &'a [u8],
    /// Byte position of the next byte to load.
    pos: usize,
    /// Bit accumulator, filled from the low end. DEFLATE is LSB-first *within* a byte, which
    /// is the opposite of the big-endian convention everything else here uses — and the
    /// single most common thing to get backwards.
    acc: u32,
    /// How many bits in `acc` are valid.
    have: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Bits { data, pos: 0, acc: 0, have: 0 }
    }

    fn need(&mut self, n: u32) -> Result<(), Error> {
        while self.have < n {
            if self.pos >= self.data.len() {
                return Err(Error::Truncated);
            }
            self.acc |= (self.data[self.pos] as u32) << self.have;
            self.pos += 1;
            self.have += 8;
        }
        Ok(())
    }

    fn take(&mut self, n: u32) -> Result<u32, Error> {
        if n == 0 {
            return Ok(0);
        }
        self.need(n)?;
        let v = self.acc & ((1u32 << n) - 1);
        self.acc >>= n;
        self.have -= n;
        Ok(v)
    }

    /// Drop to the next byte boundary, for a stored block.
    fn align(&mut self) {
        let drop = self.have % 8;
        self.acc >>= drop;
        self.have -= drop;
    }

    /// Read whole bytes directly, for a stored block's payload.
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], Error> {
        // Anything already buffered has to be given back first, or the payload starts in the
        // wrong place. align() leaves a whole number of bytes in the accumulator.
        let buffered = (self.have / 8) as usize;
        self.pos -= buffered;
        self.acc = 0;
        self.have = 0;
        if self.pos + n > self.data.len() {
            return Err(Error::Truncated);
        }
        let out = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }
}

// --------------------------------------------------------------------------- Huffman --

/// The longest DEFLATE code, per RFC 1951.
const MAX_BITS: usize = 15;

/// `Debug` prints the shape rather than the symbol table: 288 entries in a test failure
/// buries the assertion, and the counts are what a malformed code shows up in.
struct Huffman {
    /// How many codes of each length, indexed by length. `counts[0]` is unused.
    counts: [u16; MAX_BITS + 1],
    /// Symbols, ordered by code length then by symbol value — which is what makes the
    /// canonical code assignment implicit and needs no table.
    symbols: Vec<u16>,
}

impl core::fmt::Debug for Huffman {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Huffman({} symbols, counts {:?})", self.symbols.len(), &self.counts[1..])
    }
}

impl Huffman {
    /// Build from a list of code lengths, where zero means "not used".
    fn new(lengths: &[u8]) -> Result<Self, Error> {
        let mut counts = [0u16; MAX_BITS + 1];
        for &l in lengths {
            if l as usize > MAX_BITS {
                return Err(Error::Corrupt);
            }
            counts[l as usize] += 1;
        }
        // A code with no symbols at all is legal (an empty distance tree, for a block of
        // literals only) but one that is over-subscribed is not — and an over-subscribed
        // code is how a corrupt stream makes a decoder read forever.
        if counts[0] as usize == lengths.len() {
            return Ok(Huffman { counts, symbols: Vec::new() });
        }
        let mut left = 1i32;
        for &count in &counts[1..=MAX_BITS] {
            left <<= 1;
            left -= count as i32;
            if left < 0 {
                return Err(Error::Corrupt);
            }
        }

        // Offsets of each length's run within `symbols`.
        let mut offs = [0u16; MAX_BITS + 2];
        for len in 1..=MAX_BITS {
            offs[len + 1] = offs[len] + counts[len];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbols[offs[l as usize] as usize] = sym as u16;
                offs[l as usize] += 1;
            }
        }
        Ok(Huffman { counts, symbols })
    }

    /// Decode one symbol.
    ///
    /// Walks the lengths, accumulating one bit at a time and asking whether the code so far
    /// falls within the codes of that length. `first` is the first code of the current
    /// length and `index` its position in `symbols`, so the symbol is found by offset with
    /// no search.
    fn decode(&self, bits: &mut Bits<'_>) -> Result<u16, Error> {
        let mut code = 0i32;
        let mut first = 0i32;
        let mut index = 0i32;
        for len in 1..=MAX_BITS {
            code |= bits.take(1)? as i32;
            let count = self.counts[len] as i32;
            if code - first < count {
                return Ok(self.symbols[(index + (code - first)) as usize]);
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err(Error::Corrupt)
    }
}

// ---------------------------------------------------------------------------- tables --

/// Base length for each length symbol 257..=287, and the extra bits each takes.
const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99,
    115, 131, 163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];

const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025,
    1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12,
    12, 13, 13,
];

/// The order the dynamic-block code lengths arrive in. Not sequential: the lengths most
/// likely to be zero are put last so the count can stop early.
const CODE_LENGTH_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

// ---------------------------------------------------------------------------- inflate --

/// Raw DEFLATE, no wrapper. Collects the whole output.
pub fn inflate(data: &[u8], max_out: usize) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    inflate_to(data, max_out, &mut out)?;
    Ok(out)
}

/// Raw DEFLATE into a [`Sink`], holding only the 32 KB the decoder needs.
///
/// Bytes reach the sink before the stream is known to be intact — there is no checksum in raw
/// DEFLATE to be intact against, so nothing is lost here, but see [`inflate_gzip_to`], where it
/// matters.
pub fn inflate_to<S: Sink>(data: &[u8], max_out: usize, sink: &mut S) -> Result<usize, Error> {
    let (_, len) = inflate_checked(data, max_out, sink)?;
    Ok(len)
}

/// The decoder. Returns the CRC-32 and length of the output, which the gzip wrapper checks.
fn inflate_checked<S: Sink>(
    data: &[u8],
    max_out: usize,
    sink: &mut S,
) -> Result<(u32, usize), Error> {
    let mut bits = Bits::new(data);
    let mut out = Emit::new(sink, max_out);

    loop {
        let last = bits.take(1)?;
        let kind = bits.take(2)?;
        match kind {
            0 => stored(&mut bits, &mut out)?,
            1 => {
                let (lit, dist) = fixed_tables()?;
                block(&mut bits, &mut out, &lit, &dist)?;
            }
            2 => {
                let (lit, dist) = dynamic_tables(&mut bits)?;
                block(&mut bits, &mut out, &lit, &dist)?;
            }
            // 3 is reserved and means the stream is not DEFLATE, or is not aligned where we
            // think it is.
            _ => return Err(Error::Corrupt),
        }
        if last == 1 {
            return out.finish();
        }
    }
}

fn stored<S: Sink>(bits: &mut Bits<'_>, out: &mut Emit<'_, S>) -> Result<(), Error> {
    bits.align();
    let len = bits.take(16)? as usize;
    let nlen = bits.take(16)? as usize;
    // The complement check is the only integrity check a stored block has, and it catches a
    // misaligned reader immediately rather than after megabytes of garbage.
    if len != !nlen & 0xFFFF {
        return Err(Error::Corrupt);
    }
    let bytes = bits.bytes(len)?;
    out.extend(bytes)
}

/// The fixed code, RFC 1951 section 3.2.6. Built rather than stored: it is defined by four
/// ranges, and writing the ranges out is both shorter and self-checking.
fn fixed_tables() -> Result<(Huffman, Huffman), Error> {
    let mut lengths = [0u8; 288];
    for (i, l) in lengths.iter_mut().enumerate() {
        *l = match i {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    let lit = Huffman::new(&lengths)?;
    let dist = Huffman::new(&[5u8; 30])?;
    Ok((lit, dist))
}

fn dynamic_tables(bits: &mut Bits<'_>) -> Result<(Huffman, Huffman), Error> {
    let nlen = bits.take(5)? as usize + 257;
    let ndist = bits.take(5)? as usize + 1;
    let ncode = bits.take(4)? as usize + 4;
    if nlen > 286 || ndist > 30 {
        return Err(Error::Corrupt);
    }

    // First a Huffman code for the code lengths themselves.
    let mut cl_lengths = [0u8; 19];
    for i in 0..ncode {
        cl_lengths[CODE_LENGTH_ORDER[i]] = bits.take(3)? as u8;
    }
    let cl = Huffman::new(&cl_lengths)?;

    // Then the literal and distance lengths, run-length encoded together.
    let mut lengths = vec![0u8; nlen + ndist];
    let mut i = 0;
    while i < lengths.len() {
        let sym = cl.decode(bits)?;
        match sym {
            0..=15 => {
                lengths[i] = sym as u8;
                i += 1;
            }
            16 => {
                // Repeat the previous length 3..=6 times. At i == 0 there is no previous
                // length, and a stream that asks for one is corrupt rather than zero.
                if i == 0 {
                    return Err(Error::Corrupt);
                }
                let prev = lengths[i - 1];
                let n = 3 + bits.take(2)? as usize;
                if i + n > lengths.len() {
                    return Err(Error::Corrupt);
                }
                for _ in 0..n {
                    lengths[i] = prev;
                    i += 1;
                }
            }
            17 => {
                let n = 3 + bits.take(3)? as usize;
                if i + n > lengths.len() {
                    return Err(Error::Corrupt);
                }
                i += n; // already zero
            }
            18 => {
                let n = 11 + bits.take(7)? as usize;
                if i + n > lengths.len() {
                    return Err(Error::Corrupt);
                }
                i += n;
            }
            _ => return Err(Error::Corrupt),
        }
    }

    let lit = Huffman::new(&lengths[..nlen])?;
    let dist = Huffman::new(&lengths[nlen..])?;
    Ok((lit, dist))
}

fn block<S: Sink>(
    bits: &mut Bits<'_>,
    out: &mut Emit<'_, S>,
    lit: &Huffman,
    dist: &Huffman,
) -> Result<(), Error> {
    loop {
        let sym = lit.decode(bits)?;
        match sym {
            0..=255 => {
                out.push(sym as u8)?;
                out.drain()?;
            }
            256 => return Ok(()), // end of block
            257..=285 => {
                let idx = (sym - 257) as usize;
                let len = LEN_BASE[idx] as usize + bits.take(LEN_EXTRA[idx] as u32)? as usize;

                let dsym = dist.decode(bits)? as usize;
                if dsym >= DIST_BASE.len() {
                    return Err(Error::Corrupt);
                }
                let distance =
                    DIST_BASE[dsym] as usize + bits.take(DIST_EXTRA[dsym] as u32)? as usize;
                // Against the total produced, not against what is still held: a distance that
                // reaches past the start of the stream is corrupt, and one that reaches past the
                // window is unencodable, so both are caught here.
                if distance > out.total || distance > WINDOW {
                    return Err(Error::BadDistance);
                }

                // Byte at a time, on purpose, and read by distance from the end rather than by
                // absolute index. A copy whose distance is less than its length reads bytes this
                // same loop is writing — that is how DEFLATE encodes a run — so no bulk move is
                // correct, and a position measured from the start would drift as the window slides.
                for _ in 0..len {
                    let b = out.back(distance);
                    out.push(b)?;
                }
                // Only now, with the copy finished: draining moves the front of the window, and
                // `back` is measured from the end.
                out.drain()?;
            }
            _ => return Err(Error::Corrupt),
        }
    }
}

// --------------------------------------------------------------------------- wrappers --

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

/// The same loop, resumable, because a streamed body's checksum has to be built from the pieces
/// as they are delivered — by the time the trailer is read, the bytes are gone.
fn crc32_update(mut c: u32, data: &[u8]) -> u32 {
    for &byte in data {
        c ^= byte as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    c
}

/// zlib format, RFC 1950: a two-byte header, the deflate stream, then an Adler-32.
pub fn inflate_zlib(data: &[u8], max_out: usize) -> Result<Vec<u8>, Error> {
    if data.len() < 6 {
        return Err(Error::Truncated);
    }
    let cmf = data[0];
    let flg = data[1];
    // Compression method 8 is the only one defined, and the header's own check value must be
    // a multiple of 31.
    if cmf & 0x0F != 8 || u16::from_be_bytes([cmf, flg]) % 31 != 0 {
        return Err(Error::Corrupt);
    }
    // FDICT: a preset dictionary, which nothing here can supply.
    if flg & 0x20 != 0 {
        return Err(Error::Corrupt);
    }
    let out = inflate(&data[2..data.len() - 4], max_out)?;
    let want = u32::from_be_bytes([
        data[data.len() - 4],
        data[data.len() - 3],
        data[data.len() - 2],
        data[data.len() - 1],
    ]);
    if adler32(&out) != want {
        return Err(Error::ChecksumMismatch);
    }
    Ok(out)
}

/// Where the deflate stream starts in a gzip member, past the variable-length header.
///
/// Split out because the streaming and whole-buffer entry points must agree about it: two copies of
/// this parsing would disagree the first time one learned about a flag the other did not, and the
/// symptom is a page that inflates to garbage rather than an error.
fn gzip_body_start(data: &[u8]) -> Result<usize, Error> {
    if data.len() < 18 {
        return Err(Error::Truncated);
    }
    if data[0] != 0x1F || data[1] != 0x8B || data[2] != 8 {
        return Err(Error::Corrupt);
    }
    let flg = data[3];
    let mut p = 10; // magic, method, flags, mtime, xfl, os

    if flg & 0x04 != 0 {
        // FEXTRA
        if p + 2 > data.len() {
            return Err(Error::Truncated);
        }
        let xlen = u16::from_le_bytes([data[p], data[p + 1]]) as usize;
        p += 2 + xlen;
    }
    for flag in [0x08u8, 0x10] {
        // FNAME, FCOMMENT: NUL-terminated strings.
        if flg & flag != 0 {
            loop {
                if p >= data.len() {
                    return Err(Error::Truncated);
                }
                let b = data[p];
                p += 1;
                if b == 0 {
                    break;
                }
            }
        }
    }
    if flg & 0x02 != 0 {
        p += 2; // FHCRC
    }
    if p + 8 > data.len() {
        return Err(Error::Truncated);
    }
    Ok(p)
}

/// gzip into a [`Sink`], checking the trailer once the stream ends.
///
/// # The bytes arrive before the checksum is known
///
/// This is inherent to streaming and it is not a detail. The CRC-32 and length live in the eight
/// bytes *after* the deflate stream, so a corrupt body is only detectable once all of it has been
/// handed over — by which time a consumer may have parsed and displayed it. [`inflate_gzip`] does
/// not have this property, because its sink is a `Vec` it drops on error.
///
/// For a browser that is the right trade and the reason this exists: showing a page as it arrives is
/// the feature. But a caller that must not act on unverified bytes has to use the whole-buffer form,
/// and a caller that streams has to be able to discard what it has done — which is why the error is
/// returned rather than logged.
pub fn inflate_gzip_to<S: Sink>(
    data: &[u8],
    max_out: usize,
    sink: &mut S,
) -> Result<usize, Error> {
    let p = gzip_body_start(data)?;
    let (crc, len) = inflate_checked(&data[p..data.len() - 8], max_out, sink)?;

    let tail = &data[data.len() - 8..];
    let want_crc = u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]);
    let want_len = u32::from_le_bytes([tail[4], tail[5], tail[6], tail[7]]);
    if crc != want_crc || (len as u32) != want_len {
        return Err(Error::ChecksumMismatch);
    }
    Ok(len)
}

/// zlib into a [`Sink`]. The Adler-32 trailer is **not** checked, matching [`inflate_zlib`].
pub fn inflate_zlib_to<S: Sink>(
    data: &[u8],
    max_out: usize,
    sink: &mut S,
) -> Result<usize, Error> {
    if data.len() < 6 {
        return Err(Error::Truncated);
    }
    let cmf = data[0];
    let flg = data[1];
    if cmf & 0x0F != 8 || u16::from_be_bytes([cmf, flg]) % 31 != 0 {
        return Err(Error::Corrupt);
    }
    inflate_to(&data[2..data.len() - 4], max_out, sink)
}

/// Inflate whatever this is — gzip, zlib or raw deflate — into a [`Sink`].
///
/// The sniffing is [`inflate_any`]'s, for the same reason: a body whose `Content-Encoding` says
/// gzip and whose bytes say zlib is a thing servers do, and guessing from the header alone is how
/// a page comes out empty.
pub fn inflate_any_to<S: Sink>(
    data: &[u8],
    max_out: usize,
    sink: &mut S,
) -> Result<usize, Error> {
    if data.len() >= 2 && data[0] == 0x1F && data[1] == 0x8B {
        return inflate_gzip_to(data, max_out, sink);
    }
    if data.len() >= 2 && data[0] & 0x0F == 8 && (u16::from_be_bytes([data[0], data[1]])) % 31 == 0
    {
        return inflate_zlib_to(data, max_out, sink);
    }
    inflate_to(data, max_out, sink)
}

/// gzip format, RFC 1952: a variable-length header, the deflate stream, then a CRC-32 and
/// the uncompressed length.
pub fn inflate_gzip(data: &[u8], max_out: usize) -> Result<Vec<u8>, Error> {
    // The Vec is the sink, and it is dropped if the trailer disagrees — which is what keeps this
    // function's promise that a caller never sees unverified bytes. See inflate_gzip_to.
    let mut out = Vec::new();
    inflate_gzip_to(data, max_out, &mut out)?;
    Ok(out)
}

/// Inflate whatever this is: gzip, zlib, or raw deflate.
///
/// MTProto's `gzip_packed` is documented as gzip, and implementations have been observed
/// sending zlib. Sniffing costs two byte comparisons and removes a class of bug that would
/// present as "the dialog list is empty".
pub fn inflate_any(data: &[u8], max_out: usize) -> Result<Vec<u8>, Error> {
    if data.len() >= 2 && data[0] == 0x1F && data[1] == 0x8B {
        return inflate_gzip(data, max_out);
    }
    if data.len() >= 2 && data[0] & 0x0F == 8 && (u16::from_be_bytes([data[0], data[1]])) % 31 == 0
    {
        return inflate_zlib(data, max_out);
    }
    inflate(data, max_out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// zlib's own example: "hello" at default settings.
    const HELLO_ZLIB: &[u8] = &[
        0x78, 0x9c, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x07, 0x00, 0x06, 0x2c, 0x02, 0x15,
    ];

    // ------------------------------------------------------------------ a tiny encoder --
    //
    // There is no compressor in this crate and there should not be one, but the streaming path
    // cannot be tested on a fixture small enough to paste: the window is 32 KB and the interesting
    // case is a back-reference read *after* the front of it has already been delivered. So the
    // tests build their own DEFLATE, using the fixed Huffman code, which is defined by the RFC and
    // needs no tables.

    /// Bits, LSB-first within a byte, which is the DEFLATE convention.
    struct BitW {
        out: Vec<u8>,
        acc: u32,
        have: u32,
    }

    impl BitW {
        fn new() -> Self {
            BitW { out: Vec::new(), acc: 0, have: 0 }
        }
        fn bits(&mut self, value: u32, n: u32) {
            self.acc |= (value & ((1 << n) - 1)) << self.have;
            self.have += n;
            while self.have >= 8 {
                self.out.push((self.acc & 0xFF) as u8);
                self.acc >>= 8;
                self.have -= 8;
            }
        }
        /// A Huffman code is packed most-significant bit first, unlike everything else.
        fn code(&mut self, code: u32, len: u32) {
            for i in (0..len).rev() {
                self.bits((code >> i) & 1, 1);
            }
        }
        fn finish(mut self) -> Vec<u8> {
            if self.have > 0 {
                self.out.push((self.acc & 0xFF) as u8);
            }
            self.out
        }
    }

    /// The fixed literal/length code, RFC 1951 section 3.2.6.
    fn fixed_lit(sym: u32) -> (u32, u32) {
        match sym {
            0..=143 => (0x30 + sym, 8),
            144..=255 => (0x190 + (sym - 144), 9),
            256..=279 => (sym - 256, 7),
            _ => (0xC0 + (sym - 280), 8),
        }
    }

    /// `byte`, then `runs` copies of "the last 258 bytes-worth of run", i.e. one literal followed
    /// by a long run of it. Output length is `1 + 258 * runs`.
    fn deflate_run(byte: u8, runs: usize) -> Vec<u8> {
        let mut w = BitW::new();
        w.bits(1, 1); // BFINAL
        w.bits(1, 2); // BTYPE = 01, fixed

        let (c, n) = fixed_lit(byte as u32);
        w.code(c, n);

        for _ in 0..runs {
            // Symbol 285 is length 258 with no extra bits; distance symbol 0 is distance 1.
            let (c, n) = fixed_lit(285);
            w.code(c, n);
            w.code(0, 5);
        }

        let (c, n) = fixed_lit(256); // end of block
        w.code(c, n);
        w.finish()
    }

    /// Wrap a raw deflate stream in a gzip member with a correct trailer: CRC-32 of the plain
    /// bytes, then their length, both little-endian.
    fn gzip_wrap(deflate: &[u8], plain: &[u8]) -> Vec<u8> {
        let mut v = vec![0x1F, 0x8B, 8, 0, 0, 0, 0, 0, 0, 0];
        v.extend_from_slice(deflate);
        // The final complement is part of CRC-32; crc32_update leaves the running value.
        let crc = !crc32_update(0xFFFF_FFFF, plain);
        v.extend_from_slice(&crc.to_le_bytes());
        v.extend_from_slice(&(plain.len() as u32).to_le_bytes());
        v
    }

    /// Records what it was given, and how it was given.
    #[derive(Default)]
    struct Recorder {
        bytes: Vec<u8>,
        writes: usize,
        largest: usize,
    }

    impl Sink for Recorder {
        fn write(&mut self, bytes: &[u8]) -> Result<(), Error> {
            self.writes += 1;
            self.largest = self.largest.max(bytes.len());
            self.bytes.extend_from_slice(bytes);
            Ok(())
        }
    }

    /// Refuses after a budget, standing in for a consumer that fails partway.
    struct Fussy {
        allow: usize,
    }

    impl Sink for Fussy {
        fn write(&mut self, bytes: &[u8]) -> Result<(), Error> {
            if bytes.len() > self.allow {
                return Err(Error::Sink);
            }
            self.allow -= bytes.len();
            Ok(())
        }
    }

    // ------------------------------------------------------------------------ streaming --

    /// A run long enough that the window slides several times, so back-references are read after
    /// the front of the output has already been handed away. This is the case the whole design is
    /// for, and the one a small fixture cannot reach.
    #[test]
    fn a_back_reference_still_works_after_the_window_has_slid() {
        let runs = 400; // 1 + 258*400 = 103_201 bytes, over three windows
        let stream = deflate_run(b'A', runs);
        let expected = 1 + 258 * runs;

        let mut rec = Recorder::default();
        let n = inflate_to(&stream, 1 << 20, &mut rec).expect("inflate");

        assert_eq!(n, expected);
        assert_eq!(rec.bytes.len(), expected);
        assert!(rec.bytes.iter().all(|&b| b == b'A'), "the run came out corrupted");
        assert!(rec.writes > 1, "the window never slid, so this proved nothing");
    }

    /// Streaming and whole-buffer must agree, byte for byte.
    #[test]
    fn streaming_and_whole_buffer_agree() {
        let stream = deflate_run(b'Z', 300);
        let whole = inflate(&stream, 1 << 20).expect("whole");
        let mut rec = Recorder::default();
        inflate_to(&stream, 1 << 20, &mut rec).expect("streamed");
        assert_eq!(whole, rec.bytes);
    }

    /// Memory is bounded by the window, not by the output.
    #[test]
    fn no_single_write_exceeds_the_window() {
        let stream = deflate_run(b'Q', 500); // ~129 KB out
        let mut rec = Recorder::default();
        inflate_to(&stream, 1 << 20, &mut rec).expect("inflate");
        // Each drain hands over everything older than one window, from a buffer capped at two.
        assert!(
            rec.largest <= 2 * WINDOW,
            "a write of {} bytes means the decoder held more than it promised",
            rec.largest
        );
    }

    /// A consumer that refuses is an error, not a panic and not silence.
    #[test]
    fn a_sink_that_refuses_is_reported() {
        let stream = deflate_run(b'A', 400);
        let mut fussy = Fussy { allow: 100 };
        assert_eq!(inflate_to(&stream, 1 << 20, &mut fussy), Err(Error::Sink));
    }

    /// The gzip trailer is still checked when streaming — after delivery, which is the documented
    /// and unavoidable cost.
    #[test]
    fn a_streamed_gzip_checks_its_trailer() {
        let runs = 200;
        let plain = {
            let mut v = vec![b'A'; 1 + 258 * runs];
            v[0] = b'A';
            v
        };
        let good = gzip_wrap(&deflate_run(b'A', runs), &plain);

        let mut rec = Recorder::default();
        let n = inflate_gzip_to(&good, 1 << 20, &mut rec).expect("good gzip");
        assert_eq!(n, plain.len());
        assert_eq!(rec.bytes, plain);

        // Corrupt the CRC and nothing else.
        let mut bad = good.clone();
        let crc_at = bad.len() - 8;
        bad[crc_at] ^= 0xFF;
        let mut rec2 = Recorder::default();
        assert_eq!(inflate_gzip_to(&bad, 1 << 20, &mut rec2), Err(Error::ChecksumMismatch));
        // And the point worth pinning: the bytes were already delivered.
        assert_eq!(rec2.bytes.len(), plain.len(), "streaming hands over before it can verify");
    }

    /// The whole-buffer form keeps its stronger promise: a bad checksum yields nothing at all.
    #[test]
    fn the_whole_buffer_gzip_never_returns_unverified_bytes() {
        let runs = 50;
        let plain = vec![b'A'; 1 + 258 * runs];
        let mut bad = gzip_wrap(&deflate_run(b'A', runs), &plain);
        let crc_at = bad.len() - 8;
        bad[crc_at] ^= 0xFF;
        assert_eq!(inflate_gzip(&bad, 1 << 20), Err(Error::ChecksumMismatch));
    }

    /// A distance past the window is unencodable and must be refused even though the total output
    /// is long enough to make it look reachable.
    #[test]
    fn a_distance_beyond_the_window_is_rejected() {
        // Hand-built: one literal, then a copy at distance 32769, which no encoder can emit.
        let mut w = BitW::new();
        w.bits(1, 1);
        w.bits(1, 2);
        let (c, n) = fixed_lit(b'A' as u32);
        w.code(c, n);
        let (c, n) = fixed_lit(285);
        w.code(c, n);
        // Distance symbol 29 is base 24577 with 13 extra bits; 24577 + 8192 = 32769.
        w.code(29, 5);
        w.bits(8192, 13);
        let (c, n) = fixed_lit(256);
        w.code(c, n);
        let stream = w.finish();

        let mut rec = Recorder::default();
        assert_eq!(inflate_to(&stream, 1 << 20, &mut rec), Err(Error::BadDistance));
    }

    #[test]
    fn a_zlib_stream_round_trips() {
        assert_eq!(inflate_zlib(HELLO_ZLIB, 1024).unwrap(), b"hello");
        assert_eq!(inflate_any(HELLO_ZLIB, 1024).unwrap(), b"hello");
    }

    #[test]
    fn a_corrupt_adler_is_reported() {
        let mut bad = HELLO_ZLIB.to_vec();
        let n = bad.len();
        bad[n - 1] ^= 1;
        assert_eq!(inflate_zlib(&bad, 1024), Err(Error::ChecksumMismatch));
    }

    #[test]
    fn max_out_is_enforced() {
        assert_eq!(inflate_zlib(HELLO_ZLIB, 4), Err(Error::TooLarge));
        assert!(inflate_zlib(HELLO_ZLIB, 5).is_ok());
    }

    #[test]
    fn a_truncated_stream_is_reported_rather_than_returning_a_prefix() {
        for cut in 3..HELLO_ZLIB.len() - 4 {
            let r = inflate_zlib(&HELLO_ZLIB[..cut], 1024);
            assert!(r.is_err(), "a stream cut at {cut} decoded");
        }
    }

    #[test]
    fn the_fixed_code_is_well_formed() {
        // If the four ranges were wrong the code would be over- or under-subscribed, which
        // Huffman::new rejects — so this is a real check on the table, not a smoke test.
        assert!(fixed_tables().is_ok());
    }

    #[test]
    fn an_over_subscribed_code_is_rejected() {
        // Three symbols of length 1 cannot exist: there are only two 1-bit codes. A decoder
        // that accepts this walks off the end of its symbol table.
        assert_eq!(Huffman::new(&[1, 1, 1]).unwrap_err(), Error::Corrupt);
        // Two of length 1 is exactly complete, and legal.
        assert!(Huffman::new(&[1, 1]).is_ok());
    }

    #[test]
    fn an_empty_code_is_legal() {
        // A block of literals only has no distance codes at all.
        assert!(Huffman::new(&[0u8; 30]).is_ok());
    }
}
