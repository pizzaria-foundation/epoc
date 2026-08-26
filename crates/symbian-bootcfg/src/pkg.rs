//! What is installed, what is available, and how the two are told apart.
//!
//! The platform cannot answer either half. `RApaLsSession` hands out a UID and a caption and
//! nothing else — `symbian::apps::AppInfo` has no version field because there is none to copy — and
//! the SIS registry that does know is behind an API this SDK does not ship. So the answer is ours,
//! and it is two files:
//!
//! - **[`PkgDb`]** (`C:\Data\bootd\pkg.db`) — the packages we manage, and the version each one is
//!   believed to be at. `apps/bootctl` owns the list; `apps/bootd` writes only [`ManagedPkg::installed`],
//!   and only after a version has proved itself.
//! - **[`Candidate`]** — a `.sis` sitting in a folder, described by *itself*. Its UID, version and
//!   name are read out of the package by [`crate::sis`], never out of its file name. There used to
//!   be a sidecar `index.txt` carrying those three facts, and it was deleted the day the file could
//!   be read: two sources for one fact is a disagreement waiting to happen, and the one that cannot
//!   be renamed or forgotten wins. `tools/symbuild` still writes a `.index` line beside each build
//!   — as a release record, like a `SHA256SUMS`, not as something the phone consults.
//!
//! The belief in "believed to be at" is load-bearing and deliberately weak. A user who installs a
//! `.sis` from the File manager leaves this file stale, and no amount of care here prevents that.
//! What closes the gap is the stamp an app writes when it *runs* — `symbian::pkg::stamp`, read back
//! by [`crate::update`] — because a version that ran is a fact, and a version in a database is a
//! record of an intention.

use alloc::string::String;
use alloc::vec::Vec;

use crate::config::DecodeError;
use crate::crc::crc16;
use crate::{BOOTCTL_UID, BOOTD_UID};

/// `b"BTPK"` read as a little-endian u32.
pub const MAGIC: u32 = 0x4B50_5442;
/// The only version this codec writes, and the highest it will read.
pub const VERSION: u16 = 1;
/// Bytes per package record in version 1.
///
/// UID, flags, the three version numbers, the name's place in the string blob, and the 32-byte
/// digest of the package that last committed.
pub const ENTRY_SIZE: u16 = 52;
/// Fixed header size, versions 1 and up.
pub const HEADER_SIZE: usize = 16;
/// Refused above this. This is a phone, not a package archive.
pub const MAX_PKGS: usize = 32;

/// The extensions the scanner accepts, lowercased. `.sisx` is a signed `.sis` and the installer
/// takes both by the same route, so both are candidates.
pub const SIS_EXTENSIONS: [&str; 2] = ["sis", "sisx"];

/// The version type, and the stamp an application writes when it runs, both come from the SDK.
///
/// They live in `symbian::pkg` and not here because every managed application writes a stamp and
/// only the boot manager reads one — the crate that owns an identity has to be the one everybody
/// can depend on, and this crate depends on that one rather than the other way round. Re-exported
/// so a reader of this module does not have to know that.
pub use symbian::pkg::{Stamp, Version};

/// One package we look after.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ManagedPkg {
    /// The UID3 of the application the package installs — the same identity the boot list uses, so
    /// a package and the entry that supervises it are the same thing to everyone who reads either.
    pub uid3: u32,
    /// The caption shown on the row.
    pub name: String,
    /// The version believed to be installed, or `None` for a package we manage but have never seen
    /// prove a version. `None` is not an error — it is the honest state before the first stamped
    /// launch, and the UI says "unknown" rather than inventing a zero.
    pub installed: Option<Version>,
    /// SHA-256 of the `.sis` that last committed for this package.
    ///
    /// The second axis, and the reason it exists: a version number is a claim a developer types,
    /// and during development the same `0.2.0` is rebuilt twenty times. Comparing versions alone,
    /// the phone would say "up to date" and refuse to install any of them. Comparing bytes, it can
    /// say *this is a different build of the same version* — which is the truth, and worth telling
    /// the user rather than hiding.
    ///
    /// `None` for a package installed before this was recorded, or by hand from the File manager.
    /// Unknown means "cannot tell", and the UI says so rather than claiming a difference it has not
    /// established.
    pub installed_sha: Option<[u8; 32]>,
    /// Seconds to wait after this package installs before its application is reopened, or **0 for
    /// not reopening it at all** — which is the default.
    ///
    /// Off by default because installing something is not a reason to start it. Most packages should
    /// be left exactly where the installer left them, and anything in the boot list comes back on its
    /// own anyway: a critical entry is watched at 5..30 s and restarted by the supervisor the moment
    /// the update's exemption lifts. Reopening is for the case where somebody wants it *now*.
    ///
    /// Per package, and it was one global number first — which was wrong twice over: wrong in scope,
    /// because one number cannot be right for both a home screen and a probe, and wrong in default,
    /// because the default was to launch things.
    pub settle_s: u16,
    /// This application writes a version stamp, so an update of it can be *proved*.
    ///
    /// Re-derived on every load from whether `symbian::pkg::stamped` finds a file — never inferred
    /// from [`ManagedPkg::installed`], and that distinction is the whole reason the field exists. A
    /// commit writes a version down whichever promise it was held to, so "we know its version" and
    /// "it reports its version" are *not* the same fact. Reading the first as the second held a
    /// browser to a stamp it never writes: it installed, was launched, never stamped, and the update
    /// rolled back a working install. Measured on the handset, on the second install of the same
    /// package — the first, as a new package, went through fine.
    ///
    /// Persisted with the rest so `xxd` on the file says what the phone believes, but never *read*
    /// as authority: the stamp on disk is.
    pub stamps: bool,
    /// Held at the installed version: candidates are listed and never offered. For the one package
    /// somebody is mid-debugging and does not want replaced by a card they forgot was in the phone.
    pub pinned: bool,
}

impl ManagedPkg {
    pub fn new(uid3: u32, name: String) -> Self {
        Self {
            uid3,
            name,
            installed: None,
            installed_sha: None,
            settle_s: 0,
            stamps: false,
            pinned: false,
        }
    }

    /// The package that carries the boot manager itself.
    ///
    /// Refused as an update target everywhere, and this is the method that says so once. Replacing
    /// `bootctl.exe`/`bootd.exe` means the installer stops the process that is conducting the
    /// replacement — there is nobody left to prove the new version, roll back, or bring the home
    /// screen up afterwards. `supervise.rs` already refuses to supervise these two UIDs for the
    /// neighbouring reason; this is the same rule one layer up.
    pub fn is_self(&self) -> bool {
        self.uid3 == BOOTCTL_UID || self.uid3 == BOOTD_UID
    }
}

/// Every package we manage, and what we believe about it.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct PkgDb {
    pub pkgs: Vec<ManagedPkg>,

    /// Seconds between the Pkgs tab re-reading itself while an update is in flight, or 0 for off.
    ///
    /// Only while one is running: a timer that ticks when nothing is happening is a phone that wakes
    /// up for no reason, and this screen has nothing to say between updates. Off is a real answer and
    /// the default one — the Refresh softkey and returning to the foreground both already work — so
    /// this is for somebody who would rather watch it happen.
    pub refresh_s: u16,
}

/// The wait offered when somebody switches reopening *on* without saying how long.
///
/// It is a floor, and the floor exists because launching into a running install is what the hold was
/// invented to prevent: the file being written gets pinned and the *next* install fails with "file
/// in use", naming the wrong culprit. 45 s covers a person reading two dialogs and a certificate
/// prompt on a phone from 2009.
pub const DEFAULT_SETTLE_S: u16 = 45;
/// The range the boot manager will accept. The bottom is not zero on purpose — an install that has
/// not been given a moment is the failure this number exists to avoid, and a home screen worth
/// waiting five seconds for is worth waiting five seconds for.
pub const MIN_SETTLE_S: u16 = 5;
pub const MAX_SETTLE_S: u16 = 120;
/// Range for [`PkgDb::refresh_s`]. Zero is off; below two seconds is a rescan of the update folder
/// more often than a scan takes.
pub const MIN_REFRESH_S: u16 = 2;
pub const MAX_REFRESH_S: u16 = 60;

impl PkgDb {
    /// The auto-refresh interval in milliseconds, or `None` when it is off.
    pub fn refresh_ms(&self) -> Option<u32> {
        match self.refresh_s {
            0 => None,
            n => Some(n.clamp(MIN_REFRESH_S, MAX_REFRESH_S) as u32 * 1_000),
        }
    }

    /// How long to wait before reopening `uid3`, or `None` for not reopening it.
    ///
    /// `None` is the default and the answer for anything nobody has asked about, including a package
    /// we do not manage yet.
    pub fn reopen(&self, uid3: u32) -> Option<u16> {
        match self.get(uid3).map(|p| p.settle_s).unwrap_or(0) {
            0 => None,
            n => Some(n.clamp(MIN_SETTLE_S, MAX_SETTLE_S)),
        }
    }

    pub fn get(&self, uid3: u32) -> Option<&ManagedPkg> {
        self.pkgs.iter().find(|p| p.uid3 == uid3)
    }

    pub fn get_mut(&mut self, uid3: u32) -> Option<&mut ManagedPkg> {
        self.pkgs.iter_mut().find(|p| p.uid3 == uid3)
    }

    /// Add `pkg` if its UID is not already present, and say whether that changed anything. An
    /// existing row is left exactly as it is — the same rule as [`crate::BootConfig::ensure_home`],
    /// and for the same reason: what is on screen is somebody's answer.
    pub fn ensure(&mut self, pkg: ManagedPkg) -> bool {
        if self.pkgs.iter().any(|p| p.uid3 == pkg.uid3) || self.pkgs.len() >= MAX_PKGS {
            return false;
        }
        self.pkgs.push(pkg);
        true
    }

    /// What is on offer for `uid3`, and the candidate it would install.
    ///
    /// `None` when the package is unmanaged, pinned, self, or has nothing installable on offer.
    /// One implementation shared with [`rows`], so the screen and the installer can never disagree
    /// about what a row means.
    pub fn offer_for<'c>(&self, uid3: u32, cands: &'c [Candidate]) -> Option<(Offer, &'c Candidate)> {
        let pkg = self.get(uid3)?;
        if pkg.pinned || pkg.is_self() {
            return None;
        }
        let (_, c) = best_for(pkg, cands)?;
        let offer = decide(pkg, c);
        offer.installable().then_some((offer, c))
    }

    /// Encode to the on-disk blob: 16-byte header, `count` × 20-byte records, then a UTF-16LE
    /// string blob the records point into. Same shape as `config.rs`, on purpose — one layout to
    /// learn, and `xxd` on the phone reads both the same way.
    pub fn encode(&self) -> Vec<u8> {
        let count = self.pkgs.len().min(MAX_PKGS);
        let mut blob: Vec<u16> = Vec::new();
        let mut out = Vec::with_capacity(HEADER_SIZE + count * ENTRY_SIZE as usize);

        out.extend_from_slice(&MAGIC.to_le_bytes());
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&ENTRY_SIZE.to_le_bytes());
        out.extend_from_slice(&(count as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&self.refresh_s.to_le_bytes());
        // Written last, over the whole file with these two bytes zeroed.
        out.extend_from_slice(&0u16.to_le_bytes());

        for p in self.pkgs.iter().take(count) {
            let name = push_str(&mut blob, &p.name);

            let mut flags = 0u8;
            if p.installed.is_some() {
                flags |= 0x01;
            }
            if p.pinned {
                flags |= 0x02;
            }
            if p.installed_sha.is_some() {
                flags |= 0x04;
            }
            if p.stamps {
                flags |= 0x08;
            }
            // An absent version encodes as 0.0.0 with the flag clear, rather than a sentinel
            // version number. A sentinel is a number that compares, and one day something compares
            // it.
            let v = p.installed.unwrap_or_default();

            out.extend_from_slice(&p.uid3.to_le_bytes());
            out.push(flags);
            out.push(0);
            out.extend_from_slice(&v.major.to_le_bytes());
            out.extend_from_slice(&v.minor.to_le_bytes());
            out.extend_from_slice(&v.patch.to_le_bytes());
            out.extend_from_slice(&name.0.to_le_bytes());
            out.extend_from_slice(&name.1.to_le_bytes());
            out.extend_from_slice(&p.settle_s.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&p.installed_sha.unwrap_or([0u8; 32]));
        }

        for u in &blob {
            out.extend_from_slice(&u.to_le_bytes());
        }

        let crc = crc16(&out);
        out[14..16].copy_from_slice(&crc.to_le_bytes());
        out
    }

    /// Decode a blob written by [`PkgDb::encode`]. Every failure means "we know nothing about any
    /// package", which costs a rebuilt list — never a wrong install.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < HEADER_SIZE {
            return Err(DecodeError::Truncated);
        }
        if u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) != MAGIC {
            return Err(DecodeError::BadMagic);
        }
        let version = u16::from_le_bytes([bytes[4], bytes[5]]);
        if version > VERSION {
            return Err(DecodeError::BadVersion(version));
        }
        let entry_size = u16::from_le_bytes([bytes[6], bytes[7]]) as usize;
        if entry_size < ENTRY_SIZE as usize {
            return Err(DecodeError::BadLayout);
        }
        let count = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
        if count > MAX_PKGS {
            return Err(DecodeError::TooMany(count));
        }
        let table_end = HEADER_SIZE.checked_add(count * entry_size).ok_or(DecodeError::BadLayout)?;
        if bytes.len() < table_end {
            return Err(DecodeError::BadLayout);
        }

        let mut check = Vec::from(bytes);
        let stored = u16::from_le_bytes([bytes[14], bytes[15]]);
        check[14..16].copy_from_slice(&[0, 0]);
        if crc16(&check) != stored {
            return Err(DecodeError::BadCrc);
        }

        // The string blob is whatever follows the record table, read as UTF-16LE units. An odd
        // trailing byte is not a unit and is not half of one; it is a file that is not what it says
        // it is.
        let tail = &bytes[table_end..];
        if tail.len() % 2 != 0 {
            return Err(DecodeError::BadLayout);
        }
        let blob: Vec<u16> =
            tail.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();

        let mut pkgs = Vec::with_capacity(count);
        for i in 0..count {
            let r = &bytes[HEADER_SIZE + i * entry_size..];
            let uid3 = u32::from_le_bytes([r[0], r[1], r[2], r[3]]);
            let flags = r[4];
            let v = Version::new(
                u16::from_le_bytes([r[6], r[7]]),
                u16::from_le_bytes([r[8], r[9]]),
                u16::from_le_bytes([r[10], r[11]]),
            );
            let name =
                take_str(&blob, u16::from_le_bytes([r[12], r[13]]), u16::from_le_bytes([r[14], r[15]]))
                    .ok_or(DecodeError::BadLayout)?;
            let mut sha = [0u8; 32];
            sha.copy_from_slice(&r[20..52]);
            pkgs.push(ManagedPkg {
                uid3,
                name,
                installed: (flags & 0x01 != 0).then_some(v),
                settle_s: u16::from_le_bytes([r[16], r[17]]),
                installed_sha: (flags & 0x04 != 0).then_some(sha),
                stamps: flags & 0x08 != 0,
                pinned: flags & 0x02 != 0,
            });
        }
        Ok(Self { pkgs, refresh_s: u16::from_le_bytes([bytes[12], bytes[13]]) })
    }
}

fn push_str(blob: &mut Vec<u16>, s: &str) -> (u16, u16) {
    let units: Vec<u16> = s.encode_utf16().take(u16::MAX as usize).collect();
    let off = blob.len() as u16;
    let len = units.len() as u16;
    blob.extend_from_slice(&units);
    (off, len)
}

fn take_str(blob: &[u16], off: u16, len: u16) -> Option<String> {
    let (off, len) = (off as usize, len as usize);
    let slice = blob.get(off..off.checked_add(len)?)?;
    Some(String::from_utf16_lossy(slice))
}

/// One `.sis` we could install, as the file itself describes it.
///
/// Everything here except the digest comes out of [`crate::sis::parse`] — the UID, the version and
/// the name are read from inside the package, not from what somebody called the file. That is the
/// difference between a build being installable and a build having to be named correctly first.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Candidate {
    /// The directory it was found in, as a device path.
    pub dir: String,
    /// The file name inside `dir`. Used to open it and shown when nothing else identifies it — it
    /// has no part in deciding what the package *is*.
    pub file: String,
    /// The application this package installs, from the package.
    pub uid3: u32,
    pub version: Version,
    /// The package's own name.
    pub name: String,
    pub size: u64,
    /// SHA-256 of the whole file — **computed only when it decides something**.
    ///
    /// `None` is not "unverified". It means nobody needed to know: a candidate whose version is
    /// already higher than what is installed is an upgrade whatever its bytes are, and a package we
    /// have never seen is new whatever its bytes are. The digest only settles one question — *is
    /// this a different build of the version already installed* — so it is computed for exactly the
    /// candidates that ask it, which keeps opening the screen from reading every package in the
    /// folder end to end.
    pub sha256: Option<[u8; 32]>,
}

impl Candidate {
    /// The full device path, for the installer and for the copy into staging.
    pub fn path(&self) -> String {
        let mut p = String::from(self.dir.trim_end_matches('\\'));
        p.push('\\');
        p.push_str(&self.file);
        p
    }

    /// Whether the digest question needs answering for this candidate against `pkg` — i.e. whether
    /// the caller should spend a read hashing it.
    ///
    /// Exactly the equal-version case. Said here so the device side does not carry its own copy of
    /// the rule and drift from [`PkgDb::offer_for`].
    pub fn needs_digest(&self, pkg: &ManagedPkg) -> bool {
        pkg.installed == Some(self.version)
    }
}

/// What, if anything, a package's row is offering.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Offer {
    /// Nothing to do: up to date, pinned, or the boot manager's own package.
    None,
    /// A higher version is available.
    Upgrade,
    /// The same version, built differently — the digest does not match what last committed.
    ///
    /// Offered rather than hidden, because during development this is the ordinary case and the
    /// alternative is a phone that says "up to date" about a build that is not the one on the desk.
    /// The UI says *rebuild* rather than *update* so nobody reads it as a version change.
    Rebuild,
    /// A package for an application we do not manage yet. Installable, and flagged as new — there
    /// is no version to compare against and nothing to roll back to.
    New,
    /// An older version than what is installed. Shown so it is not invisible, never offered: going
    /// backwards is what the rollback is for, and it is the supervisor's decision rather than a
    /// row somebody can tap by accident.
    Older,
    /// The same version and the same bytes as what committed. Nothing to do, and we can say so
    /// with certainty rather than by assuming.
    Same,
    /// The same version, and whether the bytes differ is unknown — no digest was recorded for what
    /// is installed, or none was computed for this file. Offered, and labelled as the uncertainty
    /// it is: refusing would hide a real update, and claiming a difference would invent one.
    Unknown,
}

impl Offer {
    /// Whether this row can be installed from.
    pub fn installable(self) -> bool {
        matches!(self, Offer::Upgrade | Offer::Rebuild | Offer::New | Offer::Unknown)
    }
}

/// One line on the Pkgs tab: a package, what we believe about it, and what is on offer.
///
/// Built by [`rows`] from the database and the candidates together, because a row is not always a
/// managed package — a `.sis` for something we have never seen has to appear too, or the answer to
/// "why is my build not listed" is invisible.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Row {
    pub uid3: u32,
    pub name: String,
    pub installed: Option<Version>,
    pub offer: Offer,
    /// Index into the candidate slice `rows` was given, when there is one.
    pub cand: Option<usize>,
    pub pinned: bool,
    pub is_self: bool,
    /// Another package in the folder installs the *same* application UID3 under a different name.
    ///
    /// Shown rather than resolved, because it cannot be resolved from here: on the phone these two
    /// files overwrite each other, and which one is "right" is a question about somebody's
    /// `app.conf`, not about this screen. The first version of this code deduplicated by UID and one
    /// of the two silently vanished — which is how a copy-pasted UID3 stays undiscovered.
    pub collides: bool,
}

/// Every row the Pkgs tab shows: the managed packages first, in their stored order, then a row for
/// each candidate belonging to an application we do not manage.
pub fn rows(db: &PkgDb, cands: &[Candidate]) -> Vec<Row> {
    let mut out = Vec::new();
    for pkg in &db.pkgs {
        let best = best_for(pkg, cands);
        out.push(Row {
            uid3: pkg.uid3,
            name: pkg.name.clone(),
            installed: pkg.installed,
            offer: match best {
                _ if pkg.is_self() || pkg.pinned => Offer::None,
                Some((_, c)) => decide(pkg, c),
                None => Offer::None,
            },
            cand: best.map(|(i, _)| i),
            pinned: pkg.pinned,
            is_self: pkg.is_self(),
            collides: cands
                .iter()
                .filter(|c| c.uid3 == pkg.uid3)
                .any(|c| cands.iter().any(|o| o.uid3 == c.uid3 && o.name != c.name)),
        });
    }
    for (i, c) in cands.iter().enumerate() {
        if db.get(c.uid3).is_some() {
            continue;
        }
        // One row per *package*, keyed by UID and name together rather than by UID alone.
        //
        // Two files with the same UID and the same name are two builds of one thing, and one row is
        // right. Two files with the same UID and different names are two different programs that
        // have been given the same identity — which on this platform means installing either one
        // removes the other. Collapsing those into one row is how the second one becomes invisible,
        // and it happened: `domprobe.sis` and `gpsprobe.sis` both declared `0xE0DD00F8`, and only
        // domprobe was ever listed.
        if out.iter().any(|r| r.uid3 == c.uid3 && r.name == c.name) {
            continue;
        }
        let clash = cands.iter().any(|o| o.uid3 == c.uid3 && o.name != c.name);
        out.push(Row {
            uid3: c.uid3,
            name: if c.name.is_empty() { alloc::format!("[{:08X}]", c.uid3) } else { c.name.clone() },
            installed: None,
            offer: Offer::New,
            cand: Some(i),
            pinned: false,
            is_self: c.uid3 == BOOTCTL_UID || c.uid3 == BOOTD_UID,
            collides: clash,
        });
    }
    // The boot manager's own package can never be a target, wherever its row came from.
    for r in out.iter_mut().filter(|r| r.is_self) {
        r.offer = Offer::None;
    }
    out
}

/// The candidate to show for `pkg`: the highest version, and among equals the one that is not
/// byte-identical to what is installed — so a rebuild wins over a copy of what is already there.
fn best_for<'c>(pkg: &ManagedPkg, cands: &'c [Candidate]) -> Option<(usize, &'c Candidate)> {
    cands
        .iter()
        .enumerate()
        .filter(|(_, c)| c.uid3 == pkg.uid3)
        .max_by_key(|(_, c)| (c.version, differs(pkg, c)))
}

/// Whether this file's bytes are known to differ from what committed.
fn differs(pkg: &ManagedPkg, c: &Candidate) -> bool {
    match (pkg.installed_sha, c.sha256) {
        (Some(have), Some(found)) => have != found,
        _ => false,
    }
}

fn decide(pkg: &ManagedPkg, c: &Candidate) -> Offer {
    match pkg.installed {
        None => Offer::New,
        Some(have) if c.version > have => Offer::Upgrade,
        Some(have) if c.version < have => Offer::Older,
        // Same version. Now the bytes are the only thing that can distinguish them.
        _ => match (pkg.installed_sha, c.sha256) {
            (Some(a), Some(b)) if a != b => Offer::Rebuild,
            (Some(a), Some(b)) if a == b => Offer::Same,
            _ => Offer::Unknown,
        },
    }
}

/// Whether a file name is worth opening at all.
///
/// The only thing the *name* is still used for. Everything else about a package now comes from
/// inside it — see [`crate::sis`] — and this is just the cheap filter that avoids reading somebody's
/// photos looking for a version.
pub fn looks_like_package(file: &str) -> bool {
    file.rsplit_once('.')
        .is_some_and(|(_, ext)| SIS_EXTENSIONS.iter().any(|e| ext.eq_ignore_ascii_case(e)))
}

impl Candidate {
    /// Build a candidate from what the file said about itself.
    ///
    /// `info` is [`crate::sis::parse`] over the head of the file; `sha256` is `None` until something
    /// needs it — see the field's own note. The device side does the reading, this does the holding,
    /// and the rule for which is which is the same one the rest of this crate follows.
    pub fn from_sis(dir: &str, file: &str, size: u64, info: crate::sis::SisInfo) -> Self {
        Self {
            dir: String::from(dir),
            file: String::from(file),
            uid3: info.uid3,
            version: info.version,
            name: info.name,
            size,
            sha256: None,
        }
    }

    /// Record the digest, once something has needed it computed.
    pub fn with_digest(mut self, sha256: [u8; 32]) -> Self {
        self.sha256 = Some(sha256);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    const LAUNCHER: u32 = 0xE0AA_0000;
    const CAL: u32 = 0xE0AA_0020;

    fn sha(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    fn db() -> PkgDb {
        PkgDb {
            pkgs: vec![
                ManagedPkg {
                    uid3: LAUNCHER,
                    name: String::from("Launcher"),
                    installed: Some(Version::new(0, 1, 0)),
                    installed_sha: Some(sha(0xAA)),
                    settle_s: 0,
                    stamps: true,
                    pinned: false,
                },
                ManagedPkg::new(CAL, String::from("Calendário")),
            ],
            refresh_s: 0,
        }
    }

    /// A candidate the way the device builds one: identity from the package, digest only when asked.
    fn cand(uid3: u32, v: (u16, u16, u16), file: &str) -> Candidate {
        Candidate {
            dir: String::from("C:\\Data\\_app_install\\"),
            file: String::from(file),
            uid3,
            version: Version::new(v.0, v.1, v.2),
            name: String::from("launcher"),
            size: 320_484,
            sha256: None,
        }
    }

    #[test]
    fn reopening_is_off_by_default_per_package_and_round_trips() {
        // Off, because installing something is not a reason to start it — and anything in the boot
        // list comes back on its own anyway.
        let mut d = db();
        assert_eq!(d.reopen(LAUNCHER), None, "nobody asked");
        assert_eq!(d.reopen(0xDEAD_0000), None, "and a package we do not manage certainly did not");
        d.get_mut(LAUNCHER).unwrap().settle_s = 5;
        d.get_mut(CAL).unwrap().settle_s = 90;
        let back = PkgDb::decode(&d.encode()).expect("round trip");
        assert_eq!(back.reopen(LAUNCHER), Some(5));
        assert_eq!(back.reopen(CAL), Some(90), "and they do not share a number");
    }

    #[test]
    fn auto_refresh_is_off_by_default_and_round_trips() {
        let mut d = db();
        assert_eq!(d.refresh_ms(), None, "nothing ticks until somebody asks for it");
        d.refresh_s = 5;
        let back = PkgDb::decode(&d.encode()).expect("round trip");
        assert_eq!(back.refresh_ms(), Some(5_000));
        // And it cannot be set to something faster than a scan takes.
        let mut fast = db();
        fast.refresh_s = 1;
        assert_eq!(fast.refresh_ms(), Some(MIN_REFRESH_S as u32 * 1_000));
    }

    #[test]
    fn a_hand_edited_wait_is_clamped_rather_than_obeyed() {
        // Zero is the one value that would do real damage: launching into a running installer pins
        // the file being written, and the *next* install fails with "file in use".
        let mut d = db();
        d.get_mut(LAUNCHER).unwrap().settle_s = 1;
        assert_eq!(d.reopen(LAUNCHER), Some(MIN_SETTLE_S));
        d.get_mut(LAUNCHER).unwrap().settle_s = 9_000;
        assert_eq!(d.reopen(LAUNCHER), Some(MAX_SETTLE_S));
    }

    #[test]
    fn the_db_survives_a_round_trip_including_the_digest() {
        let d = db();
        let back = PkgDb::decode(&d.encode()).expect("round trip");
        assert_eq!(back, d);
        assert_eq!(back.pkgs[0].installed_sha, Some(sha(0xAA)));
        assert_eq!(back.pkgs[1].installed, None, "unknown stays unknown, not 0.0.0");
        assert_eq!(back.pkgs[1].installed_sha, None, "and so does an unknown digest");
    }

    #[test]
    fn one_flipped_byte_is_refused_rather_than_read() {
        let mut bytes = db().encode();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        assert_eq!(PkgDb::decode(&bytes), Err(DecodeError::BadCrc));
    }

    #[test]
    fn a_db_from_a_newer_bootctl_is_refused_not_half_read() {
        let mut bytes = db().encode();
        bytes[4..6].copy_from_slice(&(VERSION + 1).to_le_bytes());
        assert_eq!(PkgDb::decode(&bytes), Err(DecodeError::BadVersion(VERSION + 1)));
    }

    #[test]
    fn an_empty_db_is_a_valid_db() {
        let back = PkgDb::decode(&PkgDb::default().encode()).expect("round trip");
        assert!(back.pkgs.is_empty());
    }

    #[test]
    fn ensure_adds_once_and_never_overwrites() {
        let mut d = db();
        assert!(!d.ensure(ManagedPkg::new(LAUNCHER, String::from("x"))));
        assert_eq!(d.get(LAUNCHER).unwrap().name, "Launcher", "the existing row is the answer");
        assert!(d.ensure(ManagedPkg::new(0xE0AA_0030, String::from("ADBian"))));
    }

    #[test]
    fn knowing_a_version_is_not_the_same_as_being_told_it() {
        // The distinction that cost a rolled-back working install. A commit writes the version down
        // whichever promise it was held to, so a package installed once as `new` comes back with a
        // known version and still no way to prove anything about it.
        let mut d = db();
        d.ensure(ManagedPkg::new(0xE0DD_00F7, String::from("browser")));
        let browser = d.get_mut(0xE0DD_00F7).unwrap();
        browser.installed = Some(Version::new(0, 1, 0)); // written by a Launch-proof commit
        assert!(!browser.stamps, "and nothing about that says it reports a version");

        let back = PkgDb::decode(&d.encode()).expect("round trip");
        assert!(back.get(LAUNCHER).unwrap().stamps, "the one that does still does");
        assert!(!back.get(0xE0DD_00F7).unwrap().stamps);
    }

    #[test]
    fn a_higher_version_is_an_upgrade_whatever_the_file_is_called() {
        let d = db();
        // No version in the name, no index, nothing but the bytes of the package.
        let c = vec![cand(LAUNCHER, (0, 2, 0), "launcher.sisx")];
        let (offer, pick) = d.offer_for(LAUNCHER, &c).expect("0.2.0 beats 0.1.0");
        assert_eq!(offer, Offer::Upgrade);
        assert_eq!(pick.path(), "C:\\Data\\_app_install\\launcher.sisx");
    }

    #[test]
    fn the_same_version_with_different_bytes_is_a_rebuild_and_is_offered() {
        // The case the digest exists for: 0.1.0 rebuilt on the desk twenty times. Comparing versions
        // alone, the phone says "up to date" about a build that is not the one being tested.
        let d = db();
        let c = vec![cand(LAUNCHER, (0, 1, 0), "launcher.sisx").with_digest(sha(0xBB))];
        assert_eq!(d.offer_for(LAUNCHER, &c).map(|(o, _)| o), Some(Offer::Rebuild));
    }

    #[test]
    fn the_same_version_with_the_same_bytes_is_nothing_to_do() {
        let d = db();
        let c = vec![cand(LAUNCHER, (0, 1, 0), "launcher.sisx").with_digest(sha(0xAA))];
        assert_eq!(d.offer_for(LAUNCHER, &c), None, "byte-identical is genuinely up to date");
        assert_eq!(rows(&d, &c)[0].offer, Offer::Same, "and the row says so with certainty");
    }

    #[test]
    fn the_same_version_with_nothing_to_compare_says_unknown_rather_than_guessing() {
        // Installed by hand from the File manager, before any digest was recorded. Refusing would
        // hide a real update; claiming a difference would invent one.
        let mut d = db();
        d.get_mut(LAUNCHER).unwrap().installed_sha = None;
        let c = vec![cand(LAUNCHER, (0, 1, 0), "launcher.sisx").with_digest(sha(0xBB))];
        assert_eq!(d.offer_for(LAUNCHER, &c).map(|(o, _)| o), Some(Offer::Unknown));
    }

    #[test]
    fn an_older_version_is_shown_and_never_offered() {
        let d = db();
        let c = vec![cand(LAUNCHER, (0, 0, 9), "old.sisx")];
        assert_eq!(d.offer_for(LAUNCHER, &c), None, "going back is the rollback's job");
        let r = rows(&d, &c);
        assert_eq!(r[0].offer, Offer::Older);
        assert!(r[0].cand.is_some(), "but the file is not invisible");
    }

    #[test]
    fn a_package_we_have_never_seen_gets_a_row_of_its_own_flagged_new() {
        let d = db();
        let c = vec![cand(0x2000_1234, (1, 0, 0), "mystery.sis")];
        let r = rows(&d, &c);
        assert_eq!(r.len(), 3, "two managed packages plus the newcomer");
        assert_eq!(r[2].uid3, 0x2000_1234);
        assert_eq!(r[2].offer, Offer::New);
        assert_eq!(r[2].installed, None);
        assert!(r[2].offer.installable(), "new is installable; there is just nothing to go back to");
    }

    #[test]
    fn a_managed_package_with_no_known_version_takes_anything() {
        // The calendar has never stamped itself, so the first install is how it gets proved.
        let d = db();
        let c = vec![cand(CAL, (0, 3, 1), "cal.sis")];
        assert_eq!(d.offer_for(CAL, &c).map(|(o, _)| o), Some(Offer::New));
    }

    #[test]
    fn among_equal_versions_a_rebuild_wins_over_a_copy_of_what_is_installed() {
        let d = db();
        let c = vec![
            cand(LAUNCHER, (0, 1, 0), "same.sisx").with_digest(sha(0xAA)),
            cand(LAUNCHER, (0, 1, 0), "rebuilt.sisx").with_digest(sha(0xCC)),
        ];
        let (offer, pick) = d.offer_for(LAUNCHER, &c).expect("the rebuild");
        assert_eq!(offer, Offer::Rebuild);
        assert_eq!(pick.file, "rebuilt.sisx");
    }

    #[test]
    fn the_highest_version_wins_before_any_of_that() {
        let d = db();
        let c = vec![
            cand(LAUNCHER, (0, 1, 0), "a.sisx").with_digest(sha(0xCC)),
            cand(LAUNCHER, (0, 3, 0), "b.sisx"),
            cand(LAUNCHER, (0, 2, 0), "c.sisx"),
        ];
        let (offer, pick) = d.offer_for(LAUNCHER, &c).expect("the newest");
        assert_eq!(offer, Offer::Upgrade);
        assert_eq!(pick.version, Version::new(0, 3, 0));
    }

    #[test]
    fn a_pinned_package_is_listed_and_never_offered() {
        let mut d = db();
        d.get_mut(LAUNCHER).unwrap().pinned = true;
        let c = vec![cand(LAUNCHER, (0, 2, 0), "launcher.sisx")];
        assert_eq!(d.offer_for(LAUNCHER, &c), None);
        let r = rows(&d, &c);
        assert_eq!(r[0].offer, Offer::None);
        assert!(r[0].pinned);
    }

    #[test]
    fn the_boot_managers_own_package_is_never_an_update_target() {
        let mut d = PkgDb::default();
        d.ensure(ManagedPkg::new(BOOTCTL_UID, String::from("Boot manager")));
        let c = vec![cand(BOOTCTL_UID, (9, 9, 9), "bootctl.sisx")];
        assert_eq!(d.offer_for(BOOTCTL_UID, &c), None);
        assert_eq!(rows(&d, &c)[0].offer, Offer::None, "even with a candidate sitting right there");

        // And the same when it is not in the database at all — it must not arrive as a `New` row.
        let empty = PkgDb::default();
        let r = rows(&empty, &c);
        assert_eq!(r[0].offer, Offer::None);
        assert!(r[0].is_self);
    }

    #[test]
    fn only_the_equal_version_case_is_worth_a_read() {
        // The rule the device follows to decide what to hash. It lives here so there is one copy.
        let pkg = &db().pkgs[0];
        assert!(cand(LAUNCHER, (0, 1, 0), "a.sisx").needs_digest(pkg));
        assert!(!cand(LAUNCHER, (0, 2, 0), "a.sisx").needs_digest(pkg));
        assert!(!cand(LAUNCHER, (0, 0, 9), "a.sisx").needs_digest(pkg));
    }

    #[test]
    fn a_path_is_built_the_same_whether_the_directory_ends_in_a_separator_or_not() {
        for dir in ["C:\\Data\\_app_install\\", "C:\\Data\\_app_install"] {
            let mut c = cand(LAUNCHER, (0, 2, 0), "launcher.sisx");
            c.dir = String::from(dir);
            assert_eq!(c.path(), "C:\\Data\\_app_install\\launcher.sisx");
        }
    }

    #[test]
    fn only_a_sis_extension_is_worth_opening() {
        assert!(looks_like_package("launcher.sisx"));
        assert!(looks_like_package("LAUNCHER.SIS"));
        assert!(!looks_like_package("photo.jpg"));
        assert!(!looks_like_package("index.txt"));
        assert!(!looks_like_package("noextension"));
    }
}
