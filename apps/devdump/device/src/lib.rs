//! The launcher's device entry point.
#![no_std]
#![no_main]

symbian_app::entry!(devdump::DevDump::new());
