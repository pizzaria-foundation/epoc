//! CRC-16/CCITT-FALSE over the config and status blobs.
//!
//! `fs::write_atomic` already makes a write all-or-nothing, so this is not about torn writes. It is
//! about the *other* ways these two files go wrong: a half-flushed filesystem after a battery pull,
//! a hand-edited byte, a blob from a future version that happens to keep the magic. A config that
//! decodes into plausible garbage costs a boot, so the codec refuses rather than guesses.

/// CRC-16/CCITT-FALSE: polynomial 0x1021, init 0xFFFF, no reflection, no final xor.
pub fn crc16(bytes: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in bytes {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vector() {
        // The canonical CCITT-FALSE check value for "123456789".
        assert_eq!(crc16(b"123456789"), 0x29B1);
    }

    #[test]
    fn empty_is_the_init_value() {
        assert_eq!(crc16(&[]), 0xFFFF);
    }

    #[test]
    fn one_flipped_bit_changes_it() {
        let a = crc16(&[0x01, 0x02, 0x03, 0x04]);
        let b = crc16(&[0x01, 0x02, 0x03, 0x05]);
        assert_ne!(a, b);
    }
}
