//! The device build.
//!
//! `entry!` supplies the allocator, the panic handler, the three `extern "C"` functions
//! the shim calls, the event translation and the theme. See crates/symbian-app for why
//! the lang items have to be expanded here rather than provided by a library.

#![no_std]
#![no_main]

extern crate alloc;

// The palette is an *expression*, re-evaluated on every step — which is what lets the left softkey
// cycle the five palettes on the handset. Checking a design decision against a real TN panel in real
// light is the one thing a contact sheet cannot do.
symbian_app::entry!(uigallery::Uigallery::new(), palette = uigallery::palette());
