//! Factor the `pq` the server sends during the handshake.
//!
//! Telegram's proof of work: `resPQ` carries a semiprime of about 63 bits and the client
//! must return its two factors in `p_q_inner_data`, smallest first. It is not a security
//! boundary — a server can factor it too — it is a cost imposed on anyone opening auth keys
//! in bulk.
//!
//! # Why Brent rather than trial division
//!
//! Both factors are around 2^31, so trial division needs on the order of 10^8 iterations
//! and would take minutes on a 600 MHz ARM11 — long enough that the login would look hung.
//! Brent's variant of Pollard's rho finds a factor in roughly `n^(1/4)` steps, about 55,000
//! for a 63-bit semiprime, which is milliseconds.
//!
//! Brent rather than plain Floyd because it needs one evaluation of `f` per step instead of
//! three, and because batching the gcd over 128 steps turns one modular inverse per step
//! into one per batch.
//!
//! # Why Montgomery
//!
//! The inner loop is `x² + c mod n` with `n` up to 63 bits, so the product is up to 126 bits
//! and does not fit a register. Two ways out:
//!
//! - `u128`, which on 32-bit ARM means `__umodti3` from `compiler_builtins` — software long
//!   division, roughly a hundred cycles, executed 55,000 times.
//! - Montgomery multiplication with 32-bit limbs, which replaces every division with
//!   multiplies and shifts. ARM has `umull`, so a 32×32→64 product is one instruction.
//!
//! The second, ported from `vendor/research/mtproto/brent2.c`, where the operation count
//! was worked out: ten 32×32 multiplies per modular multiplication.
//!
//! This is also why the modulus must be odd — Montgomery needs an inverse of `n` modulo
//! 2^32, which only exists for odd `n`. A semiprime of two odd primes always is, and
//! [`factor`] refuses even input rather than looping forever on it.

/// Montgomery arithmetic modulo an odd 64-bit `n`, with 32-bit limbs.
struct Mont {
    n: u64,
    /// `-n⁻¹ mod 2^32`, the multiplier that makes the low limb cancel.
    n0inv: u32,
    /// `R² mod n`, for converting into Montgomery form. `R` is 2^64.
    r2: u64,
}

impl Mont {
    fn new(n: u64) -> Option<Self> {
        if n % 2 == 0 || n < 3 {
            return None;
        }

        // Newton's method modulo 2^32: each step doubles the correct bits, so five steps
        // take 2 bits to 64 — more than the 32 needed. Starting from n itself is the usual
        // trick, since n is already correct to 3 bits for odd n.
        let n32 = n as u32;
        let mut inv = n32;
        for _ in 0..5 {
            inv = inv.wrapping_mul(2u32.wrapping_sub(n32.wrapping_mul(inv)));
        }
        // inv is now n⁻¹ mod 2^32; CIOS wants the negative.
        let n0inv = inv.wrapping_neg();

        // R mod n, computed without a 128-bit division: (2^64 - 1) mod n, plus one.
        let r = (u64::MAX % n).wrapping_add(1) % n;
        // R² mod n by doubling R sixty-four times. Each doubling is exact in 64 bits
        // because n < 2^63 keeps 2r below 2^64 — which is also why `factor` bounds n.
        let mut r2 = r;
        for _ in 0..64 {
            r2 = (r2 << 1) % n;
        }

        Some(Mont { n, n0inv, r2 })
    }

    /// `a · b · R⁻¹ mod n`, by CIOS with two 32-bit limbs.
    ///
    /// Every product here is `u32 × u32 → u64`, which is one `umull` on ARM. Widening to
    /// `u128` anywhere in this function would undo the entire point of it.
    fn mul(&self, a: u64, b: u64) -> u64 {
        let a = [a as u32, (a >> 32) as u32];
        let b = [b as u32, (b >> 32) as u32];
        let n = [self.n as u32, (self.n >> 32) as u32];
        let mut acc = [0u32; 4];

        for &bi in b.iter() {
            let mut carry = 0u64;
            for j in 0..2 {
                let s = acc[j] as u64 + a[j] as u64 * bi as u64 + carry;
                acc[j] = s as u32;
                carry = s >> 32;
            }
            let s = acc[2] as u64 + carry;
            acc[2] = s as u32;
            acc[3] = acc[3].wrapping_add((s >> 32) as u32);

            // The multiplier that makes the low limb zero, so the whole accumulator can be
            // shifted down by one limb without losing anything.
            let m = (acc[0] as u64 * self.n0inv as u64) as u32;
            let mut carry = 0u64;
            for j in 0..2 {
                let s = acc[j] as u64 + m as u64 * n[j] as u64 + carry;
                acc[j] = s as u32;
                carry = s >> 32;
            }
            let s = acc[2] as u64 + carry;
            acc[2] = s as u32;
            acc[3] = acc[3].wrapping_add((s >> 32) as u32);

            acc = [acc[1], acc[2], acc[3], 0];
        }

        let r = ((acc[1] as u64) << 32) | acc[0] as u64;
        // One conditional subtraction. CIOS leaves a result below 2n, not below n, and the
        // overflow limb has to be consulted: a result that wrapped past 2^64 is greater than
        // n even when the low 64 bits are not.
        if acc[2] != 0 || r >= self.n {
            r.wrapping_sub(self.n)
        } else {
            r
        }
    }

    fn to_mont(&self, a: u64) -> u64 {
        self.mul(a % self.n, self.r2)
    }

    fn add(&self, a: u64, b: u64) -> u64 {
        // a and b are both below n < 2^63, so the sum cannot overflow.
        let s = a + b;
        if s >= self.n {
            s - self.n
        } else {
            s
        }
    }

    fn sub(&self, a: u64, b: u64) -> u64 {
        if a >= b {
            a - b
        } else {
            a + self.n - b
        }
    }
}

/// Binary GCD: no division, which is the whole reason it is here rather than Euclid's.
fn gcd(mut u: u64, mut v: u64) -> u64 {
    if u == 0 {
        return v;
    }
    if v == 0 {
        return u;
    }
    let shift = (u | v).trailing_zeros();
    u >>= u.trailing_zeros();
    loop {
        v >>= v.trailing_zeros();
        if u > v {
            core::mem::swap(&mut u, &mut v);
        }
        v -= u;
        if v == 0 {
            break;
        }
    }
    u << shift
}

/// Both prime factors of `n`, smallest first.
///
/// Returns `None` for input this cannot handle: even, too small, prime, or 2^63 and above
/// (see [`Mont::new`] on why that bound exists). A Telegram `pq` is none of those, so `None`
/// from here means the server sent something unexpected rather than that factoring failed.
///
/// `seed` varies the polynomial. Rho is randomised — one choice of `c` can cycle without
/// finding anything — so a caller retries with a different seed rather than looping here
/// forever. Passing a fixed seed makes this deterministic, which is what the tests want.
pub fn factor(n: u64, seed: u64) -> Option<(u64, u64)> {
    if n < 4 {
        return None;
    }
    // Even is not a case worth handling generically: it cannot be a Telegram pq, and
    // pretending otherwise would mean carrying a division path for input that never comes.
    if n % 2 == 0 {
        return None;
    }
    if n >> 63 != 0 {
        return None;
    }

    let m = Mont::new(n)?;

    // Brent's improvements over Floyd: one evaluation of f per step, and the gcd batched
    // over BATCH steps by accumulating the product of differences. A batch that finds
    // nothing costs one gcd instead of 128.
    const BATCH: u64 = 128;

    let c = m.to_mont(1 + (seed % (n - 3)));
    let mut y = m.to_mont(2 + (seed >> 32) % (n - 3));
    let one = m.to_mont(1);

    let mut g = 1u64;
    let mut r = 1u64;
    let mut q = one;
    let mut x = y;
    let mut ys = y;

    while g == 1 {
        x = y;
        for _ in 0..r {
            y = m.add(m.mul(y, y), c);
        }
        let mut k = 0u64;
        while k < r && g == 1 {
            ys = y;
            let steps = core::cmp::min(BATCH, r - k);
            for _ in 0..steps {
                y = m.add(m.mul(y, y), c);
                // The difference of two Montgomery forms is the Montgomery form of the
                // difference, and multiplying by R⁻¹ cannot introduce or remove a factor of
                // n — so the accumulated product has exactly the factors that matter.
                q = m.mul(q, m.sub(x, y));
            }
            g = gcd(q, n);
            k += BATCH;
        }
        r *= 2;
        // Rho's running time is probabilistic and a bad polynomial can run long. The bound
        // is generous against the ~55,000 steps a 63-bit semiprime needs, and exists so a
        // login reports failure and retries rather than freezing the phone.
        if r > (1 << 24) {
            return None;
        }
    }

    if g == n {
        // The batch multiplied the factor away: some difference in it was divisible by one
        // prime and another by the other, so the product picked up both. Back up and walk
        // the batch a step at a time.
        g = 1;
        while g == 1 {
            ys = m.add(m.mul(ys, ys), c);
            g = gcd(m.sub(x, ys), n);
        }
    }

    if g == n || g == 1 {
        return None;
    }
    let other = n / g;
    Some((core::cmp::min(g, other), core::cmp::max(g, other)))
}

/// [`factor`] with a few different polynomials, since any one can fail.
///
/// Four attempts. Each has a high probability of succeeding on its own, so four failures in
/// a row on a genuine semiprime is not something to design around — but a `None` that a
/// caller can report beats a loop that never ends on a `pq` that is not what it claims.
pub fn factor_retry(n: u64) -> Option<(u64, u64)> {
    for i in 0..4u64 {
        if let Some(f) = factor(n, 0x9e37_79b9_7f4a_7c15u64.wrapping_mul(i + 1)) {
            return Some(f);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn montgomery_multiplication_agrees_with_u128() {
        // u128 is the definition here and Montgomery is the optimisation, so the
        // optimisation is checked against the definition. On the device u128 would be
        // software division; in a test it is free and it is the ground truth.
        for &n in &[3u64, 0x7fff_ffff, 0x1_0000_0003, 0x7fff_ffff_ffff_ffe1] {
            let m = Mont::new(n).unwrap();
            let mut s = 0x1234_5678_9abc_def1u64;
            for _ in 0..200 {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                let a = s % n;
                let b = s.rotate_left(31) % n;
                let got = m.mul(m.to_mont(a), m.to_mont(b));
                let got = m.mul(got, 1); // out of Montgomery form
                let want = ((a as u128 * b as u128) % n as u128) as u64;
                assert_eq!(got, want, "n={n} a={a} b={b}");
            }
        }
    }

    #[test]
    fn montgomery_rejects_an_even_modulus() {
        // Not a limitation to work around: the inverse mod 2^32 does not exist, and a
        // version that silently proceeded would produce wrong answers rather than fail.
        assert!(Mont::new(100).is_none());
        assert!(Mont::new(2).is_none());
        assert!(Mont::new(101).is_some());
    }

    #[test]
    fn gcd_matches_the_definition() {
        fn slow(a: u64, b: u64) -> u64 {
            if b == 0 { a } else { slow(b, a % b) }
        }
        for (a, b) in [(12u64, 18u64), (0, 5), (5, 0), (1, 1), (1 << 40, 1 << 20),
                       (0xffff_ffff, 0xffff_fffd), (997 * 991, 991 * 983)] {
            assert_eq!(gcd(a, b), slow(a, b), "gcd({a}, {b})");
        }
    }

    #[test]
    fn factors_small_semiprimes() {
        for &(p, q) in &[(3u64, 5u64), (101, 103), (65537, 65539), (1_000_003, 1_000_033)] {
            let n = p * q;
            let (a, b) = factor_retry(n).unwrap_or_else(|| panic!("failed on {n}"));
            assert_eq!((a, b), (p.min(q), p.max(q)), "n={n}");
        }
    }

    #[test]
    fn factors_a_realistic_pq() {
        // Two primes near 2^31, which is the size Telegram actually sends. A rho that
        // works on 101 × 103 and not on this is a rho with an overflow.
        let p = 2_147_483_647u64; // 2^31 - 1, Mersenne
        let q = 2_147_483_629u64;
        let n = p * q;
        assert!(n < 1 << 63);
        let (a, b) = factor_retry(n).expect("failed on a realistic pq");
        assert_eq!((a, b), (q, p));
    }

    #[test]
    fn factors_many_random_semiprimes() {
        // The distribution matters more than any single case: rho's running time varies,
        // and a bound that is too tight shows up as an occasional failure rather than a
        // consistent one. Sixty-four is enough to catch that and fast enough to keep.
        let primes: [u64; 8] = [
            1_073_741_827, 1_073_741_831, 1_073_741_833, 1_073_741_839,
            2_147_483_647, 2_147_483_629, 2_147_483_587, 2_147_483_579,
        ];
        let mut tried = 0;
        for (i, &p) in primes.iter().enumerate() {
            for &q in primes.iter().skip(i + 1) {
                let n = p * q;
                let (a, b) = factor_retry(n).unwrap_or_else(|| panic!("failed on {p}*{q}"));
                assert_eq!(a * b, n);
                assert_eq!((a, b), (p.min(q), p.max(q)));
                tried += 1;
            }
        }
        assert!(tried >= 28, "only {tried} pairs exercised");
    }

    #[test]
    fn a_prime_is_not_factored_into_anything() {
        // Rho cannot factor a prime and must say so rather than returning (1, n).
        assert_eq!(factor_retry(2_147_483_647), None);
    }

    #[test]
    fn input_it_cannot_handle_is_refused() {
        assert_eq!(factor_retry(0), None);
        assert_eq!(factor_retry(1), None);
        assert_eq!(factor_retry(4), None); // even
        assert_eq!(factor_retry(u64::MAX), None); // 64 bits, past the 2^63 bound
    }

    #[test]
    fn the_result_is_ordered_smallest_first() {
        // p_q_inner_data wants p then q with p < q, and the server rejects the other order
        // by closing the connection.
        let (a, b) = factor_retry(1_000_003 * 1_000_033).unwrap();
        assert!(a < b);
    }
}
