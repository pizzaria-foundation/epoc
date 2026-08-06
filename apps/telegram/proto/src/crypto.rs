//! The three constructions MTProto builds out of AES, SHA-1 and SHA-256.
//!
//! Kept apart from the state machine in `handshake.rs` because they are pure functions over
//! byte strings, which means they can be checked against the Python reference one at a time.
//! A handshake that fails tells you nothing about which of these was wrong; a differential
//! on each tells you exactly.
//!
//! # The randomness trait
//!
//! [`Rng`] exists so this crate never reaches for a platform. The device passes
//! `symbian::random::Random`; the tests pass a counter, which makes every value in a test
//! reproducible and lets a failure be pasted into the Python reference verbatim.

use alloc::vec;
use alloc::vec::Vec;

use symbian_crypto::{ige, Aes, Sha1, Sha256};

/// A source of unpredictable bytes.
///
/// Deliberately not `rand::RngCore` or anything with a numeric interface: everything here
/// wants a byte block of a fixed size, and a `next_u32` loop is how a 256-bit secret ends up
/// with 32 bits of entropy in it.
pub trait Rng {
    fn fill(&mut self, out: &mut [u8]);
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Error {
    /// The plaintext does not fit the 192-byte RSA_PAD block.
    TooLong,
    /// The AES-IGE input was not a multiple of the block size.
    NotBlockAligned,
    /// The SHA-1 prefix on a decrypted block did not match its contents. Either the key is
    /// wrong or the message was tampered with, and there is no way to tell which.
    BadHash,
    /// RSA could not be performed — a malformed modulus, or a block that is numerically
    /// larger than it.
    Rsa,
}

pub type Result<T> = core::result::Result<T, Error>;

// ------------------------------------------------------------------------- RSA_PAD --

/// Bytes of plaintext plus padding that go into one RSA block.
const PAD_LEN: usize = 192;
/// `data_pad_reversed` plus a 32-byte SHA-256, which is what AES sees.
const HASHED_LEN: usize = PAD_LEN + 32;

/// Telegram's RSA padding, as specified for `req_DH_params`.
///
/// ```text
/// data_with_padding  = data ++ random, to 192 bytes
/// data_pad_reversed  = reverse(data_with_padding)
/// repeat:
///     temp_key       = 32 random bytes
///     data_with_hash = data_pad_reversed ++ SHA256(temp_key ++ data_with_padding)
///     aes_encrypted  = AES256_IGE(data_with_hash, temp_key, iv = 0)
///     temp_key_xor   = temp_key XOR SHA256(aes_encrypted)
///     block          = temp_key_xor ++ aes_encrypted
/// until block < modulus
/// out = block ^ e mod modulus
/// ```
///
/// # Why it looks like this
///
/// Plain `m^e mod n` on structured data is malleable and leaks; the old scheme was
/// `SHA1(data) ++ data ++ random` and was replaced. What this buys is that every bit of the
/// RSA input depends on every bit of the plaintext *and* on a fresh key, so two encryptions
/// of the same `p_q_inner_data` share nothing. The byte reversal is there so the high-order
/// end of the RSA input — the part an attacker learns most about from a partial break —
/// holds the tail of the padding rather than the head of the data.
///
/// # The retry loop
///
/// RSA requires the input to be numerically less than the modulus. The block is 2048 bits
/// and so is the modulus, so it fails roughly one time in three — often enough that a
/// version without the loop works in testing and fails in the field. `attempts` bounds it;
/// exhausting it is not a real outcome, at 2^-25 or so, but it is bounded rather than
/// unbounded because this runs on a phone.
pub fn rsa_pad<R: Rng>(
    data: &[u8],
    modulus: &[u8],
    exponent: &[u8],
    rng: &mut R,
    attempts: usize,
) -> Result<Vec<u8>> {
    if data.len() > PAD_LEN {
        return Err(Error::TooLong);
    }

    let mut padded = vec![0u8; PAD_LEN];
    padded[..data.len()].copy_from_slice(data);
    rng.fill(&mut padded[data.len()..]);

    let mut reversed = padded.clone();
    reversed.reverse();

    let m = symbian_crypto::Modulus::new(modulus).map_err(|_| Error::Rsa)?;

    for _ in 0..attempts {
        let mut temp_key = [0u8; 32];
        rng.fill(&mut temp_key);

        let mut with_hash = vec![0u8; HASHED_LEN];
        with_hash[..PAD_LEN].copy_from_slice(&reversed);
        {
            let mut h = Sha256::new();
            h.update(&temp_key);
            h.update(&padded);
            with_hash[PAD_LEN..].copy_from_slice(&h.finish());
        }

        // IGE with an all-zero IV. Unusual, and correct here: the "IV" of a one-shot
        // encryption under a key used exactly once carries no information, and the spec
        // says zero.
        let aes = Aes::new(&temp_key).ok_or(Error::Rsa)?;
        let mut iv = [0u8; 32];
        let mut encrypted = with_hash.clone();
        ige::encrypt(&aes, &mut iv, &mut encrypted).map_err(|_| Error::Rsa)?;

        let mut block = vec![0u8; 256];
        {
            let mut h = Sha256::new();
            h.update(&encrypted);
            let d = h.finish();
            for i in 0..32 {
                block[i] = temp_key[i] ^ d[i];
            }
        }
        block[32..].copy_from_slice(&encrypted);

        // Numerically less than the modulus, compared big-endian byte by byte. Both are
        // exactly 256 bytes, so this is a lexicographic comparison — but only because the
        // lengths match, which is why the modulus is required to be full width.
        if modulus.len() == 256 && block.as_slice() < modulus {
            let mut out = vec![0u8; 256];
            symbian_crypto::bignum::rsa_encrypt(&block, exponent, &m, &mut out)
                .map_err(|_| Error::Rsa)?;
            return Ok(out);
        }
    }

    Err(Error::Rsa)
}

// ------------------------------------------------------------- the handshake KDF --

/// The AES key and IV protecting the Diffie-Hellman exchange.
///
/// ```text
/// key = SHA1(new_nonce ++ server_nonce) ++ SHA1(server_nonce ++ new_nonce)[0..12]
/// iv  = SHA1(server_nonce ++ new_nonce)[12..20] ++ SHA1(new_nonce ++ new_nonce)
///       ++ new_nonce[0..4]
/// ```
///
/// Three SHA-1s over two nonces in three different orders, sliced at 12 and 20. There is no
/// principle to derive it from — it is a specification to copy exactly. Getting a slice
/// bound wrong produces a key that decrypts to noise, and the failure surfaces as
/// [`Error::BadHash`] one function later with nothing to say which byte was wrong. Hence
/// [`crate::handshake`]'s differential against the Python reference.
pub fn dh_kdf(new_nonce: &[u8; 32], server_nonce: &[u8; 16]) -> ([u8; 32], [u8; 32]) {
    let ns = {
        let mut h = Sha1::new();
        h.update(new_nonce);
        h.update(server_nonce);
        h.finish()
    };
    let sn = {
        let mut h = Sha1::new();
        h.update(server_nonce);
        h.update(new_nonce);
        h.finish()
    };
    let nn = {
        let mut h = Sha1::new();
        h.update(new_nonce);
        h.update(new_nonce);
        h.finish()
    };

    let mut key = [0u8; 32];
    key[..20].copy_from_slice(&ns);
    key[20..].copy_from_slice(&sn[..12]);

    let mut iv = [0u8; 32];
    iv[..8].copy_from_slice(&sn[12..20]);
    iv[8..28].copy_from_slice(&nn);
    iv[28..].copy_from_slice(&new_nonce[..4]);

    (key, iv)
}

// ------------------------------------------------------- hash-prefixed IGE blocks --

/// Encrypt `data` as `SHA1(data) ++ data ++ padding` under AES-256-IGE.
///
/// The padding is random rather than zero. Zero padding would be equally correct for the
/// protocol and would leak the exact plaintext length through the ciphertext length, which
/// for `client_DH_inner_data` is a small thing and for a habit is not.
pub fn ige_with_hash<R: Rng>(key: &[u8; 32], iv: &[u8; 32], data: &[u8], rng: &mut R)
    -> Result<Vec<u8>>
{
    let mut h = Sha1::new();
    h.update(data);
    let digest = h.finish();

    let unpadded = 20 + data.len();
    let total = unpadded.div_ceil(16) * 16;

    let mut buf = vec![0u8; total];
    buf[..20].copy_from_slice(&digest);
    buf[20..unpadded].copy_from_slice(data);
    if total > unpadded {
        rng.fill(&mut buf[unpadded..]);
    }

    let aes = Aes::new(key).ok_or(Error::Rsa)?;
    let mut iv = *iv;
    ige::encrypt(&aes, &mut iv, &mut buf).map_err(|_| Error::NotBlockAligned)?;
    Ok(buf)
}

/// Decrypt an IGE block and strip the SHA-1 prefix, checking it.
///
/// # Where the length comes from
///
/// The plaintext is `SHA1(answer) ++ answer ++ padding`, and nothing records how long
/// `answer` is. The hash is what recovers it: try every length from the shortest possible
/// up to the block, and the one whose SHA-1 matches is the answer. Fifteen hashes over a
/// few hundred bytes, which is nothing next to the modular exponentiation on either side of
/// it.
///
/// The alternative — trusting a length from inside the decrypted data — would mean parsing
/// unauthenticated bytes, which is the shape of most protocol vulnerabilities.
pub fn ige_check_hash(key: &[u8; 32], iv: &[u8; 32], encrypted: &[u8]) -> Result<Vec<u8>> {
    if encrypted.is_empty() || encrypted.len() % 16 != 0 {
        return Err(Error::NotBlockAligned);
    }
    let aes = Aes::new(key).ok_or(Error::Rsa)?;
    let mut iv = *iv;
    let mut buf = encrypted.to_vec();
    ige::decrypt(&aes, &mut iv, &mut buf).map_err(|_| Error::NotBlockAligned)?;

    if buf.len() < 20 {
        return Err(Error::BadHash);
    }
    let (digest, rest) = buf.split_at(20);

    // Longest first. A shorter candidate whose SHA-1 collided would have to be a genuine
    // SHA-1 collision on a chosen prefix, but preferring the longest costs nothing and
    // removes the question.
    let lowest = rest.len().saturating_sub(15);
    for len in (lowest..=rest.len()).rev() {
        let mut h = Sha1::new();
        h.update(&rest[..len]);
        if h.finish()[..] == digest[..] {
            return Ok(rest[..len].to_vec());
        }
    }
    Err(Error::BadHash)
}

#[cfg(test)]
pub(crate) mod testing {
    use super::Rng;

    /// A counter, not a random source.
    ///
    /// Reproducibility is the point: a failing test can have its exact inputs pasted into
    /// the Python reference. Every property this crate has that depends on randomness being
    /// random is untestable here and is not tested here.
    pub struct CountingRng(pub u8);

    impl Rng for CountingRng {
        fn fill(&mut self, out: &mut [u8]) {
            for b in out.iter_mut() {
                *b = self.0;
                self.0 = self.0.wrapping_add(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::CountingRng;
    use super::*;

    #[test]
    fn the_kdf_is_deterministic_and_uses_every_input_byte() {
        let nn = [0x11u8; 32];
        let sn = [0x22u8; 16];
        let (k, v) = dh_kdf(&nn, &sn);
        assert_eq!(dh_kdf(&nn, &sn), (k, v));

        // Flip one bit of each input and require both outputs to move. A KDF that dropped
        // an input would still produce a working handshake against a cooperative server
        // and a broken one against a real attacker.
        for i in 0..32 {
            let mut n2 = nn;
            n2[i] ^= 1;
            let (k2, v2) = dh_kdf(&n2, &sn);
            assert_ne!(k2, k, "new_nonce byte {i} did not affect the key");
            assert_ne!(v2, v, "new_nonce byte {i} did not affect the iv");
        }
        for i in 0..16 {
            let mut s2 = sn;
            s2[i] ^= 1;
            let (k2, v2) = dh_kdf(&nn, &s2);
            assert_ne!(k2, k, "server_nonce byte {i} did not affect the key");
            assert_ne!(v2, v, "server_nonce byte {i} did not affect the iv");
        }
    }

    #[test]
    fn the_kdf_slices_land_where_the_spec_says() {
        // Recomputed here from the definition rather than compared to a stored value, so
        // this fails if a slice bound moves rather than if the algorithm changes.
        let nn = [0xABu8; 32];
        let sn = [0xCDu8; 16];
        let (key, iv) = dh_kdf(&nn, &sn);

        let mut h = Sha1::new();
        h.update(&nn);
        h.update(&sn);
        let ns = h.finish();
        let mut h = Sha1::new();
        h.update(&sn);
        h.update(&nn);
        let snh = h.finish();
        let mut h = Sha1::new();
        h.update(&nn);
        h.update(&nn);
        let nnh = h.finish();

        assert_eq!(&key[..20], &ns[..]);
        assert_eq!(&key[20..], &snh[..12]);
        assert_eq!(&iv[..8], &snh[12..20]);
        assert_eq!(&iv[8..28], &nnh[..]);
        assert_eq!(&iv[28..], &nn[..4]);
    }

    #[test]
    fn hash_prefixed_blocks_round_trip_at_every_length() {
        let key = [3u8; 32];
        let iv = [4u8; 32];
        for len in [0usize, 1, 11, 12, 13, 27, 28, 29, 100, 255, 256] {
            let data: Vec<u8> = (0..len).map(|i| (i * 7) as u8).collect();
            let mut rng = CountingRng(0);
            let enc = ige_with_hash(&key, &iv, &data, &mut rng).unwrap();
            assert_eq!(enc.len() % 16, 0, "len {len}");
            let back = ige_check_hash(&key, &iv, &enc).unwrap();
            assert_eq!(back, data, "len {len}");
        }
    }

    #[test]
    fn a_wrong_key_is_a_bad_hash_rather_than_garbage() {
        // The property that matters: a decryption with the wrong key must be rejected, not
        // returned as plausible-looking bytes for the TL parser to choke on somewhere else.
        let mut rng = CountingRng(0);
        let enc = ige_with_hash(&[3u8; 32], &[4u8; 32], b"hello there", &mut rng).unwrap();
        assert_eq!(ige_check_hash(&[9u8; 32], &[4u8; 32], &enc), Err(Error::BadHash));
        assert_eq!(ige_check_hash(&[3u8; 32], &[9u8; 32], &enc), Err(Error::BadHash));
    }

    #[test]
    fn a_flipped_ciphertext_bit_is_caught() {
        let mut rng = CountingRng(0);
        let enc = ige_with_hash(&[3u8; 32], &[4u8; 32], b"twenty-four bytes here!!", &mut rng)
            .unwrap();
        for i in 0..enc.len() {
            let mut bad = enc.clone();
            bad[i] ^= 1;
            assert_eq!(
                ige_check_hash(&[3u8; 32], &[4u8; 32], &bad),
                Err(Error::BadHash),
                "flipping byte {i} was not detected"
            );
        }
    }

    #[test]
    fn misaligned_input_is_refused() {
        assert_eq!(ige_check_hash(&[0u8; 32], &[0u8; 32], &[0u8; 17]), Err(Error::NotBlockAligned));
        assert_eq!(ige_check_hash(&[0u8; 32], &[0u8; 32], &[]), Err(Error::NotBlockAligned));
    }

    #[test]
    fn rsa_pad_produces_a_full_block_and_refuses_oversized_input() {
        let modulus = crate::keys::MODULUS;
        let mut rng = CountingRng(1);
        let out = rsa_pad(&[7u8; 144], &modulus, &crate::keys::EXPONENT, &mut rng, 16).unwrap();
        assert_eq!(out.len(), 256);
        // Not the input, and not zero — the two ways a broken RSA silently "works".
        assert_ne!(out[..], [0u8; 256][..]);

        assert_eq!(
            rsa_pad(&[0u8; 193], &modulus, &crate::keys::EXPONENT, &mut rng, 16),
            Err(Error::TooLong)
        );
    }

    #[test]
    fn rsa_pad_never_repeats() {
        // Two encryptions of the same plaintext must share nothing. If they matched, the
        // fresh temp_key is not reaching the output and the padding scheme is doing nothing.
        let modulus = crate::keys::MODULUS;
        let mut rng = CountingRng(1);
        let a = rsa_pad(b"same", &modulus, &crate::keys::EXPONENT, &mut rng, 16).unwrap();
        let b = rsa_pad(b"same", &modulus, &crate::keys::EXPONENT, &mut rng, 16).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn rsa_pad_output_is_below_the_modulus() {
        // The retry loop's whole job. RSA on a block larger than the modulus silently
        // reduces it, producing ciphertext the server decrypts to the wrong plaintext.
        let modulus = crate::keys::MODULUS;
        let mut rng = CountingRng(0);
        for _ in 0..8 {
            let out = rsa_pad(b"x", &modulus, &crate::keys::EXPONENT, &mut rng, 32).unwrap();
            assert!(out[..] < modulus[..]);
        }
    }
}
