//! Device inventory through `HAL::Get`.
//!
//! The shim exposes one call, `shim_hal_get(attr, out)`, because `HAL::Get` is already a
//! flat integer interface over the kernel's own figures. Everything else — which attributes
//! are worth asking for, what they are called, how to render them — is data, and lives here
//! where a host test can cover it.
//!
//! # `KErrNotSupported` is an answer, not a failure
//!
//! A handset returns it for an attribute its hardware does not implement. That is exactly
//! the kind of thing a device inventory exists to discover, so [`get`] hands it back as
//! [`Error::Platform`] carrying `-5` and callers are expected to record it rather than
//! abandon the sweep. A report that stopped at the first unsupported attribute would
//! describe the first gap in the hardware and nothing after it.

use symbian_sys as sys;

use crate::error::{Error, Result};

/// A `HALData::TAttribute` worth asking about, with the name to print it under.
///
/// The numeric values are the enum's ordinals, which are stable ABI: they are baked into
/// every binary ever built against a Symbian SDK, so they cannot be renumbered.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Attr {
    pub id: i32,
    pub name: &'static str,
}

const fn a(id: i32, name: &'static str) -> Attr {
    Attr { id, name }
}

/// The attributes a device dump asks for, in the order it asks.
///
/// Grouped by subject rather than by ordinal so the report reads like a description of the
/// handset. Nothing here is expensive — `HAL::Get` is a kernel read, not a device probe —
/// so the list is generous on purpose: an attribute nobody wanted costs one line, and an
/// attribute nobody asked for costs another trip to the phone.
pub const INVENTORY: &[Attr] = &[
    // identity
    a(0, "EManufacturer"),
    a(1, "EManufacturerHardwareRev"),
    a(2, "EManufacturerSoftwareRev"),
    a(3, "EManufacturerSoftwareBuild"),
    a(4, "EModel"),
    a(5, "EMachineUid"),
    a(6, "EDeviceFamily"),
    a(7, "EDeviceFamilyRev"),
    // cpu
    a(8, "ECPU"),
    a(9, "ECPUArch"),
    a(10, "ECPUABI"),
    a(11, "ECPUSpeed"),
    a(12, "ESystemStartupReason"),
    a(13, "ESystemException"),
    a(14, "ESystemTickPeriod"),
    // memory
    a(15, "EMemoryRAM"),
    a(16, "EMemoryRAMFree"),
    a(17, "EMemoryROM"),
    a(18, "EMemoryPageSize"),
    // power
    a(19, "EPowerGood"),
    a(20, "EPowerBatteryStatus"),
    a(21, "EPowerBackup"),
    a(22, "EPowerBackupStatus"),
    a(23, "EPowerExternal"),
    // display
    a(24, "EKeyboard"),
    a(25, "EKeyboardDeviceKeys"),
    a(26, "EKeyboardAppKeys"),
    a(27, "EKeyboardClick"),
    a(28, "EKeyboardClickState"),
    a(29, "EKeyboardClickVolume"),
    a(30, "EKeyboardClickVolumeMax"),
    a(31, "EDisplayXPixels"),
    a(32, "EDisplayYPixels"),
    a(33, "EDisplayXTwips"),
    a(34, "EDisplayYTwips"),
    a(35, "EDisplayColors"),
    a(36, "EDisplayState"),
    a(37, "EDisplayContrast"),
    a(38, "EDisplayContrastMax"),
    a(39, "EBacklight"),
    a(40, "EBacklightState"),
    a(64, "EDisplayBrightness"),
    a(65, "EDisplayBrightnessMax"),
    // input
    a(41, "EPen"),
    a(42, "EPenX"),
    a(43, "EPenY"),
    a(44, "EPenDisplayOn"),
    a(45, "EPenClick"),
    a(46, "EPenClickState"),
    a(47, "EPenClickVolume"),
    a(48, "EPenClickVolumeMax"),
    a(49, "EMouse"),
    // storage and system
    a(70, "EMaxRAMDriveSize"),
    a(72, "ESystemDrive"),
    // timing — the one that has already cost this project a wrong number, twice
    a(92, "ENanoTickPeriod"),
    a(93, "EFastCounterFrequency"),
    a(94, "EFastCounterCountsUp"),
];

/// Read one attribute.
///
/// `Err(Error::Platform(-5))` — `KErrNotSupported` — means the handset does not implement
/// it. See the module note: that is a finding, not a failure.
pub fn get(attr: i32) -> Result<i32> {
    let mut out = 0i32;
    // SAFETY: `out` is a live local; the shim writes at most one i32 through it.
    Error::check(unsafe { sys::shim_hal_get(attr, &mut out) })?;
    Ok(out)
}

/// [`get`] for a named attribute.
pub fn get_attr(attr: &Attr) -> Result<i32> {
    get(attr.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeSet;

    /// A duplicate id would make the report print one attribute twice under two names, and
    /// the second reading would look like a second fact.
    #[test]
    fn every_attribute_id_appears_once() {
        let mut seen = BTreeSet::new();
        for at in INVENTORY {
            assert!(seen.insert(at.id), "duplicate HAL attribute id {}", at.id);
        }
    }

    #[test]
    fn every_attribute_is_named() {
        for at in INVENTORY {
            assert!(!at.name.is_empty(), "HAL attribute {} has no name", at.id);
            assert!(at.name.starts_with('E'), "{} is not a TAttribute name", at.name);
        }
    }

    /// ENanoTickPeriod is in *microseconds*, and reading it as anything else made every
    /// duration this SDK printed wrong by a factor of a thousand — with nothing failing,
    /// because a measurement has no error bars (docs/device-notes.md). Pinned so the entry
    /// cannot quietly go missing from the sweep that would catch it again.
    #[test]
    fn the_timing_attributes_are_in_the_sweep() {
        for want in ["ENanoTickPeriod", "ESystemTickPeriod", "EFastCounterFrequency"] {
            assert!(INVENTORY.iter().any(|a| a.name == want), "{want} missing");
        }
    }

    /// Names paired with the ordinals `HALData::TAttribute` actually has.
    ///
    /// This test exists because the one above did not catch the bug it was written for. The
    /// table had `ENanoTickPeriod` at 70 for months; 70 is `EMaxRAMDriveSize`, so every dump
    /// reported that attribute's answer under the timing attribute's name — and the report read
    /// perfectly well, because a wrong number and a right number look identical. Asserting on
    /// names alone can only catch a deletion; only the id catches a lie.
    ///
    /// The enum carries no explicit `= value` on any enumerator, so these are positional
    /// ordinals read straight out of `sdk/epoc32/include/hal_data.h`. Spot checks across the
    /// whole range rather than the full 104: enough that a shift anywhere moves at least one.
    #[test]
    fn the_ids_match_the_platform_enum() {
        const KNOWN: &[(i32, &str)] = &[
            (0, "EManufacturer"),
            (5, "EMachineUid"),
            (11, "ECPUSpeed"),
            (14, "ESystemTickPeriod"),
            (15, "EMemoryRAM"),
            (16, "EMemoryRAMFree"),
            (20, "EPowerBatteryStatus"),
            (21, "EPowerBackup"),
            (22, "EPowerBackupStatus"),
            (31, "EDisplayXPixels"),
            (32, "EDisplayYPixels"),
            (41, "EPen"),
            (49, "EMouse"),
            (64, "EDisplayBrightness"),
            (65, "EDisplayBrightnessMax"),
            (70, "EMaxRAMDriveSize"),
            (72, "ESystemDrive"),
            (92, "ENanoTickPeriod"),
            (93, "EFastCounterFrequency"),
            (94, "EFastCounterCountsUp"),
        ];
        for (id, name) in KNOWN {
            if let Some(at) = INVENTORY.iter().find(|a| a.name == *name) {
                assert_eq!(at.id, *id, "{name} is attribute {id}, not {}", at.id);
            }
            // The reverse direction matters just as much: an id in the table under the wrong
            // name is exactly the bug this is here for.
            if let Some(at) = INVENTORY.iter().find(|a| a.id == *id) {
                assert_eq!(at.name, *name, "attribute {id} is {name}, not {}", at.name);
            }
        }
    }
}
