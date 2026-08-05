//! AES-IGE, the mode MTProto encrypts every message with.
//!
//! # Why this has to be written by hand
//!
//! No OpenSSL provides it any more. It existed as `AES_ige_encrypt` in 0.9.8 and 1.x, was
//! deprecated, and was **removed in OpenSSL 3.0** — the host's own 3.5 has no trace of it.
//! So even a phone with Open C would not give us this, and there is no version of an
//! MTProto client that does not need it.
//!
//! # The recurrence
//!
//! IGE — Infinite Garble Extension — chains in both directions, so every ciphertext block
//! depends on all preceding plaintext *and* ciphertext:
//!
//! ```text
//! encrypt:   c[i] = E(m[i] ^ c[i-1]) ^ m[i-1]
//! decrypt:   m[i] = D(c[i] ^ m[i-1]) ^ c[i-1]
//! ```
//!
//! The 32-byte IV supplies the two initial values: `c[-1] = iv[0..16]` and
//! `m[-1] = iv[16..32]`. That split is the one `AES_ige_encrypt` used and the one
//! Telegram's `msg_key`-derived IV assumes.
//!
//! It is written out here because a mode is exactly the kind of thing that is easy to
//! implement into a *different* working cipher — swap the two XOR operands and you get
//! something that round-trips against itself and agrees with nobody. The tests carry
//! reference vectors generated from an independent AES implementation to close that gap.

use crate::aes::{Aes, BLOCK_LEN};

/// The IV is two blocks: the initial ciphertext and the initial plaintext.
pub const IV_LEN: usize = BLOCK_LEN * 2;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// Not a whole number of 16-byte blocks. IGE has no padding of its own — MTProto pads
    /// to a block boundary itself, and inventing padding here would silently change the
    /// message.
    NotBlockAligned,
    /// The IV was not 32 bytes.
    BadIv,
}

fn xor_into(dst: &mut [u8; BLOCK_LEN], src: &[u8]) {
    for (d, s) in dst.iter_mut().zip(src) {
        *d ^= s;
    }
}

/// Encrypt in place. `iv` is advanced, so consecutive calls continue one stream.
pub fn encrypt(aes: &Aes, iv: &mut [u8], data: &mut [u8]) -> Result<(), Error> {
    if iv.len() != IV_LEN {
        return Err(Error::BadIv);
    }
    if data.len() % BLOCK_LEN != 0 {
        return Err(Error::NotBlockAligned);
    }

    let mut c_prev = [0u8; BLOCK_LEN];
    let mut m_prev = [0u8; BLOCK_LEN];
    c_prev.copy_from_slice(&iv[..BLOCK_LEN]);
    m_prev.copy_from_slice(&iv[BLOCK_LEN..]);

    for chunk in data.chunks_mut(BLOCK_LEN) {
        let mut m = [0u8; BLOCK_LEN];
        m.copy_from_slice(chunk);

        let mut block = m;
        xor_into(&mut block, &c_prev);
        aes.encrypt_block(&mut block);
        xor_into(&mut block, &m_prev);

        chunk.copy_from_slice(&block);
        c_prev = block;
        m_prev = m;
    }

    iv[..BLOCK_LEN].copy_from_slice(&c_prev);
    iv[BLOCK_LEN..].copy_from_slice(&m_prev);
    Ok(())
}

/// Decrypt in place. `iv` is advanced.
pub fn decrypt(aes: &Aes, iv: &mut [u8], data: &mut [u8]) -> Result<(), Error> {
    if iv.len() != IV_LEN {
        return Err(Error::BadIv);
    }
    if data.len() % BLOCK_LEN != 0 {
        return Err(Error::NotBlockAligned);
    }

    let mut c_prev = [0u8; BLOCK_LEN];
    let mut m_prev = [0u8; BLOCK_LEN];
    c_prev.copy_from_slice(&iv[..BLOCK_LEN]);
    m_prev.copy_from_slice(&iv[BLOCK_LEN..]);

    for chunk in data.chunks_mut(BLOCK_LEN) {
        let mut c = [0u8; BLOCK_LEN];
        c.copy_from_slice(chunk);

        let mut block = c;
        xor_into(&mut block, &m_prev);
        aes.decrypt_block(&mut block);
        xor_into(&mut block, &c_prev);

        chunk.copy_from_slice(&block);
        c_prev = c;
        m_prev = block;
    }

    iv[..BLOCK_LEN].copy_from_slice(&c_prev);
    iv[BLOCK_LEN..].copy_from_slice(&m_prev);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    const KEY: &str = "603deb1015ca71be2b73aef0857d77811f352c073b6108d72d9810a30914dff4";
    const IV: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

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

    /// Generated from an independent AES implementation (pycrypto, itself checked against
    /// FIPS 197) driving the recurrence in the module docs.
    ///
    /// Independence is the point. A round-trip test passes for a mode that XORs the wrong
    /// operands, because the same mistake happens in reverse. These vectors are what pin
    /// the mode to *the* IGE rather than to a self-consistent variant of it.
    #[test]
    fn reference_vectors() {
        let aes = Aes::new(&unhex(KEY)).unwrap();
        let cases = [
            (
                "6bc1bee22e409f96e93d7e117393172a",
                "e59d5e17c2f0e7ad6f87b1e04366e5c9",
            ),
            (
                "6bc1bee22e409f96e93d7e117393172a\
                 ae2d8a571e03ac9c9eb76fac45af8e51\
                 30c81c46a35ce411e5fbc1191a0a52ef\
                 f69f2445df4f9b17ad2b417be66c3710",
                "e59d5e17c2f0e7ad6f87b1e04366e5c9\
                 37548827e9054ba571202bd0d80107ba\
                 f50fcb568a246c0bf23daf21b1b95c73\
                 4a0c70a4ad5857136c4b03af07531bf5",
            ),
        ];
        for (pt_hex, ct_hex) in cases {
            let pt_hex: alloc::string::String =
                pt_hex.chars().filter(|c| !c.is_whitespace()).collect();
            let ct_hex: alloc::string::String =
                ct_hex.chars().filter(|c| !c.is_whitespace()).collect();

            let mut data = unhex(&pt_hex);
            let mut iv = unhex(IV);
            encrypt(&aes, &mut iv, &mut data).unwrap();
            assert_eq!(hex(&data), ct_hex, "encrypt, {} blocks", pt_hex.len() / 32);

            let mut iv = unhex(IV);
            decrypt(&aes, &mut iv, &mut data).unwrap();
            assert_eq!(hex(&data), pt_hex, "decrypt did not invert");
        }
    }

    #[test]
    fn one_bit_of_the_first_block_changes_every_later_block() {
        // This is what distinguishes IGE from ECB, where blocks are independent, and it is
        // the property MTProto relies on: a tampered ciphertext decrypts to garbage from
        // the tampered block onwards, so the plaintext's own hash detects it.
        let aes = Aes::new(&unhex(KEY)).unwrap();
        let pt = unhex(&"6bc1bee22e409f96e93d7e117393172a".repeat(4));

        let mut a = pt.clone();
        let mut iv = unhex(IV);
        encrypt(&aes, &mut iv, &mut a).unwrap();

        let mut flipped = pt.clone();
        flipped[0] ^= 1;
        let mut iv = unhex(IV);
        encrypt(&aes, &mut iv, &mut flipped).unwrap();

        for block in 0..4 {
            let r = block * 16..block * 16 + 16;
            assert_ne!(a[r.clone()], flipped[r], "block {block} did not change");
        }
    }

    #[test]
    fn a_changed_iv_changes_the_output() {
        let aes = Aes::new(&unhex(KEY)).unwrap();
        let pt = unhex("6bc1bee22e409f96e93d7e117393172a");

        let mut a = pt.clone();
        let mut iv = unhex(IV);
        encrypt(&aes, &mut iv, &mut a).unwrap();

        let mut b = pt.clone();
        let mut iv2 = unhex(IV);
        iv2[31] ^= 1; // the plaintext half, which only affects the post-XOR
        encrypt(&aes, &mut iv2, &mut b).unwrap();
        assert_ne!(a, b, "the second half of the IV must matter");

        let mut c = pt;
        let mut iv3 = unhex(IV);
        iv3[0] ^= 1; // the ciphertext half, which affects the pre-XOR
        encrypt(&aes, &mut iv3, &mut c).unwrap();
        assert_ne!(a, c, "the first half of the IV must matter");
    }

    #[test]
    fn the_iv_advances_so_a_split_stream_matches_one_call() {
        // MTProto encrypts a message in one call, but a large payload assembled in pieces
        // has to be able to continue the stream. If the IV were not advanced, the second
        // call would restart the chain and produce something no peer can read.
        let aes = Aes::new(&unhex(KEY)).unwrap();
        let pt = unhex(&"6bc1bee22e409f96e93d7e117393172a".repeat(4));

        let mut whole = pt.clone();
        let mut iv = unhex(IV);
        encrypt(&aes, &mut iv, &mut whole).unwrap();

        let mut split = pt;
        let mut iv = unhex(IV);
        let (first, rest) = split.split_at_mut(32);
        encrypt(&aes, &mut iv, first).unwrap();
        encrypt(&aes, &mut iv, rest).unwrap();
        assert_eq!(split, whole);
    }

    #[test]
    fn a_partial_block_is_refused() {
        // Rather than padded. IGE has no padding, MTProto does its own, and inventing some
        // here would silently change the message.
        let aes = Aes::new(&unhex(KEY)).unwrap();
        let mut iv = unhex(IV);
        for len in [1usize, 15, 17, 31, 33] {
            let mut data = alloc::vec![0u8; len];
            assert_eq!(
                encrypt(&aes, &mut iv, &mut data),
                Err(Error::NotBlockAligned),
                "len {len}"
            );
        }
    }

    #[test]
    fn a_wrong_length_iv_is_refused() {
        let aes = Aes::new(&unhex(KEY)).unwrap();
        let mut data = alloc::vec![0u8; 16];
        for len in [0usize, 16, 31, 33, 48] {
            let mut iv = alloc::vec![0u8; len];
            assert_eq!(encrypt(&aes, &mut iv, &mut data), Err(Error::BadIv), "iv len {len}");
        }
    }

    #[test]
    fn an_empty_message_is_a_no_op_not_an_error() {
        let aes = Aes::new(&unhex(KEY)).unwrap();
        let mut iv = unhex(IV);
        let before = iv.clone();
        let mut data: [u8; 0] = [];
        assert!(encrypt(&aes, &mut iv, &mut data).is_ok());
        assert_eq!(iv, before, "an empty message must not advance the IV");
    }
}
