//! AES, FIPS 197. The block cipher only — modes are in [`crate::ige`].
//!
//! All three key lengths, because the code is the same for each and only the schedule
//! differs. MTProto uses AES-256; SIS packaging and anything else here does not use AES at
//! all, so 128 and 192 exist for the tests, which is worth it: FIPS 197's own examples
//! cover all three, and a key-schedule bug that only shows at one length is exactly the
//! kind that survives a single-length test.
//!
//! # Size
//!
//! The forward and inverse S-boxes are 256 bytes each and the round tables are computed on
//! the fly rather than stored, so the whole thing is under 1 KB of `.rodata` against a
//! 106 KB image. A table-driven implementation would be perhaps 30% faster and cost 8 KB;
//! that trade would be worth revisiting only if AES ever turned out to be on a hot path,
//! and on this device the network is four orders of magnitude slower than the cipher.

/// FIPS 197 figure 7.
const SBOX: [u8; 256] = [
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7,
    0xab, 0x76, 0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf,
    0x9c, 0xa4, 0x72, 0xc0, 0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5,
    0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15, 0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a,
    0x07, 0x12, 0x80, 0xe2, 0xeb, 0x27, 0xb2, 0x75, 0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e,
    0x5a, 0xa0, 0x52, 0x3b, 0xd6, 0xb3, 0x29, 0xe3, 0x2f, 0x84, 0x53, 0xd1, 0x00, 0xed,
    0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf, 0xd0, 0xef,
    0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8,
    0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5, 0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff,
    0xf3, 0xd2, 0xcd, 0x0c, 0x13, 0xec, 0x5f, 0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d,
    0x64, 0x5d, 0x19, 0x73, 0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88, 0x46, 0xee,
    0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb, 0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c,
    0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79, 0xe7, 0xc8, 0x37, 0x6d, 0x8d, 0xd5,
    0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08, 0xba, 0x78, 0x25, 0x2e,
    0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a, 0x70, 0x3e,
    0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e,
    0xe1, 0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55,
    0x28, 0xdf, 0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f,
    0xb0, 0x54, 0xbb, 0x16,
];

/// FIPS 197 figure 14.
const INV_SBOX: [u8; 256] = [
    0x52, 0x09, 0x6a, 0xd5, 0x30, 0x36, 0xa5, 0x38, 0xbf, 0x40, 0xa3, 0x9e, 0x81, 0xf3,
    0xd7, 0xfb, 0x7c, 0xe3, 0x39, 0x82, 0x9b, 0x2f, 0xff, 0x87, 0x34, 0x8e, 0x43, 0x44,
    0xc4, 0xde, 0xe9, 0xcb, 0x54, 0x7b, 0x94, 0x32, 0xa6, 0xc2, 0x23, 0x3d, 0xee, 0x4c,
    0x95, 0x0b, 0x42, 0xfa, 0xc3, 0x4e, 0x08, 0x2e, 0xa1, 0x66, 0x28, 0xd9, 0x24, 0xb2,
    0x76, 0x5b, 0xa2, 0x49, 0x6d, 0x8b, 0xd1, 0x25, 0x72, 0xf8, 0xf6, 0x64, 0x86, 0x68,
    0x98, 0x16, 0xd4, 0xa4, 0x5c, 0xcc, 0x5d, 0x65, 0xb6, 0x92, 0x6c, 0x70, 0x48, 0x50,
    0xfd, 0xed, 0xb9, 0xda, 0x5e, 0x15, 0x46, 0x57, 0xa7, 0x8d, 0x9d, 0x84, 0x90, 0xd8,
    0xab, 0x00, 0x8c, 0xbc, 0xd3, 0x0a, 0xf7, 0xe4, 0x58, 0x05, 0xb8, 0xb3, 0x45, 0x06,
    0xd0, 0x2c, 0x1e, 0x8f, 0xca, 0x3f, 0x0f, 0x02, 0xc1, 0xaf, 0xbd, 0x03, 0x01, 0x13,
    0x8a, 0x6b, 0x3a, 0x91, 0x11, 0x41, 0x4f, 0x67, 0xdc, 0xea, 0x97, 0xf2, 0xcf, 0xce,
    0xf0, 0xb4, 0xe6, 0x73, 0x96, 0xac, 0x74, 0x22, 0xe7, 0xad, 0x35, 0x85, 0xe2, 0xf9,
    0x37, 0xe8, 0x1c, 0x75, 0xdf, 0x6e, 0x47, 0xf1, 0x1a, 0x71, 0x1d, 0x29, 0xc5, 0x89,
    0x6f, 0xb7, 0x62, 0x0e, 0xaa, 0x18, 0xbe, 0x1b, 0xfc, 0x56, 0x3e, 0x4b, 0xc6, 0xd2,
    0x79, 0x20, 0x9a, 0xdb, 0xc0, 0xfe, 0x78, 0xcd, 0x5a, 0xf4, 0x1f, 0xdd, 0xa8, 0x33,
    0x88, 0x07, 0xc7, 0x31, 0xb1, 0x12, 0x10, 0x59, 0x27, 0x80, 0xec, 0x5f, 0x60, 0x51,
    0x7f, 0xa9, 0x19, 0xb5, 0x4a, 0x0d, 0x2d, 0xe5, 0x7a, 0x9f, 0x93, 0xc9, 0x9c, 0xef,
    0xa0, 0xe0, 0x3b, 0x4d, 0xae, 0x2a, 0xf5, 0xb0, 0xc8, 0xeb, 0xbb, 0x3c, 0x83, 0x53,
    0x99, 0x61, 0x17, 0x2b, 0x04, 0x7e, 0xba, 0x77, 0xd6, 0x26, 0xe1, 0x69, 0x14, 0x63,
    0x55, 0x21, 0x0c, 0x7d,
];

pub const BLOCK_LEN: usize = 16;

/// The largest schedule: AES-256 has 14 rounds, so 15 round keys of 16 bytes.
const MAX_ROUND_KEYS: usize = 60;

/// Multiply in GF(2^8) with the AES polynomial 0x11b.
///
/// Branch-free on the reduction so the loop shape does not depend on the data, though see
/// the crate docs on why constant time is not the goal here.
fn xmul(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0u8;
    for _ in 0..8 {
        if b & 1 != 0 {
            p ^= a;
        }
        let hi = a & 0x80;
        a <<= 1;
        if hi != 0 {
            a ^= 0x1b;
        }
        b >>= 1;
    }
    p
}

#[derive(Clone)]
pub struct Aes {
    /// Expanded key, as 32-bit words in column order.
    w: [u32; MAX_ROUND_KEYS],
    rounds: usize,
}

impl Aes {
    /// 16, 24 or 32 bytes. Returns `None` for any other length rather than padding or
    /// truncating — a key silently extended with zeros is a catastrophe that produces
    /// perfectly valid-looking ciphertext.
    pub fn new(key: &[u8]) -> Option<Self> {
        let nk = match key.len() {
            16 => 4,
            24 => 6,
            32 => 8,
            _ => return None,
        };
        let rounds = nk + 6;
        let total = 4 * (rounds + 1);

        let mut w = [0u32; MAX_ROUND_KEYS];
        for i in 0..nk {
            w[i] = u32::from_be_bytes([
                key[4 * i],
                key[4 * i + 1],
                key[4 * i + 2],
                key[4 * i + 3],
            ]);
        }

        let mut rcon = 1u8;
        for i in nk..total {
            let mut t = w[i - 1];
            if i % nk == 0 {
                t = sub_word(t.rotate_left(8)) ^ ((rcon as u32) << 24);
                rcon = xmul(rcon, 2);
            } else if nk > 6 && i % nk == 4 {
                // AES-256 only: an extra SubWord with no rotation or round constant. It is
                // the step most often left out, and leaving it out still produces a working
                // cipher — just a different one, which agrees with nothing.
                t = sub_word(t);
            }
            w[i] = w[i - nk] ^ t;
        }

        Some(Aes { w, rounds })
    }

    pub fn encrypt_block(&self, block: &mut [u8; BLOCK_LEN]) {
        self.add_round_key(block, 0);
        for round in 1..self.rounds {
            sub_bytes(block);
            shift_rows(block);
            mix_columns(block);
            self.add_round_key(block, round);
        }
        // The last round omits MixColumns, which is what makes decryption possible at all.
        sub_bytes(block);
        shift_rows(block);
        self.add_round_key(block, self.rounds);
    }

    pub fn decrypt_block(&self, block: &mut [u8; BLOCK_LEN]) {
        self.add_round_key(block, self.rounds);
        for round in (1..self.rounds).rev() {
            inv_shift_rows(block);
            inv_sub_bytes(block);
            self.add_round_key(block, round);
            inv_mix_columns(block);
        }
        inv_shift_rows(block);
        inv_sub_bytes(block);
        self.add_round_key(block, 0);
    }

    fn add_round_key(&self, block: &mut [u8; BLOCK_LEN], round: usize) {
        for col in 0..4 {
            let k = self.w[round * 4 + col].to_be_bytes();
            for row in 0..4 {
                block[col * 4 + row] ^= k[row];
            }
        }
    }
}

fn sub_word(w: u32) -> u32 {
    let b = w.to_be_bytes();
    u32::from_be_bytes([
        SBOX[b[0] as usize],
        SBOX[b[1] as usize],
        SBOX[b[2] as usize],
        SBOX[b[3] as usize],
    ])
}

fn sub_bytes(b: &mut [u8; BLOCK_LEN]) {
    for x in b.iter_mut() {
        *x = SBOX[*x as usize];
    }
}

fn inv_sub_bytes(b: &mut [u8; BLOCK_LEN]) {
    for x in b.iter_mut() {
        *x = INV_SBOX[*x as usize];
    }
}

// The state is column-major: byte `col * 4 + row`. ShiftRows rotates row `r` left by `r`.
fn shift_rows(b: &mut [u8; BLOCK_LEN]) {
    for row in 1..4 {
        let mut tmp = [0u8; 4];
        for col in 0..4 {
            tmp[col] = b[((col + row) % 4) * 4 + row];
        }
        for col in 0..4 {
            b[col * 4 + row] = tmp[col];
        }
    }
}

fn inv_shift_rows(b: &mut [u8; BLOCK_LEN]) {
    for row in 1..4 {
        let mut tmp = [0u8; 4];
        for col in 0..4 {
            tmp[(col + row) % 4] = b[col * 4 + row];
        }
        for col in 0..4 {
            b[col * 4 + row] = tmp[col];
        }
    }
}

fn mix_columns(b: &mut [u8; BLOCK_LEN]) {
    for col in 0..4 {
        let c = [b[col * 4], b[col * 4 + 1], b[col * 4 + 2], b[col * 4 + 3]];
        b[col * 4] = xmul(c[0], 2) ^ xmul(c[1], 3) ^ c[2] ^ c[3];
        b[col * 4 + 1] = c[0] ^ xmul(c[1], 2) ^ xmul(c[2], 3) ^ c[3];
        b[col * 4 + 2] = c[0] ^ c[1] ^ xmul(c[2], 2) ^ xmul(c[3], 3);
        b[col * 4 + 3] = xmul(c[0], 3) ^ c[1] ^ c[2] ^ xmul(c[3], 2);
    }
}

fn inv_mix_columns(b: &mut [u8; BLOCK_LEN]) {
    for col in 0..4 {
        let c = [b[col * 4], b[col * 4 + 1], b[col * 4 + 2], b[col * 4 + 3]];
        b[col * 4] = xmul(c[0], 14) ^ xmul(c[1], 11) ^ xmul(c[2], 13) ^ xmul(c[3], 9);
        b[col * 4 + 1] = xmul(c[0], 9) ^ xmul(c[1], 14) ^ xmul(c[2], 11) ^ xmul(c[3], 13);
        b[col * 4 + 2] = xmul(c[0], 13) ^ xmul(c[1], 9) ^ xmul(c[2], 14) ^ xmul(c[3], 11);
        b[col * 4 + 3] = xmul(c[0], 11) ^ xmul(c[1], 13) ^ xmul(c[2], 9) ^ xmul(c[3], 14);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn hex(b: &[u8]) -> alloc::string::String {
        use core::fmt::Write;
        let mut s = alloc::string::String::new();
        for x in b {
            let _ = write!(s, "{x:02x}");
        }
        s
    }

    /// FIPS 197 appendix C, all three key lengths. These are the definition.
    #[test]
    fn fips197_appendix_c() {
        let pt = unhex("00112233445566778899aabbccddeeff");
        let cases = [
            ("000102030405060708090a0b0c0d0e0f", "69c4e0d86a7b0430d8cdb78070b4c55a"),
            (
                "000102030405060708090a0b0c0d0e0f1011121314151617",
                "dda97ca4864cdfe06eaf70a0ec0d7191",
            ),
            (
                "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
                "8ea2b7ca516745bfeafc49904b496089",
            ),
        ];
        for (key, want) in cases {
            let aes = Aes::new(&unhex(key)).unwrap();
            let mut b = [0u8; 16];
            b.copy_from_slice(&pt);
            aes.encrypt_block(&mut b);
            assert_eq!(hex(&b), want, "key len {}", key.len() / 2);

            aes.decrypt_block(&mut b);
            assert_eq!(hex(&b), hex(&pt), "decrypt did not invert, key len {}", key.len() / 2);
        }
    }

    #[test]
    fn nist_sp800_38a_block_vectors() {
        // From the AES-256 ECB section of SP 800-38A: a second, independent key.
        let aes =
            Aes::new(&unhex("603deb1015ca71be2b73aef0857d77811f352c073b6108d72d9810a30914dff4"))
                .unwrap();
        let cases = [
            ("6bc1bee22e409f96e93d7e117393172a", "f3eed1bdb5d2a03c064b5a7e3db181f8"),
            ("ae2d8a571e03ac9c9eb76fac45af8e51", "591ccb10d410ed26dc5ba74a31362870"),
            ("30c81c46a35ce411e5fbc1191a0a52ef", "b6ed21b99ca6f4f9f153e7b1beafed1d"),
            ("f69f2445df4f9b17ad2b417be66c3710", "23304b7a39f9f3ff067d8d8f9e24ecc7"),
        ];
        for (pt, want) in cases {
            let mut b = [0u8; 16];
            b.copy_from_slice(&unhex(pt));
            aes.encrypt_block(&mut b);
            assert_eq!(hex(&b), want);
        }
    }

    #[test]
    fn a_wrong_key_length_is_rejected() {
        // Not padded, not truncated. A key silently extended with zeros produces
        // perfectly valid-looking ciphertext that nothing else can read.
        for len in [0usize, 1, 15, 17, 20, 23, 25, 31, 33, 64] {
            assert!(Aes::new(&alloc::vec![0u8; len]).is_none(), "len {len} was accepted");
        }
        for len in [16, 24, 32] {
            assert!(Aes::new(&alloc::vec![0u8; len]).is_some(), "len {len} was rejected");
        }
    }

    #[test]
    fn shift_rows_inverts() {
        let mut b: [u8; 16] = core::array::from_fn(|i| i as u8);
        let orig = b;
        shift_rows(&mut b);
        assert_ne!(b, orig, "ShiftRows must actually move something");
        inv_shift_rows(&mut b);
        assert_eq!(b, orig);
    }

    #[test]
    fn mix_columns_inverts() {
        let mut b: [u8; 16] = core::array::from_fn(|i| (i * 17 + 3) as u8);
        let orig = b;
        mix_columns(&mut b);
        assert_ne!(b, orig);
        inv_mix_columns(&mut b);
        assert_eq!(b, orig);
    }

    #[test]
    fn gf_multiplication_matches_known_products() {
        // The four constants MixColumns uses, on values that exercise the reduction.
        assert_eq!(xmul(0x57, 0x01), 0x57);
        assert_eq!(xmul(0x57, 0x02), 0xae);
        assert_eq!(xmul(0x57, 0x04), 0x47);
        assert_eq!(xmul(0x57, 0x13), 0xfe);
        // 0x80 * 2 is the case that must reduce.
        assert_eq!(xmul(0x80, 0x02), 0x1b);
        // Commutative, and 0 and 1 behave.
        for a in [0u8, 1, 0x53, 0xca, 0xff] {
            for b in [0u8, 1, 0x02, 0x1b, 0xff] {
                assert_eq!(xmul(a, b), xmul(b, a));
            }
            assert_eq!(xmul(a, 0), 0);
            assert_eq!(xmul(a, 1), a);
        }
    }

    #[test]
    fn the_sboxes_are_inverses_and_are_permutations() {
        // A single transposed byte in either table produces a cipher that passes nothing —
        // but it would also be invisible on inspection, and this catches it precisely.
        for i in 0..256usize {
            assert_eq!(INV_SBOX[SBOX[i] as usize] as usize, i, "S-box not invertible at {i}");
        }
        let mut seen = [false; 256];
        for &v in SBOX.iter() {
            assert!(!seen[v as usize], "S-box repeats {v:#02x}");
            seen[v as usize] = true;
        }
    }

    #[test]
    fn round_trip_over_many_random_looking_blocks() {
        let aes = Aes::new(&unhex(
            "603deb1015ca71be2b73aef0857d77811f352c073b6108d72d9810a30914dff4",
        ))
        .unwrap();
        let mut state = 0x12345678u32;
        for _ in 0..256 {
            let mut b = [0u8; 16];
            for byte in b.iter_mut() {
                // xorshift, so the inputs vary without needing a random source.
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                *byte = state as u8;
            }
            let orig = b;
            aes.encrypt_block(&mut b);
            assert_ne!(b, orig);
            aes.decrypt_block(&mut b);
            assert_eq!(b, orig);
        }
    }
}
