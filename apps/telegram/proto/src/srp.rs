//! SRP-2048, as Telegram specifies it, for the two-factor password check.
//!
//! # What makes this worth eleven steps of care
//!
//! A wrong SRP produces exactly one symptom: the server says the password is incorrect. Not
//! an error, not a byte offset, not a hint — the same answer someone gets for typing the
//! wrong password. Every step below can be wrong in a way that produces that and nothing
//! else, so every step is checked against `vendor/research/mtproto/srp.py`, which is the
//! specification transcribed into a language with arbitrary-precision integers and
//! therefore no arithmetic of its own to be wrong.
//!
//! ```text
//! SH(data, salt) = SHA256(salt ++ data ++ salt)
//! x   = SH( pbkdf2_sha512( SH(SH(pw, salt1), salt2), salt1, 100000 ), salt2 )
//! v   = g^x mod p
//! k   = SHA256(p ++ pad(g))
//! g_a = g^a mod p
//! u   = SHA256(pad(g_a) ++ pad(g_b))
//! t   = (g_b - k·v) mod p
//! s_a = t^(a + u·x) mod p
//! M1  = SHA256( SHA256(p) XOR SHA256(pad(g)) ++ SHA256(salt1) ++ SHA256(salt2)
//!               ++ pad(g_a) ++ pad(g_b) ++ SHA256(s_a) )
//! ```
//!
//! # Padding to 256 bytes is the trap
//!
//! Every operand is hashed at the full width of the prime. A value whose top byte happens
//! to be zero is 255 bytes as a minimal encoding, hashes to something else, and fails —
//! about one time in 256, so a version that gets this wrong works in testing and fails in
//! the field on one login in a couple of hundred.
//!
//! # Three exponentiations, none of them here
//!
//! `v`, `g_a` and `s_a` are 2048-bit modular exponentiations, 815 ms each on an E72. They
//! come out as [`Step`] for the caller to place on the worker thread, the same contract the
//! handshake uses — see [`crate::client`].
//!
//! The exponent of the third is `a + u·x`, which is **2049 bits**: `a` is a full 2048 and
//! `u·x` is 512, so the sum carries out. `bignum::modpow` walks the exponent slice bit by
//! bit with no width bound, so it takes a 257-byte operand as it stands — but that is not
//! obvious from its signature and a future tidy-up could remove it, so there is a test that
//! says so.

use alloc::vec;
use alloc::vec::Vec;

use symbian_crypto::{bignum, pbkdf2::pbkdf2_hmac_sha512, Sha256};

use crate::crypto::Rng;

/// Telegram's iteration count, from the KDF algorithm's own name:
/// `passwordKdfAlgoSHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow`.
pub const ITERATIONS: u32 = 100_000;

/// Width every operand is padded to before hashing, and the width of the prime.
pub const WIDTH: usize = 256;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Error {
    /// `p` was not 2048 bits, or not Telegram's.
    BadPrime,
    /// `g` outside the published range.
    BadGenerator,
    /// `g_a` or `g_b` at a degenerate value, where the shared secret is known without
    /// solving anything.
    BadPublicValue,
    /// The arithmetic refused an operand.
    Bignum,
}

pub type Result<T> = core::result::Result<T, Error>;

/// What the caller must compute before the next step.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Step {
    /// `base^exp mod modulus`, big-endian, off the GUI thread.
    ModPow { base: Vec<u8>, exp: Vec<u8>, modulus: Vec<u8> },
    /// Finished: send these to `auth.checkPassword`.
    Done { srp_id: i64, a: Vec<u8>, m1: Vec<u8> },
}

/// `SH(data, salt) = SHA256(salt ++ data ++ salt)`.
///
/// The salt on **both** sides. One side is a different function that produces plausible
/// output and matches nothing.
fn sh(data: &[u8], salt: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(salt);
    h.update(data);
    h.update(salt);
    h.finish()
}

/// Left-pad to the width of the prime. See the module docs on why this is not optional.
fn pad(v: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; WIDTH];
    let n = v.len().min(WIDTH);
    out[WIDTH - n..].copy_from_slice(&v[v.len() - n..]);
    out
}

fn sha256_of(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    h.finish()
}

/// `x`, the password's derived secret. The slow part: 100,000 PBKDF2 iterations.
///
/// Separate from the state machine because it is the one piece that belongs on the worker
/// thread for a reason other than modular arithmetic, and because it is what the self test
/// measures.
pub fn derive_x(password: &[u8], salt1: &[u8], salt2: &[u8]) -> [u8; 32] {
    let ph1 = sh(&sh(password, salt1), salt2);
    let mut dk = [0u8; 64];
    pbkdf2_hmac_sha512(&ph1, salt1, ITERATIONS, &mut dk);
    sh(&dk, salt2)
}

/// The exchange, as a state machine over the three exponentiations.
pub struct Srp {
    srp_id: i64,
    p: Vec<u8>,
    g: u32,
    g_b: Vec<u8>,
    salt1: Vec<u8>,
    salt2: Vec<u8>,
    x: [u8; 32],
    a: Vec<u8>,
    /// Filled in as the exponentiations come back.
    v: Vec<u8>,
    g_a: Vec<u8>,
    stage: Stage,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Stage {
    AwaitV,
    AwaitGa,
    AwaitSa,
    Done,
}

impl Srp {
    /// Begin, having already derived `x`.
    ///
    /// `x` is passed in rather than derived here because deriving it is twelve seconds of
    /// PBKDF2 on the handset and belongs on the worker thread, which this crate cannot
    /// reach. [`derive_x`] is what the worker runs.
    pub fn start<R: Rng>(
        srp_id: i64,
        p: &[u8],
        g: u32,
        g_b: &[u8],
        salt1: &[u8],
        salt2: &[u8],
        x: [u8; 32],
        rng: &mut R,
    ) -> Result<(Self, Step)> {
        check_params(p, g, g_b)?;

        let mut a = vec![0u8; WIDTH];
        rng.fill(&mut a);
        // Full width, so the exponent is genuinely 2048 bits however the draw fell. The
        // ladder costs the same either way.
        a[0] |= 0x80;

        let srp = Srp {
            srp_id,
            p: p.to_vec(),
            g,
            g_b: pad(g_b),
            salt1: salt1.to_vec(),
            salt2: salt2.to_vec(),
            x,
            a,
            v: Vec::new(),
            g_a: Vec::new(),
            stage: Stage::AwaitV,
        };

        // v = g^x mod p. A 256-bit exponent, so about an eighth of the cost of the others.
        let step = Step::ModPow {
            base: vec![srp.g as u8],
            exp: srp.x.to_vec(),
            modulus: srp.p.clone(),
        };
        Ok((srp, step))
    }

    /// Feed back a [`Step::ModPow`] result.
    pub fn on_modpow(&mut self, result: &[u8]) -> Result<Step> {
        match self.stage {
            Stage::AwaitV => {
                self.v = pad(result);
                self.stage = Stage::AwaitGa;
                Ok(Step::ModPow {
                    base: vec![self.g as u8],
                    exp: self.a.clone(),
                    modulus: self.p.clone(),
                })
            }
            Stage::AwaitGa => {
                self.g_a = pad(result);
                // g_a must be in range too. A degenerate one is a bad draw rather than an
                // attack, and it is one chance in 2^2040 -- but the check costs nothing and
                // the alternative is a proof the server rejects for no visible reason.
                in_range(&self.g_a, &self.p)?;
                self.stage = Stage::AwaitSa;
                self.build_sa_step()
            }
            Stage::AwaitSa => {
                let s_a = pad(result);
                self.stage = Stage::Done;
                self.finish(&s_a)
            }
            Stage::Done => Err(Error::Bignum),
        }
    }

    /// `t = (g_b - k·v) mod p`, and the exponent `a + u·x`.
    fn build_sa_step(&mut self) -> Result<Step> {
        let m = bignum::Modulus::new(&self.p).map_err(|_| Error::Bignum)?;

        let g_padded = pad(&[self.g as u8]);
        let k = sha256_of(&[&self.p, &g_padded]);

        let mut k_v = vec![0u8; WIDTH];
        bignum::mulmod(&k, &self.v, &m, &mut k_v).map_err(|_| Error::Bignum)?;

        let mut t = vec![0u8; WIDTH];
        bignum::submod(&self.g_b, &k_v, &m, &mut t).map_err(|_| Error::Bignum)?;

        let u = sha256_of(&[&self.g_a, &self.g_b]);
        let exp = add_mul(&self.a, &u, &self.x);

        Ok(Step::ModPow { base: t, exp, modulus: self.p.clone() })
    }

    fn finish(&mut self, s_a: &[u8]) -> Result<Step> {
        let g_padded = pad(&[self.g as u8]);
        let k_a = sha256_of(&[s_a]);

        let hp = sha256_of(&[&self.p]);
        let hg = sha256_of(&[&g_padded]);
        let mut xored = [0u8; 32];
        for i in 0..32 {
            xored[i] = hp[i] ^ hg[i];
        }

        let m1 = sha256_of(&[
            &xored,
            &sha256_of(&[&self.salt1]),
            &sha256_of(&[&self.salt2]),
            &self.g_a,
            &self.g_b,
            &k_a,
        ]);

        Ok(Step::Done {
            srp_id: self.srp_id,
            a: self.g_a.clone(),
            m1: m1.to_vec(),
        })
    }
}

/// `a + u·x`, big-endian, with room for the carry.
///
/// `a` is 256 bytes and `u·x` is 64, so the sum is at most 257 — one bit past the prime.
/// Schoolbook, because it runs once per login against three exponentiations of 815 ms each,
/// and a clever version would be a second implementation of something the reference already
/// does in one line.
fn add_mul(a: &[u8], u: &[u8; 32], x: &[u8; 32]) -> Vec<u8> {
    // u * x, 32x32 bytes into 64.
    let mut prod = [0u8; 64];
    for i in (0..32).rev() {
        let mut carry = 0u32;
        for j in (0..32).rev() {
            let at = i + j + 1;
            let v = prod[at] as u32 + u[i] as u32 * x[j] as u32 + carry;
            prod[at] = v as u8;
            carry = v >> 8;
        }
        // The carry lands one place left of the last written byte.
        let mut at = i;
        while carry != 0 {
            let v = prod[at] as u32 + (carry & 0xff);
            prod[at] = v as u8;
            carry = (carry >> 8) + (v >> 8);
            if at == 0 {
                break;
            }
            at -= 1;
        }
    }

    // a + prod, right-aligned, into a buffer one byte wider than `a`.
    let width = a.len() + 1;
    let mut out = vec![0u8; width];
    let mut carry = 0u16;
    for k in 0..width {
        let ia = a.len().checked_sub(k + 1).map(|i| a[i] as u16).unwrap_or(0);
        let ip = 64usize.checked_sub(k + 1).map(|i| prod[i] as u16).unwrap_or(0);
        let v = ia + ip + carry;
        out[width - 1 - k] = v as u8;
        carry = v >> 8;
    }
    out
}

/// The prime, the generator and the server's public value.
fn check_params(p: &[u8], g: u32, g_b: &[u8]) -> Result<()> {
    if p.len() != WIDTH || p[0] & 0x80 == 0 {
        return Err(Error::BadPrime);
    }
    // The same hash the handshake checks dh_prime against. Telegram uses one prime for both,
    // and verifying that a 2048-bit number is a safe prime is minutes on this hardware --
    // long enough that no login would complete. If it changes, this fails loudly.
    let mut h = symbian_crypto::Sha256::new();
    h.update(p);
    if h.finish() != crate::handshake::DH_PRIME_SHA256 {
        return Err(Error::BadPrime);
    }
    if !(2..=7).contains(&g) {
        return Err(Error::BadGenerator);
    }
    in_range(&pad(g_b), p).map_err(|_| Error::BadPublicValue)
}

/// `1 < v < p - 1`.
///
/// Both ends. `v` of 0, 1 or `p-1` forces the shared secret to something an attacker knows
/// without solving anything, and clients have shipped without the check.
fn in_range(v: &[u8], p: &[u8]) -> Result<()> {
    if v.len() != WIDTH || p.len() != WIDTH {
        return Err(Error::BadPublicValue);
    }
    if v.iter().all(|&b| b == 0) {
        return Err(Error::BadPublicValue);
    }
    if v[..WIDTH - 1].iter().all(|&b| b == 0) && v[WIDTH - 1] == 1 {
        return Err(Error::BadPublicValue);
    }
    if v[..] >= p[..] {
        return Err(Error::BadPublicValue);
    }
    let mut p1 = [0u8; WIDTH];
    p1.copy_from_slice(p);
    // The prime is odd, so subtracting one touches only the last byte.
    p1[WIDTH - 1] -= 1;
    if v[..] == p1[..] {
        return Err(Error::BadPublicValue);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sh_puts_the_salt_on_both_sides() {
        // One side produces plausible output and matches nothing. Checked against the
        // definition rather than a stored value.
        let got = sh(b"data", b"salt");
        let want = {
            let mut h = Sha256::new();
            h.update(b"salt");
            h.update(b"data");
            h.update(b"salt");
            h.finish()
        };
        assert_eq!(got, want);
        let one_sided = {
            let mut h = Sha256::new();
            h.update(b"salt");
            h.update(b"data");
            h.finish()
        };
        assert_ne!(got, one_sided);
    }

    #[test]
    fn padding_is_left_aligned_to_the_prime_width() {
        assert_eq!(pad(&[1]).len(), WIDTH);
        assert_eq!(pad(&[1])[WIDTH - 1], 1);
        assert_eq!(pad(&[1])[0], 0);
        // A value already at full width is unchanged.
        let full = vec![0xABu8; WIDTH];
        assert_eq!(pad(&full), full);
    }

    #[test]
    fn add_mul_matches_schoolbook_on_small_values() {
        // a + u*x, checked against u128 arithmetic on operands small enough to fit.
        let mut a = [0u8; 256];
        a[255] = 5;
        let mut u = [0u8; 32];
        u[31] = 7;
        let mut x = [0u8; 32];
        x[31] = 11;
        let out = add_mul(&a, &u, &x);
        assert_eq!(out.len(), 257);
        assert_eq!(out[256], 5 + 7 * 11);
        assert!(out[..256].iter().all(|&b| b == 0));
    }

    #[test]
    fn add_mul_carries_across_the_width() {
        // a is all ones, so adding anything carries out into the extra byte. That byte is
        // the whole reason the exponent is 257 bytes and not 256.
        let a = [0xFFu8; 256];
        let mut u = [0u8; 32];
        u[31] = 1;
        let mut x = [0u8; 32];
        x[31] = 1;
        let out = add_mul(&a, &u, &x);
        assert_eq!(out[0], 1, "the carry did not reach the extra byte");
        assert_eq!(out[256], 0);
    }

    #[test]
    fn degenerate_public_values_are_refused() {
        let p = vec![0xFFu8; WIDTH];
        let zero = vec![0u8; WIDTH];
        assert_eq!(in_range(&zero, &p), Err(Error::BadPublicValue));
        let mut one = vec![0u8; WIDTH];
        one[WIDTH - 1] = 1;
        assert_eq!(in_range(&one, &p), Err(Error::BadPublicValue));
        assert_eq!(in_range(&p, &p), Err(Error::BadPublicValue));
        let mut pm1 = p.clone();
        pm1[WIDTH - 1] -= 1;
        assert_eq!(in_range(&pm1, &p), Err(Error::BadPublicValue));
        // And something in the middle is fine.
        let mut ok = vec![0u8; WIDTH];
        ok[WIDTH - 1] = 2;
        assert_eq!(in_range(&ok, &p), Ok(()));
    }

    #[test]
    fn a_foreign_prime_is_refused() {
        // The one check that stands between this and a server-chosen prime with small
        // factors, which would make the exchange breakable.
        let mut p = vec![0u8; WIDTH];
        p[0] = 0xFF;
        p[WIDTH - 1] = 1;
        assert_eq!(check_params(&p, 3, &vec![2u8; WIDTH]), Err(Error::BadPrime));
    }

    #[test]
    fn the_exponent_is_wider_than_the_modulus_and_modpow_takes_it() {
        // 2049 bits: `a` is a full 2048 and u*x adds 512, so the sum carries out. modpow
        // walks the exponent slice bit by bit with no width bound -- true today, not
        // obvious from the signature, and this is the test that would fail if a tidy-up
        // ever bounded it.
        let p = vec![0x8Fu8; 32]; // odd, top bit set
        let m = bignum::Modulus::new(&p).unwrap();
        let wide = vec![0xFFu8; 40]; // 320 bits of exponent against a 256-bit modulus
        let mut out = vec![0u8; 32];
        assert!(bignum::modpow(&[3], &wide, &m, &mut out).is_ok());
        assert!(out.iter().any(|&b| b != 0));
    }
}
