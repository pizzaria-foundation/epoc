//! MTProto 2.0, as much of it as a login needs.
//!
//! # No I/O
//!
//! Nothing here opens a socket, reads a file, asks the clock or generates a random number.
//! Bytes come in, bytes go out, and everything unpredictable is passed in by the caller.
//!
//! That is not purity for its own sake. It is what makes the whole protocol runnable under
//! `cargo test`, and it is the only way this could be debugged at all: the alternative is a
//! remote server, a real phone number, no log, and a connection that closes without
//! explanation when a field is wrong. The Python client in `vendor/research/mtproto/`
//! performs a real handshake and dumps every intermediate value; the tests here replay
//! those bytes and check that this code produces the same ones.
//!
//! The same shape as `symbian::net`, and for the same reason — a state machine behind a
//! trait, with a fake standing in for the platform.
//!
//! # Layers
//!
//! ```text
//!  transport   length framing over TCP           transport.rs
//!  session     auth_key_id, msg_key, AES-IGE     session.rs
//!  message     msg_id, seq_no, containers        session.rs
//!  TL          constructors and scalars          tl.rs
//! ```
//!
//! The handshake in `handshake.rs` runs *below* `session`: it is unencrypted, which is what
//! `auth_key_id = 0` on the wire means, and its whole job is to produce the key the session
//! layer needs.
//!
//! # Endianness, which is where the hours go
//!
//! TL is little-endian. The cryptography is big-endian — RSA, DH and the SHA hashes all
//! treat their operands as big-endian integers, because that is how the RFCs define them.
//! Both appear in the same fifty lines of `handshake.rs`.
//!
//! Every conversion between them is marked. A number written in the wrong order is not a
//! crash and not a parse error: it is a perfectly well-formed request that the server
//! rejects by closing the connection, and there is nothing to inspect afterwards.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod auth;
pub mod chats;
pub mod client;
pub mod crypto;
pub mod handshake;
pub mod keys;
pub mod pq;
pub mod rpc;
pub mod schema;
pub mod session;
pub mod srp;
pub mod tl;
pub mod walk;
pub mod transport;

pub use tl::{Reader, Writer};
pub use client::{Client, Step};

/// Telegram's Diffie-Hellman and SRP prime, for tests that need a real one.
///
/// Behind `cfg(test)` because nothing at run time needs the prime as a literal — the
/// handshake and SRP both check the one the server sends against a hash of it, which is
/// 32 bytes rather than 256.
#[cfg(test)]
pub(crate) fn srp_test_prime() -> alloc::vec::Vec<u8> {
    const HEX: &str = concat!(
        "C71CAEB9C6B1C9048E6C522F70F13F73980D40238E3E21C14934D037563D930F",
        "48198A0AA7C14058229493D22530F4DBFA336F6E0AC925139543AED44CCE7C37",
        "20FD51F69458705AC68CD4FE6B6B13ABDC9746512969328454F18FAF8C595F64",
        "2477FE96BB2A941D5BCD1D4AC8CC49880708FA9B378E3C4F3A9060BEE67CF9A4",
        "A4A695811051907E162753B56B0F6B410DBA74D8A84B2A14B3144E0EF1284754",
        "FD17ED950D5965B4B9DD46582DB1178D169C6BC465B0D6FF9CA3928FEF5B9AE4",
        "E418FC15E83EBEA0F87FA9FF5EED70050DED2849F47BF959D956850CE929851F",
        "0D8115F635B105EE2E4E15D04B2454BF6F4FADF034B10403119CD8E3B92FCC5B",
    );
    (0..HEX.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&HEX[i..i + 2], 16).unwrap())
        .collect()
}
pub use transport::{Frame, Transport};
