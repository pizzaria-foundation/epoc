//! Boot order and restart policy for S60 — the part the platform does not have.
//!
//! The S60 Startup List Management API can register an executable to run at boot and nothing more:
//! `STARTUP_ITEM_INFO` (`sdk/epoc32/include/startupitem.rh`) carries a path and a `recovery` field
//! whose only legal value is `EStartupItemExPolicyNone`, "do nothing"
//! (`sdk/epoc32/include/startupitem.hrh`). There is no order, phase or priority member, and the
//! import file is consumed by the Software Installer once, at install time — so there is no
//! platform setting to expose and no runtime edit that would take effect.
//!
//! So the mechanism is ours. One supervisor (`apps/bootd`) is the single registered startup item;
//! it reads a [`BootConfig`], launches the listed apps in order, watches them, and restarts them
//! according to their [`Policy`]. This crate is the part that has no device in it: the on-disk
//! codec, the last-boot report, and the whole supervisor as a pure state machine.
//!
//! The two binaries that use this crate — `apps/bootd`, the supervisor, and `apps/bootctl`, the
//! editor — live in the **home** repository, not here. The rule is I/O: this crate has none, and a
//! format plus a state machine is a library. Reading the disk and asking the OS who is alive is the
//! system being built, and the system lives with the home screen it supervises.
//!
//! - [`config`] — what to launch, in what order, with which policy.
//! - [`status`] — what happened last boot, for `apps/bootctl` to display.
//! - [`supervise`] — the state machine `apps/bootd` executes.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod config;
pub mod crc;
pub mod status;
pub mod supervise;

pub use config::{BootConfig, DecodeError, Entry, Policy};
pub use status::{BootStatus, EntryStatus, Mode, State};
pub use supervise::{Action, Supervisor};

/// `apps/bootctl`'s UID3 — the GUI editor. Refused as a supervised entry.
pub const BOOTCTL_UID: u32 = 0xE0AA_0010;
/// `apps/bootd`'s UID3 — the supervisor itself. Refused as a supervised entry.
pub const BOOTD_UID: u32 = 0xE0AA_0011;

/// Where the two files live. `C:\Data` is outside `\sys`, `\resource` and `\private`, so both
/// binaries reach it with no capability at all — the same reason `symbian::log` already writes
/// `C:\Data\logs_<app>.txt` from apps declaring `CAPABILITIES=none`. A private cage would force
/// `AllFiles` on the editor, and a protected capability has broken an install in this repo before.
pub const CONFIG_PATH: &str = "C:\\Data\\bootd\\boot.cfg";
pub const STATUS_PATH: &str = "C:\\Data\\bootd\\boot.status";
/// One byte: how many boots in a row failed to settle. Cleared after a healthy one.
pub const COUNT_PATH: &str = "C:\\Data\\bootd\\boot.count";
/// The directory both live in; bootd creates it before its first write.
pub const DATA_DIR: &str = "C:\\Data\\bootd";

/// The Software Installer's start-up import directory, and the file bootd must have in it to be
/// launched at boot. `101f875a` is the SWI UI's SID; the file is named for the *package's* UID3,
/// and the executable it starts is read from inside the resource — which is why a package can
/// register a binary other than its own.
///
/// bootd writes this itself, from [`STARTUP_SOURCE`]. It is supposed to be the installer's job and
/// on the target handset it is not done: the package installs with the resource in it and nothing
/// runs at boot. Placing it needs `AllFiles`, since it is another application's private cage.
pub const STARTUP_IMPORT_DIR: &str = "C:\\private\\101f875a\\import";
pub const STARTUP_IMPORT_PATH: &str = "C:\\private\\101f875a\\import\\[E0AA0010].rsc";
/// The same compiled resource, shipped as plain data by bootctl's package so bootd has the bytes
/// without embedding a build artefact in its own image.
pub const STARTUP_SOURCE: &str = "C:\\Data\\bootd\\startup_item.rsc";

/// Unsettled boots in a row before bootd launches nothing and waits to be told why.
pub const SAFE_MODE_STRIKES: u8 = 3;

/// A supervised app's way of saying "I am going down on purpose; do not put me back yet".
///
/// The case it exists for is installing a new build over a running one. The Software Installer
/// stops the old process so it can replace `\\sys\\bin\\<app>.exe`; the supervisor, whose entire
/// job is to notice that death, would put the app straight back and pin the file the installer is
/// holding open. The install then fails, and the error the user reads is "file in use" — which
/// names the wrong culprit and is unfixable from the UI.
///
/// Deliberately a file and not the installer's process UID. The UID would be a guess about a 2009
/// ROM, and a guess that is wrong fails silently in the direction that breaks installs. A file
/// written by our own app on its way out is a fact we control, and any app we supervise can use it.
///
/// It is a floor, not a lock: the supervisor defers restarts while it is in force, and resumes on
/// the first poll after it expires. Nothing is lost if the writer dies before deleting it.
pub const HOLD_PATH: &str = "C:\\Data\\bootd\\hold";

/// How long a hold lasts. Long enough for the installer to close the app, swap the file and finish;
/// short enough that a user who closed the home screen on purpose gets it back within a minute
/// rather than never — which on a phone whose home screen is the product is the right default in
/// both directions.
pub const HOLD_SECS: i64 = 45;

/// The bytes an app writes to [`HOLD_PATH`]: the Unix second the hold expires, little-endian.
pub fn hold_blob(now_s: i64) -> alloc::vec::Vec<u8> {
    alloc::vec::Vec::from(now_s.saturating_add(HOLD_SECS).to_le_bytes())
}

/// Whether a hold read from [`HOLD_PATH`] is still in force at `now_s`.
///
/// Refuses a hold that expires more than an hour out. The handset's clock has been months wrong in
/// this repo before, and a hold written under a bad clock could otherwise suppress every restart
/// for years — the supervisor failing open is the only acceptable direction here.
pub fn hold_active(bytes: &[u8], now_s: i64) -> bool {
    if bytes.len() < 8 {
        return false;
    }
    let mut raw = [0u8; 8];
    raw.copy_from_slice(&bytes[..8]);
    let until = i64::from_le_bytes(raw);
    until > now_s && until.saturating_sub(now_s) <= 3_600
}

#[cfg(test)]
mod hold_tests {
    use super::*;

    fn blob(now: i64) -> alloc::vec::Vec<u8> {
        hold_blob(now)
    }

    #[test]
    fn a_fresh_hold_holds_and_an_expired_one_does_not() {
        let b = blob(1_000);
        assert!(hold_active(&b, 1_000));
        assert!(hold_active(&b, 1_000 + HOLD_SECS - 1));
        assert!(!hold_active(&b, 1_000 + HOLD_SECS));
    }

    #[test]
    fn a_hold_from_a_wrong_clock_fails_open_rather_than_silencing_the_supervisor() {
        let far = 4_000_000_000i64.to_le_bytes();
        assert!(!hold_active(&far, 1_000), "a hold decades out is refused, not obeyed");
    }

    #[test]
    fn a_truncated_or_missing_hold_is_no_hold() {
        assert!(!hold_active(&[], 1_000));
        assert!(!hold_active(&[1, 2, 3], 1_000));
    }
}
