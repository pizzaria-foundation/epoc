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
        }
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
                },
                Entry {
                    uid3: 0xE0AA_0000,
                    enabled: false,
                    auto_disarmed: true,
                    policy: Policy::Times(3),
                    delay_ms: 5_500,
                    name: "Início".to_string(),
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
