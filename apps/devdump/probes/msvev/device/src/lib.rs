//! The msvev probe's device entry point.
//!
//! Unlike every other probe here it is not a `OneShot`: it stays resident, watching Message
//! Server session events while the operator replies in the Messaging application. Everything it
//! knows is in `devdump::probes::msvev`.
#![no_std]
#![no_main]

symbian_app::daemon_entry!(devdump::probes::msvev::Watcher::new());
