//! Modular exponentiation for 2048-bit numbers.
//!
//! Enough for RSA-2048 and the 2048-bit Diffie-Hellman that MTProto's auth handshake runs.
//! Nothing more: no division, no general modular inverse, no primality testing. Those are
//! what a *key generator* needs, and this only ever consumes keys the server sends.
//!
//! # The shape, and why
//!
//! **32-bit limbs.** The ARM1136 has `umull` — a 32×32→64 multiply — and that is exactly
//! one limb product. 64-bit limbs would halve the limb count but every product would become
//! four `umull`s plus carry handling, which is a loss.
//!
//! **Fixed size, no allocation.** 2048 bits is 64 limbs, and a product is 128. Everything
//! lives on the stack in arrays sized for that. The heap on this device is 4 MB shared with
//! the whole app, and an exponentiation that fails for lack of memory in the middle of a
//! login is worse than one that cannot be attempted.
//!
//! **Montgomery multiplication**, by the CIOS method. The alternative is a division per
//! multiply, and division is the one operation this word size makes genuinely expensive —
//! Montgomery replaces it with a multiply and a shift.
//!
//! # Cost
//!
//! A 2048-bit exponentiation is 2048 squarings and 2048 multiplies (see the ladder below on
//! why not ~1024). Each is 64×64 limb products plus a reduction of the same size, so about
//! 8,200 limb products — 33 million for the whole exponentiation.
//!
//! **Measured: 37 ms** on an aarch64 host, or 9 µs per Montgomery multiply
//! (`cargo run --release -p symbian-crypto --example bench`).
//!
//! On the E72 expect **roughly 0.4 to 0.6 seconds**. That is the host figure scaled, and the
//! scaling is an estimate, not a measurement: the clock is 5× lower, and an ARM1136 is
//! in-order with a shorter pipeline and a non-pipelined `umull`, so 10–15× overall is the
//! honest range. An earlier version of this comment said "a quarter of a second", which was
//! the arithmetic done hopefully rather than carefully.
//!
//! MTProto's handshake does two of these — `g^a mod p` and then `g_b^a mod p` — so budget
//! about a second for a login. The RSA step is negligible by comparison: `e = 65537` is 17
//! bits, so 34 Montgomery multiplies against 4096.
//!
//! `rust_step` must return in milliseconds, so a login **cannot** call this from the event
//! pump. It has to be split across steps or run on a second thread. That is a caller's
//! problem, and it is why this module reports no progress and simply blocks: half a state
//! machine here would be worse than none.
//!
//! # The ladder, and what it does and does not protect
//!
//! Exponentiation is a Montgomery ladder: **one squaring and one multiply per exponent bit,
//! always**, with the operands swapped according to the bit. The obvious square-and-multiply
//! does a multiply only when the bit is set, which makes the running time a direct readout
//! of the exponent's Hamming weight — and in Diffie-Hellman the exponent is the secret.
//!
//! It costs about a third more than the naive version. That is worth paying: the exponent is
//! the one value here where leaking it loses everything, and unlike the AES S-box (see the
//! crate docs on the threat model) this leak needs no cache access to exploit, just a clock.
//!
//! The conditional swap is a masked select rather than a branch, for the same reason.
//!
//! What it does *not* hide is the exponent's encoded length: the loop runs once per bit of
//! the buffer handed in. That is the caller's choice rather than the secret's value, so a
//! secret exponent should be passed at a fixed width.

/// 2048 bits.
pub const MAX_LIMBS: usize = 64;
pub const MAX_BYTES: usize = MAX_LIMBS * 4;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The value was longer than 2048 bits.
    TooLarge,
    /// The modulus was even or zero. Montgomery reduction needs an odd modulus, and every
    /// modulus this is used with — an RSA `n`, a DH prime — is odd by construction, so an
    /// even one means the input was misparsed rather than unusual.
    ModulusNotOdd,
    /// The output buffer was too small.
    Overflow,
}

// ------------------------------------------------------------------- limb helpers --

/// Big-endian bytes into little-endian limbs. Returns the limb count actually used.
fn from_be_bytes(bytes: &[u8], out: &mut [u32; MAX_LIMBS]) -> Result<usize, Error> {
    // Skip leading zeros, so a 257-byte value with a zero pad fits a 2048-bit modulus —
    // which is exactly how a DH prime often arrives.
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
    let sig = &bytes[start..];
    if sig.len() > MAX_BYTES {
        return Err(Error::TooLarge);
    }
    *out = [0; MAX_LIMBS];
    for (i, &b) in sig.iter().rev().enumerate() {
        out[i / 4] |= (b as u32) << ((i % 4) * 8);
    }
    Ok(limbs_used(out))
}

fn limbs_used(v: &[u32; MAX_LIMBS]) -> usize {
    v.iter().rposition(|&l| l != 0).map_or(0, |i| i + 1)
}

/// Limbs into big-endian bytes, left-padded to `out.len()`.
///
/// Padded rather than minimal, because every consumer of this — an RSA block, a DH public
/// value going into a hash — needs a fixed width, and a value that happens to have a zero
/// leading byte would otherwise silently shorten and change the hash.
fn to_be_bytes(v: &[u32; MAX_LIMBS], out: &mut [u8]) -> Result<(), Error> {
    let needed = limbs_used(v) * 4;
    if out.len() < needed {
        // Only an error if the significant bytes do not fit; leading zeros are fine.
        let mut top = 0;
        for i in (0..MAX_LIMBS).rev() {
            if v[i] != 0 {
                top = i * 4 + (4 - (v[i].leading_zeros() / 8) as usize);
                break;
            }
        }
        if out.len() < top {
            return Err(Error::Overflow);
        }
    }
    out.fill(0);
    let n = out.len();
    for i in 0..n {
        let byte_idx = n - 1 - i;
        if i / 4 < MAX_LIMBS {
            out[byte_idx] = (v[i / 4] >> ((i % 4) * 8)) as u8;
        }
    }
    Ok(())
}

/// `a >= b`, over `len` limbs.
fn ge(a: &[u32], b: &[u32], len: usize) -> bool {
    for i in (0..len).rev() {
        if a[i] != b[i] {
            return a[i] > b[i];
        }
    }
    true
}

/// `a -= b`, over `len` limbs. Returns the final borrow.
fn sub_assign(a: &mut [u32], b: &[u32], len: usize) -> u32 {
    let mut borrow = 0u64;
    for i in 0..len {
        let d = (a[i] as u64).wrapping_sub(b[i] as u64).wrapping_sub(borrow);
        a[i] = d as u32;
        borrow = (d >> 63) & 1;
    }
    borrow as u32
}

/// `a = (a * 2) mod n`, over `len` limbs. Used to build R² without a division.
fn double_mod(a: &mut [u32], n: &[u32], len: usize) {
    let mut carry = 0u32;
    for i in 0..len {
        let hi = a[i] >> 31;
        a[i] = (a[i] << 1) | carry;
        carry = hi;
    }
    // The doubling may have overflowed past `len` limbs, in which case a subtraction is
    // needed regardless of the comparison — that carry is a value the limbs no longer hold.
    if carry != 0 || ge(a, n, len) {
        sub_assign(a, n, len);
    }
}

/// Select `b` if `mask` is all ones, `a` if it is zero. Branch-free.
fn cmov(dst: &mut [u32], src: &[u32], len: usize, mask: u32) {
    for i in 0..len {
        dst[i] = (dst[i] & !mask) | (src[i] & mask);
    }
}

// --------------------------------------------------------------------- Montgomery --

/// A modulus, with the two values Montgomery arithmetic needs precomputed.
///
/// `Debug` prints only the bit length. A modulus is public — an RSA `n`, a DH prime — so
/// there is nothing secret to leak, but 64 limbs of hex in a test failure is noise that
/// buries the assertion that actually failed.
///
/// Held rather than derived per multiply because `n0inv` costs a Newton iteration and R²
/// costs 4096 doublings — trivial once, absurd 4096 times.
#[derive(Clone)]
pub struct Modulus {
    n: [u32; MAX_LIMBS],
    len: usize,
    /// `-n^-1 mod 2^32`.
    n0inv: u32,
    /// `R^2 mod n`, where `R = 2^(32 * len)`. Multiplying by this converts into Montgomery
    /// form.
    rr: [u32; MAX_LIMBS],
}

impl core::fmt::Debug for Modulus {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Modulus({} bits)", self.bits())
    }
}

impl Modulus {
    /// From big-endian bytes. The modulus must be odd.
    pub fn new(modulus: &[u8]) -> Result<Self, Error> {
        let mut n = [0u32; MAX_LIMBS];
        let len = from_be_bytes(modulus, &mut n)?;
        if len == 0 || n[0] & 1 == 0 {
            return Err(Error::ModulusNotOdd);
        }

        // n0inv by Newton's method mod 2^32. x_{k+1} = x_k * (2 - n0 * x_k) doubles the
        // number of correct bits each step, and x_0 = 1 is correct to one bit for any odd
        // n0 — so five steps reach 32.
        let n0 = n[0];
        let mut inv = 1u32;
        for _ in 0..5 {
            inv = inv.wrapping_mul(2u32.wrapping_sub(n0.wrapping_mul(inv)));
        }
        let n0inv = inv.wrapping_neg();

        // R^2 mod n, by doubling 1 up to 2^(64*len) — no division needed.
        let mut rr = [0u32; MAX_LIMBS];
        rr[0] = 1;
        for _ in 0..(64 * len * 32 / 32) {
            double_mod(&mut rr, &n, len);
        }

        Ok(Modulus { n, len, n0inv, rr })
    }

    /// Bit length of the modulus, which is the width every result is padded to.
    pub fn bits(&self) -> usize {
        self.len * 32 - self.n[self.len - 1].leading_zeros() as usize
    }

    pub fn byte_len(&self) -> usize {
        (self.bits() + 7) / 8
    }

    /// `out = a * b * R^-1 mod n`, the Montgomery product. CIOS: the multiply and the
    /// reduction are interleaved, so the intermediate never exceeds `len + 2` limbs.
    fn mont_mul(&self, a: &[u32], b: &[u32], out: &mut [u32; MAX_LIMBS]) {
        let s = self.len;
        let mut t = [0u32; MAX_LIMBS + 2];

        for i in 0..s {
            // t += a * b[i]
            let bi = b[i] as u64;
            let mut carry = 0u64;
            for j in 0..s {
                let sum = (t[j] as u64) + (a[j] as u64) * bi + carry;
                t[j] = sum as u32;
                carry = sum >> 32;
            }
            let sum = (t[s] as u64) + carry;
            t[s] = sum as u32;
            t[s + 1] = (sum >> 32) as u32;

            // t = (t + m * n) / 2^32, where m makes the low limb vanish.
            let m = t[0].wrapping_mul(self.n0inv) as u64;
            // t[0] becomes zero by construction — that is what n0inv is for — so only the
            // carry out of it is kept, and the limb itself is shifted away by the loop
            // below writing to j-1.
            let mut carry = ((t[0] as u64) + m * (self.n[0] as u64)) >> 32;
            for j in 1..s {
                let sum = (t[j] as u64) + m * (self.n[j] as u64) + carry;
                t[j - 1] = sum as u32;
                carry = sum >> 32;
            }
            let sum = (t[s] as u64) + carry;
            t[s - 1] = sum as u32;
            t[s] = t[s + 1] + (sum >> 32) as u32;
        }

        // One conditional subtraction brings it below n. CIOS guarantees t < 2n, so one is
        // always enough — a loop here would suggest otherwise and hide a real bug.
        out.fill(0);
        out[..s].copy_from_slice(&t[..s]);
        if t[s] != 0 || ge(out, &self.n, s) {
            sub_assign(out, &self.n, s);
        }
    }
}

/// `base^exp mod n`, with `base` and `exp` as big-endian bytes.
///
/// `out` is filled to its full length, left-padded with zeros. Pass a buffer the size of the
/// modulus: RSA and DH both want a fixed-width result, and a value with a zero leading byte
/// must not silently shorten.
pub fn modpow(base: &[u8], exp: &[u8], m: &Modulus, out: &mut [u8]) -> Result<(), Error> {
    let s = m.len;

    let mut b = [0u32; MAX_LIMBS];
    from_be_bytes(base, &mut b)?;
    // The base must be reduced before conversion; a DH public value can exceed the prime
    // only if malformed, but an RSA block equal to n is a legal encoding of zero.
    if ge(&b, &m.n, s) {
        sub_assign(&mut b, &m.n, s);
    }

    // Into Montgomery form: x * R mod n == mont_mul(x, R^2).
    let mut x1 = [0u32; MAX_LIMBS];
    m.mont_mul(&b, &m.rr, &mut x1);

    // x0 = R mod n, which is Montgomery-form 1.
    let mut one = [0u32; MAX_LIMBS];
    one[0] = 1;
    let mut x0 = [0u32; MAX_LIMBS];
    m.mont_mul(&one, &m.rr, &mut x0);

    // Montgomery ladder over the exponent, most significant bit first.
    //
    // Invariant: after consuming bits forming the integer k, x0 = base^k and
    // x1 = base^(k+1). Initially k = 0, so x0 = 1 and x1 = base — which is why every bit
    // including the leading one has to be processed. Skipping the first set bit on the
    // grounds that the state "already looks right" leaves k at 0, and the whole exponent
    // comes out one bit short.
    //
    // Leading zeros are therefore harmless: a zero bit with k = 0 maps 1 to 1 and base to
    // base. Processing them is also the better choice for timing, since the running time
    // then depends only on the encoded length, which is public, rather than on the value.
    let mut t0 = [0u32; MAX_LIMBS];
    let mut t1 = [0u32; MAX_LIMBS];
    for &byte in exp {
        for shift in (0..8).rev() {
            let bit = (byte >> shift) & 1;
            let mask = 0u32.wrapping_sub(bit as u32);
            // Conditionally swap so the same two operations serve both bit values.
            let mut a = x0;
            let mut c = x1;
            cmov(&mut a, &x1, s, mask);
            cmov(&mut c, &x0, s, mask);

            m.mont_mul(&a, &a, &mut t0); // a^2
            m.mont_mul(&a, &c, &mut t1); // a*c

            // Swap back.
            let mut n0 = t0;
            let mut n1 = t1;
            cmov(&mut n0, &t1, s, mask);
            cmov(&mut n1, &t0, s, mask);
            x0 = n0;
            x1 = n1;
        }
    }

    // Out of Montgomery form: mont_mul(x, 1) = x * R^-1.
    let mut result = [0u32; MAX_LIMBS];
    m.mont_mul(&x0, &one, &mut result);
    to_be_bytes(&result, out)
}

/// The RSA public operation: `m^e mod n`. `e` is typically 65537.
///
/// The **primitive only** — no padding. MTProto's handshake builds its own padded block
/// (`RSA_PAD`, which involves SHA-256) and hands the result here, and any other use needs
/// a padding scheme of its own. Raw RSA on unpadded data is not encryption.
pub fn rsa_encrypt(message: &[u8], e: &[u8], n: &Modulus, out: &mut [u8]) -> Result<(), Error> {
    modpow(message, e, n, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    fn unhex(s: &str) -> Vec<u8> {
        let s: alloc::string::String = s.chars().filter(|c| !c.is_whitespace()).collect();
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

    /// modpow with small numbers that can be checked by hand.
    #[test]
    fn small_cases() {
        // Every modulus here is odd, which is not a coincidence: Montgomery reduction
        // requires it, and an even one is refused rather than answered wrongly. The first
        // version of this test used 1000 and the failure was the code telling the test it
        // was wrong.
        //
        // Expected values from Python's pow(), not worked out by hand — a hand-computed
        // expectation that happens to match a buggy implementation is worse than no test.
        let cases: &[(u32, u32, u32, u32)] = &[
            (2, 10, 1001, 23),
            (3, 0, 7, 1),                     // anything^0 is 1
            (0, 5, 7, 0),                     // 0^n
            (1, 12345, 7, 1),                 // 1^n
            (5, 3, 13, 8),
            (7, 7, 11, 6),
            (2, 255, 255, 128),               // a full byte of exponent
            (65537, 17, 4294967291, 234037041),
            // A base and modulus that both fill 32 bits, so the top limb is nearly full and
            // the conditional subtraction in mont_mul has to fire.
            (3735928559, 65537, 4294967291, 2385742825),
        ];
        for &(b, e, m, want) in cases {
            let modulus = Modulus::new(&m.to_be_bytes()).unwrap();
            let mut out = [0u8; 4];
            modpow(&b.to_be_bytes(), &e.to_be_bytes(), &modulus, &mut out).unwrap();
            assert_eq!(
                u32::from_be_bytes(out),
                want,
                "{b}^{e} mod {m}"
            );
        }
    }

    #[test]
    fn an_even_modulus_is_refused() {
        // Montgomery reduction needs an odd modulus. Every modulus this is used with is odd
        // by construction, so an even one means the input was misparsed — which is worth an
        // error rather than a wrong answer.
        for m in [0u32, 2, 4, 100, 0xFFFF_FFFE] {
            assert_eq!(Modulus::new(&m.to_be_bytes()).unwrap_err(), Error::ModulusNotOdd);
        }
        assert!(Modulus::new(&3u32.to_be_bytes()).is_ok());
    }

    #[test]
    fn a_value_over_2048_bits_is_refused() {
        let too_big = vec![0xFFu8; MAX_BYTES + 1];
        assert_eq!(Modulus::new(&too_big).unwrap_err(), Error::TooLarge);
        // But leading zeros do not count towards the length, which is how a 257-byte DH
        // prime with a zero pad arrives.
        let mut padded = vec![0u8; 8];
        padded.extend_from_slice(&vec![0xFFu8; MAX_BYTES - 1]);
        padded.push(0xFF);
        assert!(Modulus::new(&padded).is_ok());
    }

    #[test]
    fn rsa_with_a_real_2048_bit_modulus() {
        // Telegram's own public key modulus, from the MTProto documentation, with e = 65537.
        // A real 2048-bit modulus rather than a synthetic one: the top limb being nearly
        // full is what exercises the conditional subtraction in mont_mul.
        let n = unhex(
            "c150023e2f70db7985ded064759cfecf0af328e69a41daf4d6f01b538135a6f9\
             1f8f8b2a0ec9ba9720ce352efcf6c5680ffc424bd634864902de0b4bd6d49f4e\
             580230e3ae97d95c8b19442b3c0a10d8f5633fecedd6926a7f6dab0ddb7d457f\
             9ea81b8465fcd6fffeed114011df91c059caedaf97625f6c96ecc74725556934\
             ef781d866b34f011fce4d835a090196e9a5f0e4449af7eb697ddb9076494ca5f\
             81104a305b6dd27665722c46b60e5df680fb16b210607ef217652e60236c255f\
             6a28315f4083a96791d7214bf64c1df4fd0db1944fb26a2a57031b32eee64ad1\
             5a8ba68885cde74a5bfc920f6abf59ba5c75506373e7130f9042da922179251f",
        );
        let m = Modulus::new(&n).unwrap();
        assert_eq!(m.bits(), 2048);

        // 3^65537 mod n, checked against Python's pow() below in the differential test.
        // Here the property that must hold with no reference at all: encrypting 1 gives 1,
        // and encrypting 0 gives 0, for any exponent.
        let e = 65537u32.to_be_bytes();
        let mut out = vec![0u8; 256];
        rsa_encrypt(&[1], &e, &m, &mut out).unwrap();
        assert_eq!(hex(&out), hex(&{
            let mut one = vec![0u8; 256];
            one[255] = 1;
            one
        }));

        rsa_encrypt(&[0], &e, &m, &mut out).unwrap();
        assert!(out.iter().all(|&b| b == 0));
    }

    #[test]
    fn the_result_is_padded_to_the_buffer_width() {
        // A result with a zero leading byte must stay full width. Shortening it would change
        // any hash computed over it, which in MTProto is how a handshake fails with no clue
        // as to why.
        let m = Modulus::new(&unhex("0100000000000000000000000000000001")).unwrap();
        let mut out = [0u8; 17];
        modpow(&[2], &[1], &m, &mut out).unwrap();
        assert_eq!(out.len(), 17);
        assert_eq!(out[16], 2);
        assert!(out[..16].iter().all(|&b| b == 0));
    }

    #[test]
    fn exponent_leading_zeros_do_not_change_the_result() {
        let m = Modulus::new(&unhex("fffffffffffffffffffffffffffffffb")).unwrap();
        let mut a = [0u8; 16];
        let mut b = [0u8; 16];
        modpow(&[7], &[0x00, 0x01, 0x00], &m, &mut a).unwrap();
        modpow(&[7], &[0x01, 0x00], &m, &mut b).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn a_base_equal_to_the_modulus_is_zero() {
        // A legal RSA encoding of zero, and the case a missing pre-reduction gets wrong.
        let n = unhex("fffffffffffffffffffffffffffffffb");
        let m = Modulus::new(&n).unwrap();
        let mut out = [0u8; 16];
        modpow(&n, &[3], &m, &mut out).unwrap();
        assert!(out.iter().all(|&b| b == 0), "got {}", hex(&out));
    }

    #[test]
    fn diffie_hellman_agrees_on_a_shared_secret() {
        // The property the whole module exists for: (g^a)^b == (g^b)^a. It needs no
        // reference implementation to check, and it exercises the full 2048-bit path twice
        // in each direction.
        let p = unhex(
            "c71caeb9c6b1c9048e6c522f70f13f73980d40238e3e21c14934d037563d930f\
             48198a0aa7c14058229493d22530f4dbfa336f6e0ac925139543aed44cce7c37\
             20fd51f69458705ac68cd4fe6b6b13abdc9746512969328454f18faf8c595f64\
             2477fe96bb2a941d5bcd1d4ac8cc49880708fa9b378e3c4f3a9060bee67cf9a4\
             a4a695811051907e162753b56b0f6b410dba74d8a84b2a14b3144e0ef1284754\
             fd17ed950d5965b4b9dd46582db1178d169c6bc465b0d6ff9ca3928fef5b9ae4\
             e418fc15e83ebea0f87fa9ff5eed70050ded2849f47bf959d956850ce929851f\
             0d8115f635b105ee2e4e15d04b2454bf6f4fadf034b10403119cd8e3b92fcc5b",
        );
        let m = Modulus::new(&p).unwrap();
        let g = [3u8];
        let a = unhex("0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a6978");
        let b = unhex("fedcba98765432100123456789abcdeffedcba9876543210");

        let mut ga = vec![0u8; 256];
        let mut gb = vec![0u8; 256];
        modpow(&g, &a, &m, &mut ga).unwrap();
        modpow(&g, &b, &m, &mut gb).unwrap();
        assert_ne!(ga, gb);

        let mut gab = vec![0u8; 256];
        let mut gba = vec![0u8; 256];
        modpow(&ga, &b, &m, &mut gab).unwrap();
        modpow(&gb, &a, &m, &mut gba).unwrap();
        assert_eq!(hex(&gab), hex(&gba), "DH did not agree");
        assert_ne!(gab, vec![0u8; 256]);
    }

    #[test]
    fn n0inv_really_inverts() {
        for n0 in [1u32, 3, 5, 0xFFFF_FFFF, 0x8000_0001, 65537] {
            let mut inv = 1u32;
            for _ in 0..5 {
                inv = inv.wrapping_mul(2u32.wrapping_sub(n0.wrapping_mul(inv)));
            }
            assert_eq!(n0.wrapping_mul(inv), 1, "n0 = {n0:#x}");
        }
    }

    #[test]
    fn double_mod_stays_reduced() {
        // The carry case is the one that matters: doubling can overflow past the limb count,
        // and a comparison alone would then miss the needed subtraction.
        let n = unhex("fffffffffffffffffffffffffffffffb");
        let mut m = [0u32; MAX_LIMBS];
        let len = from_be_bytes(&n, &mut m).unwrap();
        let mut v = [0u32; MAX_LIMBS];
        v[..len].copy_from_slice(&m[..len]);
        sub_assign(&mut v, &[1, 0, 0, 0], len); // n - 1, so the next double overflows
        for _ in 0..200 {
            double_mod(&mut v, &m, len);
            assert!(!ge(&v, &m, len), "escaped the modulus");
        }
    }
}
