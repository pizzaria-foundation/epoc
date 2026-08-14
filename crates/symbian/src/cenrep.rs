//! A minimal Central Repository read — for status values that live in CenRep rather than
//! Publish&Subscribe (the Bluetooth power state first). Device-only, reached from the isolated
//! network daemon; the host stubs to [`Error::NotReady`].

use crate::error::{Error, Result};
use symbian_sys as sys;

/// Read an integer CenRep key from repository `repo`. An access-denied or missing key surfaces as
/// an [`Error`], which callers treat as "unknown".
pub fn get(repo: u32, key: u32) -> Result<i32> {
    let mut out = 0i32;
    // SAFETY: `out` is a live local the shim writes exactly once.
    let rc = unsafe { sys::shim_cenrep_get(repo, key, &mut out) };
    Error::check(rc)?;
    Ok(out)
}
