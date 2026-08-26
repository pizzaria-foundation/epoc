//! The device build — headless, one shot.
//!
//! `daemon_entry!` rather than `entry!`, for the reason `apps/httpprobe/device/src/lib.rs` records: a
//! GUI application is one instance per UID3, so a run that died leaving its window group behind made
//! the next launch exit on the spot, with no log to say why. A probe that cannot report is not a
//! probe.
#![no_std]
#![no_main]

extern crate alloc;

symbian_app::daemon_entry!(ghprobe::GhProbe::new());
