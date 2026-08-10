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
/// `path` is a full device path in UTF-16, e.g. `!:\sys\bin\myappd.exe`. Returns once the
/// child completes its rendezvous — so success means the daemon is actually running, not
/// merely that a process was created. Creating a process needs no capability; the child
/// runs with whatever its own image was signed for.
pub fn start(path: &Utf16Path) -> Result<()> {
    let units = path.as_units();
    // SAFETY: `units` is valid for `units.len()` u16 and only read.
    Error::check(unsafe { sys::shim_process_start(units.as_ptr(), units.len() as i32) })
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
