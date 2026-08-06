//! The worker thread, from Rust.
//!
//! ```no_run
//! # use symbian::work::{Job, ModPow};
//! # let (g, secret, prime) = ([3u8], [0u8; 256], [0xffu8; 256]);
//! let mut job = Job::new();
//! job.submit(&ModPow { base: &g, exp: &secret, modulus: &prime })?;
//! // ... then feed raw events until job.on_event(ev) returns Some
//! # Ok::<(), symbian::Error>(())
//! ```
//!
//! # Why this exists
//!
//! A 2048-bit modular exponentiation measures **815 ms** on an E72. `rust_step` runs from a
//! `CIdle` on the GUI thread and must return in milliseconds — a long one starves the
//! window server, which freezes the whole phone rather than just the application, and no
//! watchdog rescues you. An MTProto login needs two.
//!
//! The shim's worker is proven on that hardware: the same exponentiation took 1933 ms of
//! wall time there with 27 GUI ticks served through it. Slower, because one core is shared
//! with a redrawing interface — the worker buys responsiveness, not speed, and a design
//! that assumes background work is free is wrong on this device.
//!
//! # The contract, and the one way to break it
//!
//! The worker gets its own heap. Anything allocated there and freed on the GUI thread is a
//! cross-heap free: silent corruption, not a clean failure. So the job may allocate
//! temporaries, but nothing it allocates may escape.
//!
//! [`Job`] enforces that by owning both buffers as `Box<[u8]>` and holding them for the
//! whole request. The shim keeps raw pointers into them until the completion arrives, so a
//! buffer freed or moved in between gets written to afterwards. A `Box`'s contents do not
//! move when the `Box` does, which is what lets a `Job` be moved while a job is in flight.
//!
//! Same shape as [`crate::net::TcpStream`], and for the same reason.

use alloc::boxed::Box;
use alloc::vec;

use symbian_sys as sys;

use crate::error::{Error, Result};

/// Opcode for a modular exponentiation. Must match the app's `rust_work` dispatcher.
pub const OP_MODPOW: i32 = 1;

/// The largest operand this facility carries, matching `symbian_crypto::bignum::MAX_BYTES`.
pub const MAX_OPERAND: usize = 256;

/// A modular exponentiation to run off the GUI thread. All operands big-endian.
pub struct ModPow<'a> {
    pub base: &'a [u8],
    pub exp: &'a [u8],
    pub modulus: &'a [u8],
}

impl ModPow<'_> {
    /// Pack into the flat buffer the worker reads.
    ///
    /// Three big-endian lengths then the operands. Flat because the ABI is a byte slice and
    /// a struct would mean agreeing on alignment and padding across a language boundary for
    /// no benefit — the packing costs one memcpy of under a kilobyte, against 815 ms of
    /// arithmetic.
    fn encode(&self, out: &mut [u8]) -> Result<usize> {
        let n = 6 + self.base.len() + self.exp.len() + self.modulus.len();
        if n > out.len()
            || self.base.len() > MAX_OPERAND
            || self.exp.len() > MAX_OPERAND
            || self.modulus.len() > MAX_OPERAND
        {
            return Err(Error::Argument);
        }
        out[0..2].copy_from_slice(&(self.base.len() as u16).to_be_bytes());
        out[2..4].copy_from_slice(&(self.exp.len() as u16).to_be_bytes());
        out[4..6].copy_from_slice(&(self.modulus.len() as u16).to_be_bytes());
        let mut at = 6;
        for part in [self.base, self.exp, self.modulus] {
            out[at..at + part.len()].copy_from_slice(part);
            at += part.len();
        }
        Ok(n)
    }
}

/// Unpack what [`ModPow::encode`] wrote.
///
/// Called on the *worker thread*, from the app's `rust_work`. Returns slices into the
/// caller's buffer rather than copies, because a copy here would allocate on the worker's
/// heap — allowed, but pointless when the input is already sitting in memory that outlives
/// the job.
pub fn decode_modpow(input: &[u8]) -> Option<(&[u8], &[u8], &[u8])> {
    if input.len() < 6 {
        return None;
    }
    let b = u16::from_be_bytes([input[0], input[1]]) as usize;
    let e = u16::from_be_bytes([input[2], input[3]]) as usize;
    let m = u16::from_be_bytes([input[4], input[5]]) as usize;
    if 6 + b + e + m > input.len() {
        return None;
    }
    Some((&input[6..6 + b], &input[6 + b..6 + b + e], &input[6 + b + e..6 + b + e + m]))
}

/// One job at a time, with the buffers it needs.
pub struct Job {
    input: Box<[u8]>,
    output: Box<[u8]>,
    busy: bool,
    /// How many bytes of `output` the last completed job filled.
    result_len: usize,
}

impl Default for Job {
    fn default() -> Self {
        Self::new()
    }
}

impl Job {
    pub fn new() -> Self {
        Job {
            input: vec![0u8; 6 + 3 * MAX_OPERAND].into_boxed_slice(),
            output: vec![0u8; MAX_OPERAND].into_boxed_slice(),
            busy: false,
            result_len: 0,
        }
    }

    pub fn is_busy(&self) -> bool {
        self.busy
    }

    /// Start an exponentiation.
    ///
    /// Returns [`Error::InUse`] if one is already running. Not a queue: the shim allows one
    /// job, and a queue here would be a scheduler the caller already has a better one of.
    pub fn submit(&mut self, job: &ModPow<'_>) -> Result<()> {
        if self.busy {
            return Err(Error::InUse);
        }
        let n = job.encode(&mut self.input)?;
        self.result_len = job.modulus.len().min(MAX_OPERAND);

        let rc = unsafe {
            sys::shim_work_submit(
                OP_MODPOW,
                self.input.as_ptr(),
                n as i32,
                self.output.as_mut_ptr(),
                self.result_len as i32,
            )
        };
        if rc != sys::SHIM_OK {
            return Err(Error::from_code(rc));
        }
        self.busy = true;
        Ok(())
    }

    /// Feed a raw event. Returns the result once the job finishes.
    ///
    /// `Err` means the worker reported a failure; the buffers are free again either way.
    pub fn on_event(&mut self, ev: &sys::ShimEvent) -> Option<Result<&[u8]>> {
        if ev.kind != sys::SHIM_EV_WORK_DONE || !self.busy {
            return None;
        }
        self.busy = false;
        if ev.status != sys::SHIM_OK {
            return Some(Err(Error::from_code(ev.status)));
        }
        Some(Ok(&self.output[..self.result_len]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_modpow_round_trips_through_the_packing() {
        let base = [3u8];
        let exp = [0xAAu8; 256];
        let modulus = [0xFFu8; 256];
        let job = ModPow { base: &base, exp: &exp, modulus: &modulus };
        let mut buf = [0u8; 6 + 3 * MAX_OPERAND];
        let n = job.encode(&mut buf).unwrap();
        assert_eq!(n, 6 + 1 + 256 + 256);

        let (b, e, m) = decode_modpow(&buf[..n]).unwrap();
        assert_eq!(b, &base[..]);
        assert_eq!(e, &exp[..]);
        assert_eq!(m, &modulus[..]);
    }

    #[test]
    fn an_operand_that_is_too_large_is_refused() {
        // Not truncated. A silently shortened modulus produces a mathematically valid
        // answer to a different question, which is the worst possible failure here.
        let big = [0u8; MAX_OPERAND + 1];
        let job = ModPow { base: &[3], exp: &[1], modulus: &big };
        let mut buf = [0u8; 6 + 3 * MAX_OPERAND];
        assert_eq!(job.encode(&mut buf), Err(Error::Argument));
    }

    #[test]
    fn a_truncated_buffer_decodes_to_nothing() {
        // The worker reads whatever the GUI thread wrote. A length that overruns is a bug
        // on this side, and returning None makes it a clean failure rather than a slice
        // panic on a thread with no cleanup stack.
        assert!(decode_modpow(&[]).is_none());
        // Three one-byte operands need 6 + 3 = 9 bytes; both of these are short.
        assert!(decode_modpow(&[0, 1, 0, 1, 0, 1]).is_none());
        assert!(decode_modpow(&[0, 1, 0, 1, 0, 1, 0, 0]).is_none());
        // And nine is enough.
        assert!(decode_modpow(&[0, 1, 0, 1, 0, 1, 7, 8, 9]).is_some());
    }

    #[test]
    fn empty_operands_are_legal() {
        // An exponent of zero length is a caller bug but not this layer's to judge, and
        // rejecting it here would mean two places deciding what a valid exponent is.
        let job = ModPow { base: &[], exp: &[], modulus: &[] };
        let mut buf = [0u8; 16];
        assert_eq!(job.encode(&mut buf).unwrap(), 6);
        let (b, e, m) = decode_modpow(&buf[..6]).unwrap();
        assert!(b.is_empty() && e.is_empty() && m.is_empty());
    }
}
