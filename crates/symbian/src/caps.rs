//! Platform security, asked twice.
//!
//! # Why one question is not enough
//!
//! [`has`] wraps `RProcess::HasCapability`, which reports what the *loader granted* this
//! image. On a handset with a patched installserver that is worth knowing on its own: it
//! says whether the patch lifted the ceiling or merely stopped refusing the package.
//!
//! But it is not the question a caller usually means. "Can this process read another
//! application's data cage" is answered by *trying it* and recording the error, and the two
//! answers can disagree. A kernel that says the capability is held while the operation
//! still returns `KErrPermissionDenied` means something other than platform security is
//! refusing — and that is a fact neither answer produces alone.
//!
//! So the table below pairs each capability with a path that exercises it, and a probe is
//! expected to report both columns. See [`ATTEMPTS`].

use symbian_sys as sys;

use crate::error::{Error, Result};
use crate::fs::Utf16Path;

/// A `TCapability`, with the name to print it under.
///
/// Values are the enum's ordinals from `e32capability.h`, which are ABI: they are compiled
/// into the capability bitmask of every image ever built, so they cannot be renumbered.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Cap {
    pub id: i32,
    pub name: &'static str,
}

const fn c(id: i32, name: &'static str) -> Cap {
    Cap { id, name }
}

/// Every capability Symbian 9.x defines, in ordinal order.
///
/// All twenty, not only the interesting ones. The point of the sweep is to find out what a
/// ROM patch actually grants, and a list filtered by what somebody expected would answer
/// the expectation rather than the question.
pub const ALL: &[Cap] = &[
    c(0, "TCB"),
    c(1, "CommDD"),
    c(2, "PowerMgmt"),
    c(3, "MultimediaDD"),
    c(4, "ReadDeviceData"),
    c(5, "WriteDeviceData"),
    c(6, "DRM"),
    c(7, "TrustedUI"),
    c(8, "ProtServ"),
    c(9, "DiskAdmin"),
    c(10, "NetworkControl"),
    c(11, "AllFiles"),
    c(12, "SwEvent"),
    c(13, "NetworkServices"),
    c(14, "LocalServices"),
    c(15, "ReadUserData"),
    c(16, "WriteUserData"),
    c(17, "Location"),
    c(18, "SurroundingsDD"),
    c(19, "UserEnvironment"),
];

/// A filesystem path whose accessibility depends on a capability, and the capability it
/// depends on.
///
/// `RFs::Att` is the operation: it reads an attribute word and creates, modifies and
/// destroys nothing, so a probe can run the whole table against a live handset without
/// consequences. The value it returns is uninteresting; the *error* is the measurement.
pub struct Attempt {
    pub cap: &'static str,
    pub path: &'static str,
    /// What the path is, for the report — a reader should not have to know why
    /// `\sys\bin` is privileged.
    pub what: &'static str,
}

/// The paired half of [`ALL`]: capabilities that can be checked by touching something.
///
/// Not every capability has an entry. `NetworkServices` needs a radio, `DRM` needs
/// protected content — attempts that are neither cheap nor side-effect-free, and a
/// reconnaissance run should not be opening connections to prove a bit. Those are reported
/// from [`has`] alone, and the report says so rather than leaving a blank.
pub const ATTEMPTS: &[Attempt] = &[
    Attempt {
        cap: "AllFiles",
        path: "C:\\sys\\bin\\",
        what: "the executable directory, unreadable without AllFiles",
    },
    Attempt {
        cap: "AllFiles",
        path: "C:\\private\\10003a3f\\",
        what: "AppArc's data cage — another application's private directory",
    },
    Attempt {
        cap: "TCB",
        path: "Z:\\sys\\bin\\",
        what: "the ROM's executable directory",
    },
    Attempt {
        cap: "WriteDeviceData",
        path: "C:\\resource\\",
        what: "the shared resource directory, writable only with WriteDeviceData",
    },
    Attempt {
        cap: "ReadUserData",
        path: "C:\\Data\\",
        what: "the user's own files (a control: this should succeed everywhere)",
    },
];

/// Whether the kernel says this process holds `cap`.
///
/// Reports what was *granted*, which is only half the question — see the module note.
pub fn has(cap: i32) -> Result<bool> {
    // SAFETY: no pointers; the shim returns 1, 0 or a negative error.
    let rc = unsafe { sys::shim_has_capability(cap) };
    if rc < 0 {
        return Err(Error::from_code(rc));
    }
    Ok(rc == 1)
}

/// Attempt `RFs::Att` on a path. The error is the result.
///
/// `Ok(attributes)` means the path was reachable. [`Error::AccessDenied`] means platform
/// security refused, which is the answer the capability table is after; [`Error::NotFound`]
/// means the path is simply absent on this handset, which is a different finding and must
/// not be read as a refusal.
pub fn attempt(path: &str) -> Result<u32> {
    let p = Utf16Path::new(path)?;
    let units = p.as_units();
    let mut out = 0u32;
    // SAFETY: `units` is valid for its length and only read; `out` is a live local.
    Error::check(unsafe { sys::shim_fs_att(units.as_ptr(), units.len() as i32, &mut out) })?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeSet;

    #[test]
    fn every_capability_id_appears_once() {
        let mut seen = BTreeSet::new();
        for cap in ALL {
            assert!(seen.insert(cap.id), "duplicate capability id {}", cap.id);
        }
    }

    /// Symbian 9.x has exactly twenty, and they are contiguous from zero. A gap here would
    /// mean the table was transcribed with an omission, and the missing one would silently
    /// never be asked about.
    #[test]
    fn the_capability_set_is_complete_and_contiguous() {
        assert_eq!(ALL.len(), 20);
        for (i, cap) in ALL.iter().enumerate() {
            assert_eq!(cap.id, i as i32, "{} is out of order", cap.name);
        }
    }

    /// An attempt naming a capability the table does not define would print a row nothing
    /// could be compared against.
    #[test]
    fn every_attempt_names_a_real_capability() {
        for at in ATTEMPTS {
            assert!(ALL.iter().any(|c| c.name == at.cap), "unknown capability {}", at.cap);
            assert!(!at.what.is_empty());
        }
    }

    /// The control row matters as much as the rest: if reading `C:\Data\` fails too, the
    /// probe is broken rather than the handset being locked down — the same reasoning that
    /// puts euser.dll and avkon.dll in the DLL sweep.
    #[test]
    fn there_is_a_control_that_should_always_succeed() {
        assert!(ATTEMPTS.iter().any(|a| a.path == "C:\\Data\\"));
    }
}
