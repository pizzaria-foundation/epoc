//! Symbian error codes as a Rust error.
//!
//! Symbian returns negative integers from a flat, platform-wide list. Most of them
//! cannot happen at a given call site, so this maps the ones that carry a decision
//! and keeps the rest as [`Error::Platform`] with the raw code.
//!
//! Keeping the raw code matters more than it looks. On a device with no debugger and
//! no log, a number you can look up in `e32err.h` is often the entire diagnosis — so
//! an unrecognised code must survive to the surface rather than being flattened into
//! a generic failure.

use core::fmt;

use symbian_sys as sys;

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// `KErrNotFound`. For a file open this is the ordinary "first run" answer, not a
    /// fault, which is why it has its own variant: callers branch on it constantly.
    NotFound,
    /// `KErrPathNotFound`.
    PathNotFound,
    /// `KErrAlreadyExists`.
    AlreadyExists,
    /// `KErrNoMemory`.
    NoMemory,
    /// `KErrPermissionDenied` or `KErrAccessDenied` — outside the data cage, or a
    /// capability we do not hold.
    AccessDenied,
    /// `KErrInUse`, including the shim running out of handle slots.
    InUse,
    /// `KErrArgument`, or a null pointer the shim refused.
    Argument,
    /// `KErrOverflow` — a buffer too small, or a 64-bit offset on a 32-bit `RFile`.
    Overflow,
    /// The shim is not initialised. Only reachable on the host, where every extern is
    /// a stub, so in practice this means "you are not running on a device".
    NotReady,
    /// `KErrEof`, or a read that returned nothing when something was expected.
    UnexpectedEof,
    /// Anything else, with the code as `e32err.h` spells it.
    Platform(i32),
}

impl Error {
    /// Map a shim return value. `Ok(())` for `SHIM_OK`; positive values are also
    /// success, since some calls return a count.
    pub fn check(code: i32) -> Result<()> {
        if code >= 0 {
            Ok(())
        } else {
            Err(Error::from_code(code))
        }
    }

    pub fn from_code(code: i32) -> Error {
        match code {
            sys::SHIM_ERR_NOT_FOUND => Error::NotFound,
            sys::SHIM_ERR_NO_MEMORY => Error::NoMemory,
            sys::SHIM_ERR_ARGUMENT => Error::Argument,
            sys::SHIM_ERR_BAD_HANDLE => Error::Platform(sys::SHIM_ERR_BAD_HANDLE),
            sys::SHIM_ERR_OVERFLOW => Error::Overflow,
            sys::SHIM_ERR_ALREADY_EXISTS => Error::AlreadyExists,
            sys::SHIM_ERR_IN_USE => Error::InUse,
            sys::SHIM_ERR_NOT_READY => Error::NotReady,
            sys::SHIM_ERR_ACCESS_DENIED => Error::AccessDenied,
            // Codes with no SHIM_ constant but a meaning worth naming.
            -12 => Error::PathNotFound,
            -25 => Error::UnexpectedEof,
            -46 => Error::AccessDenied,
            other => Error::Platform(other),
        }
    }

    /// True when the error means "this does not exist yet", which for a settings or
    /// session file is the normal first-run path rather than a failure.
    pub fn is_missing(self) -> bool {
        matches!(self, Error::NotFound | Error::PathNotFound)
    }

    /// True when the platform has no plugin for what was asked.
    ///
    /// Worth naming because it is the whole of the sticker story: a Telegram sticker is
    /// WebP, WebP is from 2010, and S60 3rd Edition is from 2008. The handset answers
    /// `KErrNotSupported` and the caller must fall back to something it can draw —
    /// which is a different decision from a corrupt file or a missing one.
    pub fn is_unsupported(self) -> bool {
        matches!(self, Error::Platform(sys::SHIM_ERR_NOT_SUPPORTED))
    }

    /// The `e32err.h` code this came from — the inverse of [`Error::from_code`].
    ///
    /// For a log. `Display` would say it in words, but pulling `core::fmt` into an image
    /// to print one integer is not a trade worth making, and the number is what a Symbian
    /// header can be searched for anyway.
    pub fn code(self) -> i32 {
        match self {
            Error::NotFound => sys::SHIM_ERR_NOT_FOUND,
            Error::PathNotFound => -12,
            Error::AlreadyExists => sys::SHIM_ERR_ALREADY_EXISTS,
            Error::NoMemory => sys::SHIM_ERR_NO_MEMORY,
            Error::AccessDenied => sys::SHIM_ERR_ACCESS_DENIED,
            Error::InUse => sys::SHIM_ERR_IN_USE,
            Error::Argument => sys::SHIM_ERR_ARGUMENT,
            Error::Overflow => sys::SHIM_ERR_OVERFLOW,
            Error::NotReady => sys::SHIM_ERR_NOT_READY,
            Error::UnexpectedEof => -25,
            Error::Platform(c) => c,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotFound => f.write_str("not found"),
            Error::PathNotFound => f.write_str("path not found"),
            Error::AlreadyExists => f.write_str("already exists"),
            Error::NoMemory => f.write_str("out of memory"),
            Error::AccessDenied => f.write_str("access denied"),
            Error::InUse => f.write_str("in use"),
            Error::Argument => f.write_str("bad argument"),
            Error::Overflow => f.write_str("overflow"),
            Error::NotReady => f.write_str("shim not ready"),
            Error::UnexpectedEof => f.write_str("unexpected end of file"),
            Error::Platform(c) => write!(f, "symbian error {c}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_and_counts_are_not_errors() {
        assert!(Error::check(0).is_ok());
        assert!(Error::check(1).is_ok(), "a returned count must not read as failure");
        assert!(Error::check(4096).is_ok());
    }

    #[test]
    fn the_codes_that_carry_a_decision_are_named() {
        assert_eq!(Error::from_code(-1), Error::NotFound);
        assert_eq!(Error::from_code(-4), Error::NoMemory);
        assert_eq!(Error::from_code(-11), Error::AlreadyExists);
        assert_eq!(Error::from_code(-14), Error::InUse);
        assert_eq!(Error::from_code(-21), Error::AccessDenied);
    }

    #[test]
    fn unknown_codes_keep_their_number() {
        // The point of Platform: on a device with no log, the number is the
        // diagnosis, so it must not be flattened into something generic.
        assert_eq!(Error::from_code(-9999), Error::Platform(-9999));
    }

    #[test]
    fn missing_covers_both_ways_a_file_can_be_absent() {
        assert!(Error::NotFound.is_missing());
        assert!(Error::PathNotFound.is_missing());
        assert!(!Error::AccessDenied.is_missing(), "denied is not the same as absent");
        assert!(!Error::Platform(-7).is_missing());
    }
}
