//! The device build of the SQL probe.

#![no_std]
#![no_main]

extern crate alloc;

symbian_app::entry!(sqlprobe::SqlProbe::new());
