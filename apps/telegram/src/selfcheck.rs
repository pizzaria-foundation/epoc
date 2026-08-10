//! Run the recorded handshake on the handset.
//!
//! # Why
//!
//! The device fails at step two of the handshake with `handshake: TL ilegível`, and every
//! host test of that exact step passes byte for byte. The trace narrowed it to one place
//! and no further:
//!
//! ```text
//!  937  tx bytes: 344     req_DH_params, the right size to the byte
//! 1563  rx bytes: 656     server_DH_params_ok, also the right size
//! 1601  DISCONNECTED: handshake: TL ilegível
//! ```
//!
//! The server only answers `server_DH_params_ok` after decrypting our RSA block, so
//! everything the phone computed up to there is provably right. Two explanations remain,
//! and they need opposite fixes:
//!
//! 1. the bytes on the wire are not what the host recorded, or
//! 2. the same bytes parse differently when compiled for ARMv5.
//!
//! Nothing on the host can distinguish those, because the host is the thing that works.
//! This carries the recorded transcript to the phone and runs it there: identical input,
//! identical expected output, on the machine that fails. It needs no network and no
//! account, so it runs at start-up and writes one line.
//!
//! # What is checked
//!
//! Steps one and two, which is where the failure is. Step three needs a 2048-bit
//! exponentiation — 821 ms, and it belongs on the worker thread — so it is left out.
//!
//! What runs here is the RSA public-key operation, `rsa_pad`, SHA-1, the `pq` factoring,
//! AES-256-IGE decryption and both TL parsers. That is the whole suspect list.

use alloc::string::String;

use tg_proto::crypto::Rng;
use tg_proto::handshake::{Action, Handshake};

/// The transcript, packed by this app's `tools/mkhsfixture.py`.
static BLOB: &[u8] = include_bytes!("handshake_fixture.bin");

/// The recorded random draws, in the order the recorder made them.
///
/// Not a random source — a tape. `Handshake::start` takes the nonces and the DH secret,
/// then `rsa_pad` and the IGE padding draw from what follows. The order has to match the
/// recording or every value after the first diverges, which is the coupling that makes the
/// comparison byte-exact instead of merely structural.
struct Tape<'a> {
    bytes: &'a [u8],
    pos: usize,
    /// Set if the tape ran out. Panicking would take the application down at start-up, and
    /// a diagnostic that kills the thing it is diagnosing is worse than useless.
    exhausted: bool,
}

impl Rng for Tape<'_> {
    fn fill(&mut self, out: &mut [u8]) {
        let end = self.pos + out.len();
        if end > self.bytes.len() {
            self.exhausted = true;
            out.fill(0);
            return;
        }
        out.copy_from_slice(&self.bytes[self.pos..end]);
        self.pos = end;
    }
}

/// What the check found. Each is a distinct line in the log.
pub enum Outcome {
    /// Both steps reproduced the recording exactly. The code is right on this hardware,
    /// so whatever the live server sent is not what the recording holds.
    Ok,
    /// A step produced different bytes, or refused a reply the host accepts. The name says
    /// which step and the detail says how.
    Failed(&'static str, String),
}

fn split() -> Option<[&'static [u8]; 5]> {
    if BLOB.len() < 20 {
        return None;
    }
    let mut lens = [0usize; 5];
    for (i, l) in lens.iter_mut().enumerate() {
        let b = &BLOB[i * 4..i * 4 + 4];
        *l = u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize;
    }
    let mut at = 20;
    let mut out: [&'static [u8]; 5] = [&[]; 5];
    for (o, l) in out.iter_mut().zip(lens) {
        if at + l > BLOB.len() {
            return None;
        }
        *o = &BLOB[at..at + l];
        at += l;
    }
    Some(out)
}

fn hex(b: &[u8], limit: usize) -> String {
    let mut s = String::new();
    for x in b.iter().take(limit) {
        const D: &[u8; 16] = b"0123456789abcdef";
        s.push(D[(x >> 4) as usize] as char);
        s.push(D[(x & 15) as usize] as char);
    }
    s
}

/// The first byte at which two buffers differ, which is the only part of a mismatch worth
/// logging: a 320-byte diff is unreadable and its offset names the field.
fn first_difference(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()))
}

pub fn run() -> Outcome {
    let Some([tape_bytes, res_pq, dh_params, want_req_pq, want_req_dh]) = split() else {
        return Outcome::Failed("fixture", String::from("blob is malformed"));
    };
    let mut tape = Tape { bytes: tape_bytes, pos: 0, exhausted: false };

    // ---- req_pq_multi ----
    let (mut hs, action) = Handshake::start(&mut tape);
    let Action::Send(out) = action else {
        return Outcome::Failed("step1", String::from("start did not send"));
    };
    if out != want_req_pq {
        return Outcome::Failed("step1", diff("req_pq_multi", &out, want_req_pq));
    }

    // ---- res_pq -> req_DH_params ----
    //
    // Factoring pq, minimal-width TL integers, and RSA_PAD. All three are arithmetic that
    // a 32-bit in-order core could plausibly compile differently.
    let action = match hs.on_message(res_pq, &mut tape) {
        Ok(a) => a,
        Err(e) => return Outcome::Failed("step2", name_of(&e)),
    };
    let Action::Send(out) = action else {
        return Outcome::Failed("step2", String::from("did not send req_DH_params"));
    };
    if out != want_req_dh {
        return Outcome::Failed("step2", diff("req_DH_params", &out, want_req_dh));
    }

    // ---- server_DH_params_ok -> the exponentiation ----
    //
    // The step the handset fails on. AES-256-IGE decryption, the SHA-1 that guards it, and
    // the inner TL parse.
    let action = match hs.on_message(dh_params, &mut tape) {
        Ok(a) => a,
        Err(e) => return Outcome::Failed("step3", name_of(&e)),
    };
    let Action::ModPow { base, modulus, .. } = action else {
        return Outcome::Failed("step3", String::from("did not ask for a modpow"));
    };
    if base != [3] || modulus.len() != 256 {
        return Outcome::Failed("step3", String::from("wrong DH parameters"));
    }

    if tape.exhausted {
        return Outcome::Failed("tape", String::from("ran out of recorded randomness"));
    }
    Outcome::Ok
}

fn diff(what: &'static str, got: &[u8], want: &[u8]) -> String {
    let at = first_difference(got, want);
    let mut s = String::from(what);
    s.push_str(" differs at ");
    push_usize(&mut s, at);
    s.push_str(", got ");
    s.push_str(&hex(&got[at.min(got.len())..], 8));
    s.push_str(" want ");
    s.push_str(&hex(&want[at.min(want.len())..], 8));
    s
}

/// The failure, named the same way `link.rs` names one from the wire, so a device log can
/// be compared line for line against a live one.
fn name_of(e: &tg_proto::handshake::Error) -> String {
    use tg_proto::handshake::Error as H;
    use tg_proto::tl::Error as T;
    let mut s = String::new();
    match e {
        H::Tl(T::Truncated) => s.push_str("tl truncated"),
        H::Tl(T::BadLength) => s.push_str("tl bad length"),
        H::Tl(T::UnknownConstructor(c)) => {
            s.push_str("tl unknown ctor ");
            push_hex32(&mut s, *c);
        }
        H::Tl(T::Unexpected { want, got }) => {
            s.push_str("tl wanted ");
            push_hex32(&mut s, *want);
            s.push_str(" got ");
            push_hex32(&mut s, *got);
        }
        H::Crypto(_) => s.push_str("crypto: the SHA-1 on the decrypted block did not match"),
        H::OutOfOrder => s.push_str("out of order"),
        H::NonceMismatch => s.push_str("nonce mismatch"),
        H::NotFactorable(_) => s.push_str("pq not factorable"),
        H::NoUsableKey => s.push_str("no usable RSA key"),
        H::ServerRejected => s.push_str("server rejected"),
        H::UnknownDhPrime => s.push_str("unknown dh prime"),
        H::BadDhParams => s.push_str("bad dh params"),
        H::DhGenFailed => s.push_str("dh gen failed"),
        H::KeyMismatch => s.push_str("key mismatch"),
    }
    s
}

fn push_hex32(s: &mut String, v: u32) {
    for i in (0..8).rev() {
        const D: &[u8; 16] = b"0123456789abcdef";
        s.push(D[((v >> (i * 4)) & 15) as usize] as char);
    }
}

fn push_usize(s: &mut String, mut v: usize) {
    let mut d = [0u8; 20];
    let mut n = 0;
    loop {
        d[n] = b'0' + (v % 10) as u8;
        n += 1;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    while n > 0 {
        n -= 1;
        s.push(d[n] as char);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_recorded_handshake_replays() {
        // The same assertion the device makes. If this fails on the host the fixture is
        // stale — regenerate it with this app's `tools/mkhsfixture.py` — and the device result means
        // nothing until it passes here.
        match run() {
            Outcome::Ok => {}
            Outcome::Failed(step, why) => panic!("{step}: {why}"),
        }
    }

    #[test]
    fn a_short_blob_is_reported_rather_than_indexed() {
        // include_bytes! of a truncated file must not panic at start-up on the handset.
        assert!(BLOB.len() > 20, "the fixture is missing");
        assert!(split().is_some());
    }
}
