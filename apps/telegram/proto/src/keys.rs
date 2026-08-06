//! Telegram's RSA public keys, and the fingerprints that name them.
//!
//! # What these are for
//!
//! `req_DH_params` carries a block encrypted to one of Telegram's public keys. The server
//! sends the fingerprints it will accept in `resPQ`, the client picks one it holds, and the
//! encryption is what proves the client is talking to Telegram and not to whoever is
//! between them — the only step in the handshake that authenticates the server at all.
//!
//! Which makes a wrong modulus here the one bug in this crate that fails *silently in the
//! attacker's favour*. Everything else produces a closed connection; this produces a
//! working session with the wrong party.
//!
//! # The fingerprint
//!
//! Not a hash of the key file. It is the low 64 bits of
//! `SHA1(tl_bytes(n) || tl_bytes(e))`, read **little-endian** — TL's byte order, not the
//! big-endian the rest of the cryptography uses. The two orders appear within one
//! expression here, which is why it is spelled out.
//!
//! [`FINGERPRINT`] is checked against a recomputation from [`MODULUS`] in the tests, so the
//! constant and the key cannot drift apart.
//!
//! # Key rotation
//!
//! Telegram has replaced these before. When it does, `resPQ` arrives listing fingerprints
//! none of which match, and [`select`] returns `None` — a clean, named failure rather than
//! a handshake that fails somewhere later for no visible reason. Recovering means shipping
//! a new key, which is a release, and that is the correct amount of friction for changing
//! the thing that authenticates the server.

/// The production server public key modulus, big-endian, 2048 bits.
///
/// Extracted from the PEM in `vendor/research/mtproto/rsafp.py`, which also prints the
/// fingerprint below. The test server key is deliberately not carried: a client that can
/// silently end up on the test cluster is a client that can silently lose messages.
pub const MODULUS: [u8; 256] = [
    0xe8, 0xbb, 0x33, 0x05, 0xc0, 0xb5, 0x2c, 0x6c, 0xf2, 0xaf, 0xdf, 0x76,
    0x37, 0x31, 0x34, 0x89, 0xe6, 0x3e, 0x05, 0x26, 0x8e, 0x5b, 0xad, 0xb6,
    0x01, 0xaf, 0x41, 0x77, 0x86, 0x47, 0x2e, 0x5f, 0x93, 0xb8, 0x54, 0x38,
    0x96, 0x8e, 0x20, 0xe6, 0x72, 0x9a, 0x30, 0x1c, 0x0a, 0xfc, 0x12, 0x1b,
    0xf7, 0x15, 0x1f, 0x83, 0x44, 0x36, 0xf7, 0xfd, 0xa6, 0x80, 0x84, 0x7a,
    0x66, 0xbf, 0x64, 0xac, 0xce, 0xc7, 0x8e, 0xe2, 0x1c, 0x0b, 0x31, 0x6f,
    0x0e, 0xda, 0xfe, 0x2f, 0x41, 0x90, 0x8d, 0xa7, 0xbd, 0x1f, 0x4a, 0x51,
    0x07, 0x63, 0x8e, 0xeb, 0x67, 0x04, 0x0a, 0xce, 0x47, 0x2a, 0x14, 0xf9,
    0x0d, 0x9f, 0x7c, 0x2b, 0x7d, 0xef, 0x99, 0x68, 0x8b, 0xa3, 0x07, 0x3a,
    0xdb, 0x57, 0x50, 0xbb, 0x02, 0x96, 0x49, 0x02, 0xa3, 0x59, 0xfe, 0x74,
    0x5d, 0x81, 0x70, 0xe3, 0x68, 0x76, 0xd4, 0xfd, 0x8a, 0x5d, 0x41, 0xb2,
    0xa7, 0x6c, 0xbf, 0xf9, 0xa1, 0x32, 0x67, 0xeb, 0x95, 0x80, 0xb2, 0xd0,
    0x6d, 0x10, 0x35, 0x74, 0x48, 0xd2, 0x0d, 0x9d, 0xa2, 0x19, 0x1c, 0xb5,
    0xd8, 0xc9, 0x39, 0x82, 0x96, 0x1c, 0xdf, 0xde, 0xda, 0x62, 0x9e, 0x37,
    0xf1, 0xfb, 0x09, 0xa0, 0x72, 0x20, 0x27, 0x69, 0x60, 0x32, 0xfe, 0x61,
    0xed, 0x66, 0x3d, 0xb7, 0xa3, 0x7f, 0x6f, 0x26, 0x3d, 0x37, 0x0f, 0x69,
    0xdb, 0x53, 0xa0, 0xdc, 0x0a, 0x17, 0x48, 0xbd, 0xaa, 0xff, 0x62, 0x09,
    0xd5, 0x64, 0x54, 0x85, 0xe6, 0xe0, 0x01, 0xd1, 0x95, 0x32, 0x55, 0x75,
    0x7e, 0x4b, 0x8e, 0x42, 0x81, 0x33, 0x47, 0xb1, 0x1d, 0xa6, 0xab, 0x50,
    0x0f, 0xd0, 0xac, 0xe7, 0xe6, 0xdf, 0xa3, 0x73, 0x61, 0x99, 0xcc, 0xaf,
    0x93, 0x97, 0xed, 0x07, 0x45, 0xa4, 0x27, 0xdc, 0xfa, 0x6c, 0xd6, 0x7b,
    0xcb, 0x1a, 0xcf, 0xf3,
];

/// The public exponent, big-endian. 65537 for every key Telegram has published.
pub const EXPONENT: [u8; 3] = [0x01, 0x00, 0x01];

/// `SHA1(tl_bytes(n) || tl_bytes(e))[12..20]` as a little-endian u64.
pub const FINGERPRINT: u64 = 0xd09d_1d85_de64_fd85;

/// Pick a key from the fingerprints the server offered.
///
/// Returns the fingerprint to echo back in `req_DH_params`, or `None` if the server named
/// no key this build holds — see the note on rotation above.
pub fn select(offered: &[u64]) -> Option<u64> {
    offered.iter().copied().find(|&f| f == FINGERPRINT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_crypto::Sha1;

    /// Recompute the fingerprint from the modulus.
    ///
    /// This is the test that makes the pair trustworthy: a modulus with a transposed byte
    /// would still be a valid 2048-bit number and would still encrypt, so nothing at
    /// runtime would notice. The fingerprint is a checksum over the exact bytes, and the
    /// server chose it — so agreeing with it means the key really is Telegram's.
    #[test]
    fn the_fingerprint_matches_the_modulus() {
        fn tl_bytes(out: &mut alloc::vec::Vec<u8>, b: &[u8]) {
            let start = out.len();
            if b.len() < 254 {
                out.push(b.len() as u8);
            } else {
                out.push(254);
                out.extend_from_slice(&(b.len() as u32).to_le_bytes()[..3]);
            }
            out.extend_from_slice(b);
            while (out.len() - start) % 4 != 0 {
                out.push(0);
            }
        }

        let mut ser = alloc::vec::Vec::new();
        tl_bytes(&mut ser, &MODULUS);
        tl_bytes(&mut ser, &EXPONENT);

        let mut h = Sha1::new();
        h.update(&ser);
        let d = h.finish();

        let mut fp = [0u8; 8];
        fp.copy_from_slice(&d[12..20]);
        // Little-endian: TL's order. Reading these eight bytes big-endian gives a number
        // that matches nothing the server sends, and the handshake then fails at
        // req_DH_params with no indication that the key was the problem.
        assert_eq!(u64::from_le_bytes(fp), FINGERPRINT);
    }

    #[test]
    fn the_modulus_is_a_full_2048_bit_number() {
        // A leading zero byte would mean 2040 bits and a modulus that is not the one the
        // fingerprint covers. It would also change the length of the RSA block.
        assert_eq!(MODULUS.len(), 256);
        assert_ne!(MODULUS[0], 0);
        assert_eq!(MODULUS[0] & 0x80, 0x80, "the top bit must be set for a 2048-bit modulus");
        // Odd, as any RSA modulus must be, being a product of two odd primes.
        assert_eq!(MODULUS[255] & 1, 1);
    }

    #[test]
    fn selection_finds_ours_and_refuses_the_rest() {
        assert_eq!(select(&[1, 2, FINGERPRINT, 3]), Some(FINGERPRINT));
        assert_eq!(select(&[1, 2, 3]), None);
        assert_eq!(select(&[]), None);
    }
}
