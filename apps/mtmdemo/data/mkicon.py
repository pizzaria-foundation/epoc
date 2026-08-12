#!/usr/bin/env python3
"""Generate the UI-data component's icons as Windows BMPs for bmconv.

Two icons, in the order mtmuidata.cpp's TMtmDemoIcon expects: message, then service.
CreateBitmapsL walks the .mbm from first to last, so the order here IS the enum.

Generated rather than committed for the same reason every other bitmap in this repo is: a
checked-in .bmp is a binary nobody can review, and the generator is the actual source.

Deliberately plain. The Messaging application on this handset resolves its own icons through
AknSkins and only falls back to a UI-data component's bitmaps, so the first question is
whether ours are consulted at all — and a recognisable shape answers that better than a
pretty one. A filled rounded square for a message, an outlined one for a service.
"""

import struct
import sys
import pathlib

SIZE = 22  # what the message list draws at on a 320x240 screen


def bmp(pixels, w, h):
    """24-bit BMP, bottom-up, rows padded to 4 bytes — what bmconv reads."""
    row_pad = (-w * 3) % 4
    raw = bytearray()
    for y in range(h - 1, -1, -1):
        for x in range(w):
            r, g, b = pixels[y][x]
            raw += bytes((b, g, r))
        raw += b"\0" * row_pad
    size = 54 + len(raw)
    return (
        b"BM" + struct.pack("<IHHI", size, 0, 0, 54)
        + struct.pack("<IiiHHIIiiII", 40, w, h, 1, 24, 0, len(raw), 2835, 2835, 0, 0)
        + bytes(raw)
    )


def rounded(fill, outline_only=False):
    bg = (255, 255, 255)
    px = [[bg] * SIZE for _ in range(SIZE)]
    m = 2
    for y in range(m, SIZE - m):
        for x in range(m, SIZE - m):
            corner = (x in (m, SIZE - m - 1)) and (y in (m, SIZE - m - 1))
            if corner:
                continue
            edge = x in (m, SIZE - m - 1) or y in (m, SIZE - m - 1)
            if outline_only and not edge:
                continue
            px[y][x] = fill
    return px


def main(outdir):
    out = pathlib.Path(outdir)
    # Order matters: it is TMtmDemoIcon.
    (out / "mtmdemo_msg.bmp").write_bytes(bmp(rounded((0, 90, 170)), SIZE, SIZE))
    (out / "mtmdemo_svc.bmp").write_bytes(bmp(rounded((0, 90, 170), True), SIZE, SIZE))


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else ".")
