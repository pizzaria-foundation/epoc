//! HMAC, RFC 2104, over SHA-256 and SHA-1.
//!
//! MTProto does not use HMAC for message authentication — it derives a `msg_key` from a
//! SHA-256 of the payload and the auth key instead. HMAC is here because everything *around*
//! the protocol wants it: PBKDF2 for the 2FA password check is HMAC-SHA-512 over an
//! HMAC-SHA-256 construction, and any local key derivation for a stored session should be a
//! KDF rather than a bare hash.
//!
//! # Why generic over the hash
//!
//! HMAC is the same three steps regardless of the digest: block-pad the key, hash it with
//! the inner constant, hash the result with the outer one. Writing it twice would be two
//! places for the same off-by-one, and the block length is the only thing that differs —
//! which is precisely the value most easily got wrong, since SHA-256's block is 64 bytes
//! while its digest is 32, and using the digest length as the block length produces a MAC
//! that is self-consistent and matches nothing.

use crate::sha1::{self, Sha1};
use crate::sha256::{self, Sha256};

/// A hash this module can build an HMAC over.
///
/// Implemented for the concrete hashes rather than being a general trait, because a general
/// one would need associated const generics for the digest size and buy nothing: there are
/// two hashes and there will not be a third.
pub trait Hash: Clone {
    const BLOCK_LEN: usize;
    const DIGEST_LEN: usize;
    fn new() -> Self;
    fn update(&mut self, data: &[u8]);
    fn finish_into(self, out: &mut [u8]);
}

impl Hash for Sha256 {
    const BLOCK_LEN: usize = sha256::BLOCK_LEN;
    const DIGEST_LEN: usize = sha256::DIGEST_LEN;

    fn new() -> Self {
        Sha256::new()
    }

    fn update(&mut self, data: &[u8]) {
        Sha256::update(self, data)
    }

    fn finish_into(self, out: &mut [u8]) {
        out[..sha256::DIGEST_LEN].copy_from_slice(&self.finish());
    }
}

impl Hash for Sha1 {
    const BLOCK_LEN: usize = sha1::BLOCK_LEN;
    const DIGEST_LEN: usize = sha1::DIGEST_LEN;

    fn new() -> Self {
        Sha1::new()
    }

    fn update(&mut self, data: &[u8]) {
        Sha1::update(self, data)
    }

    fn finish_into(self, out: &mut [u8]) {
        out[..sha1::DIGEST_LEN].copy_from_slice(&self.finish());
    }
}

/// The largest block length in use, so the padded key can live on the stack.
const MAX_BLOCK: usize = 64;
const MAX_DIGEST: usize = 32;

pub struct Hmac<H: Hash> {
    inner: H,
    /// The outer-padded key, kept until `finish`. Held rather than the whole outer hash
    /// state so that `Hmac` is one hash plus 64 bytes rather than two hash states.
    outer_key: [u8; MAX_BLOCK],
}

impl<H: Hash> Hmac<H> {
    pub fn new(key: &[u8]) -> Self {
        debug_assert!(H::BLOCK_LEN <= MAX_BLOCK);

        // A key longer than the block is replaced by its own hash. Not truncated: two keys
        // sharing a prefix would then produce identical MACs.
        let mut k = [0u8; MAX_BLOCK];
        if key.len() > H::BLOCK_LEN {
            let mut h = H::new();
            h.update(key);
            h.finish_into(&mut k);
        } else {
            k[..key.len()].copy_from_slice(key);
        }

        let mut ipad = [0u8; MAX_BLOCK];
        let mut outer_key = [0u8; MAX_BLOCK];
        for i in 0..H::BLOCK_LEN {
            ipad[i] = k[i] ^ 0x36;
            outer_key[i] = k[i] ^ 0x5c;
        }

        let mut inner = H::new();
        inner.update(&ipad[..H::BLOCK_LEN]);
        Hmac { inner, outer_key }
    }

    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    /// Write the tag into `out`, which must be at least `H::DIGEST_LEN` long.
    pub fn finish_into(self, out: &mut [u8]) {
        let mut inner_digest = [0u8; MAX_DIGEST];
        self.inner.finish_into(&mut inner_digest);

        let mut outer = H::new();
        outer.update(&self.outer_key[..H::BLOCK_LEN]);
        outer.update(&inner_digest[..H::DIGEST_LEN]);
        outer.finish_into(out);
    }
}

/// One-shot HMAC-SHA-256.
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; sha256::DIGEST_LEN] {
    let mut m = Hmac::<Sha256>::new(key);
    m.update(data);
    let mut out = [0u8; sha256::DIGEST_LEN];
    m.finish_into(&mut out);
    out
}

/// One-shot HMAC-SHA-1.
pub fn hmac_sha1(key: &[u8], data: &[u8]) -> [u8; sha1::DIGEST_LEN] {
    let mut m = Hmac::<Sha1>::new(key);
    m.update(data);
    let mut out = [0u8; sha1::DIGEST_LEN];
    m.finish_into(&mut out);
    out
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

    /// RFC 4231, the HMAC-SHA-256 cases.
    #[test]
    fn rfc4231_sha256() {
        // Case 1: a 20-byte key.
        assert_eq!(
            hex(&hmac_sha256(&[0x0b; 20], b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        // Case 2: a short ASCII key, which is the one that catches a padding error.
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // Case 3: a 20-byte key of 0xaa with 50 bytes of 0xdd.
        assert_eq!(
            hex(&hmac_sha256(&[0xaa; 20], &[0xdd; 50])),
            "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe"
        );
        // Case 6: a key *longer* than the block, so it gets hashed first. The case a
        // truncating implementation fails.
        assert_eq!(
            hex(&hmac_sha256(&[0xaa; 131], b"Test Using Larger Than Block-Size Key - Hash Key First")),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    /// RFC 2202, the HMAC-SHA-1 cases.
    #[test]
    fn rfc2202_sha1() {
        assert_eq!(
            hex(&hmac_sha1(&[0x0b; 20], b"Hi There")),
            "b617318655057264e28bc0b6fb378c8ef146be00"
        );
        assert_eq!(
            hex(&hmac_sha1(b"Jefe", b"what do ya want for nothing?")),
            "effcdf6ae5eb2fa2d27416d5f184df9c259a7c79"
        );
        assert_eq!(
            hex(&hmac_sha1(&[0xaa; 20], &[0xdd; 50])),
            "125d7342b9ac11cd91a39af48aa17b4f63f175d3"
        );
        assert_eq!(
            hex(&hmac_sha1(&[0xaa; 80], b"Test Using Larger Than Block-Size Key - Hash Key First")),
            "aa4ae5e15272d00e95705637ce8a3b55ed402112"
        );
    }

    #[test]
    fn streaming_matches_one_shot() {
        let data: Vec<u8> = (0..300u32).map(|i| (i * 13 % 251) as u8).collect();
        let want = hmac_sha256(b"key", &data);
        for chunk in [1usize, 7, 31, 64, 65, 128] {
            let mut m = Hmac::<Sha256>::new(b"key");
            for part in data.chunks(chunk) {
                m.update(part);
            }
            let mut out = [0u8; 32];
            m.finish_into(&mut out);
            assert_eq!(out, want, "chunk size {chunk}");
        }
    }

    #[test]
    fn a_key_at_exactly_the_block_length_is_not_hashed() {
        // 64 bytes is the boundary: `>` rather than `>=`, per RFC 2104. Getting it wrong
        // gives a MAC that is self-consistent and matches no other implementation — which
        // is the worst kind of bug, because everything works until you talk to a peer.
        let key = [0x42u8; 64];
        let mut ipad = [0u8; 64];
        for (i, b) in ipad.iter_mut().enumerate() {
            *b = key[i] ^ 0x36;
        }
        let mut inner = Sha256::new();
        inner.update(&ipad);
        inner.update(b"payload");
        let inner_digest = inner.finish();

        let mut opad = [0u8; 64];
        for (i, b) in opad.iter_mut().enumerate() {
            *b = key[i] ^ 0x5c;
        }
        let mut outer = Sha256::new();
        outer.update(&opad);
        outer.update(&inner_digest);

        assert_eq!(hmac_sha256(&key, b"payload"), outer.finish());
    }

    #[test]
    fn keys_that_share_a_prefix_give_different_tags() {
        // The reason an over-long key is hashed rather than truncated.
        let a = vec![0x11u8; 100];
        let mut b = a.clone();
        b.push(0x22);
        assert_ne!(hmac_sha256(&a, b"x"), hmac_sha256(&b, b"x"));
    }

    #[test]
    fn an_empty_key_and_empty_data_still_produce_a_tag() {
        // Degenerate but legal, and a MAC over nothing must not be all zeros — which is
        // what an implementation that skips the padding when the key is empty would give.
        let t = hmac_sha256(b"", b"");
        assert_ne!(t, [0u8; 32]);
        assert_eq!(t, hmac_sha256(b"", b""), "and it must be deterministic");
    }
}
