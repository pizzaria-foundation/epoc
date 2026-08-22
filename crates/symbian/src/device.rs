//! One reading of what the handset has and what is left of it.
//!
//! Everything here already existed, scattered: RAM in [`crate::mem`], drives in [`crate::vol`],
//! identity in [`crate::hal`], signal in [`crate::tele`], and battery — nowhere, because the
//! platform's obvious route for it does not work on this hardware. An app that wanted to show "how
//! full is this phone" had to know all five, plus which of them lie. This module is that knowledge
//! written once.
//!
//! Two rules shape it.
//!
//! **Absence is a value, not an error.** Every field is an [`Option`], and `None` means the handset
//! does not answer — not that the call failed. `KErrNotSupported` from HAL is a fact about the
//! device and gets recorded as one. Nothing here substitutes a plausible number for a missing one.
//!
//! **The units live here.** [`crate::hal`]'s table is `(id, name)` and nothing more, so every
//! consumer that wanted "50 MB" instead of `51228672` was re-deriving the scale. [`fmt_kb`] and
//! friends do it once.
//!
//! ## What this handset does not give
//!
//! Measured on the E72 and worth knowing before looking for it:
//!
//! - **Battery through HAL.** `EPowerBatteryStatus`, `EPowerBackup` and `EPowerExternal` all answer
//!   `KErrNotSupported`. The route that works is HWRM's Publish & Subscribe keys, which is what
//!   [`battery`] reads — no capability, no risky import.
//! - **CPU load, so far.** There is no HAL attribute and no single call for it — `ECPUSpeed` is a
//!   static clock rate, not utilisation. There *is* `RThread::GetCpuTime`, which is exported and
//!   gives one thread's cumulative CPU time, so a load figure is derivable by differencing across a
//!   process's threads. Whether this kernel accounts for it at all is a measurement nobody has taken
//!   yet (`apps/cpuprobe`), so no field here reports it.
//! - **Per-process memory.** [`crate::mem::heap_used_kb`] is *this* thread's allocator. The
//!   platform offers no way to attribute RAM to another process.
//! - **Precise uptime, maybe.** The monotonic clock underneath [`Snapshot::uptime_us`] falls back
//!   to a nominal microsecond per tick because `ENanoTickPeriod` "failed". That reading was taken at
//!   the wrong attribute id — 70 is `EMaxRAMDriveSize` — so the attribute has in fact never been
//!   asked for on this handset. Treat the resolution as the system tick (15625 µs) until a corrected
//!   dump says otherwise.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::error::Result;
use crate::{hal, mem, prop, tele, vol};

/// HWRM's Publish & Subscribe category, where the platform's power state is published.
///
/// This is the battery route on this handset — HAL's own battery attributes answer
/// `KErrNotSupported`. Reading it needs no capability and imports nothing, which is why the
/// launcher's status bar has always used it.
pub const HWRM_CATEGORY: u32 = 0x1020_5041;
/// HWRM key carrying the charge level, `0..=7`, or negative when unknown.
pub const HWRM_LEVEL_KEY: u32 = 1;
/// HWRM key carrying the charging state, `1` while charging.
pub const HWRM_CHARGING_KEY: u32 = 3;

/// The number of bars a full battery or a full signal shows. Both scales the platform publishes
/// here run `0..=7`, so a percentage is `value * 100 / BARS_MAX`.
pub const BARS_MAX: i32 = 7;

/// Charge state, as the platform publishes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Battery {
    /// Charge in bars, `0..=7`. Negative when the platform has not answered yet — at boot the
    /// key exists before anything has written to it.
    pub level: i32,
    /// Whether the phone is charging right now.
    pub charging: bool,
}

impl Battery {
    /// Charge as a percentage, or `None` while the level is unknown.
    pub fn percent(&self) -> Option<i32> {
        (self.level >= 0).then(|| (self.level * 100 / BARS_MAX).clamp(0, 100))
    }
}

/// One drive letter and what is left on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Storage {
    /// The drive letter, `'C'` and friends.
    pub letter: char,
    /// `TDriveInfo::iType`; name it with [`crate::vol::media::name`].
    pub media: i32,
    /// Whether the medium can be taken out — an empty card slot is removable and unmounted.
    pub removable: bool,
    /// Total size in KiB, `None` when nothing is mounted.
    pub total_kb: Option<u64>,
    /// Free space in KiB, `None` when nothing is mounted.
    pub free_kb: Option<u64>,
}

impl Storage {
    /// Whether a volume is mounted. An empty memory-card slot is a present drive that is not
    /// mounted, and that is a state worth showing as itself rather than as zero bytes free.
    pub fn mounted(&self) -> bool {
        self.total_kb.is_some()
    }

    /// Used space in KiB, or `None` when nothing is mounted.
    pub fn used_kb(&self) -> Option<u64> {
        Some(self.total_kb?.saturating_sub(self.free_kb?))
    }

    /// How full the drive is, 0..=100, or `None` when nothing is mounted or it reports no size.
    pub fn percent_used(&self) -> Option<i32> {
        let total = self.total_kb?;
        if total == 0 {
            return None;
        }
        Some(((self.used_kb()? * 100) / total).min(100) as i32)
    }
}

/// Everything readable about the device at one moment.
///
/// Read it with [`Snapshot::read`]. Each field is independent: one facility answering
/// `KErrNotSupported` leaves that field `None` and the rest intact, because a device report whose
/// every line disappears when one attribute is missing is not a report.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// Free RAM in KiB.
    pub ram_free_kb: Option<u32>,
    /// Total RAM in KiB.
    pub ram_total_kb: Option<u32>,
    /// This thread's own heap in KiB — not the device's, and not another process's.
    pub heap_used_kb: Option<u32>,
    /// Charge state, from HWRM.
    pub battery: Option<Battery>,
    /// Cellular signal as `(bars 0..=7, dBm)`.
    ///
    /// `None` unless the binary was built with telephony compiled in. A GUI app should generally
    /// leave this `None` and read a daemon's published value instead: the telephony import is
    /// quarantined in its own binary on purpose.
    pub signal: Option<(i32, i32)>,
    /// Microseconds since boot; see the resolution caveat in the module docs.
    pub uptime_us: u64,
    /// `EMachineUid` — the model identifier. `0x20015090` is the E72.
    pub machine_uid: Option<i32>,
    /// `ECPUSpeed`, in kHz. A clock rate, never a load.
    pub cpu_speed_khz: Option<i32>,
    /// Screen size in pixels, `(width, height)`.
    pub screen: Option<(i32, i32)>,
    /// Every drive letter the file server reports, in letter order.
    pub drives: Vec<Storage>,
}

impl Snapshot {
    /// Read every facility once. Cheap enough to call from a timer: all of it is either HAL, a
    /// Publish & Subscribe integer, or the file server, and none of it blocks on the network.
    pub fn read() -> Self {
        Self {
            ram_free_kb: mem::free_kb().ok(),
            ram_total_kb: mem::total_kb().ok(),
            heap_used_kb: mem::heap_used_kb().ok(),
            battery: battery().ok(),
            signal: tele::signal().ok(),
            uptime_us: crate::monotonic_us(),
            machine_uid: hal::get(hal_attr::MACHINE_UID).ok(),
            cpu_speed_khz: hal::get(hal_attr::CPU_SPEED).ok(),
            screen: match (
                hal::get(hal_attr::DISPLAY_X_PIXELS),
                hal::get(hal_attr::DISPLAY_Y_PIXELS),
            ) {
                (Ok(w), Ok(h)) => Some((w, h)),
                _ => None,
            },
            drives: storage(),
        }
    }

    /// RAM in use, in KiB — total minus free, when both are known.
    pub fn ram_used_kb(&self) -> Option<u32> {
        Some(self.ram_total_kb?.saturating_sub(self.ram_free_kb?))
    }

    /// The drive by letter, if the file server reports it.
    pub fn drive(&self, letter: char) -> Option<&Storage> {
        let want = letter.to_ascii_uppercase();
        self.drives.iter().find(|d| d.letter == want)
    }
}

/// The HAL attribute ids this module reads by name rather than by position in
/// [`crate::hal::INVENTORY`], so a reordering of that table cannot silently change what is read.
/// These are positional ordinals of `HALData::TAttribute`, which carries no explicit `= value` on
/// any enumerator. They were wrong once — the screen size was read from `EKeyboardAppKeys` — and
/// the report still looked plausible, because a wrong number reads exactly like a right one. The
/// test at the bottom of this file pins them against the same source `hal::INVENTORY` uses.
pub mod hal_attr {
    /// `EMachineUid` — the model identifier.
    pub const MACHINE_UID: i32 = 5;
    /// `ECPUSpeed`, in kHz.
    pub const CPU_SPEED: i32 = 11;
    /// `EDisplayXPixels`.
    pub const DISPLAY_X_PIXELS: i32 = 31;
    /// `EDisplayYPixels`.
    pub const DISPLAY_Y_PIXELS: i32 = 32;
}

/// Publish&Subscribe address the home screen publishes the keypad lock on, for the processes that
/// cannot read it themselves.
///
/// [`keylock`] needs a control environment, so a headless daemon can never ask — but "the phone is in
/// a pocket" is exactly what a poller wants to know. So the application that *can* ask writes the
/// answer here, and a daemon reads an integer.
///
/// The category is the launcher's, shared with [`crate::intent`], [`crate::agenda`] and
/// [`crate::daily`]; keys 100–104 are taken, so this is 105. Non-zero means locked. A key that has
/// never been written reads as an error, which callers must treat as *unlocked* — a stop signal
/// nobody publishes must not stop anything.
pub const LOCK_CATEGORY: u32 = crate::intent::CATEGORY;
pub const LOCK_KEY: u32 = 105;

/// Publish the keypad lock for the daemons. Define-then-set, so whichever side runs first creates it.
pub fn publish_keylock(locked: bool) -> Result<()> {
    let _ = crate::prop::define_public(LOCK_CATEGORY, LOCK_KEY);
    crate::prop::set(LOCK_CATEGORY, LOCK_KEY, locked as i32)
}

/// Whether the keypad is locked — or the phone is in autolock, which for a caller is the same fact.
///
/// # What this is for
///
/// It is the stop signal a background job actually wants. "Are we in the foreground" does not answer
/// it for a home screen, which is foreground by definition, and the keypad lock is the one state that
/// means *the phone is in a pocket*: nobody is reading the screen and nobody is about to press
/// anything. A poller that keeps its cadence through that is spending battery on an audience of
/// nobody.
///
/// # Where it comes from
///
/// `RAknKeyLock::IsKeyLockEnabled`, out of avkon — already linked by every GUI build, so it costs no
/// import. Gated behind `USE_KEYLOCK` all the same, and it needs a control environment: a headless
/// daemon gets [`Error::NotReady`], which is why the pattern is for the application to read this and
/// tell its daemons rather than each asking.
///
/// The Publish&Subscribe route the write-ups name (`KCoreAppUIsAutolockStatus`) is not available
/// here: the public SDK ships no header for the category, and the candidates answer `KErrNotFound`
/// when read on the handset. Measured before this was written, rather than shipped as a guess.
pub fn keylock() -> Result<bool> {
    let rc = unsafe { symbian_sys::shim_keylock() };
    if rc < 0 {
        return Err(crate::Error::from_code(rc));
    }
    Ok(rc == 1)
}

/// Read the charge state from HWRM.
///
/// The level key is the one that must be present; the charging key is treated as "not charging"
/// when it is missing, because a phone that cannot say is far more often on battery than on mains,
/// and refusing to report a level over a missing companion key would lose the useful half.
pub fn battery() -> Result<Battery> {
    let level = prop::get(HWRM_CATEGORY, HWRM_LEVEL_KEY)?;
    let charging = prop::get(HWRM_CATEGORY, HWRM_CHARGING_KEY).unwrap_or(0) == 1;
    Ok(Battery { level, charging })
}

/// Every drive the file server reports, in letter order.
///
/// A present drive with nothing mounted (an empty card slot) comes back with `None` sizes rather
/// than being dropped — "no card inserted" is information, and a list that omitted the slot would
/// read as "this phone has no card slot".
pub fn storage() -> Vec<Storage> {
    let Ok(mask) = vol::list() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for n in vol::present(mask) {
        let Ok(info) = vol::drive(n) else { continue };
        // A volume that will not answer is an unmounted drive, not a failure to enumerate.
        let volume = vol::volume(n).ok();
        out.push(Storage {
            letter: vol::letter(n),
            media: info.media_type,
            removable: info.drive_att & vol::drive_att::REMOVABLE != 0,
            total_kb: volume.as_ref().map(|v| (v.size.max(0) as u64) / 1024),
            free_kb: volume.as_ref().map(|v| (v.free.max(0) as u64) / 1024),
        });
    }
    out
}

/// One line of a human-readable device report.
///
/// Deliberately a plain data type rather than a widget: this crate is about the device and knows
/// nothing about drawing, and `symbian-ui` is about drawing and knows nothing about the device.
/// A caller maps these onto `symbian_ui::DeviceEntry` in a few lines and both halves stay testable
/// on the host without the other.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
    /// A heading grouping the fields under it.
    Section(String),
    /// A label, its value as text, and — when the value is really a fraction — a 0..=100 meter.
    Field { label: String, value: String, meter: Option<i32> },
}

impl Line {
    fn section(title: &str) -> Self {
        Line::Section(String::from(title))
    }

    fn field(label: &str, value: String) -> Self {
        Line::Field { label: String::from(label), value, meter: None }
    }

    fn gauge(label: &str, value: String, percent: i32) -> Self {
        Line::Field { label: String::from(label), value, meter: Some(percent) }
    }
}

/// What a value reads as when the handset does not provide it. Shown rather than omitted: a missing
/// line looks like an oversight, a line saying "not supported" is a finding about the hardware.
const ABSENT: &str = "not supported";

impl Snapshot {
    /// Render this snapshot as report lines, in the order a person would want to read them: what is
    /// running low first (memory, storage), then what the device simply is.
    pub fn report(&self) -> Vec<Line> {
        let mut out = Vec::new();

        out.push(Line::section("Memory"));
        match (self.ram_total_kb, self.ram_free_kb) {
            (Some(total), Some(free)) if total > 0 => {
                let used = total.saturating_sub(free);
                out.push(Line::gauge(
                    "RAM used",
                    format!("{} of {}", fmt_kb(used as u64), fmt_kb(total as u64)),
                    ((used as u64 * 100) / total as u64).min(100) as i32,
                ));
                out.push(Line::field("Free", fmt_kb(free as u64)));
            }
            _ => out.push(Line::field("RAM", String::from(ABSENT))),
        }
        if let Some(heap) = self.heap_used_kb {
            out.push(Line::field("This app's heap", fmt_kb(heap as u64)));
        }

        out.push(Line::section("Storage"));
        for d in &self.drives {
            let label = format!("{}:", d.letter);
            match (d.free_kb, d.percent_used()) {
                // A drive with room to report: how much is left, and how full it is.
                (Some(free), Some(pct)) => {
                    out.push(Line::gauge(&label, format!("{} free", fmt_kb(free)), pct))
                }
                // Mounted but sizeless — Z: reports 0 of 0, which is true, not broken.
                (Some(_), None) => out.push(Line::field(&label, String::from("read-only"))),
                // Present, nothing mounted: an empty card slot. Saying so beats "0 KB free".
                (None, _) => out.push(Line::field(&label, String::from("no card"))),
            }
        }
        if self.drives.is_empty() {
            out.push(Line::field("Drives", String::from(ABSENT)));
        }

        out.push(Line::section("Power"));
        match self.battery {
            Some(b) => {
                let state = if b.charging { " (charging)" } else { "" };
                match b.percent() {
                    Some(pct) => out.push(Line::gauge("Battery", format!("{pct}%{state}"), pct)),
                    None => out.push(Line::field("Battery", format!("unknown{state}"))),
                }
            }
            None => out.push(Line::field("Battery", String::from(ABSENT))),
        }
        match self.signal {
            Some((bars, dbm)) => out.push(Line::field("Signal", format!("{bars}/{BARS_MAX}  {dbm} dBm"))),
            None => out.push(Line::field("Signal", String::from("not read here"))),
        }

        out.push(Line::section("Device"));
        out.push(Line::field("Uptime", fmt_uptime(self.uptime_us)));
        match self.machine_uid {
            Some(uid) => out.push(Line::field("Model", format!("{uid:#010x}"))),
            None => out.push(Line::field("Model", String::from(ABSENT))),
        }
        match self.cpu_speed_khz {
            Some(khz) => out.push(Line::field("CPU clock", format!("{} MHz", khz / 1000))),
            None => out.push(Line::field("CPU clock", String::from(ABSENT))),
        }
        // Named explicitly, because its absence is the surprising part: there is no API for it at
        // all, so a screen that just left it out would invite someone to go looking again.
        out.push(Line::field("CPU load", String::from(ABSENT)));
        match self.screen {
            Some((w, h)) => out.push(Line::field("Screen", format!("{w}x{h}"))),
            None => out.push(Line::field("Screen", String::from(ABSENT))),
        }

        out
    }
}

/// Format a KiB count the way a phone screen wants it: `"48 KB"`, `"1.4 MB"`, `"1.2 GB"`.
///
/// One decimal above the KiB range, none inside it — a status bar has no room for `"51228.7 KB"`,
/// and nobody reads the last three digits of a free-space figure anyway.
pub fn fmt_kb(kb: u64) -> String {
    const MB: u64 = 1024;
    const GB: u64 = 1024 * 1024;
    if kb >= GB {
        format!("{}.{} GB", kb / GB, (kb % GB) * 10 / GB)
    } else if kb >= MB {
        format!("{}.{} MB", kb / MB, (kb % MB) * 10 / MB)
    } else {
        format!("{kb} KB")
    }
}

/// The terse form of [`fmt_kb`], for somewhere with no room: `"48M"`, `"1G"`.
///
/// A status-bar cell is a few characters wide, so it cannot take `"48.8 MB"` — but the rounding
/// rule should not therefore live in the status bar. Same source of truth, two presentations.
pub fn fmt_kb_short(kb: u64) -> String {
    const MB: u64 = 1024;
    const GB: u64 = 1024 * 1024;
    if kb >= GB {
        format!("{}G", kb / GB)
    } else if kb >= MB {
        format!("{}M", kb / MB)
    } else {
        format!("{kb}K")
    }
}

/// Format microseconds since boot as `"3d 04:21"`, `"4:21:07"` or `"21:07"`.
///
/// Days appear only once there are any, and seconds disappear once there are hours, so the string
/// stays short enough for a list row at every magnitude.
pub fn fmt_uptime(us: u64) -> String {
    let secs = us / 1_000_000;
    let (d, h, m, s) = (secs / 86400, (secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    if d > 0 {
        format!("{d}d {h:02}:{m:02}")
    } else if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_percent_scales_bars_and_hides_unknown() {
        assert_eq!(Battery { level: 7, charging: false }.percent(), Some(100));
        assert_eq!(Battery { level: 0, charging: false }.percent(), Some(0));
        assert_eq!(Battery { level: -1, charging: false }.percent(), None);
    }

    #[test]
    fn storage_reports_fullness_and_survives_an_empty_slot() {
        let mounted = Storage {
            letter: 'C',
            media: 0,
            removable: false,
            total_kb: Some(1000),
            free_kb: Some(250),
        };
        assert!(mounted.mounted());
        assert_eq!(mounted.used_kb(), Some(750));
        assert_eq!(mounted.percent_used(), Some(75));

        // An empty card slot: present, but nothing to measure. It must not read as "full".
        let slot = Storage {
            letter: 'E',
            media: 0,
            removable: true,
            total_kb: None,
            free_kb: None,
        };
        assert!(!slot.mounted());
        assert_eq!(slot.used_kb(), None);
        assert_eq!(slot.percent_used(), None);
    }

    #[test]
    fn a_zero_sized_volume_does_not_divide_by_zero() {
        // Z: on the handset reports 0 of 0 — a real reading, not a malformed one.
        let rom = Storage {
            letter: 'Z',
            media: 0,
            removable: false,
            total_kb: Some(0),
            free_kb: Some(0),
        };
        assert_eq!(rom.percent_used(), None);
    }

    #[test]
    fn kb_formatting_picks_a_unit_a_screen_can_hold() {
        assert_eq!(fmt_kb(48), "48 KB");
        assert_eq!(fmt_kb(1024), "1.0 MB");
        assert_eq!(fmt_kb(1536), "1.5 MB");
        // The E72's measured free RAM, and its C: drive.
        assert_eq!(fmt_kb(50028), "48.8 MB");
        assert_eq!(fmt_kb(1024 * 1024 + 512 * 1024), "1.5 GB");
    }

    #[test]
    fn the_short_form_fits_a_status_bar() {
        assert_eq!(fmt_kb_short(50_028), "48M");
        assert_eq!(fmt_kb_short(1024), "1M");
        assert_eq!(fmt_kb_short(512), "512K");
        assert_eq!(fmt_kb_short(2 * 1024 * 1024), "2G");
        // It must stay short: the cell it goes in is a handful of characters wide.
        for kb in [0, 1, 999, 50_028, 265_272, 4 * 1024 * 1024] {
            assert!(fmt_kb_short(kb).len() <= 5, "{kb} formats too wide");
        }
    }

    #[test]
    fn uptime_drops_seconds_as_it_grows() {
        assert_eq!(fmt_uptime(90 * 1_000_000), "1:30");
        assert_eq!(fmt_uptime(3_725 * 1_000_000), "1:02:05");
        assert_eq!(fmt_uptime(3 * 86_400 * 1_000_000 + 4 * 3600 * 1_000_000), "3d 04:00");
    }

    /// A snapshot shaped like the real E72 readings, so the report is exercised against the values
    /// the handset actually produces rather than round numbers.
    fn e72() -> Snapshot {
        Snapshot {
            ram_free_kb: Some(50_028),
            ram_total_kb: Some(122_752),
            heap_used_kb: Some(96),
            battery: Some(Battery { level: 5, charging: false }),
            signal: None,
            uptime_us: 3_725_000_000,
            machine_uid: Some(0x2001_5090),
            cpu_speed_khz: Some(192_000),
            screen: Some((320, 240)),
            drives: alloc::vec![
                Storage { letter: 'C', media: 9, removable: false, total_kb: Some(265_272), free_kb: Some(191_960) },
                Storage { letter: 'E', media: 0, removable: true, total_kb: None, free_kb: None },
                Storage { letter: 'Z', media: 7, removable: false, total_kb: Some(0), free_kb: Some(0) },
            ],
        }
    }

    fn values(lines: &[Line], label: &str) -> Option<String> {
        lines.iter().find_map(|l| match l {
            Line::Field { label: got, value, .. } if got == label => Some(value.clone()),
            _ => None,
        })
    }

    #[test]
    fn the_report_says_what_the_handset_cannot_answer() {
        let lines = e72().report();
        // The two absences that are findings, not omissions.
        assert_eq!(values(&lines, "CPU load").as_deref(), Some(ABSENT));
        assert_eq!(values(&lines, "Signal").as_deref(), Some("not read here"));
        // An empty card slot must not read as a full or empty drive.
        assert_eq!(values(&lines, "E:").as_deref(), Some("no card"));
        // Z: is mounted and sizeless — true, and not an error.
        assert_eq!(values(&lines, "Z:").as_deref(), Some("read-only"));
    }

    #[test]
    fn the_report_scales_the_real_numbers() {
        let lines = e72().report();
        assert_eq!(values(&lines, "Free").as_deref(), Some("48.8 MB"));
        assert_eq!(values(&lines, "C:").as_deref(), Some("187.4 MB free"));
        assert_eq!(values(&lines, "CPU clock").as_deref(), Some("192 MHz"));
        assert_eq!(values(&lines, "Model").as_deref(), Some("0x20015090"));
        assert_eq!(values(&lines, "Uptime").as_deref(), Some("1:02:05"));
    }

    #[test]
    fn every_meter_stays_in_range() {
        for line in e72().report() {
            if let Line::Field { label, meter: Some(pct), .. } = line {
                assert!((0..=100).contains(&pct), "{label} meter out of range: {pct}");
            }
        }
    }

    #[test]
    fn a_report_off_device_still_has_every_section() {
        // Nothing readable, but the shape must survive: a screen showing four headings and
        // "not supported" is honest; one showing an empty list looks broken.
        let lines = Snapshot::default().report();
        let sections: Vec<_> = lines
            .iter()
            .filter_map(|l| match l {
                Line::Section(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(sections, alloc::vec!["Memory", "Storage", "Power", "Device"]);
        assert_eq!(values(&lines, "RAM").as_deref(), Some(ABSENT));
        assert_eq!(values(&lines, "Drives").as_deref(), Some(ABSENT));
        assert_eq!(values(&lines, "Battery").as_deref(), Some(ABSENT));
    }

    #[test]
    fn the_hal_ids_agree_with_the_shared_inventory() {
        // Both tables name the same platform enum, and they drifted apart once: the screen size was
        // read from EKeyboardAppKeys for as long as nobody compared them. `hal::INVENTORY` is
        // itself pinned against `hal_data.h`, so agreeing with it is agreeing with the platform.
        for (id, name) in [
            (hal_attr::MACHINE_UID, "EMachineUid"),
            (hal_attr::CPU_SPEED, "ECPUSpeed"),
            (hal_attr::DISPLAY_X_PIXELS, "EDisplayXPixels"),
            (hal_attr::DISPLAY_Y_PIXELS, "EDisplayYPixels"),
        ] {
            let found = hal::INVENTORY.iter().find(|a| a.id == id);
            assert_eq!(found.map(|a| a.name), Some(name), "attribute {id} should be {name}");
        }
    }

    #[test]
    fn a_snapshot_off_device_is_empty_rather_than_wrong() {
        // The host shim stubs every facility, so this must produce a snapshot full of None —
        // never zeroes that would read as "0 KB free".
        let s = Snapshot::read();
        assert_eq!(s.ram_free_kb, None);
        assert_eq!(s.battery, None);
        assert!(s.drives.is_empty());
        assert_eq!(s.ram_used_kb(), None);
        assert!(s.drive('C').is_none());
    }
}
