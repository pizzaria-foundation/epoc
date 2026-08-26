//! Which version of this program is running — the one fact the platform will not tell anybody.
//!
//! `RApaLsSession` hands out a UID and a caption. The SIS registry knows versions and is behind an
//! API this SDK does not ship. So an application that wants to be *updatable* has to say what it is
//! itself, and that is the whole of this module: one small file, written once at start-up, naming
//! the version and the instant it ran.
//!
//! ```ignore
//! fn main() {
//!     symbian::pkg::stamp();   // before anything that can fail
//!     …
//! }
//! ```
//!
//! It looks like logging and it is not. The boot manager's updater
//! (`symbian_bootcfg::update`) commits a new version **only** when this file names it, because a
//! `.sis` that installs cleanly and a program that then refuses to start are the same event to
//! every API within reach. A launch that AppArc accepted is not a program that worked; a stamp on
//! disk is. Without it there is no way to roll an update back for the right reason, and the
//! difference between an update system and a hopeful one is exactly that.
//!
//! The instant is as load-bearing as the version. The stamp of the *old* build sits on disk from
//! the last time it started, so "the file says 0.1.0" means nothing until you know whether it was
//! written before or after the install.
//!
//! ## Where the numbers come from
//!
//! `tools/symbuild` passes `SYMBIAN_APP_UID3` and `SYMBIAN_APP_VERSION` from `app.conf` into the
//! build, so [`stamp`] takes no arguments and cannot disagree with the package that installed it.
//! Built any other way — `cargo test` on the host, a crate compiled outside `symbuild` — the
//! variables are absent, [`self_version`] is `None`, and [`stamp`] is a no-op that returns
//! `KErrNotSupported`. An app that is not built as a package has no package version, and
//! inventing one would be the lie this file exists to avoid.

use alloc::string::String;
use alloc::vec::Vec;

use crate::error::{Error, Result};
use symbian_sys as sys;
use crate::fs::{self, Fs, ShimFs, Utf16Path};

/// One file per application UID3: `C:\Data\bootd\ver\E0AA0000`.
///
/// Under `C:\Data` with everything else the boot manager owns, for the reason stated there — it is
/// outside `\sys`, `\resource` and `\private`, so a reader and a writer both need no capability at
/// all. A stamp inside an application's private cage would be unreadable by the supervisor that has
/// to check it, which is the one thing it is for.
pub const VER_DIR: &str = "C:\\Data\\bootd\\ver";

/// `b"BTVR"` little-endian, the UID, the three version numbers, and the instant. 24 bytes.
const MAGIC: u32 = 0x5256_5442;
const BLOB_LEN: usize = 24;

/// A package version: the three numbers `app.conf` writes as `VERSION=0,1,0`.
///
/// Lives here rather than beside the update machinery because it is the identity every managed
/// application shares, and the crate that owns the identity has to be one everybody can depend on.
/// `symbian_bootcfg` re-exports it, so there is exactly one of these in the tree.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct Version {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
}

impl Version {
    pub const fn new(major: u16, minor: u16, patch: u16) -> Self {
        Self { major, minor, patch }
    }

    /// Parse `0.2.0`, or `0,2,0`, or `v0.2.0`.
    ///
    /// The comma form is what `app.conf` uses, so a version can be pasted from one to the other
    /// without a silent rejection. The `v` prefix is what a git tag looks like — GitHub releases are
    /// tagged `v0.2.0` far more often than `0.2.0`, and a package manager that refused those would
    /// refuse most of the world. Obtainium takes the tag as the version for the same reason.
    ///
    /// Exactly three numbers. Two is not "patch 0": a truncated version string is far more likely
    /// to be a corrupted line than an abbreviation, and guessing turns a bad index into a bad
    /// install.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let s = s.strip_prefix('v').or_else(|| s.strip_prefix('V')).unwrap_or(s);
        let mut it = s.split(['.', ',']);
        let mut next = || it.next().and_then(|f| f.trim().parse::<u16>().ok());
        let (major, minor, patch) = (next()?, next()?, next()?);
        if it.next().is_some() {
            return None;
        }
        Some(Self { major, minor, patch })
    }
}

impl core::fmt::Display for Version {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// What a stamp file says: which version ran, and when it started.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Stamp {
    pub uid3: u32,
    pub version: Version,
    /// Unix seconds at the moment the application started.
    pub at_s: i64,
}

impl Stamp {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(BLOB_LEN);
        out.extend_from_slice(&MAGIC.to_le_bytes());
        out.extend_from_slice(&self.uid3.to_le_bytes());
        out.extend_from_slice(&self.version.major.to_le_bytes());
        out.extend_from_slice(&self.version.minor.to_le_bytes());
        out.extend_from_slice(&self.version.patch.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&self.at_s.to_le_bytes());
        out
    }

    /// `None` for anything that is not a stamp. No CRC here, unlike the boot manager's own files:
    /// this blob is 24 bytes written in one call, and a reader that cannot make sense of it treats
    /// that exactly as it treats an absent file — *no evidence* — which is already the safe
    /// direction. A refused stamp costs an update that rolls back; it never costs a wrong commit.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < BLOB_LEN {
            return None;
        }
        if u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) != MAGIC {
            return None;
        }
        let mut at = [0u8; 8];
        at.copy_from_slice(&bytes[16..24]);
        Some(Self {
            uid3: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            version: Version::new(
                u16::from_le_bytes([bytes[8], bytes[9]]),
                u16::from_le_bytes([bytes[10], bytes[11]]),
                u16::from_le_bytes([bytes[12], bytes[13]]),
            ),
            at_s: i64::from_le_bytes(at),
        })
    }
}

/// This application's UID3, as `app.conf` declared it, or `None` outside a `symbuild` build.
pub fn self_uid3() -> Option<u32> {
    let raw = option_env!("SYMBIAN_APP_UID3")?.trim();
    let hex = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X"))?;
    u32::from_str_radix(hex, 16).ok()
}

/// This application's version, as `app.conf` declared it, or `None` outside a `symbuild` build.
pub fn self_version() -> Option<Version> {
    Version::parse(option_env!("SYMBIAN_APP_VERSION")?)
}

/// The stamp file for one application.
pub fn path_of(uid3: u32) -> Result<Utf16Path> {
    Utf16Path::new(&alloc::format!("{VER_DIR}\\{uid3:08X}"))
}

/// Record that this version of this application is running, now.
///
/// Best effort and cheap — one directory create and one 24-byte atomic write — but not silent: the
/// error is returned so a caller who cares can log it. Most callers do not care and should not,
/// because an application whose start-up fails on a stamp it could not write is worse than an
/// application that cannot be updated automatically.
///
/// `KErrNotSupported` when the build carries no package identity; see the module docs.
pub fn stamp() -> Result<()> {
    let (uid3, version) = self_uid3().zip(self_version()).ok_or(Error::Platform(sys::SHIM_ERR_NOT_SUPPORTED))?;
    stamp_as(&mut ShimFs, uid3, version, crate::unix_time())
}

/// [`stamp`] with everything named explicitly, for the tests and for a caller that stamps on behalf
/// of something else.
pub fn stamp_as<F: Fs>(fs_: &mut F, uid3: u32, version: Version, now_s: i64) -> Result<()> {
    if let Ok(dir) = Utf16Path::new(VER_DIR) {
        let _ = fs_.mkdir(dir.as_units());
    }
    let path = path_of(uid3)?;
    fs::write_atomic(fs_, &path, &Stamp { uid3, version, at_s: now_s }.encode())
}

/// Read back what `uid3` last recorded, or `None` if it never has.
pub fn stamped(uid3: u32) -> Option<Stamp> {
    stamped_in(&mut ShimFs, uid3)
}

/// [`stamped`] against a given filesystem.
pub fn stamped_in<F: Fs>(fs_: &mut F, uid3: u32) -> Option<Stamp> {
    let bytes = fs::read(fs_, &path_of(uid3).ok()?).ok().flatten()?;
    // A stamp naming a different UID is somebody else's file under our name, which is a filesystem
    // we should not draw conclusions from.
    Stamp::decode(&bytes).filter(|s| s.uid3 == uid3)
}

/// The stamp as `apps/bootctl` shows it: `0.2.0`, or `unknown`.
pub fn describe(stamp: Option<Stamp>) -> String {
    match stamp {
        Some(s) => alloc::format!("{}", s.version),
        None => String::from("unknown"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::MemFs;

    #[test]
    fn a_version_parses_from_both_spellings_and_refuses_a_truncated_one() {
        assert_eq!(Version::parse("0.2.0"), Some(Version::new(0, 2, 0)));
        assert_eq!(Version::parse("0,2,0"), Some(Version::new(0, 2, 0)));
        assert_eq!(Version::parse("0.2"), None);
        assert_eq!(Version::parse("x.y.z"), None);
        // A git tag, which is what a GitHub release is named by.
        assert_eq!(Version::parse("v0.2.0"), Some(Version::new(0, 2, 0)));
        assert_eq!(Version::parse(" V1.98.0 "), Some(Version::new(1, 98, 0)));
        assert_eq!(Version::parse("version-1.0.0"), None, "a prefix is `v`, not any word");
    }

    #[test]
    fn a_stamp_round_trips() {
        let s = Stamp { uid3: 0xE0AA_0000, version: Version::new(0, 2, 0), at_s: 1_700_000_000 };
        assert_eq!(Stamp::decode(&s.encode()), Some(s));
    }

    #[test]
    fn anything_that_is_not_a_stamp_is_no_evidence_rather_than_a_guess() {
        assert_eq!(Stamp::decode(&[]), None);
        assert_eq!(Stamp::decode(&[0u8; BLOB_LEN]), None, "no magic");
        let mut short = Stamp {
            uid3: 1,
            version: Version::new(1, 0, 0),
            at_s: 0,
        }
        .encode();
        short.truncate(BLOB_LEN - 1);
        assert_eq!(Stamp::decode(&short), None);
    }

    #[test]
    fn writing_then_reading_gives_the_version_back() {
        let mut fs_ = MemFs::new();
        stamp_as(&mut fs_, 0xE0AA_0000, Version::new(0, 2, 0), 42).expect("write");
        let got = stamped_in(&mut fs_, 0xE0AA_0000).expect("read");
        assert_eq!(got.version, Version::new(0, 2, 0));
        assert_eq!(got.at_s, 42);
    }

    #[test]
    fn a_stamp_under_someone_elses_name_is_not_read_as_theirs() {
        let mut fs_ = MemFs::new();
        // Written for one UID, then asked for under another: on a real filesystem this is a stale
        // or hand-copied file, and taking its word for it would commit the wrong version.
        stamp_as(&mut fs_, 0xE0AA_0000, Version::new(0, 2, 0), 42).expect("write");
        let path = path_of(0xE0AA_0000).unwrap();
        let bytes = fs::read(&mut fs_, &path).unwrap().unwrap();
        let other = path_of(0xE0AA_0020).unwrap();
        fs::write_atomic(&mut fs_, &other, &bytes).unwrap();
        assert_eq!(stamped_in(&mut fs_, 0xE0AA_0020), None);
    }

    #[test]
    fn an_app_that_never_ran_has_no_stamp() {
        let mut fs_ = MemFs::new();
        assert_eq!(stamped_in(&mut fs_, 0xE0AA_0000), None);
        assert_eq!(describe(None), "unknown");
    }

    #[test]
    fn a_build_outside_symbuild_has_no_package_identity_and_says_so() {
        // These tests are exactly that build, which is why this can be asserted rather than mocked.
        assert_eq!(self_uid3(), None);
        assert_eq!(self_version(), None);
        assert!(stamp().unwrap_err().is_unsupported());
    }
}
