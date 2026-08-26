//! The device build — headless.
//!
//! `daemon_entry!` rather than `entry!`: this probe writes a report and a log, and never draws. The
//! reason is in the crate docs, and it is not tidiness — a GUI application is one instance per UID3,
//! so a run that died leaving its window group behind made the next launch exit on the spot, with no
//! log and no report to say why.

#![no_std]
#![no_main]

extern crate alloc;

// `work =` names the worker-thread dispatcher. Without it the macro supplies `no_work`, and the
// worker drill would be refused by a dispatcher that knows no opcodes.
symbian_app::daemon_entry!(httpprobe::HttpProbe::new(), work = httpprobe::worker_dispatch);
