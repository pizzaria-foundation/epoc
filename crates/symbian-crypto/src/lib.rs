//! Hashes and ciphers, written because the platform does not have them.
//!
//! # What the platform actually provides
//!
//! Measured against the public S60 3rd Edition FP2 SDK, not recalled:
//!
//! | | |
//! |---|---|
//! | SHA-1, MD5 | `hash.dso` |
//! | SHA-256 | **nowhere** |
//! | AES, RSA, bignum | not in the public SDK — `crypto.dso` exports certificates and signatures, not primitives |
//! | random | `random.dso` |
//!
//! Open C, *if a given handset has it*, adds OpenSSL **0.9.8a**: `AES_encrypt`,
//! `RSA_public_encrypt`, `BN_mod_exp`, `RAND_bytes`, `HMAC`. Not SHA-256 — 0.9.8a is from
//! 2005 and predates it, and its `sha.h` does not mention it. And whether the runtime is
//! installed is a property of the phone, not of the SDK (see `examples/libprobe`).
//!
//! So: SHA-256 has to be written no matter what, AES-IGE has to be written no matter what
//! because no OpenSSL ever had it, and the rest is written here rather than bet on.
//!
//! # Why this is the least risky part of the project
//!
//! Everything here is pure integer arithmetic with **published test vectors**. Unlike the
//! platform work, where every guess turned out wrong and every answer needed a probe built
//! for it, a hash either matches FIPS 180-4 or it does not. The tests are the specification.
//!
//! # Threat model, stated rather than assumed
//!
//! These are not constant-time. AES uses S-box table lookups, which leak through cache
//! timing on a machine where an attacker can run code alongside you.
//!
//! On this device that is not the exposure. An ARM1136 at 600 MHz has no shared cache with
//! another tenant, Symbian has no multi-user model, and anyone who can run code on the
//! phone already has the file the key is stored in. The real risks here are a wrong
//! implementation and a leaked key file — which is what the vectors and
//! `symbian::fs::private_path` address. If this code is ever reused somewhere with actual
//! co-tenancy, the S-box needs revisiting first.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod aes;
pub mod bignum;
pub mod hmac;
pub mod inflate;
pub mod ige;
pub mod sha1;
pub mod sha256;
pub mod sha512;

pub use aes::Aes;
pub use bignum::{modpow, Modulus};
pub use inflate::{inflate, inflate_any, inflate_gzip, inflate_zlib};
pub use sha1::Sha1;
pub use sha256::Sha256;
pub use sha512::Sha512;

/// Compare two byte slices without an early exit.
///
/// For MAC verification. `a == b` on slices stops at the first differing byte, which tells
/// an attacker who can time the comparison how many leading bytes of a forged tag were
/// right — enough to find a valid tag one byte at a time.
///
/// The loop is written so it cannot be turned into an early return: the accumulator is
/// read after every iteration and only tested at the end.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ct_eq_agrees_with_equality() {
        assert!(ct_eq(b"", b""));
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab"));
        // Differing in the first byte and in the last must both be rejected — a
        // short-circuiting implementation passes this too, but a *broken* accumulator
        // (say, `=` instead of `|=`) fails the first-byte case only.
        assert!(!ct_eq(b"Xbc", b"abc"));
        assert!(!ct_eq(b"abX", b"abc"));
    }
}
