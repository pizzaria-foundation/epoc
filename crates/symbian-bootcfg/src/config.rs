//! What to launch at boot, in what order, and what to do when it dies.
//!
//! The S60 startup list cannot express any of this: `STARTUP_ITEM_INFO` carries an executable path
//! and a `recovery` field whose only legal value is `EStartupItemExPolicyNone` ("do nothing"), and
//! it has no order, phase or priority member at all. So ordering and restart policy are ours, and
//! this is where they live — one file, written by `apps/bootctl`, read by `apps/bootd`.
//!
//! **Position in [`BootConfig::entries`] IS the launch order.** There is no order field, which is
//! deliberate: an explicit number lets two entries both claim position 3, and then something has to
//! decide what that means at boot. A `Vec::swap` cannot go inconsistent.

use alloc::string::String;
use alloc::vec::Vec;

use crate::crc::crc16;

/// `b"BTCF"` read as a little-endian u32.
pub const MAGIC: u32 = 0x4643_5442;
/// The only version this codec writes, and the highest it will read.
pub const VERSION: u16 = 1;
/// Bytes per entry record in version 1.
pub const ENTRY_SIZE: u16 = 16;
/// Fixed header size, versions 1 and up.
pub const HEADER_SIZE: usize = 16;
/// Refused above this. A boot supervisor with 33 entries is a mistake, not a configuration.
pub const MAX_ENTRIES: usize = 32;

/// Delay before the first launch when a home screen is in the list.
///
/// It was 25 s, and 25 s was a wrong answer to a question that had a different cause. The home
/// screen was dying seconds after launch, and the theory was that it had been started before the
/// window server was serving — so the cure was to wait longer. The real cause was
/// `User::WaitForRequest` on the GUI thread taking the process down with a stray-signal panic;
/// waiting longer never addressed it and only made every boot slower.
///
/// With that fixed, the delay is back to covering what it actually covers: AppArc not yet serving,
/// which fails the launch outright and which the supervisor already retries. Kept above zero
/// because a launch that lands too early is still a wasted attempt.
pub const HOME_FIRST_DELAY_MS: u32 = 10_000;

/// The value the 25 s floor wrote into every config it touched.
///
/// [`BootConfig::ensure_home`] replaces exactly this number and no other, which is what lets it
/// undo its own mistake without overruling a delay somebody chose. Same distinction as
/// [`Entry::auto_disarmed`]: a value a machine wrote is not a decision a person made, and only the
/// first is ours to revisit.
pub const HOME_FIRST_DELAY_LEGACY_MS: u32 = 25_000;
/// Delay before the *first* launch. The phone is still bringing up the shell when the startup list
/// runs; a launch that lands too early gets `KErrNotFound` from an AppArc that is not serving yet.
pub const DEFAULT_FIRST_DELAY_MS: u32 = 8_000;
/// Delay before each subsequent launch, so a boot does not fire five apps in the same instant.
pub const DEFAULT_DELAY_MS: u32 = 2_000;
/// Restarts allowed across all entries in one boot, before the supervisor stops restarting anything.
pub const DEFAULT_MAX_RESTARTS: u16 = 10;

/// What the supervisor does when a launched entry stops running.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Policy {
    /// Launch once; if it dies, leave it dead.
    Never,
    /// Restart up to `n` times, then leave it dead.
    Times(u8),
    /// Restart for as long as the global ceiling allows.
    Always,
}

impl Policy {
    fn tag(self) -> u8 {
        match self {
            Policy::Never => 0,
            Policy::Times(_) => 1,
            Policy::Always => 2,
        }
    }

    fn arg(self) -> u8 {
        match self {
            Policy::Times(n) => n,
            _ => 0,
        }
    }

    fn from_parts(tag: u8, arg: u8) -> Policy {
        match tag {
            // A zero-retry `Times` is just `Never`; normalising here keeps the budget arithmetic
            // from having to care about the degenerate case.
            1 if arg > 0 => Policy::Times(arg),
            1 => Policy::Never,
            2 => Policy::Always,
            _ => Policy::Never,
        }
    }

    /// How many restarts this policy permits. `None` means unbounded (still under the global cap).
    pub fn budget(self) -> Option<u16> {
        match self {
            Policy::Never => Some(0),
            Policy::Times(n) => Some(n as u16),
            Policy::Always => None,
        }
    }

    /// The label shown on the bootctl row and in the policy picker, in picker order.
    pub const LABELS: [&'static str; 3] = ["Never restart", "Restart N times", "Always restart"];

    /// Index into [`Policy::LABELS`].
    pub fn label_index(self) -> usize {
        match self {
            Policy::Never => 0,
            Policy::Times(_) => 1,
            Policy::Always => 2,
        }
    }
}

/// One app in the boot list.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    /// The application's UID3, as AppArc knows it. Launched with `symbian::apps::launch`.
    pub uid3: u32,
    pub enabled: bool,
    /// Set by the supervisor when this entry burned its restart budget, so bootctl can say *why*
    /// it is off rather than showing a switch the user does not remember flipping.
    pub auto_disarmed: bool,
    pub policy: Policy,
    /// Wait this long after the previous entry's launch before launching this one.
    pub delay_ms: u32,
    /// The caption, cached at the time it was added. Cached and not resolved live so an entry whose
    /// app has since been uninstalled still draws a readable row instead of a bare hex UID.
    pub name: String,
    /// This entry is the thing the phone is for, and its death is an outage rather than an
    /// inconvenience. Two consequences in `supervise`: the whole supervisor polls at the fast
    /// cadence while one of these is armed, so a crash costs seconds instead of minutes; and it is
    /// exempt from the global restart ceiling, which exists to stop several flapping apps from
    /// owning the phone and must not be the reason the home screen stays dead.
    ///
    /// Bounded anyway, and by the two limits that matter: its own [`Policy`] budget, and safe mode,
    /// which after three boots that never settle launches nothing at all.
    pub critical: bool,
}

impl Entry {
    /// A new entry with the defaults bootctl offers: on, restart three times, 2 s after the one
    /// before it.
    pub fn new(uid3: u32, name: String) -> Self {
        Self {
            uid3,
            enabled: true,
            auto_disarmed: false,
            policy: Policy::Times(3),
            delay_ms: DEFAULT_DELAY_MS,
            name,
            critical: false,
        }
    }

    /// The entry a home screen needs: always restarted, watched at the fast cadence, and first in
    /// the boot. `apps/launcher` writes exactly this on a phone that has no `boot.cfg` yet, so a
    /// fresh install supervises the home without anybody opening the boot manager.
    pub fn home(uid3: u32, name: String) -> Self {
        Self { policy: Policy::Always, critical: true, ..Self::new(uid3, name) }
    }
}

/// The whole boot list, in launch order.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BootConfig {
    /// Master switch. Off means bootd launches nothing at all this boot.
    pub enabled: bool,
    pub first_delay_ms: u32,
    pub max_restarts: u16,
    pub entries: Vec<Entry>,
}

impl Default for BootConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            first_delay_ms: DEFAULT_FIRST_DELAY_MS,
            max_restarts: DEFAULT_MAX_RESTARTS,
            entries: Vec::new(),
        }
    }
}

/// Why a blob was refused. Every variant means "launch nothing", never "guess".
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum DecodeError {
    /// Shorter than a header.
    Truncated,
    /// Not our file.
    BadMagic,
    /// Written by a newer bootctl than this bootd. Refused rather than half-read.
    BadVersion(u16),
    /// The header's own length fields do not describe the bytes that follow.
    BadLayout,
    /// The blob is intact in shape but not in content.
    BadCrc,
    /// More than [`MAX_ENTRIES`].
    TooMany(usize),
}

impl BootConfig {
    /// The entries that will actually be launched, in order.
    pub fn active(&self) -> impl Iterator<Item = (usize, &Entry)> {
        self.entries.iter().enumerate().filter(|(_, e)| e.enabled)
    }

    /// Move entry `i` one place earlier; returns where the cursor should follow it to. A no-op at
    /// the top, which is what makes holding Up at the top of the list harmless.
    pub fn move_up(&mut self, i: usize) -> usize {
        if i == 0 || i >= self.entries.len() {
            return i;
        }
        self.entries.swap(i - 1, i);
        i - 1
    }

    /// Move entry `i` one place later; returns where the cursor should follow it to.
    pub fn move_down(&mut self, i: usize) -> usize {
        if i + 1 >= self.entries.len() {
            return i;
        }
        self.entries.swap(i, i + 1);
        i + 1
    }

    /// Make sure `uid3` is in the list as a critical, always-restarted entry, and say whether that
    /// changed anything.
    ///
    /// Three cases, and the middle one is why this exists rather than a bare "write if absent":
    ///
    /// - **No config at all** — the caller writes a fresh one containing this entry. Handled by the
    ///   caller, not here; there is nothing to inspect.
    /// - **A config that does not mention this app** — append it. This is the case a seed-only rule
    ///   misses: somebody opens the boot manager first and saves a list, and from then on the home
    ///   screen is never supervised because a file exists. Appending an entry for an app that has
    ///   none is not editing anyone's choice; it is the absence of one.
    /// - **A config that already mentions it** — leave it entirely alone, policy and all, even if
    ///   it is disabled or not critical. That row is the user's answer, and overruling it every
    ///   start would make the boot manager a screen that argues back.
    pub fn ensure_home(&mut self, uid3: u32, name: String) -> bool {
        let mut changed = false;

        // An auto-disarm is a machine's conclusion, and this call is evidence against it: the app
        // that burned its restart budget is the one asking, so it is running. Clearing it here is
        // the difference between a supervisor that learns and a phone that is silently without a
        // home screen until somebody finds the boot manager.
        //
        // `auto_disarmed` is what makes this safe to do. A row the *user* switched off is
        // `enabled == false` with the flag clear, and that is left alone — the two look the same on
        // screen and mean opposite things, which is exactly why the flag exists.
        if let Some(e) = self.entries.iter_mut().find(|e| e.uid3 == uid3) {
            if e.auto_disarmed {
                e.auto_disarmed = false;
                e.enabled = true;
                changed = true;
            }
        }

        if !self.entries.iter().any(|e| e.uid3 == uid3) {
            if self.entries.len() >= MAX_ENTRIES {
                return false;
            }
            // First, because position is launch order and the home screen is what the user is
            // waiting to see. Everything else can start behind it.
            self.entries.insert(0, Entry::home(uid3, name));
            changed = true;
        }

        // The floor applies whether or not the row was just added, and that is the point. A GUI app
        // launched before the window server is serving does not fail — it comes up half-initialised,
        // alive as a process and useless as an application, which is the one failure the supervisor
        // cannot see. A config written before that was understood carries the old 8 s and would keep
        // reproducing it forever.
        //
        // `max`, so a longer delay somebody chose deliberately is left alone. This raises a floor;
        // it does not set a value.
        if self.first_delay_ms < HOME_FIRST_DELAY_MS
            || self.first_delay_ms == HOME_FIRST_DELAY_LEGACY_MS
        {
            self.first_delay_ms = HOME_FIRST_DELAY_MS;
            changed = true;
        }
        changed
    }

    /// Encode to the on-disk blob: 16-byte header, `count` × 16-byte records, then a UTF-16LE
    /// string blob the records point into.
    pub fn encode(&self) -> Vec<u8> {
        let count = self.entries.len().min(MAX_ENTRIES);
        let mut blob: Vec<u16> = Vec::new();
        let mut out = Vec::with_capacity(HEADER_SIZE + count * ENTRY_SIZE as usize);

        // Bit 15 of entry_size is free — records are 16 bytes, never 32768 — so the master switch
        // rides there instead of growing the header and breaking readers of this version.
        let es_field = if self.enabled { ENTRY_SIZE | 0x8000 } else { ENTRY_SIZE };

        out.extend_from_slice(&MAGIC.to_le_bytes());
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&es_field.to_le_bytes());
        out.extend_from_slice(&(count as u16).to_le_bytes());
        out.extend_from_slice(&to_ds(self.first_delay_ms).to_le_bytes());
        out.extend_from_slice(&self.max_restarts.to_le_bytes());
        // Written last, over the whole file with these two bytes zeroed.
        out.extend_from_slice(&0u16.to_le_bytes());

        for e in self.entries.iter().take(count) {
            let units: Vec<u16> = e.name.encode_utf16().take(u16::MAX as usize).collect();
            let name_off = blob.len() as u16;
            let name_len = units.len() as u16;
            blob.extend_from_slice(&units);

            let mut flags = 0u8;
            if e.enabled {
                flags |= 0x01;
            }
            if e.auto_disarmed {
                flags |= 0x02;
            }
            // Bit 2 of a byte that had six spare, rather than a new field and a version bump. That
            // choice is what keeps this backward AND forward compatible: a bootd built before this
            // flag existed reads the record, ignores the bit, and supervises the entry as an
            // ordinary one — degraded, never refused. A version bump would have made the same file
            // `BadVersion`, and `BadVersion` means launch nothing.
            if e.critical {
                flags |= 0x04;
            }
            out.extend_from_slice(&e.uid3.to_le_bytes());
            out.push(flags);
            out.push(e.policy.tag());
            out.push(e.policy.arg());
            out.push(0);
            out.extend_from_slice(&to_ds(e.delay_ms).to_le_bytes());
            out.extend_from_slice(&name_off.to_le_bytes());
            out.extend_from_slice(&name_len.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
        }

        for u in &blob {
            out.extend_from_slice(&u.to_le_bytes());
        }

        let crc = crc16(&out);
        out[14..16].copy_from_slice(&crc.to_le_bytes());
        out
    }

    /// Decode a blob written by [`encode`].
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
        let es_field = u16::from_le_bytes([bytes[6], bytes[7]]);
        let enabled = es_field & 0x8000 != 0;
        let entry_size = (es_field & 0x7FFF) as usize;
        if entry_size < ENTRY_SIZE as usize {
            return Err(DecodeError::BadLayout);
        }
        let count = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
        if count > MAX_ENTRIES {
            return Err(DecodeError::TooMany(count));
        }
        let first_delay_ms = from_ds(u16::from_le_bytes([bytes[10], bytes[11]]));
        let max_restarts = u16::from_le_bytes([bytes[12], bytes[13]]);
        let want_crc = u16::from_le_bytes([bytes[14], bytes[15]]);

        let table_end = HEADER_SIZE
            .checked_add(count.checked_mul(entry_size).ok_or(DecodeError::BadLayout)?)
            .ok_or(DecodeError::BadLayout)?;
        if bytes.len() < table_end {
            return Err(DecodeError::Truncated);
        }

        // The crc covers the whole file with its own two bytes zeroed.
        let mut check = Vec::from(bytes);
        check[14] = 0;
        check[15] = 0;
        if crc16(&check) != want_crc {
            return Err(DecodeError::BadCrc);
        }

        // UTF-16 units after the record table. An odd trailing byte is ignored, not an error.
        let blob: Vec<u16> = bytes[table_end..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();

        let mut entries = Vec::with_capacity(count);
        for i in 0..count {
            let r = &bytes[HEADER_SIZE + i * entry_size..][..ENTRY_SIZE as usize];
            let uid3 = u32::from_le_bytes([r[0], r[1], r[2], r[3]]);
            let flags = r[4];
            let policy = Policy::from_parts(r[5], r[6]);
            let delay_ms = from_ds(u16::from_le_bytes([r[8], r[9]]));
            let name_off = u16::from_le_bytes([r[10], r[11]]) as usize;
            let name_len = u16::from_le_bytes([r[12], r[13]]) as usize;
            // A string that points outside the blob degrades to an empty caption. bootctl will draw
            // the UID instead; that is a cosmetic loss, and panicking here would cost the boot.
            let name = match name_off.checked_add(name_len) {
                Some(end) if end <= blob.len() => String::from_utf16_lossy(&blob[name_off..end]),
                _ => String::new(),
            };
            entries.push(Entry {
                uid3,
                enabled: flags & 0x01 != 0,
                auto_disarmed: flags & 0x02 != 0,
                policy,
                delay_ms,
                name,
                critical: flags & 0x04 != 0,
            });
        }

        Ok(Self { enabled, first_delay_ms, max_restarts, entries })
    }
}

/// Milliseconds to deciseconds, saturating. Delays are a user-facing dial in half-seconds; storing
/// tenths keeps the field at two bytes and still covers 0..~109 minutes.
fn to_ds(ms: u32) -> u16 {
    (ms / 100).min(u16::MAX as u32) as u16
}

fn from_ds(ds: u16) -> u32 {
    ds as u32 * 100
}

#[cfg(test)]
mod ensure_home_tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;

    const HOME: u32 = 0xE0AA_0000;

    #[test]
    fn an_empty_config_gets_the_home_entry_first_and_critical() {
        let mut c = BootConfig::default();
        assert!(c.ensure_home(HOME, "Home".to_string()));
        assert_eq!(c.entries.len(), 1);
        assert!(c.entries[0].critical);
        assert_eq!(c.entries[0].policy, Policy::Always);
        assert_eq!(c.first_delay_ms, HOME_FIRST_DELAY_MS, "a GUI app must not race the boot");
    }

    #[test]
    fn a_longer_delay_the_user_chose_is_not_shortened() {
        // A floor is a floor: somebody who set 40 s had a reason.
        let mut c = BootConfig { first_delay_ms: 40_000, ..BootConfig::default() };
        c.ensure_home(HOME, "Home".to_string());
        assert_eq!(c.first_delay_ms, 40_000);
    }

    #[test]
    fn the_delay_this_code_wrote_when_it_was_wrong_is_taken_back() {
        // 25 s was written by an earlier version of this function, to cure a crash whose cause was
        // somewhere else entirely. Every phone that ran it carries the number, and a floor can only
        // ever raise — so without this the mistake would outlive the bug it was aimed at.
        let mut c = BootConfig {
            first_delay_ms: HOME_FIRST_DELAY_LEGACY_MS,
            entries: vec![Entry::new(HOME, "Home".to_string())],
            ..BootConfig::default()
        };
        assert!(c.ensure_home(HOME, "Home".to_string()));
        assert_eq!(c.first_delay_ms, HOME_FIRST_DELAY_MS);
    }

    #[test]
    fn a_delay_that_merely_resembles_ours_is_still_the_users_if_it_is_not_that_number() {
        let mut c = BootConfig { first_delay_ms: 24_000, ..BootConfig::default() };
        c.ensure_home(HOME, "Home".to_string());
        assert_eq!(c.first_delay_ms, 24_000, "only the exact value we wrote is ours to revisit");
    }

    #[test]
    fn a_list_somebody_else_wrote_gains_the_home_in_front_of_it() {
        // The case a seed-only rule misses: the boot manager was opened first, so a file exists and
        // the home screen would never be supervised.
        let mut c = BootConfig {
            entries: vec![Entry::new(0x1000_0001, "Calculator".to_string())],
            ..BootConfig::default()
        };
        assert!(c.ensure_home(HOME, "Home".to_string()));
        assert_eq!(c.entries[0].uid3, HOME, "the home launches before the rest");
        assert_eq!(c.entries[1].uid3, 0x1000_0001, "and nothing else moved or changed");
    }

    #[test]
    fn an_old_config_has_its_first_delay_raised_even_though_the_row_stays() {
        // The phone that already had a boot list written before the delay was understood. The row
        // is the user's and is not touched; the delay is a safety floor and is.
        let mut c = BootConfig {
            first_delay_ms: 8_000,
            entries: vec![Entry::new(HOME, "Home".to_string())],
            ..BootConfig::default()
        };
        assert!(c.ensure_home(HOME, "Home".to_string()));
        assert_eq!(c.first_delay_ms, HOME_FIRST_DELAY_MS);
        assert_eq!(c.entries.len(), 1);
        assert_eq!(c.entries[0].policy, Policy::Times(3), "the row itself is left as it was");
    }

    #[test]
    fn a_row_the_supervisor_disarmed_is_re_armed_by_the_app_that_is_running() {
        let mut disarmed = Entry::new(HOME, "Home".to_string());
        disarmed.enabled = false;
        disarmed.auto_disarmed = true;
        let mut c = BootConfig {
            first_delay_ms: HOME_FIRST_DELAY_MS,
            entries: vec![disarmed],
            ..BootConfig::default()
        };
        assert!(c.ensure_home(HOME, "Home".to_string()));
        assert!(c.entries[0].enabled, "the crash loop it was disarmed for is demonstrably over");
        assert!(!c.entries[0].auto_disarmed);
    }

    #[test]
    fn an_existing_row_for_the_home_is_never_overruled() {
        // Disabled, ordinary policy, not critical — every one of those is an answer the user gave.
        let mut existing = Entry::new(HOME, "Home".to_string());
        existing.enabled = false;
        existing.policy = Policy::Never;
        let mut c = BootConfig {
            first_delay_ms: HOME_FIRST_DELAY_MS,
            entries: vec![existing.clone()],
            ..BootConfig::default()
        };
        assert!(!c.ensure_home(HOME, "Home".to_string()));
        assert_eq!(c.entries, vec![existing]);
    }

    #[test]
    fn a_full_list_is_not_grown_past_the_ceiling() {
        let mut c = BootConfig {
            entries: (0..MAX_ENTRIES as u32).map(|i| Entry::new(i + 1, String::new())).collect(),
            ..BootConfig::default()
        };
        assert!(!c.ensure_home(HOME, "Home".to_string()));
        assert_eq!(c.entries.len(), MAX_ENTRIES);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;

    fn sample() -> BootConfig {
        BootConfig {
            enabled: true,
            first_delay_ms: 8_000,
            max_restarts: 10,
            entries: vec![
                Entry {
                    uid3: 0x1000_1234,
                    enabled: true,
                    auto_disarmed: false,
                    policy: Policy::Always,
                    delay_ms: 2_000,
                    name: "Calculator".to_string(),
                    critical: false,
                },
                Entry {
                    uid3: 0xE0AA_0000,
                    enabled: false,
                    auto_disarmed: true,
                    policy: Policy::Times(3),
                    delay_ms: 5_500,
                    name: "Início".to_string(),
                    critical: true,
                },
            ],
        }
    }

    #[test]
    fn round_trip() {
        let cfg = sample();
        assert_eq!(BootConfig::decode(&cfg.encode()), Ok(cfg));
    }

    #[test]
    fn master_switch_round_trips() {
        let mut cfg = sample();
        cfg.enabled = false;
        let back = BootConfig::decode(&cfg.encode()).unwrap();
        assert!(!back.enabled);
    }

    #[test]
    fn empty_config_round_trips() {
        let cfg = BootConfig::default();
        assert_eq!(BootConfig::decode(&cfg.encode()), Ok(cfg));
    }

    #[test]
    fn short_blob_is_truncated_not_a_panic() {
        assert_eq!(BootConfig::decode(&[]), Err(DecodeError::Truncated));
        assert_eq!(BootConfig::decode(&[0u8; 8]), Err(DecodeError::Truncated));
    }

    #[test]
    fn foreign_blob_is_rejected() {
        let mut b = sample().encode();
        b[0] = b'X';
        assert_eq!(BootConfig::decode(&b), Err(DecodeError::BadMagic));
    }

    #[test]
    fn a_newer_version_is_refused_not_guessed() {
        let mut b = sample().encode();
        b[4..6].copy_from_slice(&2u16.to_le_bytes());
        assert_eq!(BootConfig::decode(&b), Err(DecodeError::BadVersion(2)));
    }

    #[test]
    fn a_flipped_byte_is_caught_by_the_crc() {
        let mut b = sample().encode();
        let last = b.len() - 1;
        b[last] ^= 0xFF;
        assert_eq!(BootConfig::decode(&b), Err(DecodeError::BadCrc));
    }

    #[test]
    fn a_truncated_record_table_is_truncated() {
        let b = sample().encode();
        assert_eq!(BootConfig::decode(&b[..HEADER_SIZE + 8]), Err(DecodeError::Truncated));
    }

    #[test]
    fn an_absurd_count_is_refused_before_allocating() {
        let mut b = sample().encode();
        b[8..10].copy_from_slice(&9_000u16.to_le_bytes());
        assert_eq!(BootConfig::decode(&b), Err(DecodeError::TooMany(9_000)));
    }

    #[test]
    fn a_name_pointing_past_the_blob_degrades_to_empty() {
        let mut cfg = BootConfig::default();
        cfg.entries.push(Entry::new(0x1234_5678, "Whatever".to_string()));
        let mut b = cfg.encode();
        // name_len is at record offset 12..14; make it reach past the end of the string blob.
        let rec = HEADER_SIZE;
        b[rec + 12..rec + 14].copy_from_slice(&999u16.to_le_bytes());
        let crc = {
            let mut c = b.clone();
            c[14] = 0;
            c[15] = 0;
            crc16(&c)
        };
        b[14..16].copy_from_slice(&crc.to_le_bytes());
        let back = BootConfig::decode(&b).unwrap();
        assert_eq!(back.entries[0].name, "");
        assert_eq!(back.entries[0].uid3, 0x1234_5678, "the rest of the record still reads");
    }

    #[test]
    fn a_longer_v2_record_is_skipped_by_its_declared_size() {
        // A future writer with 20-byte records: this reader takes the first 16 bytes of each and
        // strides by 20, so it still reads v1's fields out of a v2 file.
        let cfg = sample();
        let v1 = cfg.encode();
        let count = cfg.entries.len();
        let mut b = Vec::new();
        b.extend_from_slice(&v1[..HEADER_SIZE]);
        b[6..8].copy_from_slice(&(20u16 | 0x8000).to_le_bytes());
        let mut blob_shift = 0usize;
        for i in 0..count {
            b.extend_from_slice(&v1[HEADER_SIZE + i * 16..][..16]);
            b.extend_from_slice(&[0u8; 4]);
            blob_shift += 4;
        }
        b.extend_from_slice(&v1[HEADER_SIZE + count * 16..]);
        assert_eq!(blob_shift, count * 4);
        let crc = {
            let mut c = b.clone();
            c[14] = 0;
            c[15] = 0;
            crc16(&c)
        };
        b[14..16].copy_from_slice(&crc.to_le_bytes());
        let back = BootConfig::decode(&b).unwrap();
        assert_eq!(back.entries.len(), count);
        assert_eq!(back.entries[0].uid3, cfg.entries[0].uid3);
        assert_eq!(back.entries[0].name, "Calculator");
    }

    #[test]
    fn move_up_and_down_are_no_ops_at_the_ends() {
        let mut cfg = sample();
        let before = cfg.entries.clone();
        assert_eq!(cfg.move_up(0), 0);
        assert_eq!(cfg.move_down(1), 1);
        assert_eq!(cfg.entries, before);
    }

    #[test]
    fn move_carries_the_cursor_with_the_row() {
        let mut cfg = sample();
        let moved = cfg.entries[1].clone();
        let landed = cfg.move_up(1);
        assert_eq!(landed, 0);
        assert_eq!(cfg.entries[0], moved, "the cursor index still points at the same entry");
    }

    #[test]
    fn a_zero_retry_times_normalises_to_never() {
        assert_eq!(Policy::from_parts(1, 0), Policy::Never);
        assert_eq!(Policy::Never.budget(), Some(0));
        assert_eq!(Policy::Times(3).budget(), Some(3));
        assert_eq!(Policy::Always.budget(), None);
    }

    #[test]
    fn delays_survive_the_decisecond_rounding() {
        let mut cfg = BootConfig { first_delay_ms: 8_000, ..Default::default() };
        cfg.entries.push(Entry { delay_ms: 5_500, ..Entry::new(1, String::new()) });
        let back = BootConfig::decode(&cfg.encode()).unwrap();
        assert_eq!(back.first_delay_ms, 8_000);
        assert_eq!(back.entries[0].delay_ms, 5_500);
    }

    #[test]
    fn active_skips_disabled_and_keeps_order() {
        let cfg = sample();
        let ids: Vec<u32> = cfg.active().map(|(_, e)| e.uid3).collect();
        assert_eq!(ids, vec![0x1000_1234]);
    }
}
