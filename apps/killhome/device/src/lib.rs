//! The kill app's device entry point.
#![no_std]
#![no_main]

symbian_app::entry!(killhome::KillHome::new());
