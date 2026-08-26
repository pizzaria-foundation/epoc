//! The device build — headless.
//!
//! `daemon_entry!` rather than `entry!`: this probe writes a report and a log, and never draws.
//! A GUI application is one instance per UID3, and a probe that may sit for six minutes waiting
//! on a cold receiver is exactly the kind whose corpse would block the next run.
//!
//! No `work =`: nothing here runs on the worker thread. A position request is an active object in
//! this thread, which is the whole point of `shim_lbs.cpp`.

#![no_std]
#![no_main]

extern crate alloc;

symbian_app::daemon_entry!(gpsprobe::GpsProbe::new());
