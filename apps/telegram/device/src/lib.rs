//! The device build of the Telegram client.
//!
//! Everything that used to be here — the allocator, the panic handler, the event
//! translation, the framebuffer setup, the three `extern "C"` entry points — is in
//! `symbian_app::entry!`. What is left is the app and the theme it wants.
//!
//! See `crates/symbian-app` for why the lang items have to be expanded into this crate
//! rather than provided by a library, and `apps/telegram/README.md` for why the app is
//! split into an rlib and this staticlib.

#![no_std]
#![no_main]

extern crate alloc;

/// A compile check that the protocol builds for the device.
///
/// `tg-proto` is a `no_std` rlib and `cargo test` only ever builds it for the host, where
/// `std` is in scope and every allocation is free. This reference keeps it in the ARM link
/// so a `BTreeMap` or a `div_ceil` that does not exist on the target fails here rather
/// than the first time someone tries to log in.
#[used]
static _PROTO_LINKS: fn(&tg_proto::client::Client) -> bool = tg_proto::client::Client::is_ready;

symbian_app::entry!(tg::App::mock());
