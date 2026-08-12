//! The probes themselves: one module per subject, each behind its own cargo feature.
//!
//! # Why features and not a runtime switch
//!
//! The choice of probe is a *link-time* one. A probe that talks to the Message Server
//! calls `shim_msv_*`, which only exists in a binary whose `app.conf` set `USE_MSG` — and
//! `--no-undefined` checks referenced symbols before `--gc-sections` gets a chance to
//! sweep them. Code for a probe that is not being built must therefore not be compiled at
//! all, or every other probe fails to link.
//!
//! That wall is also what keeps the risky imports isolated. `probe-msg` is the only
//! feature that can drag `msgs.dso` into an image, so it is the only image that can be
//! refused by the loader for having it.
//!
//! # What every probe owes the launcher
//!
//! - Write to its own section file, named from [`crate::registry`].
//! - Flush after every phase, and leave a breadcrumb *before* each step rather than after.
//!   On a platform where a fault presents as the process simply closing, the last line
//!   written is the diagnosis.
//! - Close with the END sentinel. Its absence is how the launcher tells "died partway"
//!   from "ran to completion", and nothing else can tell them apart.
//!
//! [`Probe::run`] does the first and the last; each subject supplies the middle.

use symbian::fs::ShimFs;
use symbian_report::Report;
use symbian_sys;

use crate::registry;

/// Open a section, run `body`, close it — in the order that survives a crash.
///
/// `body` gets a report whose BEGIN line is already on disk. Whatever it writes is
/// flushed as it goes; whatever it does not reach is the finding.
pub fn section(order: u8, name: &str, body: impl FnOnce(&mut Report, &mut ShimFs)) {
    let mut fs = ShimFs;
    let mut report = Report::new(name);
    report.open_output(&mut fs, registry::DIR, &registry::filename(order, name));
    body(&mut report, &mut fs);
    report.finish(&mut fs);
}

/// A probe as a headless daemon: run once, write the section, exit.
///
/// # Why the work happens on a timer and not in the constructor
///
/// `shim_daemon.cpp`'s `MainL` calls `rust_app_start()` *before* `RProcess::Rendezvous`.
/// A probe that did its work in its constructor would therefore hold the rendezvous open
/// for the whole run — and the launcher, which waits on exactly that rendezvous with a
/// deadline, would kill every probe that took longer than a moment and record a timeout
/// for work that was proceeding perfectly well.
///
/// So the constructor does one cheap thing: arms a 1 ms shim timer. The rendezvous is
/// signalled, the scheduler starts, the pump drains the timer event, and the probe runs
/// from there — which is the mechanism `DaemonApp` documents for itself ("its periodic
/// work is itself driven by shim timer events it arms").
pub struct OneShot {
    run: fn(&mut Report, &mut ShimFs),
    order: u8,
    name: &'static str,
    done: bool,
}

impl OneShot {
    pub fn new(order: u8, name: &'static str, run: fn(&mut Report, &mut ShimFs)) -> Self {
        // Failure is survivable and must not be silent: with no timer the probe never
        // runs, and `should_exit` below then ends the process immediately, leaving no
        // section — which the launcher records as NO OUTPUT rather than as a hang.
        let _ = symbian::timer_after(1);
        OneShot { run, order, name, done: false }
    }
}

impl symbian_app::DaemonApp for OneShot {
    fn handle_raw(&mut self, ev: &symbian_sys::ShimEvent) {
        if self.done || ev.kind != symbian_sys::SHIM_EV_TIMER {
            return;
        }
        section(self.order, self.name, self.run);
        self.done = true;
    }

    fn should_exit(&self) -> bool {
        self.done
    }
}

#[cfg(feature = "probe-system")]
pub mod system;

#[cfg(feature = "probe-libsweep")]
pub mod libsweep;

#[cfg(feature = "probe-caps")]
pub mod caps;

#[cfg(feature = "probe-dll")]
pub mod dll;

#[cfg(feature = "probe-net")]
pub mod net;

#[cfg(feature = "probe-fs")]
pub mod fs;

#[cfg(feature = "probe-msg")]
pub mod msg;

#[cfg(feature = "probe-mtm")]
pub mod mtm;

#[cfg(feature = "probe-ncn")]
pub mod ncn;

#[cfg(feature = "probe-msvev")]
pub mod msvev;
