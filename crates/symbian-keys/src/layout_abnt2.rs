//! The Brazilian (ABNT2) E72 keymap.
//!
//! # GENERATED — do not edit
//!
//! `tools/mkkeymap.py examples/keydump/keymap-brpt.txt`, language `brazilian-portuguese`.
//!
//! The source is a dump of the handset's own keymap, read out of its
//! `ptiengine.dll` by `examples/keydump`. Editing this file instead of
//! regenerating it detaches the table from the measurement it came from, which is
//! the failure mode `docs/device-notes.md` spends a whole section on.
//!
//! `ptiengine` is the platform's keymap database, the layer *underneath* Avkon's
//! FEP — not the FEP itself, which this SDK deliberately does not use. It was asked
//! once, offline; nothing that ships imports it.

use crate::layout::KeyRow;

/// One row per physical key. `'\0'` means the key produces nothing in that case.
///
/// Column order is [`crate::layout::Case`]: lower, upper, chr-lower, chr-upper.
pub const ROWS: &[KeyRow] = &[
    // 0x2A: hardware-verified overlay, overrides the dump
    // 0x2A: numeric binding U+002A for an undescribed key, skipped
    KeyRow { scan: 0x2A, chars: ['u', 'U', '*', '*'], dead: 0b0000 },
    // 0x30: hardware-verified overlay, overrides the dump
    // 0x30: lower=006D; upper=004D
    // 0x30: chr=0030 from numeric binding (case 4)
    KeyRow { scan: 0x30, chars: ['m', 'M', '0', '0'], dead: 0b0000 },
    // 0x31: hardware-verified overlay, overrides the dump
    // 0x31: lower=0072; upper=0052
    // 0x31: chr=0031 from numeric binding (case 4)
    KeyRow { scan: 0x31, chars: ['r', 'R', '1', '1'], dead: 0b0000 },
    // 0x32: hardware-verified overlay, overrides the dump
    // 0x32: lower=0074; upper=0054
    // 0x32: chr=0032 from numeric binding (case 4)
    KeyRow { scan: 0x32, chars: ['t', 'T', '2', '2'], dead: 0b0000 },
    // 0x33: hardware-verified overlay, overrides the dump
    // 0x33: lower=0079; upper=0059
    // 0x33: chr=0033 from numeric binding (case 4)
    KeyRow { scan: 0x33, chars: ['y', 'Y', '3', '3'], dead: 0b0000 },
    // 0x34: hardware-verified overlay, overrides the dump
    // 0x34: lower=0066; upper=0046
    // 0x34: chr=0034 from numeric binding (case 4)
    KeyRow { scan: 0x34, chars: ['f', 'F', '4', '4'], dead: 0b0000 },
    // 0x35: hardware-verified overlay, overrides the dump
    // 0x35: lower=0067; upper=0047
    // 0x35: chr=0035 from numeric binding (case 4)
    KeyRow { scan: 0x35, chars: ['g', 'G', '5', '5'], dead: 0b0000 },
    // 0x36: hardware-verified overlay, overrides the dump
    // 0x36: lower=0068; upper=0048
    // 0x36: chr=0036 from numeric binding (case 4)
    KeyRow { scan: 0x36, chars: ['h', 'H', '6', '6'], dead: 0b0000 },
    // 0x37: hardware-verified overlay, overrides the dump
    // 0x37: lower=0076; upper=0056
    // 0x37: chr=0037 from numeric binding (case 4)
    KeyRow { scan: 0x37, chars: ['v', 'V', '7', '7'], dead: 0b0000 },
    // 0x38: hardware-verified overlay, overrides the dump
    // 0x38: lower=0062; upper=0042
    // 0x38: chr=0038 from numeric binding (case 4)
    KeyRow { scan: 0x38, chars: ['b', 'B', '8', '8'], dead: 0b0000 },
    // 0x39: hardware-verified overlay, overrides the dump
    // 0x39: lower=006E 00F1; upper=004E 00D1
    // 0x39: chr=0039 from numeric binding (case 4)
    KeyRow { scan: 0x39, chars: ['n', 'N', '9', '9'], dead: 0b0000 },
    // 0x41: lower=0061 00E3 00E1 00E0 00E2 00AA 00E4 00E6; upper=0041 00C3 00C1 00C0 00C2 00AA 00C4 00C6
    KeyRow { scan: 0x41, chars: ['a', 'A', '\0', '\0'], dead: 0b0000 },
    // 0x43: lower=0063 00E7; upper=0043 00C7
    KeyRow { scan: 0x43, chars: ['c', 'C', '\0', '\0'], dead: 0b0000 },
    // 0x44: lower=0064; upper=0044
    KeyRow { scan: 0x44, chars: ['d', 'D', '\0', '\0'], dead: 0b0000 },
    // 0x45: lower=0065 00E9 00EA 00E8 00EB; upper=0045 00C9 00CA 00C8 00CB
    KeyRow { scan: 0x45, chars: ['e', 'E', '\0', '\0'], dead: 0b0000 },
    // 0x49: hardware-verified overlay, overrides the dump
    // 0x49: lower=0069 00ED 00EE 00EC 00EF; upper=0049 00CD 00CE 00CC 00CF
    // 0x49: chr=002B from numeric binding (case 4)
    KeyRow { scan: 0x49, chars: ['i', 'I', '+', '+'], dead: 0b0000 },
    // 0x4B: hardware-verified overlay, overrides the dump
    // 0x4B: lower=006B; upper=004B
    KeyRow { scan: 0x4B, chars: ['k', 'K', '-', '-'], dead: 0b0000 },
    // 0x4C: lower=006C; upper=004C
    KeyRow { scan: 0x4C, chars: ['l', 'L', '\0', '\0'], dead: 0b0000 },
    // 0x4F: lower=006F 00F3 00F5 00F4 00BA 00F2; upper=004F 00D3 00D5 00D4 00BA 00D2
    KeyRow { scan: 0x4F, chars: ['o', 'O', '\0', '\0'], dead: 0b0000 },
    // 0x50: lower=0070; upper=0050
    KeyRow { scan: 0x50, chars: ['p', 'P', '\0', '\0'], dead: 0b0000 },
    // 0x51: lower=0071; upper=0051
    KeyRow { scan: 0x51, chars: ['q', 'Q', '\0', '\0'], dead: 0b0000 },
    // 0x53: lower=0073 00DF; upper=0053
    KeyRow { scan: 0x53, chars: ['s', 'S', '\0', '\0'], dead: 0b0000 },
    // 0x57: lower=0077; upper=0057
    KeyRow { scan: 0x57, chars: ['w', 'W', '\0', '\0'], dead: 0b0000 },
    // 0x58: lower=0078; upper=0058
    KeyRow { scan: 0x58, chars: ['x', 'X', '\0', '\0'], dead: 0b0000 },
    // 0x5A: lower=007A; upper=005A
    KeyRow { scan: 0x5A, chars: ['z', 'Z', '\0', '\0'], dead: 0b0000 },
    // 0x79: lower=00E7; upper=00C7
    KeyRow { scan: 0x79, chars: ['\u{00E7}', '\u{00C7}', '\0', '\0'], dead: 0b0000 },
    // 0x7A: lower=F001 00B4; upper=F002 0060
    KeyRow { scan: 0x7A, chars: ['\u{00B4}', '`', '\0', '\0'], dead: 0b0011 },
    // 0x7D: lower=002E; upper=003A
    KeyRow { scan: 0x7D, chars: ['.', ':', '\0', '\0'], dead: 0b0000 },
    // 0x7E: lower=F003 007E; upper=F004 005E
    KeyRow { scan: 0x7E, chars: ['~', '^', '\0', '\0'], dead: 0b0011 },
    // 0x7F: hardware-verified overlay, overrides the dump
    // 0x7F: lower=006A; upper=004A
    // 0x7F: chr=0023 from numeric binding (case 4)
    KeyRow { scan: 0x7F, chars: ['j', 'J', '#', '#'], dead: 0b0000 },
    // 0x82: lower=002C; upper=003B
    KeyRow { scan: 0x82, chars: [',', ';', '\0', '\0'], dead: 0b0000 },
];

/// PtiEngine dead-key code -> the mark it carries.
///
/// With no FEP running, the window server hands these codes over as the character:
/// Shift and the `'` key on this handset arrive as `iCode == 0xF004`. So the code says
/// both "dead key" and "which mark", and the key it came from is irrelevant — which
/// is why this is keyed by code and not by key. Measured; see the module comment.
pub const DEAD_CODES: &[(u16, char)] = &[
    (0xF001, '\u{00B4}'),
    (0xF002, '`'),
    (0xF003, '~'),
    (0xF004, '^'),
];
