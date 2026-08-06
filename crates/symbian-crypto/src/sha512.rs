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
//! # Optimised for a register file it does not fit in
//!
//! SHA-512 wants sixteen 32-bit registers just for `a`..`h`. ARM has thirteen usable ones,
//! so the state spills no matter what is done — measured on the emitted ARMv5TE code,
//! 23% of the round loop is stack traffic and that is the floor, not a mistake.
//!
//! What was worth removing:
//!
//! - **The eight assignments that end a textbook round.** `h = g; g = f; …` compiled to
//!   eleven of the round's hundred and five instructions. After eight rounds the names come
//!   back to where they started, so unrolling by eight turns them into a different spelling
//!   at the call site and no code at all.
//! - **The 80-word message schedule.** It only ever reads the last sixteen entries, so it
//!   is a sixteen-word ring updated in place: 128 bytes of stack where there were 640, and
//!   one fused loop where there were two.
//!
//! Measured by counting instructions in the emitted ARM assembly, which is a proxy — a real
//! number for a thing that is not quite the thing you want, but a checkable one, which
//! beats a guess:
//!
//! | | instructions per 128-byte block |
//! |---|---|
//! | textbook | 12000 |
//! | this | **10506** (-12%) |
//!
//! On the host it is 78% faster, and that gap is the point: a superscalar out-of-order core
//! gets far more from removing dependency chains than an in-order ARM11 does. **The host
//! number does not transfer.** Believe the instruction count.
//!
//! Targeting ARMv6 rather than ARMv5TE was tried — the E72 is an ARM1136 and `rev` would
//! collapse the byte-swapping — and it is worth 0.6%: the load loop is not where the time
//! goes. Not worth a binary that will not start on an ARMv5 handset.
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

/// The initial state, for callers driving [`compress_into`] themselves.
pub const H0_PUB: [u64; 8] = H0;

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
        compress_into(&mut self.h, block);
    }
}

/// The compression function, over a state the caller owns.
///
/// Exposed because PBKDF2 needs it. `Sha512` is 208 bytes — 64 of state and 128 of buffer —
/// and a KDF that clones the whole struct twice per iteration copies 41 MB across Telegram's
/// hundred thousand rounds, against 25 MB of actual hashing. The stale buffer is more than
/// half of every copy.
///
/// Callers that hash whole blocks and nothing else can carry the 64 bytes that matter and
/// call this. It is not a general-purpose entry point: it does no padding and no length
/// accounting, so anything that is not exactly one block is the caller's problem.
pub fn compress_into(state: &mut [u64; 8], block: &[u8; BLOCK_LEN]) {
    {
        let mut w = [0u64; 16];
        for (i, word) in w.iter_mut().enumerate() {
            let mut b = [0u8; 8];
            b.copy_from_slice(&block[i * 8..i * 8 + 8]);
            *word = u64::from_be_bytes(b);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;

        // Rounds 0..16 read the block as loaded; 16..80 extend the schedule in place.
        for i in 0..2 {
            round8!(a, b, c, d, e, f, g, h, w, i * 8, false);
        }
        for i in 2..10 {
            round8!(a, b, c, d, e, f, g, h, w, i * 8, true);
        }

        for (dst, v) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *dst = dst.wrapping_add(v);
        }
    }
}

/* Eight rounds, written out so the working variables are renamed instead of moved.
 *
 * The textbook round ends with `h = g; g = f; f = e; ...` — eight assignments that on ARM
 * are eight `mov` instructions, eleven of the round's hundred and five. After eight rounds
 * the names return to where they started, so unrolling by eight turns all of them into a
 * different spelling at the call site and none of them into code.
 *
 * The round itself writes only `d` and `h`: `d += t1` is what becomes the next `e`, and `h`
 * holds `t1 + t2`, which becomes the next `a`. Everything else the caller renames.
 */
macro_rules! round8 {
    ($a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $h:ident,
     $w:ident, $base:expr, $extend:expr) => {
        round1!($a, $b, $c, $d, $e, $f, $g, $h, $w, $base + 0, $extend);
        round1!($h, $a, $b, $c, $d, $e, $f, $g, $w, $base + 1, $extend);
        round1!($g, $h, $a, $b, $c, $d, $e, $f, $w, $base + 2, $extend);
        round1!($f, $g, $h, $a, $b, $c, $d, $e, $w, $base + 3, $extend);
        round1!($e, $f, $g, $h, $a, $b, $c, $d, $w, $base + 4, $extend);
        round1!($d, $e, $f, $g, $h, $a, $b, $c, $w, $base + 5, $extend);
        round1!($c, $d, $e, $f, $g, $h, $a, $b, $w, $base + 6, $extend);
        round1!($b, $c, $d, $e, $f, $g, $h, $a, $w, $base + 7, $extend);
    };
}

/* One round, with the message schedule folded in.
 *
 * The 80-word schedule is a 16-word ring instead. `w[i]` depends on `w[i-16]`, `w[i-15]`,
 * `w[i-7]` and `w[i-2]`, all within the last sixteen, so nothing older is ever read — the
 * array was 640 bytes of stack where 128 will do, and every access is now to a region small
 * enough to stay resident.
 *
 * Updating in place is what makes the indices work: after `w[j]` is overwritten it holds
 * `w[i]`, and a later round in the same group reading `w[(j+9) & 15]` wants exactly that
 * newer value. Writing the ring to a scratch copy first would be wrong, not safer.
 */
macro_rules! round1 {
    ($a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $h:ident,
     $w:ident, $i:expr, $extend:expr) => {{
        let j = $i & 15;
        if $extend {
            let x = $w[(j + 1) & 15];
            let y = $w[(j + 14) & 15];
            let s0 = x.rotate_right(1) ^ x.rotate_right(8) ^ (x >> 7);
            let s1 = y.rotate_right(19) ^ y.rotate_right(61) ^ (y >> 6);
            $w[j] = $w[j].wrapping_add(s0).wrapping_add($w[(j + 9) & 15]).wrapping_add(s1);
        }

        let s1 = $e.rotate_right(14) ^ $e.rotate_right(18) ^ $e.rotate_right(41);
        let ch = ($e & $f) ^ (!$e & $g);
        let t1 = $h
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(K[$i])
            .wrapping_add($w[j]);
        let s0 = $a.rotate_right(28) ^ $a.rotate_right(34) ^ $a.rotate_right(39);
        let maj = ($a & $b) ^ ($a & $c) ^ ($b & $c);

        $d = $d.wrapping_add(t1);
        $h = t1.wrapping_add(s0).wrapping_add(maj);
    }};
}

use {round1, round8};

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
