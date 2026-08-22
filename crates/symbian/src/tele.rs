//! Telephony reads for the status bar — currently signal strength. Device-only, and reached only
//! from the isolated network daemon (`apps/netd`): the underlying `etel3rdparty` import is a load
//! risk quarantined away from the launcher. On the host the shim stubs to [`Error::NotReady`].
//!
//! # Do not call this from a thread running an active scheduler
//!
//! Which is every thread in every app and daemon this SDK builds. The C++ underneath waits on
//! `CTelephony::GetSignalStrength` with `User::WaitForRequest`, and that steals whatever completes
//! next — a timer, a property subscription, any active object's completion. The scheduler then dies
//! on a signal it does not own: a stray-signal *kernel* panic, so no Rust panic handler runs and no
//! `panic.txt` is written. The process is simply gone.
//!
//! `shim_process.cpp` documents the same rule and the same cost (the launcher died on two starts in
//! three), and this call has now been measured doing it to `apps/netd`: nine sessions in one log that
//! logged their first line and never reached the second.
//!
//! It is left in place rather than deleted because the fix is known and small — an active object that
//! posts an event when the modem answers, the shape `shim_net.cpp` already uses — and deleting the
//! entry point would hide the lesson. But **nothing should call it** until then; `apps/netd` does not.

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
