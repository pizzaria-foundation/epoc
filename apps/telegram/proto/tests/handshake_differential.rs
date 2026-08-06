//! Replay a real handshake and require the Rust to produce the same bytes.
//!
//! `tests/fixtures/handshake.json` is a transcript of an actual auth-key negotiation with
//! Telegram, recorded by `vendor/research/mtproto/handshake.py`. It holds the four server
//! replies, the four client messages that were sent, and the randomness that was used —
//! the nonces, the Diffie-Hellman secret, and the padding stream.
//!
//! Feeding the same randomness in makes the whole handshake deterministic, so this asserts
//! that `tg-proto` sends byte-for-byte what the Python client sent and derives the same
//! `auth_key`.
//!
//! # Why this rather than testing against the server
//!
//! Telegram answers a malformed request by closing the connection. No error, no log, no
//! indication of which field was wrong — and a wrong slice bound in the key derivation
//! looks exactly like a wrong byte order in `p`. Every debugging cycle would be a socket
//! round trip with a one-bit result.
//!
//! Here a failure names the step and the byte. This is the same method that produced 1077
//! identical lines between `symbian-crypto` and its Python reference, and it found real
//! bugs there.
//!
//! # Refreshing the fixture
//!
//! ```text
//! python3 vendor/research/mtproto/handshake.py \
//!     --fixture apps/telegram/proto/tests/fixtures/handshake.json
//! ```
//!
//! It needs a network and it negotiates a real key, which is then thrown away. The
//! recorded key belongs to nobody: it authenticates no account until an account is signed
//! in with it, and this one never was.

use std::fs;

use tg_proto::crypto::Rng;
use tg_proto::handshake::{Action, Handshake};

/// The exact bytes the Python client drew, in the order it drew them.
///
/// Not a random source at all — a tape. `Handshake::start` takes 16 + 32 + 256 bytes for
/// the nonces and the secret, then `rsa_pad` and the IGE padding draw from what follows,
/// and the order has to match the Python or every subsequent value diverges. That coupling
/// is unpleasant and it is the price of a byte-exact differential; the alternative is
/// comparing structure instead of bytes, which is what lets an endianness bug through.
struct Tape {
    bytes: Vec<u8>,
    pos: usize,
}

impl Rng for Tape {
    fn fill(&mut self, out: &mut [u8]) {
        let end = self.pos + out.len();
        assert!(end <= self.bytes.len(), "the tape ran out: wanted {end}, have {}", self.bytes.len());
        out.copy_from_slice(&self.bytes[self.pos..end]);
        self.pos = end;
    }
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

struct Fixture {
    nonce: Vec<u8>,
    new_nonce: Vec<u8>,
    b: Vec<u8>,
    pad_stream: Vec<u8>,
    received: Vec<String>,
    sent: Vec<String>,
    auth_key: String,
    auth_key_id: String,
    server_salt: String,
}

/// A three-field JSON reader, because pulling in serde for one test file is not a trade
/// worth making in a crate that has to build for a phone.
fn load() -> Fixture {
    let raw = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/handshake.json"
    ))
    .expect("fixture missing; see the module docs for how to record one");

    fn field<'a>(raw: &'a str, name: &str) -> &'a str {
        let key = format!("\"{name}\":");
        let at = raw.find(&key).unwrap_or_else(|| panic!("no field {name}"));
        let rest = &raw[at + key.len()..];
        let start = rest.find('"').unwrap() + 1;
        let end = rest[start..].find('"').unwrap() + start;
        &rest[start..end]
    }

    fn array(raw: &str, name: &str) -> Vec<String> {
        let key = format!("\"{name}\":");
        let at = raw.find(&key).unwrap_or_else(|| panic!("no field {name}"));
        let rest = &raw[at + key.len()..];
        let open = rest.find('[').unwrap();
        let close = rest.find(']').unwrap();
        rest[open + 1..close]
            .split(',')
            .filter_map(|s| {
                let s = s.trim().trim_matches('"');
                if s.is_empty() { None } else { Some(s.to_string()) }
            })
            .collect()
    }

    Fixture {
        nonce: unhex(field(&raw, "nonce")),
        new_nonce: unhex(field(&raw, "new_nonce")),
        b: unhex(field(&raw, "b")),
        pad_stream: unhex(field(&raw, "pad_stream")),
        received: array(&raw, "received"),
        sent: array(&raw, "sent"),
        auth_key: field(&raw, "auth_key").to_string(),
        auth_key_id: field(&raw, "auth_key_id").to_string(),
        server_salt: field(&raw, "server_salt").to_string(),
    }
}

/// The body of a recorded client message, with the 20-byte unencrypted header removed.
///
/// The header holds a `msg_id` derived from the clock, so it differs on every run and is
/// not something the Rust produces — `handshake.rs` has no clock on purpose. Comparing
/// bodies is comparing everything this crate is responsible for.
fn body_of(sent_hex: &str) -> String {
    sent_hex[40..].to_string()
}

#[test]
fn the_rust_handshake_reproduces_the_recorded_one() {
    let fx = load();

    let mut tape = Tape {
        bytes: [fx.nonce.clone(), fx.new_nonce.clone(), fx.b.clone(), fx.pad_stream.clone()]
            .concat(),
        pos: 0,
    };

    // ---- req_pq_multi ----
    let (mut hs, action) = Handshake::start(&mut tape);
    let Action::Send(out) = action else { panic!("start did not send") };
    assert_eq!(hex(&out), body_of(&fx.sent[0]), "req_pq_multi differs");

    // ---- resPQ -> req_DH_params ----
    //
    // Everything hard is in this step: factoring pq, TL-encoding p and q at minimal width,
    // and RSA_PAD. A mismatch here is almost always one of those three, and the byte offset
    // says which.
    let reply = unhex(&fx.received[0]);
    let action = hs.on_message(&reply, &mut tape).expect("resPQ rejected");
    let Action::Send(out) = action else { panic!("expected req_DH_params") };
    assert_eq!(hex(&out), body_of(&fx.sent[1]), "req_DH_params differs");

    // ---- server_DH_params_ok -> the first exponentiation ----
    let reply = unhex(&fx.received[1]);
    let action = hs.on_message(&reply, &mut tape).expect("server_DH_params rejected");
    let Action::ModPow { base, exp, modulus } = action else {
        panic!("expected a modpow for g^b")
    };
    assert_eq!(base, vec![3], "Telegram uses g = 3");
    assert_eq!(exp, fx.b, "the exponent must be the recorded secret");
    let g_b = modpow(&base, &exp, &modulus);

    // ---- g^b -> set_client_DH_params ----
    let action = hs.on_modpow(&g_b, &mut tape).expect("g_b rejected");
    let Action::Send(out) = action else { panic!("expected set_client_DH_params") };
    assert_eq!(hex(&out), body_of(&fx.sent[2]), "set_client_DH_params differs");

    // ---- dh_gen_ok -> the second exponentiation ----
    let reply = unhex(&fx.received[2]);
    let action = hs.on_message(&reply, &mut tape).expect("dh_gen rejected");
    let Action::ModPow { base, exp, modulus } = action else {
        panic!("expected a modpow for the auth key")
    };
    let key = modpow(&base, &exp, &modulus);

    // ---- the key ----
    let action = hs.on_modpow(&key, &mut tape).expect("auth key rejected");
    let Action::Done(auth) = action else { panic!("expected Done") };

    assert_eq!(hex(&auth.key), fx.auth_key, "auth_key differs");
    assert_eq!(format!("{:016x}", auth.id), fx.auth_key_id, "auth_key_id differs");
    assert_eq!(hex(&auth.salt), fx.server_salt, "server_salt differs");
}

/// `base^exp mod modulus`, big-endian, left-padded to the modulus width.
fn modpow(base: &[u8], exp: &[u8], modulus: &[u8]) -> Vec<u8> {
    let m = symbian_crypto::Modulus::new(modulus).expect("bad modulus");
    let mut out = vec![0u8; modulus.len()];
    symbian_crypto::modpow(base, exp, &m, &mut out).expect("modpow failed");
    out
}

/// The fixture must be a real transcript, not a stub someone left behind.
#[test]
fn the_fixture_looks_like_a_real_handshake() {
    let fx = load();
    assert_eq!(fx.nonce.len(), 16);
    assert_eq!(fx.new_nonce.len(), 32);
    assert_eq!(fx.b.len(), 256);
    assert_eq!(fx.b[0] & 0x80, 0x80, "the DH secret must be full width");
    assert_eq!(fx.sent.len(), 3, "three client messages");
    assert_eq!(fx.received.len(), 3, "three server replies");
    assert_eq!(fx.auth_key.len(), 512, "a 2048-bit key in hex");
    assert_ne!(fx.auth_key.trim_matches('0'), "", "the key is all zeros");
}

/// A tampered reply must be rejected rather than producing a key.
///
/// Corrupting the last byte of the DH answer breaks its SHA-1, which is the only thing
/// standing between a man in the middle and a session. A version that skipped that check
/// would pass every other test in this file.
#[test]
fn a_corrupted_dh_answer_is_rejected() {
    let fx = load();
    let mut tape = Tape {
        bytes: [fx.nonce.clone(), fx.new_nonce.clone(), fx.b.clone(), fx.pad_stream.clone()]
            .concat(),
        pos: 0,
    };

    let (mut hs, _) = Handshake::start(&mut tape);
    hs.on_message(&unhex(&fx.received[0]), &mut tape).unwrap();

    let mut bad = unhex(&fx.received[1]);
    let last = bad.len() - 1;
    bad[last] ^= 1;
    let outcome = hs.on_message(&bad, &mut tape);
    assert!(outcome.is_err(), "a flipped bit in the DH answer was accepted");
}

/// A reply carrying someone else's nonce must be rejected.
#[test]
fn a_spliced_reply_is_rejected() {
    let fx = load();
    let mut tape = Tape {
        bytes: [fx.nonce.clone(), fx.new_nonce.clone(), fx.b.clone(), fx.pad_stream.clone()]
            .concat(),
        pos: 0,
    };

    let (mut hs, _) = Handshake::start(&mut tape);
    let mut bad = unhex(&fx.received[0]);
    // The nonce echo sits right after the 20-byte header and the 4-byte constructor.
    bad[24] ^= 0xff;
    assert!(hs.on_message(&bad, &mut tape).is_err(), "a foreign nonce was accepted");
}
