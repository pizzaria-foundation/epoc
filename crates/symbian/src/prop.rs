//! Safe wrappers over the shim's Publish & Subscribe (`RProperty`).
//!
//! A one-integer control channel between two processes of the same app — a GUI app and the
//! headless daemon it launched, say: the launcher [`set`]s a key, the daemon —
//! [`subscribe`]d to it — receives a `SHIM_EV_PROP` and stops. Both processes share one UID3, so they share the property
//! category (the app's own SecureId) and neither needs a capability.
//!
//! As elsewhere, every `unsafe` block stays here; the caller stays `forbid(unsafe_code)`.
//! On the host the shim functions are stubs returning `SHIM_ERR_NOT_READY`.

use symbian_sys as sys;

use crate::error::{Error, Result};

/// Define an integer property `{category, key}`. The category must be the calling app's own
/// SecureId (its UID3) for the no-capability path. Idempotent: an already-defined key is
/// success, so the controller and the daemon may both define it independently.
pub fn define(category: u32, key: u32) -> Result<()> {
    // SAFETY: scalar arguments only.
    Error::check(unsafe { sys::shim_prop_define(category, key) })
}

/// Define an integer property with an **open read policy**, so a process in a *different* SID can
/// read it. Still cap-free when `category` is the caller's own SecureId. For a bundled daemon that
/// publishes a value (e.g. the inbox unread count) the launcher — a different UID — reads.
pub fn define_public(category: u32, key: u32) -> Result<()> {
    // SAFETY: scalar arguments only.
    Error::check(unsafe { sys::shim_prop_define_public(category, key) })
}

/// Set the integer value of a property.
pub fn set(category: u32, key: u32, value: i32) -> Result<()> {
    // SAFETY: scalar arguments only.
    Error::check(unsafe { sys::shim_prop_set(category, key, value) })
}

/// Read the current integer value of a property.
pub fn get(category: u32, key: u32) -> Result<i32> {
    let mut out = 0i32;
    // SAFETY: `out` is a live local the shim writes exactly once.
    let rc = unsafe { sys::shim_prop_get(category, key, &mut out) };
    Error::check(rc)?;
    Ok(out)
}

/// Subscribe to a property. Every change afterwards posts a `SHIM_EV_PROP` carrying the key
/// in `a` and the freshly read value in `c`. The initial value is not delivered — read it
/// once with [`get`] at startup if it matters.
pub fn subscribe(category: u32, key: u32) -> Result<()> {
    // SAFETY: scalar arguments only.
    Error::check(unsafe { sys::shim_prop_subscribe(category, key) })
}

/// Cancel a subscription started by [`subscribe`]. Safe to call when not subscribed.
pub fn unsubscribe(category: u32, key: u32) {
    // SAFETY: scalar arguments only; the shim ignores an unknown key.
    unsafe { sys::shim_prop_unsubscribe(category, key) }
}
