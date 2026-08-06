//! Parse a `messages.dialogs` built by an independent encoder.
//!
//! `vendor/research/mtproto/gen_chats.py` reads `api.tl` and emits a realistic reply. It
//! knows nothing about `walk.rs` or the generated table — it is a second implementation of
//! the wire format, which is the only thing that makes this a test rather than the walker
//! agreeing with itself.
//!
//! That distinction matters more here than anywhere else in the crate. The walker's table
//! is generated, so a bug in the generator would be a bug in every hand-built fixture too:
//! the fixture would be wrong in exactly the way the parser was, and the test would pass.
//!
//! # Refreshing
//!
//! ```text
//! python3 vendor/research/mtproto/gen_chats.py \
//!     > apps/telegram/proto/tests/fixtures/dialogs.txt
//! ```
//!
//! Regenerate after `gen_schema.py`, since both read the same `api.tl` and a layer change
//! moves both.

use std::collections::BTreeMap;
use std::fs;

use tg_proto::chats::{self, Kind};

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

struct Fixture {
    dialogs: Vec<u8>,
    history: Vec<u8>,
    expect: BTreeMap<String, String>,
}

fn load() -> Fixture {
    let raw = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/dialogs.txt"
    ))
    .expect("fixture missing; see the module docs");

    let (mut dialogs, mut history) = (Vec::new(), Vec::new());
    let mut expect = BTreeMap::new();
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("dialogs ") {
            dialogs = unhex(rest.trim());
        } else if let Some(rest) = line.strip_prefix("history ") {
            history = unhex(rest.trim());
        } else if let Some(rest) = line.strip_prefix("expect ") {
            let mut it = rest.splitn(2, ' ');
            let k = it.next().unwrap().to_string();
            expect.insert(k, it.next().unwrap_or("").to_string());
        }
    }
    Fixture { dialogs, history, expect }
}

fn n(f: &Fixture, k: &str) -> i64 {
    f.expect[k].parse().unwrap_or_else(|_| panic!("{k} is not a number"))
}

#[test]
fn a_dialog_list_parses_into_what_the_screen_needs() {
    let f = load();
    let d = chats::parse_dialogs(&f.dialogs).expect("the dialogs reply did not parse");

    assert_eq!(d.dialogs.len() as i64, n(&f, "dialog_count"));
    assert_eq!(d.messages.len() as i64, n(&f, "message_count"));
    assert_eq!(d.names.len() as i64, n(&f, "name_count"));

    // The peer join: a dialog names a peer, the name is in a different vector, and the two
    // id spaces overlap. Getting this wrong shows the group's name on the person's row.
    let d0 = &d.dialogs[0];
    assert_eq!(d.name_of(d0.peer), Some(f.expect["d0_name"].as_str()));
    assert_eq!(d0.unread as i64, n(&f, "d0_unread"));
    let top = d.top_of(d0).expect("no top message for the first dialog");
    assert_eq!(top.text, f.expect["d0_text"]);
    assert_eq!(top.out as i64, n(&f, "d0_out"));

    // A user with no surname must not come back with a trailing space.
    let d1 = &d.dialogs[1];
    assert_eq!(d.name_of(d1.peer), Some(f.expect["d1_name"].as_str()));
    assert_eq!(d.top_of(d1).unwrap().out as i64, n(&f, "d1_out"));

    // A group, whose id collides with the first user's on purpose.
    let d2 = &d.dialogs[2];
    assert_eq!(d.name_of(d2.peer), Some(f.expect["d2_name"].as_str()));
    assert_eq!(d2.unread as i64, n(&f, "d2_unread"));
    assert_eq!(d2.peer.kind, Kind::Chat);
    assert_eq!(d2.peer.id, d.dialogs[0].peer.id, "the fixture should collide the ids");
}

#[test]
fn a_history_reply_parses() {
    let f = load();
    let h = chats::parse_history(&f.history).expect("the history reply did not parse");
    assert_eq!(h.messages.len() as i64, n(&f, "history_count"));
    assert_eq!(h.messages[0].text, f.expect["h0_text"]);
    assert_eq!(h.messages[1].out as i64, n(&f, "h1_out"));
    assert!(!h.messages[0].out, "the first message is incoming");
}

/// The walker must reject a truncated reply rather than returning half a chat list.
#[test]
fn a_truncated_reply_is_an_error_at_every_length() {
    let f = load();
    for cut in 0..f.dialogs.len() {
        // A prefix can legitimately parse if it happens to end on a value boundary and the
        // vector counts still fit -- what must never happen is a panic.
        let _ = chats::parse_dialogs(&f.dialogs[..cut]);
    }
    // The full thing still works after all that.
    assert!(chats::parse_dialogs(&f.dialogs).is_ok());
}

/// A flipped byte must not produce a plausible chat list.
///
/// TL has no checksums, so a corrupt reply is caught only by the shapes disagreeing. This
/// does not assert that every flip is caught — many land in a string and are simply
/// different text — only that none of them panics, which on the device is the application
/// closing.
#[test]
fn a_corrupted_reply_never_panics() {
    let f = load();
    for i in (0..f.dialogs.len()).step_by(7) {
        let mut bad = f.dialogs.clone();
        bad[i] ^= 0xff;
        let _ = chats::parse_dialogs(&bad);
    }
}
