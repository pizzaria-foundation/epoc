//! SHA-256, FIPS 180-4.
//!
//! Written because it exists nowhere on this platform: not in Symbian's `hash.dso`, which
//! stops at SHA-1, and not in Open C's OpenSSL 0.9.8a, which predates it. MTProto 2.0 uses
//! it for every message key and every KDF, so there is no version of this project that
//! does not need it.
//!
//! Streaming rather than one-shot only, because the things that get hashed here are built
//! up in pieces — an auth key, a salt, a session id and a payload — and buffering them into
//! one allocation first would mean holding two copies of a message.

/// The first 32 bits of the fractional parts of the cube roots of the first 64 primes.
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
    0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
    0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
    0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
    0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
    0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
    0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
    0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
    0xc67178f2,
];

/// The first 32 bits of the fractional parts of the square roots of the first 8 primes.
const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
    0x5be0cd19,
];

pub const DIGEST_LEN: usize = 32;
pub const BLOCK_LEN: usize = 64;

#[derive(Clone)]
pub struct Sha256 {
    h: [u32; 8],
    /// Partial block. SHA-256 processes 64 bytes at a time, and `update` may be called
    /// with any length, so whatever does not complete a block waits here.
    buf: [u8; BLOCK_LEN],
    buf_len: usize,
    /// Total input length in **bits**, which is what the padding encodes. Counted in bits
    /// rather than bytes and multiplied at the end because the field is 64 bits either way
    /// and this makes the padding code read like the specification.
    bits: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    pub const fn new() -> Self {
        Sha256 { h: H0, buf: [0; BLOCK_LEN], buf_len: 0, bits: 0 }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.bits = self.bits.wrapping_add((data.len() as u64) * 8);

        // Finish the partial block first, if there is one.
        if self.buf_len > 0 {
            let want = BLOCK_LEN - self.buf_len;
            let take = want.min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len == BLOCK_LEN {
                let block = self.buf;
                self.compress(&block);
                self.buf_len = 0;
            }
        }

        // Whole blocks straight from the input, with no copy through the buffer.
        while data.len() >= BLOCK_LEN {
            let (block, rest) = data.split_at(BLOCK_LEN);
            let mut b = [0u8; BLOCK_LEN];
            b.copy_from_slice(block);
            self.compress(&b);
            data = rest;
        }

        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.buf_len = data.len();
        }
    }

    /// Consume the state and produce the digest.
    ///
    /// By value, so a hash cannot be finalised twice — the padding mutates the state, and a
    /// second `finish` on the same value would hash the padding as data and return
    /// something that looks like a digest and is not one.
    pub fn finish(mut self) -> [u8; DIGEST_LEN] {
        let bits = self.bits;

        // Padding: a single 1 bit, then zeros, then the length as a 64-bit big-endian
        // count — arranged so the total is a multiple of 64 bytes.
        self.pad_and_update(&[0x80]);
        while self.buf_len != BLOCK_LEN - 8 {
            self.pad_and_update(&[0x00]);
        }
        self.pad_and_update(&bits.to_be_bytes());

        let mut out = [0u8; DIGEST_LEN];
        for (i, word) in self.h.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    /// Feed padding bytes without counting them towards the length.
    fn pad_and_update(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.buf[self.buf_len] = b;
            self.buf_len += 1;
            if self.buf_len == BLOCK_LEN {
                let block = self.buf;
                self.compress(&block);
                self.buf_len = 0;
            }
        }
    }

    fn compress(&mut self, block: &[u8; BLOCK_LEN]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (dst, v) in self.h.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *dst = dst.wrapping_add(v);
        }
    }
}

/// One-shot, for the common case.
pub fn sha256(data: &[u8]) -> [u8; DIGEST_LEN] {
    let mut h = Sha256::new();
    h.update(data);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    fn hex(b: &[u8]) -> alloc::string::String {
        use core::fmt::Write;
        let mut s = alloc::string::String::new();
        for x in b {
            let _ = write!(s, "{x:02x}");
        }
        s
    }

    /// FIPS 180-4 and the NIST example set. These vectors are the specification; if they
    /// pass, the implementation is correct, and no amount of code review substitutes.
    #[test]
    fn nist_vectors() {
        let cases: &[(&[u8], &str)] = &[
            (b"", "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
            (b"abc", "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
            (
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
                "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
            ),
            (
                b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmno\
                  ijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu",
                "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1",
            ),
        ];
        for (input, want) in cases {
            // The fourth case has whitespace from the line continuation; strip it.
            let input: Vec<u8> =
                input.iter().copied().filter(|b| !b.is_ascii_whitespace()).collect();
            assert_eq!(hex(&sha256(&input)), *want, "input len {}", input.len());
        }
    }

    #[test]
    fn a_million_a_s() {
        // The long-message vector. Also the only test here that exercises the block loop
        // enough times for a carry bug in the bit counter to show up.
        let mut h = Sha256::new();
        for _ in 0..1000 {
            h.update(&[b'a'; 1000]);
        }
        assert_eq!(
            hex(&h.finish()),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn streaming_matches_one_shot_at_every_split() {
        // The padding and the partial-block path are where a hash goes wrong, and they only
        // go wrong at particular lengths — around a block boundary, and around the point
        // where the length field no longer fits in the final block. So every split of a
        // 200-byte input is checked rather than a few chosen ones.
        let data: Vec<u8> = (0..200u32).map(|i| (i * 7 % 251) as u8).collect();
        let want = sha256(&data);
        for split in 0..=data.len() {
            let mut h = Sha256::new();
            h.update(&data[..split]);
            h.update(&data[split..]);
            assert_eq!(h.finish(), want, "split at {split}");
        }
    }

    #[test]
    fn lengths_around_the_padding_boundaries() {
        // 55 and 56 bytes are the interesting ones: at 56 the length field no longer fits
        // in the same block as the 0x80, so finish() has to emit a second block. 63, 64 and
        // 65 exercise the partial-block buffer either side of full.
        for len in [0usize, 1, 54, 55, 56, 57, 63, 64, 65, 119, 120, 127, 128] {
            let data = vec![0xABu8; len];
            let one = sha256(&data);
            let mut h = Sha256::new();
            for chunk in data.chunks(7).filter(|c| !c.is_empty()) {
                h.update(chunk);
            }
            assert_eq!(h.finish(), one, "len {len}");
        }
    }

    #[test]
    fn empty_updates_change_nothing() {
        let mut h = Sha256::new();
        h.update(b"");
        h.update(b"abc");
        h.update(b"");
        assert_eq!(h.finish(), sha256(b"abc"));
    }

    #[test]
    fn a_clone_diverges_from_its_source() {
        // Clone exists so a prefix can be hashed once and extended two ways — an HMAC does
        // exactly that. If the clone shared state, both branches would return the same
        // digest and the bug would look like a protocol error.
        let mut base = Sha256::new();
        base.update(b"prefix");
        let mut a = base.clone();
        let mut b = base;
        a.update(b"one");
        b.update(b"two");
        assert_ne!(a.finish(), b.finish());
    }
}
