//! The boot manager's device entry point.
#![no_std]
#![no_main]

symbian_app::entry!(bootctl::BootCtl::new());
