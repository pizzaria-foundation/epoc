//! Tests for the generated [`crate::layout_abnt2`] table.
//!
//! Its own file because `layout_abnt2.rs` is written by `tools/mkkeymap.py` and every
//! regeneration replaces it wholesale. Tests living there would be deleted by the next
//! dump — quietly, and at exactly the moment the table changed enough to need them.

use crate::layout::{Case, Keyboard, Layout, Press, Stroke};
use crate::layout_abnt2::{DEAD_CODES, ROWS};

/// Two rows for one physical key would make one of them unreachable, and which one
/// depends on iteration order. Cheap to rule out, and a generated table is exactly where
/// a duplicate would come from.
#[test]
fn scan_codes_are_unique() {
    for (i, a) in ROWS.iter().enumerate() {
        for b in &ROWS[i + 1..] {
            assert_ne!(a.scan, b.scan, "scan 0x{:02X} appears twice", a.scan);
        }
    }
}

/// A row that produces nothing in any case is dead weight the lookup still walks past,
/// and a sign the generator emitted an empty mapping.
#[test]
fn every_row_produces_something() {
    for r in ROWS {
        assert!(
            r.chars.iter().any(|&c| c != '\0'),
            "scan 0x{:02X} produces nothing in any case",
            r.scan
        );
    }
}

/// A dead-key bit set on a column with no character means the mask and the characters
/// disagree, which would arm an accent made of nothing.
#[test]
fn dead_bits_only_mark_columns_that_have_a_character() {
    for r in ROWS {
        for case in [Case::Lower, Case::Upper, Case::ChrLower, Case::ChrUpper] {
            if r.is_dead(case) {
                assert_ne!(
                    r.ch(case),
                    '\0',
                    "scan 0x{:02X} is dead in {case:?} but has no character",
                    r.scan
                );
            }
        }
    }
}

/// Everything the table can produce has to be renderable. `tools/mkfont.py` rasterizes
/// ASCII printable plus the Latin-1 supplement, and a character outside that goes into
/// the buffer and draws nothing — which reads as a swallowed keypress, the exact bug this
/// crate exists to remove. The generator drops these and says so; this is the backstop.
#[test]
fn every_character_is_inside_the_font_charset() {
    for r in ROWS {
        for &c in &r.chars {
            if c == '\0' {
                continue;
            }
            let cp = c as u32;
            assert!(
                (0x20..0x7F).contains(&cp) || (0xA0..0x100).contains(&cp),
                "scan 0x{:02X} maps {c:?} (U+{cp:04X}), outside the atlas charset",
                r.scan
            );
        }
    }
}

/// The regression this table has to survive: without Fn the twelve overlaid keys must
/// give letters, with Fn they must give what is printed on them. This is the behaviour
/// that was already working on hardware before the table existed, so breaking it is the
/// most likely cost of the whole change.
#[test]
fn the_overlaid_keypad_gives_letters_without_fn_and_digits_with_it() {
    let mut kb = Keyboard::new(Layout::Abnt2E72);
    // The window server did not translate these keys, so `code` is the digit — the exact
    // situation the table exists to correct.
    let press = |scan: u16, code: u16, shift, func| Press { scan, code, shift, func, ctrl: false };

    assert_eq!(kb.translate(press(0x31, 0x31, false, false)), Stroke::One('r'));
    assert_eq!(kb.translate(press(0x31, 0x31, true, false)), Stroke::One('R'));
    assert_eq!(kb.translate(press(0x31, 0x31, false, true)), Stroke::One('1'));
    assert_eq!(kb.translate(press(0x30, 0x30, false, false)), Stroke::One('m'));
    assert_eq!(kb.translate(press(0x30, 0x30, false, true)), Stroke::One('0'));

    // The two keys of the twelve that carry a symbol rather than a digit, where `iCode`
    // is the symbol. Treating them like the other ten is what made the J key produce `#`
    // forever, with no way to type a j at all.
    assert_eq!(kb.translate(press(0x7F, u16::from(b'#'), false, false)), Stroke::One('j'));
    assert_eq!(kb.translate(press(0x7F, u16::from(b'#'), false, true)), Stroke::One('#'));
    // The U key reports scan 0x2A, measured; 0x85 was an unconfirmed guess inherited from
    // the C++ table this replaced, and it made the U key type `*`.
    assert_eq!(kb.translate(press(0x2A, u16::from(b'*'), false, false)), Stroke::One('u'));
    assert_eq!(kb.translate(press(0x2A, u16::from(b'*'), true, false)), Stroke::One('U'));
    assert_eq!(kb.translate(press(0x2A, u16::from(b'*'), false, true)), Stroke::One('*'));
}

/// Every mark in DEAD_CODES is one `compose` knows, and reaches at least one vowel.
///
/// A dead key the composer has never heard of would arm itself and then swallow the next
/// letter, silently — the exact failure this crate was written to remove, reintroduced by a
/// generated table nobody read.
#[test]
fn every_dead_mark_can_actually_compose() {
    assert!(!DEAD_CODES.is_empty(), "a table with no marks composes nothing");
    for &(code, mark) in DEAD_CODES {
        assert!(
            (code & 0xFF00) == 0xF000 && (code & 0xFF) <= 5,
            "0x{code:04X} is not a PtiEngine dead-key code"
        );
        assert!(
            crate::compose::is_mark(mark),
            "0x{code:04X} carries {mark:?}, which compose.rs does not know"
        );
        assert!(
            "aeiounAEIOUN".chars().any(|b| crate::compose::compose(mark, b).is_some()),
            "{mark:?} composes with nothing"
        );
    }
}

/// The two paths to a dead key agree wherever they overlap.
///
/// A mark is reachable two ways, because the handset offers it two ways: the table says
/// which key and case is dead, and DEAD_CODES resolves a PtiEngine dead-key code whatever
/// key sent it. If the same mark came out differently through the two, one of them would be
/// silently wrong and typing would depend on which path a press happened to take.
#[test]
fn the_table_and_the_dead_codes_agree() {
    // No Vec: this crate is `no_std` with no allocator, so the two directions are checked
    // by scanning rather than by collecting.
    const CASES: [Case; 4] = [Case::Lower, Case::Upper, Case::ChrLower, Case::ChrUpper];
    let in_table = |mark: char| {
        ROWS.iter().any(|r| CASES.iter().any(|&c| r.is_dead(c) && r.ch(c) == mark))
    };

    let mut found = 0;
    for r in ROWS {
        for &c in &CASES {
            if !r.is_dead(c) {
                continue;
            }
            found += 1;
            let mark = r.ch(c);
            assert!(
                DEAD_CODES.iter().any(|&(_, m)| m == mark),
                "scan 0x{:02X} is dead in {c:?} carrying {mark:?}, which has no dead code",
                r.scan
            );
        }
    }
    assert!(found > 0, "no dead key in the table");

    for &(code, mark) in DEAD_CODES {
        assert!(
            in_table(mark),
            "0x{code:04X} carries {mark:?}, which no key in the table produces"
        );
    }
}

/// The whole point, end to end: the measured dead keys compose Portuguese.
///
/// Driven by *code*, as the handset drives it. Measured with keyprobe on a Brazilian E72:
///
///     chr F002      scan 007A mod 01      Shift and the `.` key -> grave
///     chr F004      scan 007E mod 01      Shift and the `'` key -> circumflex
#[test]
fn the_measured_dead_keys_compose_portuguese() {
    // A dead key press: the window server puts the dead-key code in `iCode`, so it arrives
    // through the ordinary character path. The scan code is deliberately one the table has a
    // row for, to prove the code wins over the row.
    let dead = |code: u16| Press { scan: 0x7A, code, shift: true, func: false, ctrl: false };
    let letter = |c: char| Press {
        scan: c.to_ascii_uppercase() as u16,
        code: c as u16,
        shift: c.is_ascii_uppercase(),
        func: false,
        ctrl: false,
    };

    for (code, base, want) in [
        (0xF003u16, 'a', 'ã'), // ~ + a  -> "não"
        (0xF003, 'o', 'õ'),
        (0xF004, 'e', 'ê'), // ^ + e  -> "você"
        (0xF004, 'o', 'ô'),
        (0xF001, 'a', 'á'),
        (0xF001, 'e', 'é'),
        (0xF001, 'A', 'Á'),
        (0xF002, 'a', 'à'),
    ] {
        let mut kb = Keyboard::new(Layout::Abnt2E72);
        assert_eq!(
            kb.translate(dead(code)),
            Stroke::None,
            "the dead key itself must insert nothing"
        );
        assert_eq!(
            kb.translate(letter(base)),
            Stroke::One(want),
            "code 0x{code:04X} + {base:?}"
        );
    }
}

/// The accent keys arm accents, and the period and comma are elsewhere.
///
/// This is the pair of facts that took a wrong turn to get right, so both are pinned here.
///
/// keyprobe shows the unshifted press of scan 0x7A arriving as `chr 002E '.'`, which looks
/// like proof that the key types a full stop. It is the opposite: 0x7A is `EStdKeyFullStop`,
/// the *US* period key, and a window server with no FEP running has no Brazilian character
/// to hand over, so it falls back to the US keymap. Overriding that fallback is this table's
/// entire job.
///
/// The dump settles it by listing the real period and comma keys separately — 0x7D and 0x82
/// — so trusting it about 0x7A displaces nothing. Two period keys and no acute key is not a
/// layout any keyboard has.
#[test]
fn the_accent_keys_arm_accents_and_the_punctuation_is_on_its_own_keys() {
    let mut kb = Keyboard::new(Layout::Abnt2E72);
    // What the window server actually sends for these presses: the US fallback character.
    let p = |scan: u16, code: u16, shift| Press { scan, code, shift, func: false, ctrl: false };

    // The ´ key, reported as a full stop, must arm the acute rather than type one.
    assert_eq!(kb.translate(p(0x7A, u16::from(b'.'), false)), Stroke::None);
    assert_eq!(kb.pending(), Some(crate::compose::ACUTE));
    assert_eq!(kb.translate(p(0x41, u16::from(b'a'), false)), Stroke::One('á'));

    // The ~ key, reported as an apostrophe.
    assert_eq!(kb.translate(p(0x7E, u16::from(b'\''), false)), Stroke::None);
    assert_eq!(kb.pending(), Some(crate::compose::TILDE));
    assert_eq!(kb.translate(p(0x41, u16::from(b'a'), false)), Stroke::One('ã'));

    // And the punctuation is reachable, on the keys the dump says own it.
    assert_eq!(kb.translate(p(0x7D, u16::from(b'.'), false)), Stroke::One('.'));
    assert_eq!(kb.translate(p(0x7D, u16::from(b'.'), true)), Stroke::One(':'));
    assert_eq!(kb.translate(p(0x82, u16::from(b','), false)), Stroke::One(','));
    assert_eq!(kb.translate(p(0x82, u16::from(b','), true)), Stroke::One(';'));
    assert_eq!(kb.pending(), None, "none of those armed an accent");
}

/// `ç` has a key of its own on this keyboard — scan 0x79, where a US layout puts the comma.
/// So it is not composed and must never be: `´`+`c` is `´c`.
#[test]
fn the_cedilla_key_types_cedilla() {
    let mut kb = Keyboard::new(Layout::Abnt2E72);
    let p = |shift| Press { scan: 0x79, code: 0x2C, shift, func: false, ctrl: false };
    assert_eq!(kb.translate(p(false)), Stroke::One('ç'));
    assert_eq!(kb.translate(p(true)), Stroke::One('Ç'));
}

/// `+` on Fn+I, which is where the handset's own numeric bindings put it.
///
/// This is the concrete bug `docs/task-login-screens.md` recorded: "The `+` cannot be
/// typed", in a screen that asks for a phone number with a country code.
#[test]
fn plus_can_be_typed() {
    let mut kb = Keyboard::new(Layout::Abnt2E72);
    let i = |func| Press { scan: 0x49, code: u16::from(b'i'), shift: false, func, ctrl: false };
    assert_eq!(kb.translate(i(false)), Stroke::One('i'));
    // Fn+I is '+' and Fn+K is '-', the two symbols printed on those keycaps. The dump carried the
    // plus and was silent about the minus, so nothing on this keyboard produced '-' at all until
    // OVERLAY in tools/mkkeymap.py was given both.
    assert_eq!(kb.translate(i(true)), Stroke::One('+'));
    let k = |func| Press { scan: 0x4B, code: u16::from(b'k'), shift: false, func, ctrl: false };
    assert_eq!(kb.translate(k(false)), Stroke::One('k'));
    assert_eq!(kb.translate(k(true)), Stroke::One('-'));
    assert_eq!(kb.translate(i(true)), Stroke::One('+'), "Fn+I is `+` on this handset");
}

/// The punctuation an ABNT2 keyboard moves. These are the rows that made the old behaviour
/// "kind of American": on a US layout 0x79 is the comma and 0x7A the full stop, and here
/// they are `ç` and the acute dead key, with `,` and `.` displaced to 0x82 and 0x7D.
#[test]
fn the_displaced_punctuation_is_where_the_device_says() {
    let mut kb = Keyboard::new(Layout::Abnt2E72);
    let p = |scan: u16, shift| Press { scan, code: 0x20, shift, func: false, ctrl: false };
    assert_eq!(kb.translate(p(0x82, false)), Stroke::One(','));
    assert_eq!(kb.translate(p(0x82, true)), Stroke::One(';'));
    assert_eq!(kb.translate(p(0x7D, false)), Stroke::One('.'));
    assert_eq!(kb.translate(p(0x7D, true)), Stroke::One(':'));
}

/// A key the table says nothing about still types, using the character the window server
/// translated. This is what makes the table safe to ship incomplete, and what protects
/// every handset that is not the one we measured.
#[test]
fn an_unmapped_key_falls_through_to_the_platform() {
    let mut kb = Keyboard::new(Layout::Abnt2E72);
    let e = Press { scan: 0x45, code: u16::from(b'e'), shift: false, func: false, ctrl: false };
    assert_eq!(kb.translate(e), Stroke::One('e'));
}
