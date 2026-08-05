//! The device build.
//!
//! `entry!` supplies the allocator, the panic handler, the three `extern "C"` functions
//! the shim calls, the event translation and the theme. See crates/symbian-app for why
//! the lang items have to be expanded here rather than provided by a library.

#![no_std]
#![no_main]

extern crate alloc;

symbian_app::entry!(netprobe::Netprobe::new());
