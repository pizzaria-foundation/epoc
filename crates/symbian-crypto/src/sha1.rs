//! SHA-1, FIPS 180-4.
//!
//! Symbian has this one — `CSHA1` in `hash.dso`. It is written here anyway, for two
//! reasons that are not "avoid the platform on principle".
//!
//! It is testable. A `CSHA1` call has to be wrapped in a shim function, and then the only
//! way to check it is on the phone; this runs against FIPS vectors in `cargo test`.
//!
//! And it is 80 lines. The shim function, the ABI entry, the host stub and the safe wrapper
//! would together be more code than the algorithm, and would drag a device dependency into
//! anything that wants to hash — including the host tests of code that only hashes as a
//! step towards something else.
//!
//! # SHA-1 is broken, and that is fine here
//!
//! Collisions are practical (SHAttered, 2017), so SHA-1 must not be used where an attacker
//! chooses the input and a collision would matter — signatures, certificates, content
//! addressing.
//!
//! MTProto's use is not that. In the auth handshake SHA-1 appears inside `RSA_PAD` and in
//! the `new_nonce_hash` values, where the inputs are nonces both sides contributed to and
//! the property needed is second-preimage resistance, which is not affected. The SIS
//! signature format also requires SHA-1 specifically, and the device accepts nothing else —
//! there is no choice to make there.

pub const DIGEST_LEN: usize = 20;
pub const BLOCK_LEN: usize = 64;

const H0: [u32; 5] = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476, 0xc3d2e1f0];

#[derive(Clone)]
pub struct Sha1 {
    h: [u32; 5],
    buf: [u8; BLOCK_LEN],
    buf_len: usize,
    bits: u64,
}

impl Default for Sha1 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha1 {
    pub const fn new() -> Self {
        Sha1 { h: H0, buf: [0; BLOCK_LEN], buf_len: 0, bits: 0 }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.bits = self.bits.wrapping_add((data.len() as u64) * 8);

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

    pub fn finish(mut self) -> [u8; DIGEST_LEN] {
        let bits = self.bits;
        self.pad(&[0x80]);
        while self.buf_len != BLOCK_LEN - 8 {
            self.pad(&[0x00]);
        }
        self.pad(&bits.to_be_bytes());

        let mut out = [0u8; DIGEST_LEN];
        for (i, word) in self.h.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    fn pad(&mut self, bytes: &[u8]) {
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
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let [mut a, mut b, mut c, mut d, mut e] = self.h;
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5a827999),
                20..=39 => (b ^ c ^ d, 0x6ed9eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1bbcdc),
                _ => (b ^ c ^ d, 0xca62c1d6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (dst, v) in self.h.iter_mut().zip([a, b, c, d, e]) {
            *dst = dst.wrapping_add(v);
        }
    }
}

pub fn sha1(data: &[u8]) -> [u8; DIGEST_LEN] {
    let mut h = Sha1::new();
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

    #[test]
    fn fips_vectors() {
        let cases: &[(&[u8], &str)] = &[
            (b"", "da39a3ee5e6b4b0d3255bfef95601890afd80709"),
            (b"abc", "a9993e364706816aba3e25717850c26c9cd0d89d"),
            (
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
                "84983e441c3bd26ebaae4aa1f95129e5e54670f1",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(hex(&sha1(input)), *want, "len {}", input.len());
        }
    }

    #[test]
    fn a_million_a_s() {
        let mut h = Sha1::new();
        for _ in 0..1000 {
            h.update(&[b'a'; 1000]);
        }
        assert_eq!(hex(&h.finish()), "34aa973cd4c4daa4f61eeb2bdbad27316534016f");
    }

    #[test]
    fn streaming_matches_one_shot_at_every_split() {
        let data: Vec<u8> = (0..200u32).map(|i| (i * 11 % 251) as u8).collect();
        let want = sha1(&data);
        for split in 0..=data.len() {
            let mut h = Sha1::new();
            h.update(&data[..split]);
            h.update(&data[split..]);
            assert_eq!(h.finish(), want, "split at {split}");
        }
    }

    #[test]
    fn lengths_around_the_padding_boundaries() {
        for len in [0usize, 1, 55, 56, 57, 63, 64, 65, 119, 120, 128] {
            let data = vec![0x5Au8; len];
            let one = sha1(&data);
            let mut h = Sha1::new();
            for chunk in data.chunks(9).filter(|c| !c.is_empty()) {
                h.update(chunk);
            }
            assert_eq!(h.finish(), one, "len {len}");
        }
    }
}
