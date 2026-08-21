//! Reading a gzip file a piece at a time, through the platform's own zlib.
//!
//! The other half of [`crate::tls::fetch_to_file`]: a body too large to hold is fetched compressed
//! to disk, and read back through here. Memory stays flat — one input block inside the shim, one
//! buffer the caller sizes — so a 17 MB calendar is readable on a phone with a few megabytes to
//! spare.
//!
//! `libz.dll` is the device's own (it loads with `inflate` resolving; see `docs/device-dump.txt`),
//! and needs `USE_ZLIB=1` in `app.conf`. Synchronous: inflating is CPU work with no asynchronous
//! request behind it, so unlike the fetch this is safe to call from anywhere, including a GUI
//! thread — though reading a megabyte in one go on the UI thread would still be rude.

use crate::error::{Error, Result};
use crate::fs::Utf16Path;
use symbian_sys as sys;

/// An open gzip file, read through [`Gunzip::read`] until it returns `Ok(0)`.
pub struct Gunzip {
    handle: i32,
}

impl Gunzip {
    /// Open `path` as a gzip (or zlib) stream. The header form is detected, not assumed.
    pub fn open(path: &Utf16Path) -> Result<Self> {
        let units = path.as_units();
        let mut handle = 0i32;
        // SAFETY: `units` is valid for its length and only read; `handle` is a live local the shim
        // writes once on success.
        let rc = unsafe {
            sys::shim_gunzip_open(units.as_ptr(), units.len() as i32, &mut handle)
        };
        Error::check(rc)?;
        Ok(Gunzip { handle })
    }

    /// Inflate the next bytes into `out`. `Ok(0)` is the end of the stream.
    ///
    /// A short read is **not** the end — only zero is. A stream that was cut short (a download that
    /// died halfway) ends without complaint, because the bytes that did arrive are still worth
    /// reading and the caller is the one who knows whether a partial answer is usable.
    pub fn read(&mut self, out: &mut [u8]) -> Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        // SAFETY: `out` is valid for `out.len()` bytes; the shim writes at most that many.
        let rc = unsafe { sys::shim_gunzip_read(self.handle, out.as_mut_ptr(), out.len() as i32) };
        if rc < 0 {
            return Err(Error::from_code(rc));
        }
        Ok(rc as usize)
    }
}

impl Drop for Gunzip {
    fn drop(&mut self) {
        // SAFETY: the handle is ours and is closed exactly once.
        unsafe { sys::shim_gunzip_close(self.handle) };
    }
}
