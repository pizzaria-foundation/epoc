//! Every intermediate of the SRP exchange, against the specification in Python.
//!
//! `vendor/research/mtproto/srp.py` is the same eleven steps written in a language with
//! arbitrary-precision integers, so it has no arithmetic of its own to be wrong. The Rust
//! has Montgomery reduction, 32-bit limbs, and a modular subtraction that decides a sign
//! without a signed type — three places to be wrong that the reference does not have.
//!
//! # Why value-by-value and not just the answer
//!
//! A wrong SRP produces one symptom: the server says the password is incorrect. Identical
//! to what a genuinely wrong password produces. Comparing only `M1` would tell you that
//! *something* among eleven steps is wrong; comparing each step tells you which.
//!
//! The state machine happens to make that possible without exposing internals: `t` is the
//! base of the third `ModPow` and the exponent is its exponent, so most of the interesting
//! values pass through [`Step`] on their way to the worker thread.
//!
//! # Refreshing
//!
//! ```text
//! python3 vendor/research/mtproto/srp.py --fixture \
//!     > apps/telegram/proto/tests/fixtures/srp.txt
//! ```
//!
//! Nothing here is secret: the passwords are `hunter2` and an empty string, and `a` is a
//! constant. Fixed inputs are the point — a random `a` would make a failure impossible to
//! reproduce.

use std::collections::BTreeMap;
use std::fs;

use tg_proto::crypto::Rng;
use tg_proto::srp::{self, Srp, Step};

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A tape, so `a` is the reference's `a` rather than something random.
struct Tape(Vec<u8>);

impl Rng for Tape {
    fn fill(&mut self, out: &mut [u8]) {
        assert!(out.len() <= self.0.len(), "the tape ran out");
        out.copy_from_slice(&self.0[..out.len()]);
        self.0.drain(..out.len());
    }
}

/// The prime, from the same source the reference uses.
const P_HEX: &str = concat!(
    "C71CAEB9C6B1C9048E6C522F70F13F73980D40238E3E21C14934D037563D930F",
    "48198A0AA7C14058229493D22530F4DBFA336F6E0AC925139543AED44CCE7C37",
    "20FD51F69458705AC68CD4FE6B6B13ABDC9746512969328454F18FAF8C595F64",
    "2477FE96BB2A941D5BCD1D4AC8CC49880708FA9B378E3C4F3A9060BEE67CF9A4",
    "A4A695811051907E162753B56B0F6B410DBA74D8A84B2A14B3144E0EF1284754",
    "FD17ED950D5965B4B9DD46582DB1178D169C6BC465B0D6FF9CA3928FEF5B9AE4",
    "E418FC15E83EBEA0F87FA9FF5EED70050DED2849F47BF959D956850CE929851F",
    "0D8115F635B105EE2E4E15D04B2454BF6F4FADF034B10403119CD8E3B92FCC5B",
);

/// `base^exp mod modulus`, standing in for the worker thread.
fn modpow(base: &[u8], exp: &[u8], modulus: &[u8]) -> Vec<u8> {
    let m = symbian_crypto::Modulus::new(modulus).expect("bad modulus");
    let mut out = vec![0u8; modulus.len()];
    symbian_crypto::modpow(base, exp, &m, &mut out).expect("modpow failed");
    out
}

fn load() -> Vec<BTreeMap<String, String>> {
    let raw = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/srp.txt"))
        .expect("fixture missing; see the module docs");
    let mut cases: Vec<BTreeMap<String, String>> = Vec::new();
    for line in raw.lines() {
        let mut it = line.splitn(3, ' ');
        let (i, k, v) = (
            it.next().unwrap().parse::<usize>().unwrap(),
            it.next().unwrap().to_string(),
            it.next().unwrap_or("").to_string(),
        );
        while cases.len() <= i {
            cases.push(BTreeMap::new());
        }
        cases[i].insert(k, v);
    }
    cases
}

/// The key derivation on its own: 100,000 PBKDF2 iterations, checked before anything else.
///
/// Separate because it is the expensive step and the one most likely to be wrong in a way
/// the rest hides — `x` feeds every subsequent value, so a wrong `x` fails at `M1` with no
/// indication that the KDF was the cause.
#[test]
fn x_matches_the_reference() {
    for (i, c) in load().iter().enumerate() {
        let got = srp::derive_x(
            &unhex(&c["password"]),
            &unhex(&c["salt1"]),
            &unhex(&c["salt2"]),
        );
        assert_eq!(hex(&got), c["x"], "case {i}: the password KDF differs");
    }
}

/// Every step, in order, through the state machine.
#[test]
fn every_intermediate_matches_the_reference() {
    let p = unhex(P_HEX);

    for (i, c) in load().iter().enumerate() {
        let g: u32 = c["g"].parse().unwrap();
        let g_b = unhex(&c["srp_B"]);
        let mut rng = Tape(unhex(&c["a"]));

        let x_bytes = unhex(&c["x"]);
        let mut x = [0u8; 32];
        x.copy_from_slice(&x_bytes);

        let (mut s, step) = Srp::start(
            42,
            &p,
            g,
            &g_b,
            &unhex(&c["salt1"]),
            &unhex(&c["salt2"]),
            x,
            &mut rng,
        )
        .unwrap_or_else(|e| panic!("case {i}: start rejected the parameters: {e:?}"));

        // ---- v = g^x mod p ----
        let Step::ModPow { base, exp, modulus } = step else { panic!("case {i}: expected v") };
        assert_eq!(hex(&exp), c["x"], "case {i}: v used the wrong exponent");
        let v = modpow(&base, &exp, &modulus);
        assert_eq!(hex(&v), c["v"], "case {i}: v differs");

        // ---- g_a = g^a mod p ----
        let step = s.on_modpow(&v).unwrap();
        let Step::ModPow { base, exp, modulus } = step else { panic!("case {i}: expected g_a") };
        assert_eq!(hex(&exp), c["a"], "case {i}: g_a used the wrong exponent");
        let g_a = modpow(&base, &exp, &modulus);
        assert_eq!(hex(&g_a), c["g_a"], "case {i}: g_a differs");

        // ---- t = (g_b - k*v) mod p, and the 2049-bit exponent ----
        //
        // Both come out through the third ModPow, which is what makes this differential
        // fine-grained enough to be useful. `t` is where the modular subtraction shows up,
        // and it goes negative before reduction about half the time.
        let step = s.on_modpow(&g_a).unwrap();
        let Step::ModPow { base, exp, modulus } = step else { panic!("case {i}: expected s_a") };
        assert_eq!(hex(&base), c["t"], "case {i}: t differs (the modular subtraction)");
        // The reference prints the exponent as a minimal-width integer; the Rust pads it to
        // 257 bytes so the carry has somewhere to go. Compare as numbers, not as strings.
        let want_exp = c["exponent"].trim_start_matches('0');
        let got_exp = hex(&exp);
        assert_eq!(
            got_exp.trim_start_matches('0'),
            want_exp,
            "case {i}: the exponent a + u*x differs"
        );
        assert_eq!(exp.len(), 257, "case {i}: the exponent must have room for the carry");

        let s_a = modpow(&base, &exp, &modulus);
        assert_eq!(hex(&s_a), c["s_a"], "case {i}: s_a differs");

        // ---- M1 ----
        let step = s.on_modpow(&s_a).unwrap();
        let Step::Done { a, m1, srp_id } = step else { panic!("case {i}: expected Done") };
        assert_eq!(srp_id, 42);
        assert_eq!(hex(&a), c["g_a"], "case {i}: the A sent to the server is not g_a");
        assert_eq!(hex(&m1), c["M1"], "case {i}: M1 differs");
    }
}

/// A password that is wrong by one bit must produce a different proof.
///
/// Trivially true if everything above passes, and worth stating because the whole point of
/// the exchange is that it distinguishes them — and because a KDF that ignored its input
/// would pass every equality test in this file.
#[test]
fn a_different_password_gives_a_different_x() {
    let c = &load()[0];
    let right = srp::derive_x(&unhex(&c["password"]), &unhex(&c["salt1"]), &unhex(&c["salt2"]));
    let mut wrong_pw = unhex(&c["password"]);
    wrong_pw[0] ^= 1;
    let wrong = srp::derive_x(&wrong_pw, &unhex(&c["salt1"]), &unhex(&c["salt2"]));
    assert_ne!(right, wrong);
}

/// Both salts must reach the derived key.
#[test]
fn both_salts_are_used() {
    let c = &load()[0];
    let (pw, s1, s2) = (unhex(&c["password"]), unhex(&c["salt1"]), unhex(&c["salt2"]));
    let base = srp::derive_x(&pw, &s1, &s2);

    let mut a1 = s1.clone();
    a1[0] ^= 1;
    assert_ne!(srp::derive_x(&pw, &a1, &s2), base, "salt1 did not affect x");

    let mut a2 = s2.clone();
    a2[0] ^= 1;
    assert_ne!(srp::derive_x(&pw, &s1, &a2), base, "salt2 did not affect x");
}
