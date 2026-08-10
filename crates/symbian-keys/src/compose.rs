//! Dead-key composition: `´` then `a` is `á`.
//!
//! # Why this is not the platform's job here
//!
//! On a Brazilian E72 the accent keys are *dead keys* — they produce no character of
//! their own, they modify the next one. Avkon's FEP does that composition, and we are
//! not part of the FEP, so what reaches the shim is a non-character key code
//! (`EKeyF21`, 0xF82A, for `~`) and then, separately, the plain letter. Nothing joins
//! them. `~` followed by `a` typed `a`, and no one could write "não".
//!
//! # Why the table is written here rather than extracted
//!
//! Which *keys* are dead is a property of the device's keymap and is measured — it
//! comes out of `examples/keydump` into [`crate::layout`]. But what `´` + `a` *means*
//! is Unicode, not Nokia: it is the same on every keyboard that has ever existed. So
//! the marks and the layout come from the device and the arithmetic lives here.
//!
//! # Why it stops at Latin-1
//!
//! `tools/mkfont.py` rasterizes `0x20..0x7F` and `0xA0..0x100`. A composition that
//! produced `ŷ` would be correct and invisible: the atlas has no glyph, so the field
//! would swallow the keystroke and look broken. Every pair below is inside Latin-1,
//! which covers Portuguese completely and Spanish, French, German and Italian nearly
//! so. Widening the table means widening the charset in the same commit.

/// Acute, U+00B4. `á é í ó ú`, and the mark Portuguese uses most.
pub const ACUTE: char = '\u{00B4}';
/// Grave, U+0060 — the ASCII backtick, which is what the keymap reports.
pub const GRAVE: char = '\u{0060}';
/// Circumflex, U+005E. `â ê ô`.
pub const CIRCUMFLEX: char = '\u{005E}';
/// Tilde, U+007E. `ã õ`, and `ñ` for Spanish.
pub const TILDE: char = '\u{007E}';
/// Diaeresis/trema, U+00A8. `ü`, and German and French.
pub const DIAERESIS: char = '\u{00A8}';

/// One mark's worth of the table, as two parallel strings.
///
/// Parallel strings rather than an array of triples because this is read by a human
/// far more often than by the compiler, and `"aeiou" -> "áéíóú"` can be checked at a
/// glance where forty tuples cannot. The two must have the same number of *chars*;
/// [`self_check`] is a test that says so, since a typo here would silently shift every
/// vowel by one and produce plausible nonsense.
struct Mark {
    mark: char,
    bases: &'static str,
    composed: &'static str,
}

/// `Ÿ` (U+0178) is deliberately absent from the diaeresis row: it is outside Latin-1
/// and so outside the font. `ÿ` (U+00FF) is inside it, so the lowercase survives — an
/// asymmetry that looks like a bug and is not.
const MARKS: &[Mark] = &[
    Mark { mark: ACUTE, bases: "aeiouyAEIOUY", composed: "áéíóúýÁÉÍÓÚÝ" },
    Mark { mark: GRAVE, bases: "aeiouAEIOU", composed: "àèìòùÀÈÌÒÙ" },
    Mark { mark: CIRCUMFLEX, bases: "aeiouAEIOU", composed: "âêîôûÂÊÎÔÛ" },
    Mark { mark: TILDE, bases: "aonAON", composed: "ãõñÃÕÑ" },
    Mark { mark: DIAERESIS, bases: "aeiouyAEIOU", composed: "äëïöüÿÄËÏÖÜ" },
];

/// The precomposed character for `mark` applied to `base`, if there is one.
///
/// `None` is not an error — it is the common case for `´` followed by `q`, and the
/// caller's job is then to emit both characters rather than to drop either. That is
/// what every other keyboard on earth does, and a user who typed a stray accent gets
/// to see it and delete it instead of wondering where the keypress went.
pub fn compose(mark: char, base: char) -> Option<char> {
    let row = MARKS.iter().find(|m| m.mark == mark)?;
    let at = row.bases.chars().position(|c| c == base)?;
    row.composed.chars().nth(at)
}

/// Whether `c` is one of the marks this module can compose with.
///
/// Only useful as a sanity check on layout data: whether a *key* is dead is decided by
/// the keymap, not by the character it carries. `~` is a dead key on the E72 and an
/// ordinary character on a US keyboard, and the same `char` arrives either way.
pub fn is_mark(c: char) -> bool {
    MARKS.iter().any(|m| m.mark == c)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one failure mode of parallel strings, caught mechanically.
    #[test]
    fn rows_are_the_same_length() {
        for m in MARKS {
            assert_eq!(
                m.bases.chars().count(),
                m.composed.chars().count(),
                "mark {:?} has {} bases and {} results",
                m.mark,
                m.bases.chars().count(),
                m.composed.chars().count()
            );
        }
    }

    /// Everything this table produces has to be renderable, or composing it is worse
    /// than not composing it: the character goes into the buffer and nothing appears.
    #[test]
    fn every_result_is_inside_the_font_charset() {
        for m in MARKS {
            for c in m.composed.chars() {
                let cp = c as u32;
                assert!(
                    (0x20..0x7F).contains(&cp) || (0xA0..0x100).contains(&cp),
                    "{c:?} (U+{cp:04X}) is outside the atlas charset in tools/mkfont.py"
                );
            }
        }
    }

    #[test]
    fn portuguese_is_complete() {
        // Every accented character Portuguese actually needs, lower and upper.
        for (mark, base, want) in [
            (ACUTE, 'a', 'á'),
            (ACUTE, 'e', 'é'),
            (ACUTE, 'i', 'í'),
            (ACUTE, 'o', 'ó'),
            (ACUTE, 'u', 'ú'),
            (ACUTE, 'A', 'Á'),
            (GRAVE, 'a', 'à'),
            (GRAVE, 'A', 'À'),
            (CIRCUMFLEX, 'a', 'â'),
            (CIRCUMFLEX, 'e', 'ê'),
            (CIRCUMFLEX, 'o', 'ô'),
            (CIRCUMFLEX, 'E', 'Ê'),
            (TILDE, 'a', 'ã'),
            (TILDE, 'o', 'õ'),
            (TILDE, 'A', 'Ã'),
            (DIAERESIS, 'u', 'ü'),
        ] {
            assert_eq!(compose(mark, base), Some(want), "{mark:?} + {base:?}");
        }
    }

    #[test]
    fn spanish_enye_and_nothing_else_on_that_row() {
        assert_eq!(compose(TILDE, 'n'), Some('ñ'));
        assert_eq!(compose(TILDE, 'N'), Some('Ñ'));
        // Tilde applies to three letters and no more: `~e` is not a character.
        assert_eq!(compose(TILDE, 'e'), None);
    }

    #[test]
    fn a_mark_that_does_not_apply_composes_to_nothing() {
        assert_eq!(compose(ACUTE, 'q'), None);
        assert_eq!(compose(ACUTE, ' '), None);
        assert_eq!(compose(ACUTE, '1'), None);
        assert_eq!(compose('x', 'a'), None);
    }

    #[test]
    fn marks_are_recognised() {
        for m in [ACUTE, GRAVE, CIRCUMFLEX, TILDE, DIAERESIS] {
            assert!(is_mark(m));
        }
        assert!(!is_mark('a'));
        assert!(!is_mark('\''));
    }
}
