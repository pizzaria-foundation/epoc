//! What happened at the last boot, written by `apps/bootd` and read by `apps/bootctl`.
//!
//! A headless supervisor that is working correctly is indistinguishable from one that never ran, so
//! it has to say so somewhere. This is the somewhere: one record per entry, the launch return code,
//! and how long after bootd started each launch actually happened — which is the number that tells
//! you whether the first-launch delay is too short for AppArc.

use alloc::vec::Vec;

use crate::crc::crc16;

/// `b"BTST"` read as a little-endian u32.
pub const MAGIC: u32 = 0x5453_5442;
pub const VERSION: u16 = 1;
pub const ENTRY_SIZE: u16 = 16;
pub const HEADER_SIZE: usize = 16;

/// How bootd behaved this boot.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum Mode {
    /// Read the config and ran it.
    #[default]
    Normal,
    /// Three unsettled boots in a row: launched nothing on purpose.
    Safe,
    /// The config would not decode. Launched nothing.
    ConfigError,
    /// The master switch in the config is off.
    Disabled,
}

impl Mode {
    fn tag(self) -> u8 {
        match self {
            Mode::Normal => 0,
            Mode::Safe => 1,
            Mode::ConfigError => 2,
            Mode::Disabled => 3,
        }
    }

    fn from_tag(t: u8) -> Mode {
        match t {
            1 => Mode::Safe,
            2 => Mode::ConfigError,
            3 => Mode::Disabled,
            _ => Mode::Normal,
        }
    }
}

/// Where one entry got to.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum State {
    /// Its turn has not come yet.
    #[default]
    Pending,
    /// Launched; not yet seen running.
    Launched,
    /// The launch call itself failed. `last_rc` says how.
    LaunchFailed,
    /// Seen running.
    Alive,
    /// Was running, now is not, and its policy says leave it.
    Dead,
    /// Burned its restart budget and was switched off in the config.
    Disarmed,
    /// Disabled in the config; skipped.
    Skipped,
    /// bootd or bootctl itself — refused, because supervising either is a loop.
    RefusedSelf,
}

impl State {
    fn tag(self) -> u8 {
        match self {
            State::Pending => 0,
            State::Launched => 1,
            State::LaunchFailed => 2,
            State::Alive => 3,
            State::Dead => 4,
            State::Disarmed => 5,
            State::Skipped => 6,
            State::RefusedSelf => 7,
        }
    }

    fn from_tag(t: u8) -> State {
        match t {
            1 => State::Launched,
            2 => State::LaunchFailed,
            3 => State::Alive,
            4 => State::Dead,
            5 => State::Disarmed,
            6 => State::Skipped,
            7 => State::RefusedSelf,
            _ => State::Pending,
        }
    }

    /// The phrase bootctl puts after the app's name on the Status tab.
    pub fn describe(self) -> &'static str {
        match self {
            State::Pending => "waiting its turn",
            State::Launched => "launched",
            State::LaunchFailed => "launch failed",
            State::Alive => "running",
            State::Dead => "stopped",
            State::Disarmed => "auto-disabled: crash loop",
            State::Skipped => "off",
            State::RefusedSelf => "refused (boot manager's own)",
        }
    }
}

/// One entry's outcome.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct EntryStatus {
    pub uid3: u32,
    /// The return code of the last launch attempt: 0 for success, a Symbian error otherwise.
    pub last_rc: i32,
    /// Seconds after bootd's own start at which the last launch happened. Monotonic on purpose —
    /// the phone's clock may be wrong at boot, and this number only has to be a duration.
    pub launch_at_s: u32,
    pub restarts: u16,
    pub state: State,
}

/// The whole last-boot report.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct BootStatus {
    pub mode: Mode,
    /// Consecutive boots that never settled. `0` after a healthy one.
    pub boot_count: u8,
    pub restarts_used: u16,
    pub entries: Vec<EntryStatus>,
}

/// Why a status blob was refused. bootctl treats any of these as "no report yet".
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum DecodeError {
    Truncated,
    BadMagic,
    BadVersion(u16),
    BadCrc,
}

impl BootStatus {
    pub fn encode(&self) -> Vec<u8> {
        let count = self.entries.len().min(u16::MAX as usize);
        let mut out = Vec::with_capacity(HEADER_SIZE + count * ENTRY_SIZE as usize);
        out.extend_from_slice(&MAGIC.to_le_bytes());
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&ENTRY_SIZE.to_le_bytes());
        out.extend_from_slice(&(count as u16).to_le_bytes());
        out.push(self.mode.tag());
        out.push(self.boot_count);
        out.extend_from_slice(&self.restarts_used.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());

        for e in self.entries.iter().take(count) {
            out.extend_from_slice(&e.uid3.to_le_bytes());
            out.extend_from_slice(&e.last_rc.to_le_bytes());
            out.extend_from_slice(&e.launch_at_s.to_le_bytes());
            out.extend_from_slice(&e.restarts.to_le_bytes());
            out.push(e.state.tag());
            out.push(0);
        }

        let crc = crc16(&out);
        out[14..16].copy_from_slice(&crc.to_le_bytes());
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
        let entry_size = u16::from_le_bytes([bytes[6], bytes[7]]) as usize;
        if entry_size < ENTRY_SIZE as usize {
            return Err(DecodeError::Truncated);
        }
        let count = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
        let mode = Mode::from_tag(bytes[10]);
        let boot_count = bytes[11];
        let restarts_used = u16::from_le_bytes([bytes[12], bytes[13]]);
        let want_crc = u16::from_le_bytes([bytes[14], bytes[15]]);

        let end = HEADER_SIZE
            .checked_add(count.checked_mul(entry_size).ok_or(DecodeError::Truncated)?)
            .ok_or(DecodeError::Truncated)?;
        if bytes.len() < end {
            return Err(DecodeError::Truncated);
        }

        let mut check = Vec::from(bytes);
        check[14] = 0;
        check[15] = 0;
        if crc16(&check) != want_crc {
            return Err(DecodeError::BadCrc);
        }

        let mut entries = Vec::with_capacity(count);
        for i in 0..count {
            let r = &bytes[HEADER_SIZE + i * entry_size..][..ENTRY_SIZE as usize];
            entries.push(EntryStatus {
                uid3: u32::from_le_bytes([r[0], r[1], r[2], r[3]]),
                last_rc: i32::from_le_bytes([r[4], r[5], r[6], r[7]]),
                launch_at_s: u32::from_le_bytes([r[8], r[9], r[10], r[11]]),
                restarts: u16::from_le_bytes([r[12], r[13]]),
                state: State::from_tag(r[14]),
            });
        }

        Ok(Self { mode, boot_count, restarts_used, entries })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn sample() -> BootStatus {
        BootStatus {
            mode: Mode::Safe,
            boot_count: 3,
            restarts_used: 7,
            entries: vec![
                EntryStatus {
                    uid3: 0x1000_1234,
                    last_rc: 0,
                    launch_at_s: 12,
                    restarts: 2,
                    state: State::Alive,
                },
                EntryStatus {
                    uid3: 0x2000_0001,
                    last_rc: -1,
                    launch_at_s: 15,
                    restarts: 0,
                    state: State::LaunchFailed,
                },
            ],
        }
    }

    #[test]
    fn round_trip() {
        let s = sample();
        assert_eq!(BootStatus::decode(&s.encode()), Ok(s));
    }

    #[test]
    fn empty_round_trips() {
        let s = BootStatus::default();
        assert_eq!(BootStatus::decode(&s.encode()), Ok(s));
    }

    #[test]
    fn a_negative_rc_survives() {
        let back = BootStatus::decode(&sample().encode()).unwrap();
        assert_eq!(back.entries[1].last_rc, -1);
    }

    #[test]
    fn corruption_and_truncation_are_refused() {
        let mut b = sample().encode();
        b[HEADER_SIZE] ^= 0xFF;
        assert_eq!(BootStatus::decode(&b), Err(DecodeError::BadCrc));
        assert_eq!(BootStatus::decode(&[0u8; 4]), Err(DecodeError::Truncated));
        let mut b = sample().encode();
        b[0] = 0;
        assert_eq!(BootStatus::decode(&b), Err(DecodeError::BadMagic));
    }
}
