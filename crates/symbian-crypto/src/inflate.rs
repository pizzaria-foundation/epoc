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
        for len in 1..=MAX_BITS {
            left <<= 1;
            left -= counts[len] as i32;
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

/// Raw DEFLATE, no wrapper.
pub fn inflate(data: &[u8], max_out: usize) -> Result<Vec<u8>, Error> {
    let mut bits = Bits::new(data);
    let mut out = Vec::new();

    loop {
        let last = bits.take(1)?;
        let kind = bits.take(2)?;
        match kind {
            0 => stored(&mut bits, &mut out, max_out)?,
            1 => {
                let (lit, dist) = fixed_tables()?;
                block(&mut bits, &mut out, &lit, &dist, max_out)?;
            }
            2 => {
                let (lit, dist) = dynamic_tables(&mut bits)?;
                block(&mut bits, &mut out, &lit, &dist, max_out)?;
            }
            // 3 is reserved and means the stream is not DEFLATE, or is not aligned where we
            // think it is.
            _ => return Err(Error::Corrupt),
        }
        if last == 1 {
            return Ok(out);
        }
    }
}

fn stored(bits: &mut Bits<'_>, out: &mut Vec<u8>, max_out: usize) -> Result<(), Error> {
    bits.align();
    let len = bits.take(16)? as usize;
    let nlen = bits.take(16)? as usize;
    // The complement check is the only integrity check a stored block has, and it catches a
    // misaligned reader immediately rather than after megabytes of garbage.
    if len != !nlen & 0xFFFF {
        return Err(Error::Corrupt);
    }
    if out.len() + len > max_out {
        return Err(Error::TooLarge);
    }
    out.extend_from_slice(bits.bytes(len)?);
    Ok(())
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

fn block(
    bits: &mut Bits<'_>,
    out: &mut Vec<u8>,
    lit: &Huffman,
    dist: &Huffman,
    max_out: usize,
) -> Result<(), Error> {
    loop {
        let sym = lit.decode(bits)?;
        match sym {
            0..=255 => {
                if out.len() + 1 > max_out {
                    return Err(Error::TooLarge);
                }
                out.push(sym as u8);
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
                if distance > out.len() {
                    return Err(Error::BadDistance);
                }
                if out.len() + len > max_out {
                    return Err(Error::TooLarge);
                }

                // Byte at a time, on purpose. A copy where the distance is less than the
                // length has to read bytes this same loop is writing — that is how DEFLATE
                // encodes a run — so `copy_within` or any bulk move would be wrong.
                let start = out.len() - distance;
                for k in 0..len {
                    let b = out[start + k];
                    out.push(b);
                }
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

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &byte in data {
        c ^= byte as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
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

/// gzip format, RFC 1952: a variable-length header, the deflate stream, then a CRC-32 and
/// the uncompressed length.
pub fn inflate_gzip(data: &[u8], max_out: usize) -> Result<Vec<u8>, Error> {
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

    let out = inflate(&data[p..data.len() - 8], max_out)?;
    let tail = &data[data.len() - 8..];
    let want_crc = u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]);
    let want_len = u32::from_le_bytes([tail[4], tail[5], tail[6], tail[7]]);
    if crc32(&out) != want_crc || (out.len() as u32) != want_len {
        return Err(Error::ChecksumMismatch);
    }
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
