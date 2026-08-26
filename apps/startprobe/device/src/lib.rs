//! The startup probe's device entry point.
#![no_std]
#![no_main]

symbian_app::daemon_entry!(startprobe::Startprobe::new());
