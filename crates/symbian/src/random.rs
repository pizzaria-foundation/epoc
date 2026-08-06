//! Random bytes: the shim's entropy pool, whitened.
//!
//! ```no_run
//! # use symbian::random::Random;
//! let mut rng = Random::new()?;
//! let mut nonce = [0u8; 16];
//! rng.fill(&mut nonce);
//! # Ok::<(), symbian::Error>(())
//! ```
//!
//! # Why this is two pieces
//!
//! [`symbian_sys::shim_entropy`] returns a pool that is *hopefully* unpredictable and
//! definitely not uniform — `Math::Random`, a jittery counter, uptime, the clock, a stack
//! address, the heap's free space. [`symbian_crypto::Drbg`] is HMAC-SHA-256 in counter mode
//! and turns that into a stream. Neither half is useful alone, and keeping them apart is
//! what lets the arithmetic be tested with `cargo test` while the collection stays in the
//! one place that can reach the platform.
//!
//! # Reseeding
//!
//! Every 256 KB, and on demand through [`Random::stir`].
//!
//! That is not because the DRBG wears out — HMAC-SHA-256 in counter mode does not, within
//! any length this device could generate in its lifetime. It is because the state is
//! readable: anyone who gets the key reproduces every output back to the last reseed and
//! forward forever. Reseeding bounds the window. On a phone with no process isolation worth
//! the name, that window is worth bounding cheaply rather than arguing about.
//!
//! # What it is not
//!
//! It is exactly as unpredictable as `shim_entropy`, which is documented honestly in
//! `shim/src/shim_rand.cpp` and is not a certified CSPRNG. `random.dll`'s `CSystemRandom` is
//! the platform's real one and is deliberately not used, because a new DLL dependency can
//! stop the image loading and that failure produces no diagnostic at all. `examples/selftest`
//! probes for it so the decision can be revisited against a measurement.

use symbian_crypto::Drbg;

use crate::error::{Error, Result};

/// Bytes of entropy pulled per seed and per reseed.
///
/// 64 is the HMAC-SHA-256 block size, so a whole-block fold costs one compression function
/// call. More would not be more entropy — the pool's sources do not have 512 bits of
/// unpredictability between them and pretending otherwise by asking for more would be
/// theatre.
const POOL: usize = 64;

/// Bytes generated before an automatic reseed. See the module docs on why this exists.
const RESEED_AFTER: usize = 256 * 1024;

pub struct Random {
    drbg: Drbg,
    since_reseed: usize,
}

impl Random {
    /// Seed from the platform.
    ///
    /// Fails only if the shim's entropy call fails, which on a real device it does not —
    /// but a `Result` rather than a panic, because the one caller that matters is a login
    /// flow and "could not get randomness" is a thing to report rather than a thing to die
    /// on.
    pub fn new() -> Result<Self> {
        let mut pool = [0u8; POOL];
        entropy(&mut pool)?;
        Ok(Random { drbg: Drbg::new(&pool), since_reseed: 0 })
    }

    /// Fill `out`.
    pub fn fill(&mut self, out: &mut [u8]) {
        if self.since_reseed >= RESEED_AFTER {
            // A failed reseed is not a failure to generate: the existing state is still
            // good, and refusing to produce bytes because the *optional* freshening did not
            // work would turn a hardening measure into an outage. The counter resets either
            // way so a permanently failing source cannot spin on retries.
            let mut pool = [0u8; POOL];
            if entropy(&mut pool).is_ok() {
                self.drbg.reseed(&pool);
            }
            self.since_reseed = 0;
        }
        self.since_reseed = self.since_reseed.saturating_add(out.len());
        self.drbg.fill(out);
    }

    /// Fold fresh platform entropy in now.
    ///
    /// Worth calling after something unpredictable has happened — a keypress, a packet
    /// arriving — because the jitter sources in the pool are most useful when sampled at a
    /// moment an observer did not choose.
    pub fn stir(&mut self) -> Result<()> {
        let mut pool = [0u8; POOL];
        entropy(&mut pool)?;
        self.drbg.reseed(&pool);
        self.since_reseed = 0;
        Ok(())
    }

    pub fn next_u32(&mut self) -> u32 {
        let mut b = [0u8; 4];
        self.fill(&mut b);
        u32::from_be_bytes(b)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut b = [0u8; 8];
        self.fill(&mut b);
        u64::from_be_bytes(b)
    }
}

fn entropy(out: &mut [u8]) -> Result<()> {
    let rc = unsafe { symbian_sys::shim_entropy(out.as_mut_ptr(), out.len() as i32) };
    if rc == symbian_sys::SHIM_OK {
        Ok(())
    } else {
        Err(Error::from_code(rc))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On the host `shim_entropy` is a counter, so this asserts the plumbing rather than
    /// any property of the output. Unpredictability is not testable here and pretending
    /// otherwise with a statistical check on a known input would be worse than no test.
    #[test]
    fn it_produces_bytes_and_advances() {
        let mut r = Random::new().unwrap();
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        r.fill(&mut a);
        r.fill(&mut b);
        assert_ne!(a, b);
        assert_ne!(a, [0u8; 32]);
    }

    #[test]
    fn stirring_does_not_break_the_stream() {
        let mut r = Random::new().unwrap();
        let before = r.next_u64();
        r.stir().unwrap();
        let after = r.next_u64();
        assert_ne!(before, after);
    }

    /// The automatic reseed must not stall or repeat. Driving past the threshold in one
    /// call and then reading again is the cheapest way to cover that branch.
    #[test]
    fn crossing_the_reseed_threshold_keeps_producing() {
        let mut r = Random::new().unwrap();
        let mut big = alloc::vec![0u8; RESEED_AFTER + 64];
        r.fill(&mut big);
        let mut after = [0u8; 32];
        r.fill(&mut after);
        assert_ne!(after, [0u8; 32]);
        assert_ne!(&big[..32], &after[..]);
    }
}
