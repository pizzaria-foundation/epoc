//! Decrypt real server messages, recorded from an authenticated call.
//!
//! `tests/fixtures/session.json` holds a live `help.getConfig` exchange: the auth key that
//! was negotiated, the salt and session id used, the request that was sent, and the two
//! encrypted replies that came back. Telegram encrypted those replies; nothing here can
//! forge them, which is what makes this a test of the encrypted layer rather than a test of
//! this crate agreeing with itself.
//!
//! `session.rs`'s own tests encrypt and decrypt with the same code. That catches a broken
//! implementation and cannot catch a *wrong* one — a client with the key-derivation slices
//! transposed round-trips its own messages perfectly and talks to nobody. Only real
//! ciphertext distinguishes those.
//!
//! # What the recording showed
//!
//! One call produced three layers of wrapping:
//!
//! ```text
//! msg_container   an ack and new_session_created
//! rpc_result      req_msg_id, then
//!   gzip_packed     the Config, deflated
//! ```
//!
//! Which is why `rpc::unwrap` exists and why `symbian-crypto` carries an inflate.
//!
//! # Recording another
//!
//! ```text
//! python3 vendor/research/mtproto/handshake.py --probe \
//!     --fixture apps/telegram/proto/tests/fixtures/session.json
//! ```
//!
//! The key in the fixture authenticates no account. `help.getConfig` needs no login, and
//! this one was never signed in with.

use std::fs;

use tg_proto::handshake::AuthKey;
use tg_proto::rpc::{self, Update};
use tg_proto::session::{Dir, Session};

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

struct Fixture {
    auth_key: Vec<u8>,
    auth_key_id: u64,
    salt: Vec<u8>,
    session_id: u64,
    sent_msg_id: u64,
    request: Vec<u8>,
    replies: Vec<Vec<u8>>,
}

fn load() -> Fixture {
    let raw = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/session.json"
    ))
    .expect("fixture missing; see the module docs for how to record one");

    fn string(raw: &str, name: &str) -> String {
        let key = format!("\"{name}\":");
        let at = raw.find(&key).unwrap_or_else(|| panic!("no field {name}"));
        let rest = &raw[at + key.len()..];
        let start = rest.find('"').unwrap() + 1;
        let end = rest[start..].find('"').unwrap() + start;
        rest[start..end].to_string()
    }

    fn number(raw: &str, name: &str) -> i64 {
        let key = format!("\"{name}\":");
        let at = raw.find(&key).unwrap_or_else(|| panic!("no field {name}"));
        let rest = raw[at + key.len()..].trim_start();
        let end = rest.find(|c: char| !c.is_ascii_digit() && c != '-').unwrap_or(rest.len());
        rest[..end].parse().unwrap()
    }

    // Every "raw" inside the replies array, in order.
    let mut replies = Vec::new();
    let mut cursor = raw.find("\"replies\"").expect("no replies");
    while let Some(at) = raw[cursor..].find("\"raw\":") {
        let rest = &raw[cursor + at + 6..];
        let start = rest.find('"').unwrap() + 1;
        let end = rest[start..].find('"').unwrap() + start;
        replies.push(unhex(&rest[start..end]));
        cursor += at + 6 + end;
    }

    Fixture {
        auth_key: unhex(&string(&raw, "auth_key")),
        auth_key_id: u64::from_str_radix(&string(&raw, "auth_key_id"), 16).unwrap(),
        salt: unhex(&string(&raw, "server_salt")),
        session_id: number(&raw, "session_id") as u64,
        sent_msg_id: number(&raw, "sent_msg_id") as u64,
        request: unhex(&string(&raw, "getConfig_body")),
        replies,
    }
}

fn session(fx: &Fixture) -> Session {
    let mut key = [0u8; 256];
    key.copy_from_slice(&fx.auth_key);
    let mut salt = [0u8; 8];
    salt.copy_from_slice(&fx.salt);
    Session::new(&AuthKey { key, id: fx.auth_key_id, salt, server_time: 0 }, fx.session_id)
}

/// The core claim: this crate can read what Telegram actually sent.
#[test]
fn real_server_messages_decrypt() {
    let fx = load();
    let s = session(&fx);
    assert_eq!(fx.replies.len(), 2, "the recording should hold two replies");

    for (i, wire) in fx.replies.iter().enumerate() {
        let msg = s.decrypt(wire).unwrap_or_else(|e| panic!("reply {i} failed to decrypt: {e:?}"));
        assert!(!msg.body.is_empty(), "reply {i} decrypted to nothing");
        // The salt the server stamped must be the one we were using, or the whole session
        // was mismatched and the decrypt succeeding was luck.
        assert_eq!(msg.salt.to_le_bytes(), fx.salt[..], "reply {i} carried a different salt");
    }
}

/// And can find the answer under three layers of wrapping.
#[test]
fn the_reply_unwraps_to_a_result_for_our_request() {
    let fx = load();
    let s = session(&fx);

    let mut updates = Vec::new();
    for wire in &fx.replies {
        let msg = s.decrypt(wire).expect("decrypt");
        updates.extend(rpc::unwrap(msg.msg_id, &msg.body).expect("unwrap"));
    }

    // The container held an ack and a new session; the second reply held the result.
    assert!(updates.len() >= 2, "expected several updates, got {}", updates.len());

    let result = updates
        .iter()
        .find_map(|u| match u {
            Update::Result { req_msg_id, body, .. } if *req_msg_id == fx.sent_msg_id => Some(body),
            _ => None,
        })
        .expect("no Result matching the msg_id we sent");

    // The Config, after the gzip layer came off. Its constructor changes with the API
    // layer, so the assertion is on the shape rather than the exact id: a body that is
    // still gzip_packed means the inflate did not happen.
    assert!(result.len() > 100, "the Config is only {} bytes; still compressed?", result.len());
    let ctor = u32::from_le_bytes([result[0], result[1], result[2], result[3]]);
    assert_ne!(ctor, rpc::GZIP_PACKED, "the result was never inflated");

    // A new salt should have arrived with the new session.
    assert!(
        updates.iter().any(|u| matches!(u, Update::NewSession { .. } | Update::NewSalt { .. })),
        "expected a session or salt update: {updates:?}"
    );
}

/// The request this crate builds must match the one that got that reply.
#[test]
fn the_request_matches_what_the_reference_sent() {
    let fx = load();
    let ours = rpc::init_connection(6, "Nokia E72", "Symbian 9.3", "0.1", &rpc::get_config());
    assert_eq!(
        ours,
        fx.request,
        "initConnection differs from the one the server accepted"
    );
}

/// Encrypting with the inbound key material must not produce something we can decrypt as
/// outbound, and vice versa.
///
/// The x = 0 / x = 8 split is one constant apart from a client that cannot talk to anyone.
/// This checks the direction is really load-bearing, using the real key rather than a
/// pattern — key material with structure in it can hide a slice bug that real bytes expose.
#[test]
fn the_direction_split_is_real_for_a_real_key() {
    let fx = load();
    let mut key = [0u8; 256];
    key.copy_from_slice(&fx.auth_key);
    let msg_key = [0x33u8; 16];
    let out = tg_proto::session::message_keys(&key, &msg_key, Dir::Out);
    let inn = tg_proto::session::message_keys(&key, &msg_key, Dir::In);
    assert_ne!(out.0, inn.0);
    assert_ne!(out.1, inn.1);
}

/// A tampered real message must be rejected.
#[test]
fn a_flipped_bit_in_a_real_message_is_caught() {
    let fx = load();
    let s = session(&fx);
    let wire = &fx.replies[0];
    // Every 7th byte past the auth_key_id, which is enough coverage to catch a msg_key
    // check that is not actually running without making the test slow.
    for i in (8..wire.len()).step_by(7) {
        let mut bad = wire.clone();
        bad[i] ^= 1;
        assert!(s.decrypt(&bad).is_err(), "flipping byte {i} of a real message was accepted");
    }
}
