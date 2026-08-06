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

pub mod client;
pub mod crypto;
pub mod handshake;
pub mod keys;
pub mod pq;
pub mod rpc;
pub mod session;
pub mod tl;
pub mod transport;

pub use tl::{Reader, Writer};
pub use client::{Client, Step};
pub use transport::{Frame, Transport};
