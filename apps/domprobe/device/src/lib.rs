//! The device build — headless.
//!
//! No window group, so it can be replaced and re-run from the desktop without anybody touching the
//! handset. That is the whole reason this app exists separately from the browser.

#![no_std]
#![no_main]

extern crate alloc;

// `work =` names the worker dispatcher. Without it the macro supplies `no_work`, the worker phase
// would be refused for every case, and the probe would report "err -1" twelve times — a wiring
// failure wearing the costume of the bug it is looking for.
symbian_app::daemon_entry!(domprobe::Probe::new(), work = domprobe::worker_dispatch);
