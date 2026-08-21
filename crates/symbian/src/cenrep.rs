//! A minimal Central Repository read — for status values that live in CenRep rather than
//! Publish&Subscribe (the Bluetooth power state first). Device-only, reached from the isolated
//! network daemon; the host stubs to [`Error::NotReady`].

use alloc::string::String;

use crate::error::{Error, Result};
use symbian_sys as sys;

/// The repository that names the application the phone shows as its home screen.
///
/// Found by measurement, and the two wrong turns are worth keeping because each looked right:
///
/// - `KPSUidAiInformation`/`KActiveIdleUid` is Publish&Subscribe and *reports* which application is
///   the idle. The write was accepted and changed nothing, because publishing a fact is not the
///   same as deciding one.
/// - `0x101F876F` holds a UID beside a read-only copy of itself, which is exactly the shape of a
///   current-value-plus-factory-default pair. It is the **theme** repository, owned by the skin
///   server `0x10207114`, and writing to it set the phone's theme to something that does not exist.
///
/// This one was found the only way that was ever going to work: by searching every repository on
/// the handset for the UID of the application that actually is the home screen — `0x102750F0`,
/// "Standby", read out of the phone's own application registry.
///
/// Its shape says what it is. Key `0x1` holds that UID; keys `0x20`/`0x21` hold sounds; and the
/// same sound keys repeat at `0x1000020`, `0x2000020` … `0x7F000020`, where the top byte is a mode
/// index. This is the E7x "Modes" feature — the platform's own mechanism for having more than one
/// home screen — and key `0x1` in the base band is the one it shows.
pub const IDLE_APP_REPO: u32 = 0x2001_5159;
/// The application UID, as an integer. `WriteDeviceData`, which the launcher declares.
pub const IDLE_APP_KEY_INT: u32 = 0x1;
/// The active mode, for context. Read only for display — changing it is the Modes application's
/// business, not ours.
pub const IDLE_MODE_KEY: u32 = 0x0;

/// The platform's own home screen, "Standby" in the application registry.
///
/// The way back, held as a constant because this repository has no factory-default file to read it
/// from: it exists only under `C:\\private\\10202be9\\`, so once the value is overwritten there is
/// nothing on the phone that still remembers it.
pub const NATIVE_IDLE_UID: u32 = 0x1027_50F0;

/// Read an integer CenRep key from repository `repo`. An access-denied or missing key surfaces as
/// an [`Error`], which callers treat as "unknown".
pub fn get(repo: u32, key: u32) -> Result<i32> {
    let mut out = 0i32;
    // SAFETY: `out` is a live local the shim writes exactly once.
    let rc = unsafe { sys::shim_cenrep_get(repo, key, &mut out) };
    Error::check(rc)?;
    Ok(out)
}

/// Read a string key. Values here are settings, so the buffer is small on purpose.
pub fn get_string(repo: u32, key: u32) -> Result<String> {
    let mut buf = [0u16; 512];
    let mut len = 0i32;
    // SAFETY: `buf` and `len` are live locals; the shim writes at most `buf.len()` units and
    // reports how many through `len`.
    let rc = unsafe {
        sys::shim_cenrep_get_string(repo, key, buf.as_mut_ptr(), buf.len() as i32, &mut len)
    };
    Error::check(rc)?;
    let n = (len.max(0) as usize).min(buf.len());
    Ok(String::from_utf16_lossy(&buf[..n]))
}

/// Write an integer key. Fails with the platform's error when the key's write policy denies the
/// caller, which is an answer and not a malfunction.
pub fn set(repo: u32, key: u32, value: i32) -> Result<()> {
    // SAFETY: no pointers.
    Error::check(unsafe { sys::shim_cenrep_set(repo, key, value) })
}

/// Write a string key.
pub fn set_string(repo: u32, key: u32, value: &str) -> Result<()> {
    let units: alloc::vec::Vec<u16> = value.encode_utf16().collect();
    // SAFETY: `units` is valid for `units.len()` u16 and only read.
    Error::check(unsafe { sys::shim_cenrep_set_string(repo, key, units.as_ptr(), units.len() as i32) })
}

/// Format a UID as decimal text, for the repository keys that store one that way.
///
/// Kept because some repositories do store a UID as a string rather than an integer, and getting
/// the base wrong there is silent: both are strings the repository accepts.
pub fn uid_to_setting(uid3: u32) -> String {
    let mut s = String::new();
    let mut n = uid3;
    if n == 0 {
        s.push('0');
        return s;
    }
    let mut digits = [0u8; 10];
    let mut i = 0;
    while n > 0 {
        digits[i] = b'0' + (n % 10) as u8;
        n /= 10;
        i += 1;
    }
    while i > 0 {
        i -= 1;
        s.push(digits[i] as char);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_uid_is_written_as_plain_decimal() {
        // The value this handset ships with, so the encoding is pinned against a real one.
        assert_eq!(uid_to_setting(0x101F_D60A), "270521866");
        assert_eq!(uid_to_setting(0xE0AA_0000), "3769237504", "the launcher, as this key spells it");
        assert_eq!(uid_to_setting(0), "0");
    }
}
