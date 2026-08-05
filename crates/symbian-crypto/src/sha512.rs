//! SHA-512, FIPS 180-4.
//!
//! Needed for the 2FA password check: Telegram's SRP construction is PBKDF2-HMAC-SHA-512
//! over a SHA-256 chain. Nothing else in the protocol uses it, which is why it arrives after
//! the rest rather than with SHA-256.
//!
//! Structurally SHA-256 with everything widened: 64-bit words, 80 rounds, 128-byte blocks,
//! and different rotation amounts. Not shared with the SHA-256 code through a generic — the
//! rotation constants and the round count are the algorithm, so a generic version would be
//! two tables and a type parameter wrapped around nothing, and the one thing it would buy
//! (a single copy of the block loop) is the part that is already trivially correct.
//!
//! # Where the constants came from
//!
//! Derived, not transcribed. K is the first 64 bits of the fractional part of the cube root
//! of each of the first 80 primes, and H0 the same for the square roots of the first 8 — so
//! they were computed from that definition to 80 decimal digits and emitted, then checked
//! against the three values FIPS publishes inline. Eighty 64-bit literals is exactly where a
//! transposed digit survives review and then fails only on inputs longer than one block.

/// First 64 bits of the fractional parts of the cube roots of the first 80 primes.
const K: [u64; 80] = [
    0x428a2f98d728ae22, 0x7137449123ef65cd, 0xb5c0fbcfec4d3b2f, 0xe9b5dba58189dbbc,
    0x3956c25bf348b538, 0x59f111f1b605d019, 0x923f82a4af194f9b, 0xab1c5ed5da6d8118,
    0xd807aa98a3030242, 0x12835b0145706fbe, 0x243185be4ee4b28c, 0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f, 0x80deb1fe3b1696b1, 0x9bdc06a725c71235, 0xc19bf174cf692694,
    0xe49b69c19ef14ad2, 0xefbe4786384f25e3, 0x0fc19dc68b8cd5b5, 0x240ca1cc77ac9c65,
    0x2de92c6f592b0275, 0x4a7484aa6ea6e483, 0x5cb0a9dcbd41fbd4, 0x76f988da831153b5,
    0x983e5152ee66dfab, 0xa831c66d2db43210, 0xb00327c898fb213f, 0xbf597fc7beef0ee4,
    0xc6e00bf33da88fc2, 0xd5a79147930aa725, 0x06ca6351e003826f, 0x142929670a0e6e70,
    0x27b70a8546d22ffc, 0x2e1b21385c26c926, 0x4d2c6dfc5ac42aed, 0x53380d139d95b3df,
    0x650a73548baf63de, 0x766a0abb3c77b2a8, 0x81c2c92e47edaee6, 0x92722c851482353b,
    0xa2bfe8a14cf10364, 0xa81a664bbc423001, 0xc24b8b70d0f89791, 0xc76c51a30654be30,
    0xd192e819d6ef5218, 0xd69906245565a910, 0xf40e35855771202a, 0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8, 0x1e376c085141ab53, 0x2748774cdf8eeb99, 0x34b0bcb5e19b48a8,
    0x391c0cb3c5c95a63, 0x4ed8aa4ae3418acb, 0x5b9cca4f7763e373, 0x682e6ff3d6b2b8a3,
    0x748f82ee5defb2fc, 0x78a5636f43172f60, 0x84c87814a1f0ab72, 0x8cc702081a6439ec,
    0x90befffa23631e28, 0xa4506cebde82bde9, 0xbef9a3f7b2c67915, 0xc67178f2e372532b,
    0xca273eceea26619c, 0xd186b8c721c0c207, 0xeada7dd6cde0eb1e, 0xf57d4f7fee6ed178,
    0x06f067aa72176fba, 0x0a637dc5a2c898a6, 0x113f9804bef90dae, 0x1b710b35131c471b,
    0x28db77f523047d84, 0x32caab7b40c72493, 0x3c9ebe0a15c9bebc, 0x431d67c49c100d4c,
    0x4cc5d4becb3e42b6, 0x597f299cfc657e2a, 0x5fcb6fab3ad6faec, 0x6c44198c4a475817,
];

/// First 64 bits of the fractional parts of the square roots of the first 8 primes.
const H0: [u64; 8] = [
    0x6a09e667f3bcc908, 0xbb67ae8584caa73b, 0x3c6ef372fe94f82b, 0xa54ff53a5f1d36f1,
    0x510e527fade682d1, 0x9b05688c2b3e6c1f, 0x1f83d9abfb41bd6b, 0x5be0cd19137e2179,
];

pub const DIGEST_LEN: usize = 64;
pub const BLOCK_LEN: usize = 128;

#[derive(Clone)]
pub struct Sha512 {
    h: [u64; 8],
    buf: [u8; BLOCK_LEN],
    buf_len: usize,
    /// Total input length in bits. The padding field is 128 bits wide; the high half is
    /// written as zero, which is exact for any input under 2 exabytes.
    bits: u64,
}

impl Default for Sha512 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha512 {
    pub const fn new() -> Self {
        Sha512 { h: H0, buf: [0; BLOCK_LEN], buf_len: 0, bits: 0 }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.bits = self.bits.wrapping_add((data.len() as u64) * 8);

        if self.buf_len > 0 {
            let take = (BLOCK_LEN - self.buf_len).min(data.len());
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
        // The length occupies the last 16 bytes, not 8 — the single place this differs from
        // SHA-256 in a way that is easy to miss, because getting it wrong still produces a
        // plausible digest for inputs of the right length.
        while self.buf_len != BLOCK_LEN - 16 {
            self.pad(&[0x00]);
        }
        self.pad(&[0u8; 8]);
        self.pad(&bits.to_be_bytes());

        let mut out = [0u8; DIGEST_LEN];
        for (i, word) in self.h.iter().enumerate() {
            out[i * 8..i * 8 + 8].copy_from_slice(&word.to_be_bytes());
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
        let mut w = [0u64; 80];
        for i in 0..16 {
            let mut b = [0u8; 8];
            b.copy_from_slice(&block[i * 8..i * 8 + 8]);
            w[i] = u64::from_be_bytes(b);
        }
        for i in 16..80 {
            let s0 = w[i - 15].rotate_right(1) ^ w[i - 15].rotate_right(8) ^ (w[i - 15] >> 7);
            let s1 = w[i - 2].rotate_right(19) ^ w[i - 2].rotate_right(61) ^ (w[i - 2] >> 6);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.h;
        for i in 0..80 {
            let s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
            let ch = (e & f) ^ (!e & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
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

pub fn sha512(data: &[u8]) -> [u8; DIGEST_LEN] {
    let mut h = Sha512::new();
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
        assert_eq!(
            hex(&sha512(b"")),
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce\
             47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"
                .chars().filter(|c| !c.is_whitespace()).collect::<alloc::string::String>()
        );
        assert_eq!(
            hex(&sha512(b"abc")),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
             2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
                .chars().filter(|c| !c.is_whitespace()).collect::<alloc::string::String>()
        );
    }

    #[test]
    fn a_million_a_s() {
        let mut h = Sha512::new();
        for _ in 0..1000 {
            h.update(&[b'a'; 1000]);
        }
        assert_eq!(
            hex(&h.finish()),
            "e718483d0ce769644e2e42c7bc15b4638e1f98b13b2044285632a803afa973eb\
             de0ff244877ea60a4cb0432ce577c31beb009c5c2c49aa2e4eadb217ad8cc09b"
                .chars().filter(|c| !c.is_whitespace()).collect::<alloc::string::String>()
        );
    }

    #[test]
    fn streaming_matches_one_shot_at_every_split() {
        let data: Vec<u8> = (0..400u32).map(|i| (i * 17 % 251) as u8).collect();
        let want = sha512(&data);
        for split in 0..=data.len() {
            let mut h = Sha512::new();
            h.update(&data[..split]);
            h.update(&data[split..]);
            assert_eq!(h.finish(), want, "split at {split}");
        }
    }

    #[test]
    fn lengths_around_the_padding_boundaries() {
        // 111 and 112 are the interesting pair: at 112 the 16-byte length field no longer
        // fits in the same block as the 0x80. That is the SHA-512-specific boundary and the
        // one a copy of the SHA-256 padding gets wrong.
        for len in [0usize, 1, 110, 111, 112, 113, 127, 128, 129, 239, 240, 256] {
            let data = vec![0x3Cu8; len];
            let one = sha512(&data);
            let mut h = Sha512::new();
            for chunk in data.chunks(13).filter(|c| !c.is_empty()) {
                h.update(chunk);
            }
            assert_eq!(h.finish(), one, "len {len}");
        }
    }
}
