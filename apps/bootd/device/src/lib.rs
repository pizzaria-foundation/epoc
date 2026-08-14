//! The boot supervisor's device entry point.
#![no_std]
#![no_main]

symbian_app::daemon_entry!(bootd::Bootd::new());
