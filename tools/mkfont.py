#!/usr/bin/env python3
"""Rasterize a TTF into the .sbf atlas that symbian-gfx reads.

Python rather than Rust because Pillow is already a competent hinted rasterizer
and this runs once at build time on the host. Nothing here ships to the device
except the .sbf bytes.

    python3 tools/mkfont.py --font /usr/share/fonts/google-noto/NotoSans-Regular.ttf \
                            --size 12 --out crates/symbian-ui/assets/ui12.sbf

Format is documented in crates/symbian-gfx/src/font.rs. Little-endian, a 16-byte
header, a codepoint-sorted index of 16-byte records, then the coverage blob.
"""

import argparse
import struct
import sys
import unicodedata

from PIL import Image, ImageDraw, ImageFont
from fontTools.ttLib import TTFont

MAGIC = b"SBF1"
FLAG_AA = 1

# Latin for the UI, Cyrillic because a Telegram client without it is useless, and
# a short list of symbols the widgets actually draw. Kept explicit rather than
# "everything in the font" so the atlas stays a predictable size.
#
# Size is the constraint that shapes this list. Three atlases are linked into the
# binary, and .rodata was 71% of a 160 KB image — bigger than all the code. The
# glyphs dropped from an earlier revision were Latin Extended-A (0x100-0x180) and
# Greek (0x370-0x400): 190 glyphs, a third of the total, for scripts a
# Portuguese/Russian client will not render. Pass --charset to get them back for a
# build that needs them.
#
# Arrows, triangles and stars are deliberately absent: the toolkit draws chevrons,
# scroll indicators and rating marks as canvas primitives. A glyph anti-aliased
# down to 9px is a grey smudge, whereas a chevron drawn as two 1px runs is exactly
# as sharp as the era's own iconography, which was hand-pixelled for the same
# reason. It also means U+2190..2193 stop mattering — neither Noto Sans nor Noto
# Sans Symbols 2 has them, so they were reported missing on every build.
def default_charset():
    cps = set()
    cps |= set(range(0x20, 0x7F))            # ASCII printable
    cps |= set(range(0xA0, 0x100))           # Latin-1 supplement: pt-BR lives here
    cps |= set(range(0x400, 0x460))          # Cyrillic
    cps |= {
        0x2018, 0x2019, 0x201C, 0x201D,      # curly quotes
        0x2013, 0x2014,                      # en/em dash
        0x2026,                              # ellipsis - font.ellipsis() wants it
        0x2022,                              # bullet
        0x00B7,                              # middot
        0x2713, 0x2714,                      # check marks - delivery ticks
        0x20AC,                              # euro
    }
    return sorted(cps)


def coverage(path):
    """The set of codepoints a font really has.

    Asking Pillow whether it rendered anything is not good enough: for a missing
    codepoint most fonts return the .notdef box, which has ink and would sail
    through into the atlas as a tofu glyph. The cmap is the only honest answer.
    """
    tt = TTFont(path, fontNumber=0, lazy=True)
    cps = set()
    for table in tt["cmap"].tables:
        cps |= set(table.cmap.keys())
    tt.close()
    return cps


def build(font_paths, size, charset, ascent_override=None):
    """Rasterize `charset`, taking each glyph from the first font that has it.

    The first path is the primary and sets the vertical metrics; the rest are
    fallbacks, which is how symbols get in — Noto Sans has no U+2713, so the
    delivery ticks come from Noto Sans Symbols 2.
    """
    faces = [(ImageFont.truetype(p, size), coverage(p), p) for p in font_paths]
    font, _, _ = faces[0]

    ascent, descent = font.getmetrics()
    if ascent_override is not None:
        ascent = ascent_override
    line_height = ascent + descent

    # One scratch canvas, reused. Generous margin so an overhanging glyph is never
    # clipped by the canvas rather than by its own bbox.
    pad = size * 2 + 8
    scratch = Image.new("L", (size * 4 + 16, pad * 2))

    records = []
    blob = bytearray()
    skipped = []

    for cp in charset:
        ch = chr(cp)
        face = next((f for f, cps, _ in faces if cp in cps), None)
        if face is None:
            # Omit entirely, so Font::glyph() reports None and the caller can fall
            # back deliberately instead of drawing a .notdef box.
            skipped.append(cp)
            continue

        bbox = face.getbbox(ch)
        advance = int(round(face.getlength(ch)))

        if bbox is None:
            x0 = y0 = x1 = y1 = 0
        else:
            x0, y0, x1, y1 = bbox
        w, h = max(0, x1 - x0), max(0, y1 - y0)

        if w == 0 or h == 0:
            # Whitespace: no ink, but it still advances the pen.
            records.append((cp, len(blob), 0, 0, advance, 0, 0))
            continue

        scratch.paste(0, (0, 0, scratch.width, scratch.height))
        d = ImageDraw.Draw(scratch)
        # Draw so the ink's top-left lands at (pad, pad); PIL's origin for
        # draw.text is the top of the ascender box, hence the -x0/-y0 shift.
        d.text((pad - x0, pad - y0), ch, fill=255, font=face)
        ink = scratch.crop((pad, pad, pad + w, pad + h))

        data = ink.tobytes()
        assert len(data) == w * h, (cp, len(data), w, h)

        # bearing_y is baseline-up to the top of the ink. The baseline sits at
        # y == ascent within PIL's box, and the ink starts at y0.
        records.append((cp, len(blob), w, h, advance, x0, ascent - y0))
        blob += data

    if any(r[2] > 255 or r[3] > 255 or r[4] > 255 for r in records):
        sys.exit("glyph exceeds the 255px field width; use a smaller --size")

    records.sort(key=lambda r: r[0])

    # fallback_advance: what a missing codepoint costs. Space is the least
    # surprising choice, and every UI font has one.
    fallback = next((r[4] for r in records if r[0] == 0x20), max(1, size // 2))

    out = bytearray()
    out += MAGIC
    out += struct.pack("<HhhHBBH", line_height, ascent, descent,
                       len(records), FLAG_AA, fallback, 0)
    for cp, off, w, h, adv, bx, by in records:
        out += struct.pack("<IIBBBBhh", cp, off, w, h, adv, 0, bx, by)
    out += blob

    return bytes(out), dict(glyphs=len(records), line_height=line_height,
                            ascent=ascent, descent=descent, blob=len(blob),
                            skipped=skipped)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--font", required=True, action="append",
                    help="repeat for a fallback chain; the first is the primary "
                         "and sets the vertical metrics")
    ap.add_argument("--size", type=int, required=True, help="pixel size")
    ap.add_argument("--out", required=True)
    ap.add_argument("--ascent", type=int, default=None,
                    help="override ascent; use to force a tighter line box")
    args = ap.parse_args()

    data, info = build(args.font, args.size, default_charset(), args.ascent)
    with open(args.out, "wb") as f:
        f.write(data)

    print(f"{args.out}: {len(data)} bytes, {info['glyphs']} glyphs "
          f"(blob {info['blob']}), line_height={info['line_height']} "
          f"ascent={info['ascent']} descent={info['descent']}")
    if info["skipped"]:
        names = []
        for cp in info["skipped"][:6]:
            try:
                names.append(f"U+{cp:04X} {unicodedata.name(chr(cp))}")
            except ValueError:
                names.append(f"U+{cp:04X}")
        print(f"  {len(info['skipped'])} codepoints absent from the font: "
              + ", ".join(names) + ("..." if len(info["skipped"]) > 6 else ""))


if __name__ == "__main__":
    main()
