//! Drives and volumes: what is mounted, what kind of medium, how much room.
//!
//! Three calls, kept apart because the platform keeps them apart and because merging them
//! would hide the case that matters most. A drive can be *present* with no volume
//! *mounted* — an empty memory-card slot is exactly that — and [`volume`] answers
//! [`Error::NotReady`] for it while [`drive`] answers happily. Anything that collapsed the
//! two would have to render "no card inserted" as either "no drive" or "a drive of size
//! zero", and both read as fact.

use symbian_sys as sys;

use crate::error::{Error, Result};

pub use sys::{ShimDriveInfo, ShimVolumeInfo};

/// `TDriveInfo::iType`, the medium behind a drive letter.
pub mod media {
    pub const NOT_PRESENT: i32 = 0;
    pub const UNKNOWN: i32 = 1;
    pub const FLOPPY: i32 = 2;
    pub const HARD_DISK: i32 = 3;
    pub const CD_ROM: i32 = 4;
    pub const RAM: i32 = 5;
    pub const FLASH: i32 = 6;
    pub const ROM: i32 = 7;
    pub const REMOTE: i32 = 8;
    pub const NAND_FLASH: i32 = 9;
    pub const ROTATING_MEDIA: i32 = 10;

    /// A printable name, or `"?"` for a value this table does not know — which is itself
    /// worth seeing in a report rather than being rendered as one of the known kinds.
    pub fn name(t: i32) -> &'static str {
        match t {
            NOT_PRESENT => "not present",
            UNKNOWN => "unknown",
            FLOPPY => "floppy",
            HARD_DISK => "hard disk",
            CD_ROM => "cd-rom",
            RAM => "ram",
            FLASH => "flash",
            ROM => "rom",
            REMOTE => "remote",
            NAND_FLASH => "nand flash",
            ROTATING_MEDIA => "rotating",
            _ => "?",
        }
    }
}

/// `TDriveInfo::iDriveAtt` bits.
pub mod drive_att {
    pub const LOCAL: u32 = 0x01;
    pub const ROM: u32 = 0x02;
    pub const REDIRECTED: u32 = 0x04;
    pub const SUBSTED: u32 = 0x08;
    pub const INTERNAL: u32 = 0x10;
    pub const REMOVABLE: u32 = 0x20;
    pub const REMOTE: u32 = 0x40;
    pub const TRANSACTION: u32 = 0x80;
}

/// `TDriveInfo::iMediaAtt` bits.
pub mod media_att {
    pub const VARIABLE_SIZE: u32 = 0x01;
    pub const DUAL_DENSITY: u32 = 0x02;
    pub const FORMATTABLE: u32 = 0x04;
    pub const WRITE_PROTECTED: u32 = 0x08;
    pub const LOCKABLE: u32 = 0x10;
    pub const LOCKED: u32 = 0x20;
    pub const HAS_PASSWORD: u32 = 0x40;
    pub const READ_WHILE_WRITE: u32 = 0x80;
    pub const DELETE_NOTIFY: u32 = 0x100;
}

/// The drive letter for a drive number, `0` → `'A'`.
pub fn letter(drive: i32) -> char {
    if (0..26).contains(&drive) {
        (b'A' + drive as u8) as char
    } else {
        '?'
    }
}

/// Which drive letters exist, as a 26-bit mask: bit N is `'A' + N`.
pub fn list() -> Result<u32> {
    let mut mask = 0u32;
    // SAFETY: `mask` is a live local; the shim writes at most one u32 through it.
    Error::check(unsafe { sys::shim_drive_list(&mut mask) })?;
    Ok(mask)
}

/// Iterator over the drive numbers present in a mask from [`list`].
pub fn present(mask: u32) -> impl Iterator<Item = i32> {
    (0..26).filter(move |i| mask & (1 << i) != 0)
}

/// What kind of thing a drive letter is. Succeeds for a slot with no card in it.
pub fn drive(n: i32) -> Result<ShimDriveInfo> {
    let mut info = ShimDriveInfo::default();
    // SAFETY: `info` is a live local of the layout the C side writes.
    Error::check(unsafe { sys::shim_drive_info(n, &mut info) })?;
    Ok(info)
}

/// Size, free space and label of a mounted volume.
///
/// [`Error::NotReady`] for a drive that exists with nothing mounted. Record it; do not
/// treat it as the call failing.
pub fn volume(n: i32) -> Result<ShimVolumeInfo> {
    let mut info = ShimVolumeInfo::default();
    // SAFETY: `info` is a live local of the layout the C side writes.
    Error::check(unsafe { sys::shim_volume_info(n, &mut info) })?;
    Ok(info)
}

/// The volume label as UTF-16 units, empty if it has none.
pub fn volume_name(info: &ShimVolumeInfo) -> &[u16] {
    let n = (info.name_len.max(0) as usize).min(info.name.len());
    &info.name[..n]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_map_from_zero() {
        assert_eq!(letter(0), 'A');
        assert_eq!(letter(2), 'C');
        assert_eq!(letter(4), 'E');
        assert_eq!(letter(25), 'Z');
        assert_eq!(letter(26), '?');
        assert_eq!(letter(-1), '?');
    }

    #[test]
    fn present_walks_the_mask() {
        // C: and E: — the two that matter on this handset.
        let mask = (1 << 2) | (1 << 4);
        let got: alloc::vec::Vec<i32> = present(mask).collect();
        assert_eq!(got, alloc::vec![2, 4]);
    }

    #[test]
    fn an_empty_mask_yields_nothing() {
        assert_eq!(present(0).count(), 0);
    }

    /// An unknown media type has to render as unknown. Folding it into one of the known
    /// names would put a fact in the report that the handset never said.
    #[test]
    fn unknown_media_types_are_not_guessed() {
        assert_eq!(media::name(media::FLASH), "flash");
        assert_eq!(media::name(99), "?");
    }

    /// The label is fixed-size and the length is what the C side wrote; trusting the array
    /// instead would print 32 units of whatever was in the buffer.
    #[test]
    fn volume_name_honours_the_written_length() {
        let mut info = ShimVolumeInfo::default();
        info.name[0] = b'M' as u16;
        info.name[1] = b'M' as u16;
        info.name[2] = b'C' as u16;
        info.name_len = 3;
        assert_eq!(volume_name(&info), &[77, 77, 67]);

        // A length the C side could never have written is clamped rather than trusted.
        info.name_len = 999;
        assert_eq!(volume_name(&info).len(), 32);
        info.name_len = -1;
        assert_eq!(volume_name(&info).len(), 0);
    }
}
