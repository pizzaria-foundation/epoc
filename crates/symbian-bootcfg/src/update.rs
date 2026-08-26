//! Installing a new version of our own software without losing the home screen.
//!
//! The Software Installer is not transactional and cannot be made so from outside it. What *can* be
//! made transactional is the operation around it, and that is what this is: a journal on disk, one
//! stage at a time, written **before** each step rather than after it. Every stage answers the
//! question a power cut asks — *what was I in the middle of?* — which is the same discipline a
//! bootloader uses, and for the same reason: the failure being designed for is losing power halfway
//! through replacing the thing that boots.
//!
//! ## What makes it atomic
//!
//! Not the install. The **proof**. A `.sis` that installs cleanly and an application that then
//! refuses to start are indistinguishable to every API this SDK can reach — `RApaLsSession` reports
//! a launch it accepted, not a program that worked. So a version is committed only when the new
//! code has *run and said so*: `symbian::pkg::stamp` writes `C:\Data\bootd\ver\<UID3>` at start-up,
//! and until that file names the version we were installing, the update has not happened. Anything
//! else — a timeout, a launch that keeps failing, a stamp that still names the old version — puts
//! the previously known-good `.sis` back.
//!
//! So the observable outcome is one of two states, never a third:
//!
//! - the new version is installed **and has run**, or
//! - the version that was working before is back.
//!
//! ## Who drives it
//!
//! `apps/bootd`, because the defining property of this operation is that the installer *stops the
//! application being replaced*, and something has to outlive that. `apps/bootctl` writes the
//! journal once, at [`Stage::Installing`], and never touches it again; every later stage is the
//! supervisor's. Two processes and one file is a race unless ownership is a rule, so it is one.
//!
//! ## The honest limitation, while the installer is the platform's
//!
//! `apps::launch_doc` on a `.sis` opens the S60 installer and a person taps "Yes". That is true of
//! the rollback too: [`Action::Install`] fires by itself, but somebody still confirms it. The state
//! machine is written as though nothing needed a tap, so when a silent-install route is proved the
//! only thing that changes is how the caller executes that one action.

use alloc::string::String;
use alloc::vec::Vec;

use crate::config::DecodeError;
use crate::crc::crc16;
use crate::pkg::{Stamp, Version};

/// `b"BTUP"` read as a little-endian u32.
pub const MAGIC: u32 = 0x5055_5442;
pub const VERSION: u16 = 1;
/// Fixed part of the journal: everything up to the `.sis` path, including the digest of the package
/// being installed.
pub const HEADER_SIZE: usize = 80;

/// How long the installer is given before the target is launched for the first time.
///
/// The floor exists because launching into a running install is the one thing the hold was invented
/// to prevent — the supervisor putting the app back on top of the file being written, and the user
/// reading "file in use". The hold covers the graceful case; this covers the case where the hold
/// expired while a person was still reading the installer's dialog.
/// The default, and what [`Updater`] uses until told otherwise. Configurable per phone through
/// [`crate::pkg::PkgDb::settle_s`], because how long a home screen may be missing is a judgement
/// about a phone rather than a constant about a format.
pub const INSTALL_SETTLE_S: i64 = 45;
/// Total time the install stage is given. Generous on purpose: it includes the user reading two
/// dialogs, a certificate prompt, and a 320 KB write to a phone from 2009.
pub const INSTALL_MAX_S: i64 = 600;
/// How long a launched version has to stamp itself before the update is declared not to have
/// happened. Long enough for a home screen that reads a config, a roster and a set of icons.
pub const PROVE_S: i64 = 60;
/// Nothing may sit in a journal longer than this, whatever stage it is in. The backstop for a
/// combination of failures nobody predicted.
pub const JOURNAL_MAX_S: i64 = 1_800;
/// Launch attempts per stage before the attempt is written off.
pub const MAX_LAUNCH_TRIES: u8 = 5;
/// Between launch attempts.
pub const RETRY_MS: u32 = 10_000;
/// The idle poll while waiting for an installer or a stamp. Short enough that the home screen comes
/// back promptly, and this only runs while an update is in flight.
pub const POLL_MS: u32 = 5_000;

/// Where the operation is. The absence of a journal file is the sixth state — idle — and it is
/// represented by not having one rather than by a value, so "is an update in flight" is a question
/// the filesystem answers.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Stage {
    /// The journal exists and the `.sis` has not been handed to an installer yet.
    ///
    /// `apps/bootctl` never leaves a journal here — it launches the installer as it arms, in one
    /// breath, so that a user watching the screen sees the installer and not a pause. The supervisor
    /// handles it anyway, and that is the seam: a silent-install route arms without launching, and
    /// [`Action::Install`] is then the only line that differs.
    Armed,
    /// An installer is running, or a person is looking at its dialog.
    Installing,
    /// Installed; the target has been launched and owes us a stamp.
    Proving,
    /// The new version ran and identified itself. Terminal, and the only good ending.
    Committed,
    /// Every attempt is spent. Terminal. The journal is kept so `apps/bootctl` can say what
    /// happened rather than showing a phone that quietly did nothing.
    Failed,
}

/// What counts as this update having worked.
///
/// The device taught this one. `Stage::Proving` waits for `symbian::pkg::stamp` to name the version
/// that was installed — which is a strong guarantee and only available for applications that call
/// it. Installing anything else, the wait could only ever time out: the install succeeded, the
/// program runs, and the machine declared failure, rolled back if it could, and counted the boot
/// against safe mode. A verification that reports failure on every subject it cannot verify is
/// worse than no verification.
///
/// So the promise is stated per update instead of assumed. What a package gets is what can honestly
/// be checked about it, and the confirmation says which one applies before anybody taps Install.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Proof {
    /// The application records the version it runs. Commit only when the stamp names what was
    /// installed — so a package that installs and then will not start is rolled back.
    ///
    /// Available once an application has stamped at least once. The *first* install of even a
    /// stamping application has no baseline, so it gets [`Proof::Launch`] and is provable from then
    /// on.
    Stamp,
    /// It does not report a version. Commit when the platform accepts a launch — and commit anyway
    /// when it will not accept one, because for this class of package that is not evidence of
    /// anything either.
    ///
    /// The second half sounds wrong and is the honest reading. A headless package has no AppArc
    /// registration at all — the tile probe declares `HEADLESS=1` and ships no `_reg.rsc` — so
    /// `apps::launch` by UID3 can *never* succeed for it, however perfectly the install went. Failing
    /// there would report a working install as a failure, roll back if it could, and count the boot
    /// against safe mode. **Measured on the handset**, twice: first for a package that never stamps,
    /// and then again for one that cannot be launched.
    ///
    /// So what this level promises is exactly one thing — *the file was handed to the installer and
    /// the installer was not interrupted* — and it promises it out loud rather than dressed up as
    /// verification. No rollback comes with it, because there is no signal that would trigger one.
    Launch,
}

/// Which `.sis` the current attempt is installing.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Attempt {
    /// The candidate the user chose.
    First,
    /// The known-good package, going back. There is no attempt after this one: a rollback that
    /// fails has already proved that reinstalling does not help, and a loop of installer dialogs is
    /// worse than a clear failure a person can act on.
    Rollback,
}

/// Why an update was written off. Recorded in the journal, shown by `apps/bootctl`.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Reason {
    /// The install stage ran out of time.
    InstallTimeout,
    /// The target was launched and never stamped itself.
    ProveTimeout,
    /// The launch itself kept failing.
    LaunchFailed,
    /// It ran, and it is still the old version — the install did not take.
    WrongVersion,
    /// The whole journal outlived [`JOURNAL_MAX_S`].
    Expired,
    /// It went wrong and there was no known-good package to go back to. The first install of a
    /// package is always in this position, which is why the UI says so before the first one.
    NoWayBack,
}

/// What the caller should do next. Executed by `apps/bootd`; every variant is one call.
///
/// Note what is absent: "close the target". Asking an application to close needs
/// `TApaTaskList`, which needs a `CCoeEnv` — and a headless daemon has none, so the symbol drags
/// `cone`/`ws32` into an image that must not import the window server. It is also not needed:
/// `apps/bootctl` closes the application as it arms the journal, before the installer is ever
/// launched, and by the time the supervisor picks the journal up the process is already gone.
/// A variant nothing emits is a promise the machine does not keep; this one cost a link error in a
/// boot process before it was removed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action {
    /// Nothing to do for this long.
    Wait(u32),
    /// Hand [`Journal::sis`] to an installer.
    Install,
    /// Launch the target application.
    Launch(u32),
    /// The new version ran. Promote the staged `.sis` to known-good, record the version **and its
    /// digest**, and delete the journal.
    Commit(Version),
    /// Terminal failure. Keep the journal, count it against the boot counter, and stop.
    GiveUp(Reason),
}

/// The update in flight, as it is written to disk.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Journal {
    pub stage: Stage,
    pub attempt: Attempt,
    /// The application being updated — the same identity the boot list supervises.
    pub uid3: u32,
    /// What was installed before, if we knew. `None` is the first install of a package.
    pub from: Option<Version>,
    /// What the candidate claims to be, and what the stamp has to say for this to commit.
    pub to: Version,
    /// The `.sis` the current attempt installs: the staged candidate, or the known-good on the way
    /// back.
    pub sis: String,
    /// SHA-256 of the candidate the user confirmed.
    ///
    /// Carried through the whole operation so that what commits is recorded as *the bytes that
    /// proved themselves*, not as a version number. It is what the next comparison is against: the
    /// same version rebuilt is a different digest, and that is the difference the phone has to be
    /// able to see. Zeroes on a rollback, where the file is the known-good package and its digest is
    /// already what is recorded.
    pub sha256: [u8; 32],
    /// What committing this update requires. See [`Proof`].
    pub proof: Proof,
    /// Unix seconds when the journal was armed, for [`JOURNAL_MAX_S`].
    pub started_s: i64,
    /// Unix seconds when the current stage was entered, for that stage's own deadline.
    pub stage_since_s: i64,
    /// Launch attempts spent in the current stage.
    pub tries: u8,
    pub reason: Option<Reason>,
}

impl Journal {
    /// The journal `apps/bootctl` writes when a person confirms an install.
    pub fn arm(
        uid3: u32,
        from: Option<Version>,
        to: Version,
        sha256: [u8; 32],
        proof: Proof,
        sis: String,
        now_s: i64,
    ) -> Self {
        Self {
            sha256,
            proof,
            stage: Stage::Installing,
            attempt: Attempt::First,
            uid3,
            from,
            to,
            sis,
            started_s: now_s,
            stage_since_s: now_s,
            tries: 0,
            reason: None,
        }
    }

    /// An update is in flight and the supervisor must not exit, defer its polling, or let anything
    /// else install over it.
    pub fn active(&self) -> bool {
        !matches!(self.stage, Stage::Committed | Stage::Failed)
    }

    /// Whether this journal is going back rather than forward — what `apps/bootctl` draws as
    /// "rolling back" and the plan calls a stage of its own. It is a flag and not a stage because
    /// the steps are identical either way; a rollback that were its own stage would be the same
    /// four transitions written twice, and the second copy is where the bug would live.
    pub fn rolling_back(&self) -> bool {
        self.attempt == Attempt::Rollback
    }

    /// The version this attempt is trying to reach: the candidate going forward, the previous one
    /// coming back. A rollback with no recorded `from` is still worth doing — the file is the
    /// package that was working — but nothing can be proved about its version, so it commits on
    /// *any* stamp. Said here once so the step function does not have to keep asking.
    pub fn expected(&self) -> Option<Version> {
        match self.attempt {
            Attempt::First => Some(self.to),
            Attempt::Rollback => self.from,
        }
    }

    fn enter(&mut self, stage: Stage, now_s: i64) {
        self.stage = stage;
        self.stage_since_s = now_s;
        self.tries = 0;
    }

    pub fn encode(&self) -> Vec<u8> {
        let units: Vec<u16> = self.sis.encode_utf16().take(u16::MAX as usize).collect();
        let mut out = Vec::with_capacity(HEADER_SIZE + units.len() * 2);
        let mut flags = 0u16;
        if self.from.is_some() {
            flags |= 0x01;
        }
        if self.proof == Proof::Launch {
            flags |= 0x02;
        }
        let from = self.from.unwrap_or_default();

        out.extend_from_slice(&MAGIC.to_le_bytes());
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.push(stage_tag(self.stage));
        out.push(match self.attempt {
            Attempt::First => 0,
            Attempt::Rollback => 1,
        });
        // Written last, over the whole file with these two bytes zeroed.
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&self.uid3.to_le_bytes());
        push_version(&mut out, from);
        push_version(&mut out, self.to);
        out.extend_from_slice(&self.started_s.to_le_bytes());
        out.extend_from_slice(&self.stage_since_s.to_le_bytes());
        out.push(self.tries);
        out.push(reason_tag(self.reason));
        out.extend_from_slice(&(units.len() as u16).to_le_bytes());
        out.extend_from_slice(&self.sha256);
        for u in &units {
            out.extend_from_slice(&u.to_le_bytes());
        }

        let crc = crc16(&out);
        out[8..10].copy_from_slice(&crc.to_le_bytes());
        out
    }

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
        let mut check = Vec::from(bytes);
        let stored = u16::from_le_bytes([bytes[8], bytes[9]]);
        check[8..10].copy_from_slice(&[0, 0]);
        if crc16(&check) != stored {
            return Err(DecodeError::BadCrc);
        }

        let sis_units = u16::from_le_bytes([bytes[46], bytes[47]]) as usize;
        let end = HEADER_SIZE.checked_add(sis_units * 2).ok_or(DecodeError::BadLayout)?;
        if bytes.len() < end {
            return Err(DecodeError::BadLayout);
        }
        let sis: Vec<u16> = bytes[HEADER_SIZE..end]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();

        let flags = u16::from_le_bytes([bytes[10], bytes[11]]);
        Ok(Self {
            // An unknown stage tag is not a stage to guess at: a journal we cannot place is one we
            // must not act on, and `Failed` is the reading that touches nothing and shows the user
            // that something went wrong.
            stage: stage_of(bytes[6]).ok_or(DecodeError::BadLayout)?,
            attempt: if bytes[7] == 0 { Attempt::First } else { Attempt::Rollback },
            uid3: u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
            from: (flags & 0x01 != 0).then(|| take_version(&bytes[16..22])),
            proof: if flags & 0x02 != 0 { Proof::Launch } else { Proof::Stamp },
            to: take_version(&bytes[22..28]),
            sis: String::from_utf16_lossy(&sis),
            started_s: take_i64(&bytes[28..36]),
            stage_since_s: take_i64(&bytes[36..44]),
            tries: bytes[44],
            reason: reason_of(bytes[45]),
            sha256: {
                let mut sha = [0u8; 32];
                sha.copy_from_slice(&bytes[48..80]);
                sha
            },
        })
    }
}

fn push_version(out: &mut Vec<u8>, v: Version) {
    out.extend_from_slice(&v.major.to_le_bytes());
    out.extend_from_slice(&v.minor.to_le_bytes());
    out.extend_from_slice(&v.patch.to_le_bytes());
}

fn take_version(b: &[u8]) -> Version {
    Version::new(
        u16::from_le_bytes([b[0], b[1]]),
        u16::from_le_bytes([b[2], b[3]]),
        u16::from_le_bytes([b[4], b[5]]),
    )
}

fn take_i64(b: &[u8]) -> i64 {
    let mut raw = [0u8; 8];
    raw.copy_from_slice(&b[..8]);
    i64::from_le_bytes(raw)
}

fn stage_tag(s: Stage) -> u8 {
    match s {
        Stage::Armed => 0,
        Stage::Installing => 1,
        Stage::Proving => 2,
        Stage::Committed => 3,
        Stage::Failed => 4,
    }
}

fn stage_of(t: u8) -> Option<Stage> {
    Some(match t {
        0 => Stage::Armed,
        1 => Stage::Installing,
        2 => Stage::Proving,
        3 => Stage::Committed,
        4 => Stage::Failed,
        _ => return None,
    })
}

fn reason_tag(r: Option<Reason>) -> u8 {
    match r {
        None => 0,
        Some(Reason::InstallTimeout) => 1,
        Some(Reason::ProveTimeout) => 2,
        Some(Reason::LaunchFailed) => 3,
        Some(Reason::WrongVersion) => 4,
        Some(Reason::Expired) => 5,
        Some(Reason::NoWayBack) => 6,
    }
}

fn reason_of(t: u8) -> Option<Reason> {
    Some(match t {
        1 => Reason::InstallTimeout,
        2 => Reason::ProveTimeout,
        3 => Reason::LaunchFailed,
        4 => Reason::WrongVersion,
        5 => Reason::Expired,
        6 => Reason::NoWayBack,
        _ => return None,
    })
}

/// What the caller can see, gathered fresh before each [`Updater::step`].
///
/// Note what is *not* here: whether the installer is running. Finding out would mean knowing the
/// Software Installer's process UID on this firmware, and a guess about a 2009 ROM that is wrong
/// fails silently in the direction that breaks installs — the same argument `HOLD_PATH` settled for
/// the supervisor. So the install stage is bounded by the hold and by time, both of which are ours.
#[derive(Copy, Clone, Debug)]
pub struct Obs {
    pub now_s: i64,
    /// `symbian_bootcfg::hold_active` over `HOLD_PATH`. A hold in force means an installer may
    /// still be holding the file, and nothing is launched into that.
    pub hold_active: bool,
    /// `symbian::process::is_running(uid3)`.
    pub target_running: bool,
    /// `symbian::pkg::stamped(uid3)` — the version of the code that last *ran*, and when. Stale
    /// stamps are the normal case, so [`Stamp::at_s`] is what makes one count.
    pub stamped: Option<Stamp>,
    /// A known-good `.sis` exists for this package, so a rollback has somewhere to go.
    pub have_known_good: bool,
    /// Somebody watched the installer close.
    ///
    /// The one honest observation available that the install is *over*, and it is not a clock. When
    /// the platform's installer exits, whatever was behind it comes back to the front — and what was
    /// behind it is `apps/bootctl`, which armed the update. So bootctl regaining the foreground while
    /// a journal is installing means the installer is gone, and it says so through a file.
    ///
    /// Not the installer's process UID, for the reason `HOLD_PATH` gives at length: that would be a
    /// guess about a 2009 ROM, and a wrong guess fails silently in the direction that breaks
    /// installs. A window coming forward is a fact the window server already told us.
    pub installer_done: bool,
}

/// The state machine `apps/bootd` executes.
///
/// Usage is the same shape as [`crate::Supervisor`], with one rule added and it is the important
/// one:
///
/// ```ignore
/// let act = up.step(&obs);
/// if up.take_dirty() { write_journal(up.journal()); }   // BEFORE acting, always
/// match act { … }
/// ```
///
/// Persisting before acting is what makes every stage resumable. Persisting after would leave a
/// window in which the installer was launched and the file still said "about to launch the
/// installer", and a power cut in that window installs twice.
pub struct Updater {
    jrn: Journal,
    dirty: bool,
    /// How long to wait before reopening the application, or `None` for not reopening it — which is
    /// the default and what most packages want. Set each round like [`Supervisor::set_installing`]
    /// rather than stored in the journal: it is a setting the user may change while an update is
    /// running, and the journal is a record of an operation, not of a preference.
    ///
    /// [`Supervisor::set_installing`]: crate::Supervisor::set_installing
    reopen_s: Option<i64>,
}

impl Updater {
    pub fn new(jrn: Journal) -> Self {
        Self { jrn, dirty: false, reopen_s: None }
    }

    /// Whether to reopen the application afterwards and, if so, how long to leave the installer alone
    /// first. `None` — the default — means the install is the whole operation.
    pub fn set_reopen_s(&mut self, secs: Option<u16>) {
        self.reopen_s = secs
            .map(|n| n.clamp(crate::pkg::MIN_SETTLE_S, crate::pkg::MAX_SETTLE_S) as i64);
    }

    pub fn journal(&self) -> &Journal {
        &self.jrn
    }

    /// Whether the journal changed since the last ask. The caller writes it out when this is true,
    /// before executing the action it was handed.
    pub fn take_dirty(&mut self) -> bool {
        core::mem::take(&mut self.dirty)
    }

    /// Report the result of the [`Action::Launch`] just executed: 0, or the platform's error.
    ///
    /// Only the *first* accepted launch opens the proof window. A relaunch inside [`Stage::Proving`]
    /// is a build dying on start, and letting it re-enter the stage would reset both the deadline
    /// and the try count on every death — a program that crashes reliably would then be retried
    /// forever, which is the one outcome this whole file exists to prevent.
    pub fn note_launch(&mut self, rc: i32, now_s: i64) {
        self.jrn.tries = self.jrn.tries.saturating_add(1);
        self.dirty = true;
        if rc == 0 && self.jrn.stage == Stage::Installing {
            self.jrn.enter(Stage::Proving, now_s);
        }
    }

    /// Report that the `.sis` was handed to an installer.
    pub fn note_install(&mut self, now_s: i64) {
        self.jrn.enter(Stage::Installing, now_s);
        self.dirty = true;
    }

    pub fn step(&mut self, obs: &Obs) -> Action {
        // The backstop, checked first so no stage can outlive it by being busy. Skipped for the
        // terminal stages, which are allowed to sit on disk forever — they are a record, not an
        // operation.
        if self.jrn.active()
            && obs.now_s.saturating_sub(self.jrn.started_s) > JOURNAL_MAX_S
        {
            return self.fail(Reason::Expired, obs);
        }

        match self.jrn.stage {
            Stage::Armed => Action::Install,

            Stage::Installing => {
                // Three ways out of here, and only the last one is a clock. The order matters: each
                // is stronger evidence than the one below it, and the floor is what is left when
                // there is no evidence at all.
                //
                // 1. **It ran and said so.** Nothing beats a stamp naming the version we installed.
                if self.proved(obs) {
                    return self.commit();
                }
                let waited = obs.now_s.saturating_sub(self.jrn.stage_since_s);
                if waited > INSTALL_MAX_S {
                    return self.fail(Reason::InstallTimeout, obs);
                }
                // 2. **It is already running.** Then the installer has finished with it — a file
                //    being replaced cannot also be an executing process — so there is nothing left
                //    to wait for and nothing to launch. This used to be checked *after* the floor,
                //    which meant a phone with the new version already on screen sat out the full 45
                //    seconds anyway. Somebody looking at the running program said it plainly: if the
                //    program's window is open, it installed.
                if obs.target_running {
                    self.jrn.enter(Stage::Proving, obs.now_s);
                    self.dirty = true;
                    return Action::Wait(POLL_MS);
                }
                // 3. The floor, and the hold. Two guards against one accident — launching into a
                //    running install pins the file being written — and `installer_done` is the
                //    observation that the accident is no longer possible, so it lifts both.
                //
                //    With no reopen asked for, the floor is only about not writing the bookkeeping
                //    down while the installer is still going, so `INSTALL_SETTLE_S` serves as the
                //    fallback timer and nothing is launched at all.
                let floor = self.reopen_s.unwrap_or(INSTALL_SETTLE_S);
                if !obs.installer_done && (waited < floor || obs.hold_active) {
                    return Action::Wait(POLL_MS);
                }
                // Nobody asked for it to be reopened. The install *is* the operation, so it is
                // finished — and a package left exactly where the installer left it is the ordinary
                // case, not a degraded one. Anything in the boot list comes back on its own.
                if self.reopen_s.is_none() {
                    return self.commit();
                }
                if self.jrn.tries >= MAX_LAUNCH_TRIES {
                    return self.spent_launches(obs);
                }
                Action::Launch(self.jrn.uid3)
            }

            Stage::Proving => {
                // Nothing to wait for. Reaching this stage means a launch was accepted, and for a
                // package that cannot report a version that is the whole of the promise — waiting
                // out a proof window would only end in a timeout that says "failed" about an
                // install that worked.
                if self.jrn.proof == Proof::Launch {
                    return self.commit();
                }
                if self.proved(obs) {
                    return self.commit();
                }
                // It ran, it identified itself, and it is the version we were replacing. The
                // install did not take, and no amount of further waiting changes that — this is the
                // one failure that is worth failing fast on.
                if let (Some(seen), Some(from)) = (obs.stamped, self.jrn.from) {
                    if self.jrn.attempt == Attempt::First
                        && self.fresh(&seen)
                        && seen.version == from
                    {
                        return self.fail(Reason::WrongVersion, obs);
                    }
                }
                if obs.now_s.saturating_sub(self.jrn.stage_since_s) > PROVE_S {
                    return self.fail(Reason::ProveTimeout, obs);
                }
                // Launched and already gone: it is crashing on start. Another launch is cheap and
                // the budget is what bounds it.
                if !obs.target_running {
                    if self.jrn.tries >= MAX_LAUNCH_TRIES {
                        return self.spent_launches(obs);
                    }
                    return Action::Launch(self.jrn.uid3);
                }
                Action::Wait(POLL_MS.min(RETRY_MS))
            }

            Stage::Committed => Action::Commit(self.jrn.to),
            Stage::Failed => Action::GiveUp(self.jrn.reason.unwrap_or(Reason::Expired)),
        }
    }

    /// Has the code we were installing run and said so?
    ///
    /// Two conditions, and both are necessary. The stamp has to name the version we were installing,
    /// and it has to have been written since this update was armed — a stamp from the last time the
    /// old build started is a fact about last week.
    ///
    /// A rollback with no known previous version commits on any *fresh* stamp, because "the package
    /// that was working is back and it runs" is the whole of what a rollback can promise.
    fn proved(&self, obs: &Obs) -> bool {
        if self.jrn.proof == Proof::Launch {
            return false;
        }
        let Some(stamp) = obs.stamped.filter(|s| self.fresh(s)) else { return false };
        match self.jrn.expected() {
            Some(want) => stamp.version == want,
            None => self.jrn.rolling_back(),
        }
    }

    /// Was this stamp written by a start that happened *because of* this update?
    ///
    /// Measured from when the journal was armed rather than from the launch, so a version the user
    /// started from the menu while the installer was finishing still counts. It ran; that is what
    /// was being asked.
    fn fresh(&self, stamp: &Stamp) -> bool {
        stamp.at_s >= self.jrn.started_s
    }

    /// The launch budget is gone. What that means depends entirely on what was promised.
    ///
    /// Under [`Proof::Stamp`] it is a real failure: the application is registered, we know what
    /// version it should be running, and it will not start. Under [`Proof::Launch`] it is not
    /// evidence of anything — a headless package cannot be launched by UID3 on any day of the week —
    /// and the install itself went fine.
    fn spent_launches(&mut self, obs: &Obs) -> Action {
        match self.jrn.proof {
            Proof::Stamp => self.fail(Reason::LaunchFailed, obs),
            Proof::Launch => self.commit(),
        }
    }

    fn commit(&mut self) -> Action {
        self.jrn.stage = Stage::Committed;
        self.jrn.reason = None;
        self.dirty = true;
        Action::Commit(self.jrn.to)
    }

    /// One attempt is spent. Go back if there is somewhere to go back to, and only once.
    fn fail(&mut self, why: Reason, obs: &Obs) -> Action {
        self.dirty = true;
        if self.jrn.attempt == Attempt::First && obs.have_known_good {
            self.jrn.attempt = Attempt::Rollback;
            self.jrn.sis = crate::known_good_path(self.jrn.uid3);
            self.jrn.enter(Stage::Armed, obs.now_s);
            // Not `Action::Install` directly: the caller has to write this journal out first, or a
            // power cut between the two installs the rollback twice.
            return Action::Wait(0);
        }
        self.jrn.stage = Stage::Failed;
        self.jrn.reason =
            Some(if obs.have_known_good || self.jrn.rolling_back() { why } else { Reason::NoWayBack });
        Action::GiveUp(self.jrn.reason.unwrap_or(why))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UID: u32 = 0xE0AA_0000;
    const OLD: Version = Version::new(0, 1, 0);
    const NEW: Version = Version::new(0, 2, 0);
    const SHA: [u8; 32] = [0x5A; 32];

    /// An updater that has been asked to reopen the application, which is what most of these tests
    /// are about. The default — not reopening — has tests of its own.
    fn reopening(j: Journal) -> Updater {
        let mut up = Updater::new(j);
        up.set_reopen_s(Some(INSTALL_SETTLE_S as u16));
        up
    }

    fn armed() -> Updater {
        reopening(Journal::arm(UID, Some(OLD), NEW, SHA, Proof::Stamp, String::from("C:\\staging.sis"), 0))
    }

    fn obs(now_s: i64) -> Obs {
        Obs {
            now_s,
            hold_active: false,
            target_running: false,
            // The stale stamp the old build left behind the last time it started. This is the
            // normal state of a phone about to be updated, and a machine that reads it as "the new
            // version failed" rolls back every update it is ever asked to do.
            stamped: Some(Stamp { uid3: UID, version: OLD, at_s: -1_000 }),
            have_known_good: true,
            installer_done: false,
        }
    }

    /// Run until the machine reaches a terminal action or a boundary it wants persisted
    /// (`Wait(0)`, which is how a re-arm asks to be written out before anything else happens),
    /// feeding the same observation forward in time. Returns every action taken.
    fn drive(up: &mut Updater, mut o: Obs, until_s: i64) -> alloc::vec::Vec<Action> {
        let mut acts = alloc::vec::Vec::new();
        while o.now_s <= until_s {
            let a = up.step(&o);
            up.take_dirty();
            acts.push(a.clone());
            match a {
                Action::Launch(_) => up.note_launch(0, o.now_s),
                Action::Install => up.note_install(o.now_s),
                Action::Commit(_) | Action::GiveUp(_) | Action::Wait(0) => break,
                _ => {}
            }
            o.now_s += 5;
        }
        acts
    }

    #[test]
    fn the_happy_path_waits_for_the_installer_launches_once_and_commits_on_the_stamp() {
        let mut up = armed();
        let mut o = obs(0);

        // Nothing happens while the installer might still be running.
        assert_eq!(up.step(&o), Action::Wait(POLL_MS));
        o.now_s = INSTALL_SETTLE_S - 1;
        assert_eq!(up.step(&o), Action::Wait(POLL_MS), "the floor is a floor");

        o.now_s = INSTALL_SETTLE_S + 1;
        assert_eq!(up.step(&o), Action::Launch(UID));
        up.note_launch(0, o.now_s);
        assert_eq!(up.journal().stage, Stage::Proving);

        // It is up but has not stamped yet.
        o.target_running = true;
        assert!(matches!(up.step(&o), Action::Wait(_)));

        o.stamped = Some(Stamp { uid3: UID, version: NEW, at_s: o.now_s });
        assert_eq!(up.step(&o), Action::Commit(NEW));
        assert_eq!(up.journal().stage, Stage::Committed);
    }

    #[test]
    fn by_default_nothing_is_reopened_and_the_install_is_the_whole_operation() {
        // The default, and the reason it is the default: installing something is not a reason to
        // start it, and anything in the boot list comes back on its own — a critical entry is watched
        // at 5..30 s and restarted the moment the update's exemption lifts.
        let mut up = Updater::new(Journal::arm(
            UID,
            Some(OLD),
            NEW,
            SHA,
            Proof::Stamp,
            String::from("C:\\staging.sis"),
            0,
        ));
        let mut o = obs(2);
        assert_eq!(up.step(&o), Action::Wait(POLL_MS), "not while the installer may still be going");

        o.installer_done = true;
        assert_eq!(up.step(&o), Action::Commit(NEW), "and then it is done, without a launch");
        assert_eq!(up.journal().stage, Stage::Committed);
    }

    #[test]
    fn an_application_already_running_is_not_waited_out() {
        // The handset's own words: if the program's window is open, it installed. A file being
        // replaced cannot also be an executing process, so there is nothing left to wait for — and
        // this used to be checked after the floor, so a phone with the new version on screen sat out
        // the full wait anyway.
        let mut up = armed();
        let mut o = obs(2); // seconds in, nowhere near the floor
        o.target_running = true;
        assert!(matches!(up.step(&o), Action::Wait(_)));
        assert_eq!(up.journal().stage, Stage::Proving, "straight to proving it");
    }

    #[test]
    fn watching_the_installer_close_ends_the_wait() {
        let mut up = armed();
        let mut o = obs(2);
        o.hold_active = true;
        assert_eq!(up.step(&o), Action::Wait(POLL_MS), "no evidence yet, so the floor stands");

        o.installer_done = true;
        assert_eq!(
            up.step(&o),
            Action::Launch(UID),
            "the guard exists to avoid launching into a running install; there is not one now"
        );
    }

    #[test]
    fn a_hold_in_force_is_never_launched_into() {
        let mut up = armed();
        let mut o = obs(INSTALL_SETTLE_S + 10);
        o.hold_active = true;
        assert_eq!(up.step(&o), Action::Wait(POLL_MS), "the installer may still hold the file");
        o.hold_active = false;
        assert_eq!(up.step(&o), Action::Launch(UID));
    }

    #[test]
    fn a_version_that_never_stamps_rolls_back() {
        let mut up = armed();
        let o = obs(0);
        // Launched, gone, launched again — a build that dies on start, which is exactly what a bad
        // update looks like from outside.
        let acts = drive(&mut up, o, INSTALL_SETTLE_S + PROVE_S + 60);

        assert!(acts.contains(&Action::Launch(UID)));
        assert!(up.journal().rolling_back(), "one attempt spent, and there is a way back");
        assert_eq!(up.journal().stage, Stage::Armed);
        assert_eq!(up.journal().sis, crate::known_good_path(UID));
        assert_eq!(up.step(&o), Action::Install, "and the way back is an install of its own");
    }

    #[test]
    fn the_old_version_still_running_is_a_failed_install_and_not_a_slow_one() {
        let mut up = armed();
        let mut o = obs(INSTALL_SETTLE_S + 1);
        assert_eq!(up.step(&o), Action::Launch(UID));
        up.note_launch(0, o.now_s);

        o.target_running = true;
        o.stamped = Some(Stamp { uid3: UID, version: OLD, at_s: o.now_s });
        o.now_s += 5;
        // Failing fast rather than sitting out the whole proof window.
        assert_eq!(up.step(&o), Action::Wait(0), "straight to the rollback");
        assert!(up.journal().rolling_back());
    }

    #[test]
    fn the_rollback_commits_on_the_previous_version_coming_back() {
        let mut up = armed();
        let mut o = obs(0);
        drive(&mut up, o, INSTALL_SETTLE_S + PROVE_S + 60);
        assert!(up.journal().rolling_back());

        o.now_s = 400;
        assert_eq!(up.step(&o), Action::Install);
        up.note_install(o.now_s);
        o.now_s += INSTALL_SETTLE_S + 1;
        assert_eq!(up.step(&o), Action::Launch(UID));
        up.note_launch(0, o.now_s);

        o.stamped = Some(Stamp { uid3: UID, version: OLD, at_s: o.now_s });
        assert!(matches!(up.step(&o), Action::Commit(_)), "the version that worked is back");
    }

    #[test]
    fn a_rollback_that_fails_gives_up_rather_than_looping() {
        let mut up = armed();
        let mut o = obs(0);
        drive(&mut up, o, INSTALL_SETTLE_S + PROVE_S + 60);
        assert!(up.journal().rolling_back());

        // Nothing ever stamps, second time round either.
        o.stamped = None;
        o.now_s = 1_000;
        let acts = drive(&mut up, o, JOURNAL_MAX_S + 100);
        assert!(
            acts.iter().any(|a| matches!(a, Action::GiveUp(_))),
            "a second rollback would just be a loop of installer dialogs"
        );
        assert_eq!(up.journal().stage, Stage::Failed);
    }

    #[test]
    fn a_first_install_with_nothing_to_go_back_to_says_so() {
        let mut up = reopening(Journal::arm(UID, None, NEW, SHA, Proof::Stamp, String::from("C:\\s.sis"), 0));
        let mut o = obs(0);
        o.have_known_good = false;
        o.stamped = None;
        o.target_running = true;
        let acts = drive(&mut up, o, INSTALL_SETTLE_S + PROVE_S + 60);
        assert!(acts.contains(&Action::GiveUp(Reason::NoWayBack)));
    }

    #[test]
    fn a_launch_that_keeps_failing_is_written_off_rather_than_retried_forever() {
        let mut up = armed();
        let mut o = obs(INSTALL_SETTLE_S + 1);
        for _ in 0..MAX_LAUNCH_TRIES {
            assert_eq!(up.step(&o), Action::Launch(UID));
            up.note_launch(-1, o.now_s); // KErrNotFound: the app is not installed at all
            o.now_s += 5;
        }
        assert_eq!(up.step(&o), Action::Wait(0), "budget spent, so it goes back");
        assert!(up.journal().rolling_back());
    }

    #[test]
    fn an_install_nobody_ever_finishes_times_out() {
        let mut up = armed();
        let mut o = obs(0);
        o.hold_active = true; // a hold renewed forever: an installer that never returns
        let acts = drive(&mut up, o, INSTALL_MAX_S + 60);
        assert!(acts.iter().any(|a| matches!(a, Action::Wait(0) | Action::GiveUp(_))));
        assert!(up.journal().rolling_back() || up.journal().stage == Stage::Failed);
    }

    #[test]
    fn nothing_sits_in_a_journal_past_the_backstop() {
        let mut up = armed();
        let mut o = obs(JOURNAL_MAX_S + 1);
        o.hold_active = true;
        up.step(&o);
        // First expiry spends the forward attempt; the rollback then inherits the same dead clock.
        assert!(up.journal().rolling_back());
        let a = up.step(&o);
        assert_eq!(a, Action::GiveUp(Reason::Expired));
    }

    #[test]
    fn every_stage_survives_a_power_cut() {
        // The property that makes this a bootloader and not a script: encode at each stage, decode,
        // and the resumed machine is the machine that was interrupted.
        for stage in [Stage::Armed, Stage::Installing, Stage::Proving, Stage::Committed, Stage::Failed]
        {
            let mut j = Journal::arm(UID, Some(OLD), NEW, SHA, Proof::Stamp, String::from("E:\\up\\l.sisx"), 100);
            j.stage = stage;
            j.tries = 2;
            j.reason = Some(Reason::ProveTimeout);
            let back = Journal::decode(&j.encode()).expect("round trip");
            assert_eq!(back, j, "{stage:?} did not survive");

            let mut a = Updater::new(j.clone());
            let mut b = Updater::new(back);
            let o = obs(200);
            assert_eq!(a.step(&o), b.step(&o), "{stage:?} resumed into a different decision");
        }
    }

    #[test]
    fn a_corrupted_journal_is_refused_rather_than_acted_on() {
        let j = Journal::arm(UID, Some(OLD), NEW, SHA, Proof::Stamp, String::from("C:\\s.sis"), 0);
        let mut bytes = j.encode();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        assert_eq!(Journal::decode(&bytes), Err(DecodeError::BadCrc));

        let mut bad_stage = j.encode();
        bad_stage[6] = 99;
        bad_stage[8..10].copy_from_slice(&[0, 0]);
        let crc = crc16(&bad_stage);
        bad_stage[8..10].copy_from_slice(&crc.to_le_bytes());
        assert_eq!(Journal::decode(&bad_stage), Err(DecodeError::BadLayout));
    }

    #[test]
    fn a_journal_with_no_path_in_it_still_decodes() {
        let j = Journal::arm(UID, None, NEW, SHA, Proof::Stamp, String::new(), 0);
        assert_eq!(Journal::decode(&j.encode()).unwrap(), j);
    }

    #[test]
    fn a_package_that_cannot_report_a_version_commits_on_the_launch() {
        // The device found this: installing something that never calls `pkg::stamp` used to wait out
        // the whole proof window, declare failure, and count the boot against safe mode — about an
        // install that had worked.
        let mut up = reopening(Journal::arm(
            UID,
            None,
            NEW,
            SHA,
            Proof::Launch,
            String::from("C:\\staging.sis"),
            0,
        ));
        let mut o = obs(INSTALL_SETTLE_S + 1);
        o.stamped = None;
        o.have_known_good = false;

        assert_eq!(up.step(&o), Action::Launch(UID));
        up.note_launch(0, o.now_s);
        assert_eq!(up.step(&o), Action::Commit(NEW), "accepted by the platform is the promise");
    }

    #[test]
    fn a_headless_package_commits_even_though_it_can_never_be_launched() {
        // Measured on the handset. The tile probe is HEADLESS=1 and ships no registration
        // resource, so `apps::launch` by UID3 fails every time however well the install went.
        // Reporting that as a failed update — and spending a safe-mode strike on it — was the same
        // mistake as demanding a version stamp from something that does not stamp.
        let mut up = reopening(Journal::arm(
            UID,
            None,
            NEW,
            SHA,
            Proof::Launch,
            String::from("C:\\s.sis"),
            0,
        ));
        let mut o = obs(INSTALL_SETTLE_S + 1);
        o.stamped = None;
        o.have_known_good = false;
        for _ in 0..MAX_LAUNCH_TRIES {
            assert_eq!(up.step(&o), Action::Launch(UID));
            up.note_launch(-1, o.now_s); // KErrNotFound: not in AppArc, and never will be
            o.now_s += 5;
        }
        assert_eq!(up.step(&o), Action::Commit(NEW), "the install is what was promised, and it held");
        assert_eq!(up.journal().stage, Stage::Committed);
    }

    #[test]
    fn the_same_dead_launch_under_a_stamp_promise_is_a_real_failure() {
        // The distinction the two levels exist for: here the application *is* registered and we know
        // what version it should be running, so refusing to start is evidence and it rolls back.
        let mut up = armed();
        let mut o = obs(INSTALL_SETTLE_S + 1);
        for _ in 0..MAX_LAUNCH_TRIES {
            assert_eq!(up.step(&o), Action::Launch(UID));
            up.note_launch(-1, o.now_s);
            o.now_s += 5;
        }
        assert_eq!(up.step(&o), Action::Wait(0), "budget spent, so it goes back");
        assert!(up.journal().rolling_back());
    }

    #[test]
    fn the_promise_survives_a_power_cut_with_the_rest_of_the_journal() {
        for proof in [Proof::Stamp, Proof::Launch] {
            let j = Journal::arm(UID, Some(OLD), NEW, SHA, proof, String::from("C:\\s.sis"), 0);
            assert_eq!(Journal::decode(&j.encode()).unwrap().proof, proof);
        }
    }

    #[test]
    fn a_terminal_journal_is_not_active_and_is_never_expired_by_the_backstop() {
        let mut j = Journal::arm(UID, Some(OLD), NEW, SHA, Proof::Stamp, String::from("C:\\s.sis"), 0);
        j.stage = Stage::Committed;
        assert!(!j.active());
        let mut up = Updater::new(j);
        assert_eq!(up.step(&obs(JOURNAL_MAX_S * 10)), Action::Commit(NEW));
    }
}
