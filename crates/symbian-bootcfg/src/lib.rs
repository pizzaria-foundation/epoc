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

/// Unsettled boots in a row before bootd launches nothing and waits to be told why.
pub const SAFE_MODE_STRIKES: u8 = 3;
