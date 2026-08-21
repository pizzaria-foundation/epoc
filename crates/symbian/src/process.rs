//! Safe wrappers over the shim's process launch and query.
//!
//! For a GUI app that starts and checks its own headless daemon. As with
//! [`crate::mon`], every `unsafe` block stays on this side of the wall so the caller
//! (`#![forbid(unsafe_code)]`) never touches the raw ABI. On the host the shim functions
//! are stubs returning `SHIM_ERR_NOT_READY`, so these fail cleanly under `cargo test`.

use symbian_sys as sys;

use crate::error::{Error, Result};
use crate::fs::Utf16Path;

/// Launch the executable at `path` and wait for it to signal it is up.
///
/// **Never from a GUI thread** — use [`spawn`]. The wait is `User::WaitForRequest`, which on a
/// thread running an active scheduler steals another request's completion and takes the process
/// down with a stray-signal panic. For a headless daemon, or a probe whose job is to block on its
/// child, this is the right call.
///
/// `path` is a full device path in UTF-16, e.g. `!:\sys\bin\myappd.exe`. Returns once the
/// child completes its rendezvous — so success means the daemon is actually running, not
/// merely that a process was created. Creating a process needs no capability; the child
/// runs with whatever its own image was signed for.
pub fn start(path: &Utf16Path) -> Result<()> {
    let units = path.as_units();
    // SAFETY: `units` is valid for `units.len()` u16 and only read.
    Error::check(unsafe { sys::shim_process_start(units.as_ptr(), units.len() as i32) })
}

/// Start the executable at `path` and return as soon as the process object exists.
///
/// **The one a GUI application must use.** [`start`] and [`start_with_timeout`] block in
/// `User::WaitForRequest`, and on a thread with a running active scheduler — which every Avkon
/// app has — that call consumes whatever completes next, including completions belonging to
/// active objects. The scheduler then finds a signal for a request it does not own and the
/// process dies with a stray-signal panic. That is a kernel panic, so nothing reaches the Rust
/// panic handler and no breadcrumb is written: the application simply vanishes.
///
/// It cost this repo a fortnight. The launcher died on roughly two starts in three, always in
/// the call that started the first daemon not already running, always with an empty
/// `C:\Data\panic.txt` — and at boot the supervisor launched it, it died before ever being
/// seen alive, and the home screen was silently absent.
///
/// The trade is real and small: `Ok(())` means a process was created, not that the child is
/// alive. Poll [`is_running`] if you need to know.
pub fn spawn(path: &Utf16Path) -> Result<()> {
    let units = path.as_units();
    // SAFETY: `units` is valid for `units.len()` u16 and only read.
    Error::check(unsafe { sys::shim_process_spawn(units.as_ptr(), units.len() as i32) })
}

/// Whether a process built from `uid3` is running right now.
///
/// The controller uses this for its status line and to avoid launching a second daemon.
/// A process on its way out reports as not running, so a fresh launch is never refused by a
/// corpse.
pub fn is_running(uid3: u32) -> bool {
    // SAFETY: no pointers; the shim walks the process list and returns 1/0/negative.
    unsafe { sys::shim_process_running(uid3) == 1 }
}

/// Kill every running process with this UID3.
///
/// The escape hatch for a resident launcher: one that has captured the Menu key and refuses to
/// close on End cannot be stopped from its own UI, so a separate app calls this to end it. Killing
/// a process this one did not create needs `PowerMgmt`, which a ROM-patched handset grants at load
/// regardless of the caller's declared capabilities. [`Error::NotFound`] if nothing matched.
pub fn kill(uid3: u32) -> Result<()> {
    // SAFETY: no pointers; the shim walks the process list and kills matches.
    Error::check(unsafe { sys::shim_process_kill(uid3) })
}

/// [`start`], but abandons the wait after `timeout_ms` and kills the child.
///
/// Carries [`start`]'s restriction: **never from a GUI thread**, use [`spawn`].
///
/// [`start`] waits on the child's rendezvous with no escape, which is right for a
/// controller that cannot proceed without its daemon. It is wrong for anything launching a
/// child it does not trust: one that neither signals nor dies hangs the caller for good,
/// and "every asynchronous request needs a way to abandon it, and the one that never
/// completes is exactly the one that needs it" (`docs/device-notes.md`).
///
/// Returns [`Error::Platform`] carrying `KErrTimedOut` (-33) if the deadline wins.
pub fn start_with_timeout(path: &Utf16Path, timeout_ms: i32) -> Result<()> {
    let units = path.as_units();
    // SAFETY: `units` is valid for `units.len()` u16 and only read.
    Error::check(unsafe {
        sys::shim_process_start_timeout(units.as_ptr(), units.len() as i32, timeout_ms)
    })
}

/// Launching and watching a child process, as an interface rather than a set of calls.
///
/// The reason this is a trait: `apps/devdump`'s launcher is a state machine whose whole job
/// is to survive children that refuse to load, die halfway and hang. Those are precisely
/// the cases a device cannot be asked to reproduce on demand — the whole point is that they
/// happen once, on a handset, at the far end of an install. Behind a trait, the state
/// machine runs on the host against a fake that produces all three on request, and the
/// device supplies only [`ShimProcs`].
///
/// Same shape and same reasoning as [`crate::fs::Fs`] / [`crate::fs::MemFs`].
pub trait Procs {
    /// Launch and wait for the child's rendezvous, up to `timeout_ms`.
    ///
    /// An `Err` here is the interesting outcome, not a malfunction: it is what an image the
    /// loader refuses looks like from the outside — the failure that otherwise produces
    /// nothing at all.
    fn start(&mut self, path: &Utf16Path, timeout_ms: i32) -> Result<()>;
    /// Whether a process with this UID3 is alive *right now*.
    ///
    /// Liveness, not completion. A child that panicked mid-write stops being alive exactly
    /// like one that finished, so a caller that needs to tell those apart must read what
    /// the child left behind.
    fn is_running(&mut self, uid3: u32) -> bool;
}

/// [`Procs`] over the shim. Zero-sized; there is nothing to hold.
#[derive(Copy, Clone, Debug, Default)]
pub struct ShimProcs;

impl Procs for ShimProcs {
    fn start(&mut self, path: &Utf16Path, timeout_ms: i32) -> Result<()> {
        start_with_timeout(path, timeout_ms)
    }

    fn is_running(&mut self, uid3: u32) -> bool {
        is_running(uid3)
    }
}
