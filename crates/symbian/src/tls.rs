//! HTTPS over the vendored mbedTLS port — a single blocking one-shot GET.
//!
//! This is deliberately tiny and deliberately blocking: the TLS handshake is several network
//! round-trips, and doing that synchronously is only safe **off the GUI thread**. So this is for
//! headless one-shot helpers (e.g. calsync), never a resident GUI app — the same rule the rest of
//! the SDK states as "never `WaitForRequest` on the UI thread". The heavy lifting (bearer, socket,
//! TLS, request/response) lives in `shim_tls.cpp`; this just hands it UTF-16 and a buffer.

use alloc::vec::Vec;

use crate::error::{Error, Result};
use symbian_sys as sys;

/// Fetch `https://<host><path>` and return the raw HTTP response (status line, headers, body).
///
/// `cap` bounds the response buffer. Blocking; brings up a bearer itself. Certificate
/// verification is currently OFF in the shim — fine for a probe, NOT for anything sensitive
/// until the CA bundle is wired.
pub fn https_get(host: &str, port: u16, path: &str, cap: usize) -> Result<Vec<u8>> {
    let h: Vec<u16> = host.encode_utf16().collect();
    let p: Vec<u16> = path.encode_utf16().collect();
    let mut out = alloc::vec![0u8; cap];
    // SAFETY: pointers are valid for their stated lengths; `out` is a live buffer of `cap` bytes
    // the shim writes at most `cap` into.
    let rc = unsafe {
        sys::shim_https_get(
            h.as_ptr(),
            h.len() as i32,
            port as i32,
            p.as_ptr(),
            p.len() as i32,
            out.as_mut_ptr(),
            cap as i32,
        )
    };
    if rc < 0 {
        return Err(Error::from_code(rc));
    }
    out.truncate(rc as usize);
    Ok(out)
}
