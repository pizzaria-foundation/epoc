//! Telephony reads for the status bar — currently signal strength. Device-only, and reached only
//! from the isolated network daemon (`apps/netd`): the underlying `etel3rdparty` import is a load
//! risk quarantined away from the launcher. On the host the shim stubs to [`Error::NotReady`].

use crate::error::{Error, Result};
use symbian_sys as sys;

/// Cellular signal: `bars` (0..7, or -1 unknown) and the raw `dbm`.
pub fn signal() -> Result<(i32, i32)> {
    let mut bars = -1i32;
    let mut dbm = 0i32;
    // SAFETY: both are live locals the shim writes exactly once.
    let rc = unsafe { sys::shim_tele_signal(&mut bars, &mut dbm) };
    Error::check(rc)?;
    Ok((bars, dbm))
}
