//! What a physical key means, keyed by scan code.
//!
//! # Why scan codes and not characters
//!
//! The window server hands the shim both an `iCode` (a character it already translated)
//! and an `iScanCode` (which physical key was pressed). For most keys the translation is
//! right and we should not second-guess it. For the ones this SDK cares about it is not
//! available at all: the E72 prints a phone keypad over twelve letter keys and the
//! window server reports those as *the digit keys*, leaving the letter identity to a FEP
//! we are not part of. And the whole Chr/Fn symbol layer — where `+` lives — is decided
//! by the same FEP, so `iCode` never carries it.
//!
//! A scan code identifies one physical key and nothing else, which is why the table is
//! keyed by it. That also retires a guard the shim needed: it used to test
//! `iCode == iScanCode` to tell "the window server did not translate this" from "it
//! did", because its table was keyed by *character*, so a real `R` key elsewhere could
//! collide with the overlaid `1`. Keyed by scan code there is no collision to guard
//! against.
//!
//! # Why there is a PassThrough layout at all
//!
//! We know one handset. Cravar its keymap as the only truth would break every other
//! device in a way that is hard to even notice: a key would quietly type the wrong
//! character. [`Layout::PassThrough`] is the honest default — use the character the
//! platform produced, exactly as before this crate existed — and a device-specific
//! table is opted into.

use crate::compose;

/// The four cases a key can be in, in the order the columns of [`KeyRow`] use them.
///
/// These are the platform's own four (`PtiDefs.h`: `EPtiCaseLower`, `EPtiCaseUpper`,
/// `EPtiCaseChrLower`, `EPtiCaseChrUpper`), and they are in the platform's order on
/// purpose: the generated table comes straight out of the device's keymap engine, and a
/// reordering here would be a silent transposition in a file no one reads.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Case {
    Lower = 0,
    Upper = 1,
    ChrLower = 2,
    ChrUpper = 3,
}

/// One physical key, in all four cases.
///
/// `'\0'` means "this key produces nothing in this case" — a real answer from the
/// keymap, not a missing entry. `Option<char>` would be the same size after niche
/// optimisation but reads worse in a generated file forty rows long.
pub struct KeyRow {
    pub scan: u16,
    /// Indexed by [`Case`].
    pub chars: [char; 4],
    /// One bit per [`Case`]: set means the character in that column is a *dead key* —
    /// it modifies the next keystroke instead of being inserted. Which keys are dead is
    /// measured from the device (`KPtiKeyDataDeadKeySeparator`), never inferred from the
    /// character: `~` is a dead key on this keyboard and an ordinary character on a US
    /// one, and the same `char` arrives either way.
    pub dead: u8,
}

impl KeyRow {
    pub const fn is_dead(&self, case: Case) -> bool {
        self.dead & (1 << case as u8) != 0
    }

    pub const fn ch(&self, case: Case) -> char {
        self.chars[case as usize]
    }
}

/// Which keymap to apply.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum Layout {
    /// Trust the character the window server produced. Identical to the behaviour
    /// before this crate existed, and the default for any handset we have not measured.
    #[default]
    PassThrough,
    /// The Brazilian (ABNT2) E72, measured with `examples/keydump`.
    Abnt2E72,
}

impl Layout {
    fn rows(self) -> &'static [KeyRow] {
        match self {
            Layout::PassThrough => &[],
            Layout::Abnt2E72 => crate::layout_abnt2::ROWS,
        }
    }

    fn dead_codes(self) -> &'static [(u16, char)] {
        match self {
            Layout::PassThrough => &[],
            Layout::Abnt2E72 => crate::layout_abnt2::DEAD_CODES,
        }
    }

    /// The mark a dead-key code carries, if this layout has one for it.
    ///
    /// The codes are PtiEngine's, in `0xF000..=0xF005` — six per layout, indexed by the low
    /// byte (`CPtiQwertyKeyMappings::IsDeadKeyCode`). The pairing of code to mark comes out
    /// of the device dump, where each dead mapping is the code followed by its character.
    ///
    /// This is the second of two paths to a dead key, and it exists because the handset uses
    /// both. Sometimes the window server delivers the dead-key code itself as the character —
    /// Shift and the `´` key arrive as `iCode == 0xF002` — and sometimes it has no idea what
    /// the key is and falls back to the US keymap, in which case the [`KeyRow`] for the scan
    /// code is what knows. Keyed by code here so it holds whatever key sent it.
    ///
    /// The two paths agree wherever they overlap, and a test in `tests_abnt2` fails if they
    /// ever stop agreeing — otherwise which character you got would depend on which path a
    /// press happened to take.
    pub fn dead_mark(self, code: u16) -> Option<char> {
        if (code & 0xFF00) != 0xF000 || (code & 0xFF) > 5 {
            return None;
        }
        self.dead_codes().iter().find(|(c, _)| *c == code).map(|(_, m)| *m)
    }

    /// The row for a physical key, if this layout has an opinion about it.
    ///
    /// Linear search over a few dozen rows, once per keystroke. A binary search would
    /// need the generated table to be sorted and gains nothing measurable against a
    /// human's typing speed; a match arm per key would be larger code for the same work.
    pub fn row(self, scan: u16) -> Option<&'static KeyRow> {
        self.rows().iter().find(|r| r.scan == scan)
    }
}

/// One key event as the shim reports it, before any interpretation.
///
/// `code` is the window server's `iCode` and `scan` its `iScanCode`; both are carried on
/// every event the shim pushes (`e.a` and `e.d`), so nothing new has to cross the ABI.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Press {
    pub scan: u16,
    pub code: u16,
    pub shift: bool,
    /// The Chr/Fn layer. The shim synthesises this — the platform's Fn key belongs to
    /// the FEP and its effect never reaches us, so the shim tracks the key itself.
    pub func: bool,
    /// Held Ctrl counts as Fn. It is a fallback that needs no state at all, and it
    /// predates the shim tracking Fn; keeping it costs one `||` and means a handset
    /// whose Fn key we cannot see is still fully typeable.
    pub ctrl: bool,
}

/// What a keystroke should insert.
///
/// `Two` exists for one case and it is not an edge case: a dead key followed by a letter
/// it cannot combine with has to produce *both* characters, in order. Dropping the mark
/// would make a mistyped accent vanish silently, which is exactly the bug this crate was
/// written to remove.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Stroke {
    /// Insert nothing. Either the key was a dead key and is now pending, or it carries
    /// no character at all.
    None,
    One(char),
    Two(char, char),
}

/// A keyboard with a layout and, at most, one pending dead key.
///
/// One pending mark and not a queue: no Latin keyboard stacks two accents on one letter,
/// and a queue would need a policy for what to do when it filled.
#[derive(Default)]
pub struct Keyboard {
    layout: Layout,
    pending: Option<char>,
}

impl Keyboard {
    pub const fn new(layout: Layout) -> Self {
        Keyboard { layout, pending: None }
    }

    pub const fn layout(&self) -> Layout {
        self.layout
    }

    pub fn set_layout(&mut self, layout: Layout) {
        self.layout = layout;
        // A mark composed under the old layout means nothing under the new one.
        self.pending = None;
    }

    /// The mark waiting for its letter, if any. For a caller that wants to show it —
    /// the platform underlines the pending accent, and a text field could too.
    pub const fn pending(&self) -> Option<char> {
        self.pending
    }

    /// Forget a pending mark. Call this on anything that is not a character: Backspace,
    /// a navigation key, losing focus. Otherwise an accent armed and abandoned three
    /// screens ago attaches itself to the next letter someone types.
    pub fn cancel(&mut self) {
        self.pending = None;
    }

    fn case_for(&self, p: &Press) -> Case {
        match (p.func || p.ctrl, p.shift) {
            (true, false) => Case::ChrLower,
            (true, true) => Case::ChrUpper,
            (false, false) => Case::Lower,
            (false, true) => Case::Upper,
        }
    }

    /// Resolve a press to the character the key means, and whether that character is a
    /// dead key.
    ///
    /// The fallback order matters. The layout wins where it has an answer, because the
    /// cases it exists for are exactly the ones the window server gets wrong. Where it
    /// has none — an unmapped key, or `PassThrough` — `code` is used unchanged, which is
    /// what keeps an unmeasured handset working.
    fn resolve(&self, p: &Press, fallback: bool) -> Option<(char, bool)> {
        // A dead-key code, first, because it is the least ambiguous thing that can arrive.
        // With no FEP running the window server sometimes hands the PtiEngine dead-key code
        // straight over as the character — Shift and the `´` key on a Brazilian E72 arrive as
        // `iCode == 0xF002` — and that code says both "dead key" and "which mark" with no
        // reference to the key it came from.
        //
        // When it does *not* do that, it falls back to the US keymap and the row below is
        // what knows. That fallback is subtle enough to have cost a wrong turn: an unshifted
        // press of scan 0x7A arrives as a plain `.`, which reads as "this key types a full
        // stop" and means "0x7A is EStdKeyFullStop on a US keyboard and this one is
        // Brazilian". Overriding it is the entire job of the table.
        if let Some(mark) = self.layout.dead_mark(p.code) {
            return Some((mark, true));
        }
        if let Some(row) = self.layout.row(p.scan) {
            let case = self.case_for(p);
            let ch = row.ch(case);
            if ch != '\0' {
                return Some((ch, row.is_dead(case)));
            }
        }
        if !fallback {
            return None;
        }
        // Symbian reserves 0xF800 and up for non-character key codes, and 0x7F is
        // Delete. Neither is text. The shim filters these too, but a caller that hands
        // us a raw event should not be able to insert a control character by accident.
        let cp = u32::from(p.code);
        if cp < 0x20 || cp == 0x7F || cp >= 0xF800 {
            return None;
        }
        char::from_u32(cp).map(|c| (c, false))
    }

    /// Translate one press, advancing the composition state.
    ///
    /// For an event the platform translated to a character. The layout wins where it has
    /// an answer — that is what the twelve overlaid keypad keys need — and otherwise the
    /// character in `code` is used unchanged, which is what keeps an unmeasured handset
    /// working.
    pub fn translate(&mut self, p: Press) -> Stroke {
        match self.resolve(&p, true) {
            Some((ch, dead)) => self.translate_resolved(ch, dead),
            // A key with no character cannot compose with a pending mark, but it also
            // should not silently discard it: navigation with an accent armed is what
            // `cancel` is for, and the caller decides.
            None => Stroke::None,
        }
    }

    /// Translate a press using the layout table *only*, never `code`.
    ///
    /// For an event that carries no character at all — on Symbian, a key the window server
    /// did not translate, which is how a dead key arrives: `~` comes through as `EKeyF21`,
    /// 0xF82A, and only its scan code identifies it.
    ///
    /// The distinction is not pedantry. `code` on such an event is not a character even
    /// when it looks like one, and falling back to it turns an unrecognised hardware key
    /// into whatever letter its key code happens to spell. That is a key typing text
    /// nobody pressed.
    pub fn translate_mapped(&mut self, p: Press) -> Stroke {
        match self.resolve(&p, false) {
            Some((ch, dead)) => self.translate_resolved(ch, dead),
            None => Stroke::None,
        }
    }

    /// The composition state machine on its own, for a caller that already knows what
    /// character a key produced and whether it is dead.
    ///
    /// Public because the host simulator is exactly that caller: it gets physical keys
    /// from minifb, not scan codes from a window server, so it does its own resolution
    /// and needs the same composition rules underneath — which is the whole point of
    /// this crate being shared rather than duplicated.
    pub fn translate_resolved(&mut self, ch: char, dead: bool) -> Stroke {
        match (self.pending.take(), dead) {
            // Ordinary typing, the overwhelmingly common path.
            (None, false) => Stroke::One(ch),

            // Arm the mark and insert nothing. This is the keystroke that used to
            // disappear.
            (None, true) => {
                self.pending = Some(ch);
                Stroke::None
            }

            // A second mark. Pressing `´` then `^` means the first one was a mistake or
            // is wanted literally: emit it and arm the new one, so neither keystroke is
            // lost and the user is where they expect to be. The same mark twice is the
            // conventional way to type the mark itself, and yields exactly one.
            (Some(mark), true) => {
                if mark != ch {
                    self.pending = Some(ch);
                }
                Stroke::One(mark)
            }

            (Some(mark), false) => {
                if ch == ' ' {
                    // Mark then space: the mark alone. The standard escape hatch, and
                    // the only way to type a bare `´` on a keyboard where it is dead.
                    Stroke::One(mark)
                } else if let Some(c) = compose::compose(mark, ch) {
                    Stroke::One(c)
                } else {
                    Stroke::Two(mark, ch)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::{ACUTE, CIRCUMFLEX, TILDE};

    /// A press of a plain letter key, as the window server reports one: it translated
    /// the character, so `code` carries it and `scan` is the uppercase scan code.
    fn letter(c: char) -> Press {
        Press {
            scan: c.to_ascii_uppercase() as u16,
            code: c as u16,
            shift: c.is_ascii_uppercase(),
            func: false,
            ctrl: false,
        }
    }

    /// A keyboard with `mark` already armed.
    ///
    /// Set directly rather than by pressing a dead key, so that these tests exercise the
    /// composition state machine without depending on the measured table having a dead
    /// key in it yet. `resolve` — the half that reads the table — is covered by the
    /// `passthrough_*` and `abnt2_*` tests instead.
    fn armed(mark: char) -> Keyboard {
        let mut kb = Keyboard::new(Layout::PassThrough);
        kb.pending = Some(mark);
        kb
    }

    #[test]
    fn passthrough_inserts_what_the_platform_translated() {
        let mut kb = Keyboard::new(Layout::PassThrough);
        assert_eq!(kb.translate(letter('a')), Stroke::One('a'));
        assert_eq!(kb.translate(letter('Q')), Stroke::One('Q'));
    }

    #[test]
    fn passthrough_ignores_non_characters() {
        let mut kb = Keyboard::new(Layout::PassThrough);
        // EKeyF21, the code a dead key arrives as when no layout claims it.
        let p = Press { scan: 0x74, code: 0xF82A, shift: false, func: false, ctrl: false };
        assert_eq!(kb.translate(p), Stroke::None);
        // Delete, and a control code.
        let p = Press { scan: 0x01, code: 0x7F, shift: false, func: false, ctrl: false };
        assert_eq!(kb.translate(p), Stroke::None);
        let p = Press { scan: 0x01, code: 0x08, shift: false, func: false, ctrl: false };
        assert_eq!(kb.translate(p), Stroke::None);
    }

    /// `translate_mapped` never invents text from a key code.
    ///
    /// The regression this guards: 0x4242 is a hardware key code and also, read as a
    /// scalar, the character 䉂. An unrecognised key on an unmeasured handset must stay
    /// unrecognised rather than start typing CJK.
    #[test]
    fn translate_mapped_does_not_fall_back_to_the_key_code() {
        let mut kb = Keyboard::new(Layout::PassThrough);
        let p = Press { scan: 0x0F01, code: 0x4242, shift: false, func: false, ctrl: false };
        assert_eq!(kb.translate_mapped(p), Stroke::None);
        // The same press through the translated path *is* a character, which is the whole
        // difference between the two entry points.
        assert_eq!(kb.translate(p), Stroke::One('\u{4242}'));
    }

    #[test]
    fn a_mark_and_a_vowel_compose() {
        let mut kb = armed(TILDE);
        assert_eq!(kb.translate(letter('a')), Stroke::One('ã'));
        assert_eq!(kb.pending(), None, "the mark is spent");

        let mut kb = armed(ACUTE);
        assert_eq!(kb.translate(letter('e')), Stroke::One('é'));

        let mut kb = armed(CIRCUMFLEX);
        assert_eq!(kb.translate(letter('O')), Stroke::One('Ô'));
    }

    #[test]
    fn a_mark_and_a_letter_it_cannot_reach_emits_both() {
        let mut kb = armed(ACUTE);
        assert_eq!(kb.translate(letter('q')), Stroke::Two(ACUTE, 'q'));
        assert_eq!(kb.pending(), None);
    }

    #[test]
    fn a_mark_and_space_is_the_mark() {
        let mut kb = armed(ACUTE);
        let space = Press { scan: 0x20, code: 0x20, shift: false, func: false, ctrl: false };
        assert_eq!(kb.translate(space), Stroke::One(ACUTE));
        assert_eq!(kb.pending(), None);
    }

    #[test]
    fn cancel_drops_a_pending_mark() {
        let mut kb = armed(ACUTE);
        kb.cancel();
        assert_eq!(kb.pending(), None);
        assert_eq!(kb.translate(letter('a')), Stroke::One('a'));
    }

    #[test]
    fn a_non_character_key_does_not_spend_a_pending_mark() {
        let mut kb = armed(TILDE);
        let up = Press { scan: 0x10, code: 0xF800, shift: false, func: false, ctrl: false };
        assert_eq!(kb.translate(up), Stroke::None);
        assert_eq!(kb.pending(), Some(TILDE), "arrow keys do not eat the accent");
        assert_eq!(kb.translate(letter('o')), Stroke::One('õ'));
    }

    /// Two marks in a row: the first is emitted literally, the second stays armed.
    #[test]
    fn a_second_mark_flushes_the_first() {
        let mut kb = armed(ACUTE);
        assert_eq!(kb.translate_resolved(CIRCUMFLEX, true), Stroke::One(ACUTE));
        assert_eq!(kb.pending(), Some(CIRCUMFLEX));
        assert_eq!(kb.translate(letter('a')), Stroke::One('â'));
    }

    /// The same mark twice is how you type the mark itself, and yields exactly one.
    #[test]
    fn the_same_mark_twice_yields_one_mark() {
        let mut kb = armed(ACUTE);
        assert_eq!(kb.translate_resolved(ACUTE, true), Stroke::One(ACUTE));
        assert_eq!(kb.pending(), None);
    }

    #[test]
    fn changing_layout_clears_composition_state() {
        let mut kb = armed(ACUTE);
        kb.set_layout(Layout::Abnt2E72);
        assert_eq!(kb.pending(), None);
    }

    #[test]
    fn case_selection_follows_shift_and_fn() {
        let kb = Keyboard::new(Layout::Abnt2E72);
        let p = |shift, func, ctrl| Press { scan: 0, code: 0, shift, func, ctrl };
        assert_eq!(kb.case_for(&p(false, false, false)), Case::Lower);
        assert_eq!(kb.case_for(&p(true, false, false)), Case::Upper);
        assert_eq!(kb.case_for(&p(false, true, false)), Case::ChrLower);
        assert_eq!(kb.case_for(&p(true, true, false)), Case::ChrUpper);
        // Ctrl is the stateless stand-in for Fn.
        assert_eq!(kb.case_for(&p(false, false, true)), Case::ChrLower);
        assert_eq!(kb.case_for(&p(true, false, true)), Case::ChrUpper);
    }

    #[test]
    fn dead_bits_address_the_right_column() {
        let row = KeyRow { scan: 1, chars: ['a', 'b', 'c', 'd'], dead: 1 << Case::ChrLower as u8 };
        assert!(!row.is_dead(Case::Lower));
        assert!(!row.is_dead(Case::Upper));
        assert!(row.is_dead(Case::ChrLower));
        assert!(!row.is_dead(Case::ChrUpper));
    }
}
