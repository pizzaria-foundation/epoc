//! Safe wrappers over the C++ shim.
//!
//! [`symbian_sys`] is the raw ABI: `unsafe`, raw pointers, error codes. This crate is
//! the layer that turns it into something an app can use without `unsafe` — owned
//! handles that close themselves, `Result` instead of negative integers, and the
//! retry loops that a partial read or a partial write needs.
//!
//! # Testable without a phone
//!
//! Every module here is written against a trait rather than against the shim
//! directly, with the shim as one implementation and an in-memory fake as another.
//! That is not architecture for its own sake: the interesting bugs in file I/O are in
//! the *loops* — reading until a zero-length read, writing until the whole buffer is
//! gone, replacing a file atomically — and those are pure logic that a host test can
//! exercise properly. The FFI call itself is the boring part.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_op_in_unsafe_fn)]

extern crate alloc;

pub mod error;
pub mod fs;
pub mod net;
pub mod random;
pub mod work;

pub use error::{Error, Result};
pub use fs::{File, Fs, OpenMode, ShimFs};
pub use net::{Bearer, Iap, Ipv4, Lookup, Net, Progress, RawEvent, ShimNet, TcpStream};
