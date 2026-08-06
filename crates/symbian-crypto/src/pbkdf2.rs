//! PBKDF2-HMAC-SHA-512, RFC 8018.
//!
//! Written for one caller: Telegram's two-factor password check, which specifies
//! `passwordKdfAlgoSHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow` — a hundred thousand
//! iterations.
//!
//! # Why the midstates matter here and nowhere else
//!
//! HMAC expands its key into a 128-byte `ipad` block and a 128-byte `opad` block, and hashes
//! each. A naive PBKDF2 does that on every iteration, so a hundred thousand iterations pay
//! for two hundred thousand compressions that all produce the *same* two intermediate
//! states — the key never changes.
//!
//! ```text
//! naive      ipad, U, opad, digest      4 compressions per iteration
//! this       U, digest                  2 compressions per iteration
//! ```
//!
//! Half the work, for the cost of holding two `Sha512` values and cloning them. On a device
//! where SHA-256 measures 8 MB/s and SHA-512 is slower still, that is seconds of a user
//! staring at a phone.
//!
//! It is only worth doing because the iteration count is enormous and fixed. Anywhere else
//! the clone would cost more than the compression it saves.
//!
//! # What it does not do
//!
//! No length or iteration validation beyond what the arithmetic requires. RFC 8018 bounds
//! `dkLen` at `(2^32 - 1) * hLen`, which on a device with 45 MB of RAM is not a bound worth
//! carrying — the caller asks for 64 bytes.

use crate::sha512::{self, Sha512};

/// HMAC-SHA-512 with the key expansion done once, carried as two 64-byte states.
///
/// **Not two `Sha512` values.** That was the first version and it was the slower half of
/// this file: `Sha512` is 208 bytes — 64 of state and a 128-byte block buffer that is empty
/// after the pad is absorbed — so cloning both twice per iteration copies 41 MB across
/// Telegram's hundred thousand rounds, against 25 MB of actual hashing. More than half of
/// every copy was a buffer holding nothing.
///
/// Carrying the eight words and building the single block by hand removes all of it.
#[derive(Clone)]
struct Primed {
    inner: [u64; 8],
    outer: [u64; 8],
}

/// Bit length of one HMAC half: a 128-byte pad block plus a 64-byte message.
///
/// Both halves hash exactly that — the inner over `ipad ++ U`, the outer over `opad ++
/// digest` — so one block layout serves both, and the length field is a constant rather
/// than something to track.
const HALF_BITS: u64 = (sha512::BLOCK_LEN as u64 + 64) * 8;

impl Primed {
    fn new(key: &[u8]) -> Self {
        // A key longer than the block is hashed first; a shorter one is zero-padded. Both
        // are RFC 2104, and getting the long case wrong is invisible until someone uses a
        // long password — which for a KDF whose input is itself a hash never happens, so
        // it would never be found.
        let mut k = [0u8; sha512::BLOCK_LEN];
        if key.len() > sha512::BLOCK_LEN {
            k[..sha512::DIGEST_LEN].copy_from_slice(&sha512::sha512(key));
        } else {
            k[..key.len()].copy_from_slice(key);
        }

        let mut ipad = [0u8; sha512::BLOCK_LEN];
        let mut opad = [0u8; sha512::BLOCK_LEN];
        for i in 0..sha512::BLOCK_LEN {
            ipad[i] = k[i] ^ 0x36;
            opad[i] = k[i] ^ 0x5c;
        }

        // One compression each: the pads are exactly one block, so absorbing them leaves a
        // state and an empty buffer, and only the state is worth keeping.
        let mut inner = sha512::H0_PUB;
        sha512::compress_into(&mut inner, &ipad);
        let mut outer = sha512::H0_PUB;
        sha512::compress_into(&mut outer, &opad);

        Primed { inner, outer }
    }

    /// One HMAC over a 64-byte message, from the primed states.
    ///
    /// Both halves are a single block of `message ++ 0x80 ++ zeros ++ length`, which is
    /// what lets this skip the streaming interface entirely. 64 bytes of message plus one
    /// terminator plus the 16-byte length is 81, comfortably inside the block, so the
    /// message never spills into a second one — the case that would make this wrong is
    /// impossible for the only caller.
    fn mac(&self, message: &[u8; 64]) -> [u8; sha512::DIGEST_LEN] {
        let mut block = [0u8; sha512::BLOCK_LEN];
        block[..64].copy_from_slice(message);
        block[64] = 0x80;
        block[sha512::BLOCK_LEN - 8..].copy_from_slice(&HALF_BITS.to_be_bytes());

        let mut h = self.inner;
        sha512::compress_into(&mut h, &block);

        let mut digest = [0u8; sha512::DIGEST_LEN];
        for (i, word) in h.iter().enumerate() {
            digest[i * 8..i * 8 + 8].copy_from_slice(&word.to_be_bytes());
        }

        block[..64].copy_from_slice(&digest);
        let mut h = self.outer;
        sha512::compress_into(&mut h, &block);

        let mut out = [0u8; sha512::DIGEST_LEN];
        for (i, word) in h.iter().enumerate() {
            out[i * 8..i * 8 + 8].copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}

/// Derive `out.len()` bytes from `password` and `salt`.
///
/// `iterations` of zero produces the salt's first MAC unmodified, which is not a useful
/// KDF but is what the definition says; it is the caller's business to pass a real count.
pub fn pbkdf2_hmac_sha512(password: &[u8], salt: &[u8], iterations: u32, out: &mut [u8]) {
    let primed = Primed::new(password);

    for (block, chunk) in out.chunks_mut(sha512::DIGEST_LEN).enumerate() {
        // U1 = PRF(P, S || INT_32_BE(i)), with i counting from one. From zero produces a
        // different derived key that no other implementation agrees with, and for a
        // single-block output — which is Telegram's case — it is the only difference.
        let index = (block as u32 + 1).to_be_bytes();
        // U1's message is the salt, which is not 64 bytes, so it goes through the streaming
        // interface once. Every iteration after it hashes a 64-byte digest and takes the
        // fast path -- which is 99,999 of the hundred thousand.
        let u1 = {
            let mut m = Sha512::new();
            let mut k = [0u8; sha512::BLOCK_LEN];
            if password.len() > sha512::BLOCK_LEN {
                k[..sha512::DIGEST_LEN].copy_from_slice(&sha512::sha512(password));
            } else {
                k[..password.len()].copy_from_slice(password);
            }
            for b in k.iter_mut() {
                *b ^= 0x36;
            }
            m.update(&k);
            m.update(salt);
            m.update(&index);
            let inner = m.finish();

            for b in k.iter_mut() {
                *b ^= 0x36 ^ 0x5c;
            }
            let mut o = Sha512::new();
            o.update(&k);
            o.update(&inner);
            o.finish()
        };

        let mut u = u1;
        let mut t = u;
        for _ in 1..iterations {
            u = primed.mac(&u);
            for (a, b) in t.iter_mut().zip(u.iter()) {
                *a ^= b;
            }
        }
        chunk.copy_from_slice(&t[..chunk.len()]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn hex(b: &[u8]) -> alloc::string::String {
        use core::fmt::Write;
        let mut s = alloc::string::String::new();
        for x in b {
            let _ = write!(s, "{x:02x}");
        }
        s
    }

    /// Published SHA-512 vectors.
    ///
    /// RFC 6070 only covers SHA-1, so these come from the widely cross-checked set used by
    /// every PBKDF2-SHA512 implementation — the same values `hashlib.pbkdf2_hmac` produces,
    /// which `examples/reference.py` checks independently.
    #[test]
    fn published_vectors() {
        let cases: [(&str, &str, u32, usize, &str); 4] = [
            (
                "password", "salt", 1, 64,
                "867f70cf1ade02cff3752599a3a53dc4af34c7a669815ae5d513554e1c8cf252\
                 c02d470a285a0501bad999bfe943c08f050235d7d68b1da55e63f73b60a57fce",
            ),
            (
                "password", "salt", 2, 64,
                "e1d9c16aa681708a45f5c7c4e215ceb66e011a2e9f0040713f18aefdb866d53c\
                 f76cab2868a39b9f7840edce4fef5a82be67335c77a6068e04112754f27ccf4e",
            ),
            (
                "password", "salt", 4096, 64,
                "d197b1b33db0143e018b12f3d1d1479e6cdebdcc97c5c0f87f6902e072f457b5\
                 143f30602641b3d55cd335988cb36b84376060ecd532e039b742a239434af2d5",
            ),
            (
                "passwordPASSWORDpassword", "saltSALTsaltSALTsaltSALTsaltSALTsalt", 4096, 64,
                "8c0511f4c6e597c6ac6315d8f0362e225f3c501495ba23b868c005174dc4ee71\
                 115b59f9e60cd9532fa33e0f75aefe30225c583a186cd82bd4daea9724a3d3b8",
            ),
        ];
        for (pw, salt, iters, len, want) in cases {
            let mut out = vec![0u8; len];
            pbkdf2_hmac_sha512(pw.as_bytes(), salt.as_bytes(), iters, &mut out);
            assert_eq!(hex(&out), want.replace([' ', '\n'], ""), "{pw}/{salt}/{iters}");
        }
    }

    /// The midstate version must agree with the obvious one.
    ///
    /// The optimisation is the only interesting thing in this file and it is invisible from
    /// outside — a clone that captured the wrong state would produce a derived key that is
    /// self-consistent, deterministic, and wrong, which for a login means "incorrect
    /// password" and nothing else. So it is checked against a version that re-keys.
    #[test]
    fn the_midstates_agree_with_re_keying() {
        fn slow(password: &[u8], salt: &[u8], iterations: u32, out: &mut [u8]) {
            for (block, chunk) in out.chunks_mut(64).enumerate() {
                let mut msg = alloc::vec::Vec::from(salt);
                msg.extend_from_slice(&(block as u32 + 1).to_be_bytes());
                let mut u = crate::hmac::hmac_sha512(password, &msg);
                let mut t = u;
                for _ in 1..iterations {
                    u = crate::hmac::hmac_sha512(password, &u);
                    for (a, b) in t.iter_mut().zip(u.iter()) {
                        *a ^= b;
                    }
                }
                chunk.copy_from_slice(&t[..chunk.len()]);
            }
        }

        for (pw, salt, iters, len) in [
            (&b"p"[..], &b"s"[..], 1u32, 64usize),
            (&b"password"[..], &b"NaCl"[..], 17, 64),
            (&b""[..], &b""[..], 3, 100),
            // A key longer than the 128-byte block, which takes the hash-the-key branch.
            (&[0x61u8; 200][..], &b"salt"[..], 5, 32),
        ] {
            let mut a = vec![0u8; len];
            let mut b = vec![0u8; len];
            pbkdf2_hmac_sha512(pw, salt, iters, &mut a);
            slow(pw, salt, iters, &mut b);
            assert_eq!(hex(&a), hex(&b), "pw {} salt {} iters {iters}", pw.len(), salt.len());
        }
    }

    #[test]
    fn output_longer_than_one_block_uses_a_counter() {
        // Two blocks must differ. A version that forgot INT_32_BE(i) produces the same 64
        // bytes twice, which looks like a 128-byte key and has 64 bytes of entropy.
        let mut out = vec![0u8; 128];
        pbkdf2_hmac_sha512(b"p", b"s", 2, &mut out);
        assert_ne!(out[..64], out[64..]);
    }

    #[test]
    fn the_counter_starts_at_one() {
        // Off by one here is the difference between agreeing with every other
        // implementation and agreeing with none, and for Telegram's single-block output it
        // is the *only* difference.
        let mut one_block = [0u8; 64];
        pbkdf2_hmac_sha512(b"password", b"salt", 1, &mut one_block);
        let mut msg = alloc::vec::Vec::from(&b"salt"[..]);
        msg.extend_from_slice(&1u32.to_be_bytes());
        assert_eq!(one_block, crate::hmac::hmac_sha512(b"password", &msg));
    }

    #[test]
    fn a_partial_final_block_is_truncated_not_padded() {
        let mut long = [0u8; 64];
        let mut short = [0u8; 40];
        pbkdf2_hmac_sha512(b"p", b"s", 3, &mut long);
        pbkdf2_hmac_sha512(b"p", b"s", 3, &mut short);
        assert_eq!(short[..], long[..40]);
    }
}
