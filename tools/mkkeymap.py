#!/usr/bin/env python3
"""Turn a keydump from the handset into the static Rust keymap.

    tools/mkkeymap.py keymap.txt -o crates/symbian-keys/src/layout_abnt2.rs

# Why a generator and not a hand-written table

The SDK was treating a Brazilian E72's ABNT2 keyboard as a US QWERTY: no accents (`~`
then `a` typed `a`, so "não" could not be written) and no Chr/Fn symbol layer, so `+`
could not be typed at all. `docs/device-notes.md` records three rounds of on-device
debugging spent on keyboard behaviour that had been reasoned about instead of measured,
so the table this emits comes from the phone. `examples/keydump` reads the keymap out of
the handset's own `ptiengine.dll` and writes the file this script reads.

# The one structural fact this relies on

`TPtiKey`'s QWERTY values *are* `TStdScanCode` values. `PtiDefs.h` defines
`EPtiKeyQwertyA = 0x41`, which is `EStdKeyA`, and the punctuation keys by name:
`EPtiKeyQwertyComma = EStdKeyComma`, `EPtiKeyQwertyHash = EStdKeyHash`. So the key column
in the dump is already the `iScanCode` the shim receives, and no separate bridge is needed.
Confirmed with examples/keyprobe for the keys that mattered: the accent keys really do fire
scan 0x7A and 0x7E, and the J key really does fire 0x7F.

Two things the dump gets wrong about this handset, both found by measuring:

The overlaid phone keypad is the one place the dump is wrong about this handset, and it was
found by measuring: the window server reports the R key *as the 1 key*, and the U key as
scan 0x2A. Those twelve rows are hardware-verified — see OVERLAY — and override the dump,
which describes the platform's idealised 4x12 grid rather than the keyboard in the user's
hand.

Dead keys are taken two ways, because the handset offers them two ways. The dump says which
key and case is dead and which mark it carries, and that goes in the table. Separately, when
the window server *does* deliver a PtiEngine dead-key code as the character — which it does
for the shifted marks — DEAD_CODES resolves it by code, whatever key it came from. The two
agree where they overlap, and between them nothing is missed. See dead_marks().
"""

import argparse
import sys
from collections import defaultdict

# How the keymap data marks a dead key, copied from the platform's own test
# (`CPtiQwertyKeyMappings::IsDeadKeyCode`, inline in PtiKeyMappings.h): a code in
# 0xF000..0xF005, so at most six dead keys per layout, indexed by the low byte.
#
# Not `KPtiKeyDataDeadKeySeparator` (0xFFFF). That constant is real and is something else —
# it separates sections of the dead-key *table* blob — and looking for it is why the first
# run of this script found zero dead keys on a keyboard that plainly has four.
#
# The mark itself is the *next* code unit. The measured E72 dump reads:
#
#     K 007A lower 2 F001 00B4      the ´ key:  dead key 1, acute
#     K 007A upper 2 F002 0060      shifted:    dead key 2, grave
#     K 007E lower 2 F003 007E      the ~ key:  dead key 3, tilde
#     K 007E upper 2 F004 005E      shifted:    dead key 4, circumflex
#
# which is better than the index alone, because it means the mark comes from the device
# rather than from reading what is printed on the plastic.
#
# Those scan codes are `EStdKeyFullStop` and `EStdKeySingleQuote` — the *US* period and
# apostrophe keys. That is not a contradiction, it is the bug: an ABNT2 keyboard puts ´ and ~
# where a US one puts . and ', and a window server with no FEP running falls back to the US
# keymap and reports `.` and `'`. The real period and comma are on 0x7D and 0x82, which the
# dump lists separately, so nothing is displaced by trusting it here.
def is_dead_code(cp):
    return (cp & 0xFF00) == 0xF000 and (cp & 0xFF) <= 5

# Column order, matching `symbian_keys::layout::Case` and the platform's own
# TPtiTextCase. Kept in the platform's order on purpose — a reordering here would be a
# silent transposition in a generated file nobody reads.
CASES = ["lower", "upper", "chr-lower", "chr-upper"]

# The twelve keys of the E72's printed phone keypad, confirmed on a Brazilian handset and
# migrated from the KKeypadOverlay table that used to live in shim/src/shim_app.cpp.
#
#     1 2 3  ->  R T Y        7 8 9  ->  V B N
#     4 5 6  ->  F G H        * 0 #  ->  U M J
#
# scan -> (letter, fn_symbol). The window server identifies these physical keys as the
# digit keys, so the letter goes in the base columns and what is printed on the key goes
# in the Chr columns. These win over the dump: the dump describes the platform's generic
# 4x12 grid, these describe the keyboard in the user's hand.
OVERLAY = {
    0x31: ("r", "1"),
    0x32: ("t", "2"),
    0x33: ("y", "3"),
    # The U key reports scan 0x2A, measured with examples/keyprobe:
    #
    #     chr 002A '*'  scan 002A mod 00
    #
    # It used to be 0x85 (EStdKeyNkpAsterisk) here, inherited from the C++ table this
    # replaced and never confirmed against the device. Nothing matched 0x85, so the U key
    # fell through to the character the window server produced and typed `*`.
    #
    # The dump had said so all along — `N 002A 002A 4` — and this script skipped that row on
    # the theory that 0x2A was an ITU-T key id rather than a scan code this handset emits.
    # It is both.
    0x2A: ("u", "*"),
    0x34: ("f", "4"),
    0x35: ("g", "5"),
    0x36: ("h", "6"),
    0x7F: ("j", "#"),  # EStdKeyHash, confirmed: chr 0023 '#' scan 007F
    0x37: ("v", "7"),
    0x38: ("b", "8"),
    0x39: ("n", "9"),
    0x30: ("m", "0"),
    # I and K carry + and - on the Chr layer, printed on the keycaps.
    #
    # The dump has the plus (`0x49: chr=002B from numeric binding`) and is silent about the
    # minus, so Fn+K produced nothing and no key on this keyboard produced `-` at all. The
    # asymmetry is the dump's, not the hardware's: both symbols are on the keys in the user's
    # hand, which is the same reason the twelve rows above override it.
    #
    # I is repeated here rather than left to the dump so the pair reads as a pair. The value is
    # identical to what the dump already gives, so this changes nothing about that key.
    0x49: ("i", "+"),
    0x4B: ("k", "-"),
}

# What tools/mkfont.py rasterizes. A composed or mapped character outside this is worse
# than no mapping at all: it goes into the buffer and nothing appears on screen, which
# reads as a swallowed keypress.
def in_font(cp):
    return 0x20 <= cp < 0x7F or 0xA0 <= cp < 0x100


class Dump:
    """One parsed keydump file."""

    def __init__(self):
        # language tag -> {ptikey -> {case -> [code units]}}
        self.keys = defaultdict(lambda: defaultdict(dict))
        # language tag -> [(char, ptikey, case_index)]
        self.numeric = defaultdict(list)
        self.activate = {}
        self.warnings = []


def parse(path):
    d = Dump()
    lang = None
    with open(path, "r", encoding="ascii", errors="replace") as f:
        for lineno, raw in enumerate(f, 1):
            line = raw.strip()
            if not line:
                continue
            if line.startswith("#"):
                # `# language <tag> id <n>` switches which language the following rows
                # belong to. Tracking it matters: the dump deliberately contains an
                # English baseline as well, and merging the two would produce a table
                # that is a blend of two keyboards.
                parts = line.split()
                if len(parts) >= 3 and parts[1] == "language":
                    lang = parts[2]
                elif len(parts) >= 4 and parts[1] == "activate" and lang:
                    d.activate[lang] = int(parts[3])
                continue

            parts = line.split()
            if parts[0] == "K":
                if lang is None:
                    d.warnings.append(f"line {lineno}: key row before any language header")
                    continue
                # K <ptikey-hex> <case-name> <n> <unit> ...
                try:
                    key = int(parts[1], 16)
                    case = parts[2]
                    count = int(parts[3])
                    units = [int(u, 16) for u in parts[4:]]
                except (IndexError, ValueError):
                    d.warnings.append(f"line {lineno}: cannot parse: {line}")
                    continue
                if case not in CASES:
                    d.warnings.append(f"line {lineno}: unknown case {case!r}")
                    continue
                if len(units) != count:
                    d.warnings.append(
                        f"line {lineno}: declared {count} units, found {len(units)}"
                    )
                d.keys[lang][key][case] = units
            elif parts[0] == "N":
                if lang is None:
                    continue
                try:
                    d.numeric[lang].append(
                        (int(parts[1], 16), int(parts[2], 16), int(parts[3]))
                    )
                except (IndexError, ValueError):
                    d.warnings.append(f"line {lineno}: cannot parse: {line}")
            else:
                d.warnings.append(f"line {lineno}: unrecognised row: {line}")
    return d


def resolve(units):
    """The character a mapping produces, and whether the key is dead.

    Returns `(codepoint, is_dead)`, or `None` when the mapping carries no usable
    character.

    A mapping normally holds one character and often holds several: the platform's keymap
    data is shared with multitap, where a long press cycles through alternatives. The E72's
    A key carries `a ã á à â ª ä æ`. For a physical qwerty key the *first* is what a press
    produces, and this SDK does not implement long-press, so the rest are recorded in the
    generated comment and otherwise dropped.

    A dead key is the exception to "first unit wins": `F001 00B4` means the key is dead and
    the mark is `´`, so the character is the second unit.
    """
    if not units:
        return None
    if is_dead_code(units[0]):
        # Index alone would not name the mark; the unit after it does.
        #
        # This trusts the dump about which key and case is dead, and that took a wrong turn
        # to settle. keyprobe shows the unshifted press of scan 0x7A arriving as `chr 002E
        # '.'`, which looked like proof that the key types a full stop and the dump was
        # wrong. It is the opposite: 0x7A is `EStdKeyFullStop`, the *US* period key, and a
        # window server with no FEP running has no Brazilian character to hand over, so it
        # falls back to the US keymap. That fallback is the "keyboard is kind of American"
        # bug itself, not evidence about the key.
        #
        # The dump settles it by having a separate row for the real period key:
        #
        #     K 007D lower 1 002E      0x7D is the `.` key
        #     K 007A lower 2 F001 00B4 0x7A is the `´` key
        #
        # Two period keys and no acute key is not a layout any keyboard has. So the mark
        # goes in the column, and the character the window server invented is overridden —
        # which is the entire job of this table.
        return (units[1], True) if len(units) > 1 else None
    return units[0], False


def dead_marks(dump, lang):
    """PtiEngine dead-key code -> the mark it carries.

    Each dead mapping in the dump is the code followed by its character, so this pairing is
    the device's own:

        K 007A lower 2 F001 00B4     ->  F001 is acute
        K 007E upper 2 F004 005E     ->  F004 is circumflex

    This is the *only* thing the generator takes from the dump's dead mappings. The key and
    case they appear under are not usable: the dump lists scan 0x7A as dead-with-acute in
    its lower case, and the handset delivers a plain `.` for that press. Measured with
    keyprobe:

        chr 002E '.'  scan 007A mod 00      the key alone: a full stop
        chr F002      scan 007A mod 01      shifted:       dead, grave

    A table built on the dump's word would have made the full stop arm an accent. So the
    code identifies the mark and the *code* is what the shim reports; which key produced it
    does not matter, which also means the Fn layer resolves for free.
    """
    marks = {}
    conflicts = []
    for cases in dump.keys[lang].values():
        for units in cases.values():
            if len(units) >= 2 and is_dead_code(units[0]):
                code, mark = units[0], units[1]
                if code in marks and marks[code] != mark:
                    conflicts.append((code, marks[code], mark))
                marks[code] = mark
    return marks, conflicts


def build(dump, lang):
    """scan -> ([4 codepoints or None], dead mask), plus notes."""
    table = {}
    notes = {}

    def note(key, text):
        notes.setdefault(key, []).append(text)

    for key, cases in sorted(dump.keys[lang].items()):
        chars = [None, None, None, None]
        dead = 0
        raw = []
        # Only the two base cases come from here. See the chr-layer loop below for why the
        # chr columns are deliberately not taken from this data.
        for i, case in enumerate(CASES[:2]):
            units = cases.get(case)
            if not units:
                continue
            got = resolve(units)
            if got is None:
                note(key, f"{case}: mapping {units} carries no character")
                continue
            cp, is_dead = got
            if not in_font(cp):
                # Recorded rather than dropped silently: a character the atlas cannot draw
                # is a real finding about the font, not about the keymap.
                note(key, f"{case}: U+{cp:04X} is outside the font charset, dropped")
                continue
            chars[i] = cp
            if is_dead:
                dead |= 1 << i
            raw.append(f"{case}={' '.join(f'{u:04X}' for u in units)}")
        if any(c is not None for c in chars):
            table[key] = [chars, dead]
            notes.setdefault(key, []).insert(0, "; ".join(raw))
        else:
            note(key, "no character in any case — no row emitted")

    # ---------------------------------------------------------------- the chr layer --
    #
    # NOT from the K rows. On the measured E72, MappingDataForKey returns the *same* data
    # for EPtiCaseChrLower as for EPtiCaseLower — every chr-lower row in the dump duplicates
    # its lower row. Taking it at face value would put the letter in the chr column, so
    # Fn+R would type `r` and the digit would be unreachable: precisely the bug this table
    # exists to fix, faithfully reproduced from data that looked authoritative.
    #
    # The Chr layer is in the numeric bindings instead, which is also a better fit for what
    # this handset actually has. The E72's Fn layer is not a general symbol layer — it is
    # the printed phone keypad plus the characters a phone number needs. The dump's fifteen
    # bindings are the ten digits, `*`, `#`, `+`, and `p`/`w` (the pause and wait
    # characters).
    #
    # `p` and `w` come back with case 0, meaning "the ordinary lower-case P and W keys" —
    # they are telling us where those letters are, not adding an Fn binding. So only rows
    # in a chr case are taken; 0 and 1 are skipped.
    for ch, key, case in dump.numeric.get(lang, []):
        if case in (0, 1):
            continue
        if key not in table:
            # A binding for a key the keymap never described. The one real instance is `*`,
            # reported against the ITU-T star key id (0x2A) rather than a scan code this
            # handset emits — the physical key reports EStdKeyNkpAsterisk (0x85), which is
            # in OVERLAY and hardware-verified. Skipped and said out loud rather than
            # emitted as a row no key can ever match.
            note(key, f"numeric binding U+{ch:04X} for an undescribed key, skipped")
            continue
        if not in_font(ch):
            note(key, f"chr: U+{ch:04X} is outside the font charset, dropped")
            continue
        # Both chr columns. A digit has no shifted form, and Fn+Shift on this handset still
        # produces the digit.
        table[key][0][2] = ch
        table[key][0][3] = ch
        note(key, f"chr={ch:04X} from numeric binding (case {case})")

    # The overlay wins. See OVERLAY.
    for scan, (letter, symbol) in OVERLAY.items():
        table[scan] = [[ord(letter), ord(letter.upper()), ord(symbol), ord(symbol)], 0]
        notes.setdefault(scan, []).insert(
            0, "hardware-verified overlay, overrides the dump"
        )

    return {k: (v[0], v[1]) for k, v in table.items()}, notes


def rust_char(cp):
    """A Rust char literal.

    Escaped as `\\u{..}` for anything above ASCII rather than written as itself: the
    generated file's encoding then does not matter, and `'\\u{00E7}'` next to a comment
    saying `ç` is auditable where a lone `ç` in a file of unknown encoding is not.
    """
    if cp is None:
        return r"'\0'"
    if cp == 0x27:
        return r"'\''"
    if cp == 0x5C:
        return r"'\\'"
    if 0x20 <= cp < 0x7F:
        return f"'{chr(cp)}'"
    return f"'\\u{{{cp:04X}}}'"


def emit(table, notes, marks, lang, source, out):
    w = out.write
    w("//! The Brazilian (ABNT2) E72 keymap.\n//!\n")
    w("//! # GENERATED — do not edit\n//!\n")
    w(f"//! `tools/mkkeymap.py {source}`, language `{lang}`.\n//!\n")
    w("//! The source is a dump of the handset's own keymap, read out of its\n")
    w("//! `ptiengine.dll` by `examples/keydump`. Editing this file instead of\n")
    w("//! regenerating it detaches the table from the measurement it came from, which is\n")
    w("//! the failure mode `docs/device-notes.md` spends a whole section on.\n//!\n")
    w("//! `ptiengine` is the platform's keymap database, the layer *underneath* Avkon's\n")
    w("//! FEP — not the FEP itself, which this SDK deliberately does not use. It was asked\n")
    w("//! once, offline; nothing that ships imports it.\n\n")
    w("use crate::layout::KeyRow;\n\n")
    w("/// One row per physical key. `'\\0'` means the key produces nothing in that case.\n")
    w("///\n/// Column order is [`crate::layout::Case`]: lower, upper, chr-lower, chr-upper.\n")
    w("pub const ROWS: &[KeyRow] = &[\n")
    for scan in sorted(table):
        chars, dead = table[scan]
        for note in notes.get(scan, []):
            if note:
                w(f"    // 0x{scan:02X}: {note}\n")
        literal = ", ".join(rust_char(c) for c in chars)
        w(f"    KeyRow {{ scan: 0x{scan:02X}, chars: [{literal}], dead: 0b{dead:04b} }},\n")
    w("];\n\n")

    # Any key whose every case was a dead key produces no row, and its notes would otherwise
    # vanish along with it. They are the audit trail for the most surprising part of the
    # table, so they are kept.
    dropped = [k for k in sorted(notes) if k not in table]
    if dropped:
        w("// Keys with no ordinary character in any case — dead keys only, resolved by code\n")
        w("// below. What they type unshifted comes from the window server, unchanged.\n")
        for key in dropped:
            for n in notes[key]:
                if n:
                    w(f"//   0x{key:02X}: {n}\n")
        w("\n")

    w("/// PtiEngine dead-key code -> the mark it carries.\n")
    w("///\n")
    w("/// With no FEP running, the window server hands these codes over as the character:\n")
    w("/// Shift and the `'` key on this handset arrive as `iCode == 0xF004`. So the code says\n")
    w("/// both \"dead key\" and \"which mark\", and the key it came from is irrelevant — which\n")
    w("/// is why this is keyed by code and not by key. Measured; see the module comment.\n")
    w("pub const DEAD_CODES: &[(u16, char)] = &[\n")
    for code in sorted(marks):
        w(f"    (0x{code:04X}, {rust_char(marks[code])}),\n")
    w("];\n")


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("dump", help="the keymap.txt written by examples/keydump")
    ap.add_argument("-o", "--out", help="where to write the Rust table (default: stdout)")
    ap.add_argument(
        "--lang",
        default="brazilian-portuguese",
        help="which language tag in the dump to generate from",
    )
    ap.add_argument(
        "--check",
        action="store_true",
        help="report what the dump says and what could not be interpreted, and write nothing",
    )
    args = ap.parse_args()

    dump = parse(args.dump)
    if args.lang not in dump.keys:
        print(
            f"{args.dump}: no rows for language {args.lang!r} "
            f"(found: {', '.join(sorted(dump.keys)) or 'none'})",
            file=sys.stderr,
        )
        return 1

    rc = dump.activate.get(args.lang)
    if rc not in (0, None):
        print(
            f"{args.dump}: the engine refused {args.lang!r} with rc {rc} — "
            "this dump does not describe that layout",
            file=sys.stderr,
        )
        return 1

    table, notes = build(dump, args.lang)
    marks, conflicts = dead_marks(dump, args.lang)
    for code, a, b in conflicts:
        print(
            f"warning: dead code {code:04X} appears as U+{a:04X} and U+{b:04X}",
            file=sys.stderr,
        )

    # The failure an error code cannot catch: if the target language and the English
    # baseline describe the same keys with the same characters, the engine never switched
    # layouts and this dump is worthless even though every row parsed.
    other = next((l for l in dump.keys if l != args.lang), None)
    if other and dump.keys[other] == dump.keys[args.lang]:
        print(
            f"{args.dump}: {args.lang!r} and {other!r} are byte-identical — the engine "
            "did not switch language. Re-run examples/keydump.",
            file=sys.stderr,
        )
        return 1

    dead_keys = sorted(marks)
    if args.check or not args.out:
        for warning in dump.warnings:
            print(f"warning: {warning}", file=sys.stderr)
        print(f"language      {args.lang} (activate rc {rc})")
        print(f"keys mapped   {len(table)}")
        print(
            "dead marks   "
            + (
                ", ".join(f"{c:04X}=U+{marks[c]:04X}" for c in dead_keys)
                or "NONE — the table cannot compose anything"
            )
        )
        print(f"numeric rows  {len(dump.numeric.get(args.lang, []))}")
        plus = [b for b in dump.numeric.get(args.lang, []) if b[0] == ord("+")]
        print(f"'+' binding   {plus or 'MISSING'}")
        if args.check:
            return 0

    if args.out:
        with open(args.out, "w", encoding="utf-8") as f:
            emit(table, notes, marks, args.lang, args.dump, f)
        print(f"{args.out}: {len(table)} keys, {len(dead_keys)} dead")
    else:
        emit(table, notes, marks, args.lang, args.dump, sys.stdout)
    return 0


if __name__ == "__main__":
    sys.exit(main())
