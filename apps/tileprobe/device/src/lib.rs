//! The device build — headless.
//!
//! `daemon_entry!` rather than `entry!`: this probe writes a report and a log, and never draws.
//!
//! No `work =` even though `USE_HTTP` implies `USE_NET`, which links `shim_work.cpp`: the macro
//! supplies `no_work`, which is the honest dispatcher for a probe that runs nothing on the worker
//! thread. Both halves measured here are active objects in this thread.

#![no_std]
#![no_main]

extern crate alloc;

symbian_app::daemon_entry!(tileprobe::TileProbe::new());
