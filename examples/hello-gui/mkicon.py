#!/usr/bin/env python3
"""Generate the icon bitmap and its mask for bmconv.

S60 3rd Edition wants a bitmap/mask pair in an .mbm (or an SVG-T in a .mif; the
bitmap route is simpler and bmconv is already known to work here). 44x44 is the
size the application shell draws in the menu grid.

Drawn at 4x and downsampled so the rounded corners are not jagged at 44px.
"""

import sys
from PIL import Image, ImageDraw

S = 44
F = 4  # supersampling factor
BLUE = (46, 143, 224)
WHITE = (255, 255, 255)


def main(outdir):
    img = Image.new("RGB", (S * F, S * F), (0, 0, 0))
    d = ImageDraw.Draw(img)
    d.rounded_rectangle([2 * F, 2 * F, (S - 2) * F, (S - 2) * F], radius=8 * F, fill=BLUE)

    # A stylised R: stem, bowl, counter, leg.
    d.rectangle([14 * F, 12 * F, 19 * F, 33 * F], fill=WHITE)
    d.rounded_rectangle([14 * F, 12 * F, 30 * F, 23 * F], radius=5 * F, fill=WHITE)
    d.rounded_rectangle([19 * F, 16 * F, 26 * F, 20 * F], radius=2 * F, fill=BLUE)
    d.polygon([(22 * F, 22 * F), (29 * F, 33 * F), (23 * F, 33 * F), (17 * F, 23 * F)],
              fill=WHITE)
    img.resize((S, S), Image.LANCZOS).save(f"{outdir}/hello_icon.bmp")

    # The mask marks which pixels are opaque. 1bpp, so threshold rather than
    # antialias — a grey mask pixel is not meaningful at this depth.
    m = Image.new("L", (S * F, S * F), 0)
    md = ImageDraw.Draw(m)
    md.rounded_rectangle([2 * F, 2 * F, (S - 2) * F, (S - 2) * F], radius=8 * F, fill=255)
    m.resize((S, S), Image.LANCZOS).convert("1").save(f"{outdir}/hello_icon_mask.bmp")

    print(f"{outdir}/hello_icon.bmp + hello_icon_mask.bmp ({S}x{S})")


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "data")
