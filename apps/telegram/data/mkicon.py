#!/usr/bin/env python3
"""Generate telegram_icon.bmp and its mask for bmconv.

44x44 is what the S60 3rd Edition application shell draws in the menu grid.

The styling is deliberately of the period rather than flat: S60 3rd Ed icons sat
on a rounded square with a vertical gradient, a one-pixel lighter top edge and a
darker bottom edge, and a soft specular band across the upper third. Nokia's own
icon guidelines called for that bevel because the grid drew icons against a
skinned background of unpredictable brightness — the light top and dark bottom are
what keep the shape legible on both. A flat 2013-era circle looks wrong next to
Messaging and Contacts, which is the row this will actually sit in.

Everything is drawn at 4x and downsampled, so the rounded corners and the plane's
diagonals are not jagged at 44px.
"""

import sys

from PIL import Image, ImageDraw, ImageFilter

S = 44
F = 4  # supersampling factor
N = S * F

# Telegram's blue, with the gradient endpoints picked so the mid-tone lands on the
# brand colour rather than above or below it.
TOP = (94, 178, 240)
BOTTOM = (26, 116, 189)
EDGE_LIGHT = (168, 214, 247)
EDGE_DARK = (14, 78, 133)
WHITE = (255, 255, 255)
PLANE_SHADE = (222, 236, 248)

RADIUS = 9 * F
INSET = 2 * F


def rounded_mask(size, inset, radius, blur=0):
    m = Image.new("L", (size, size), 0)
    ImageDraw.Draw(m).rounded_rectangle(
        [inset, inset, size - 1 - inset, size - 1 - inset], radius=radius, fill=255
    )
    if blur:
        m = m.filter(ImageFilter.GaussianBlur(blur))
    return m


def vertical_gradient(size, top, bottom):
    g = Image.new("RGB", (1, size))
    px = g.load()
    for y in range(size):
        t = y / (size - 1)
        px[0, y] = tuple(round(a + (b - a) * t) for a, b in zip(top, bottom))
    return g.resize((size, size), Image.NEAREST)


def main(outdir):
    body = vertical_gradient(N, TOP, BOTTOM)

    # Specular band across the upper third: a wide, very soft ellipse. Subtle on
    # purpose — the era's icons suggested gloss, they did not mirror.
    gloss = Image.new("L", (N, N), 0)
    ImageDraw.Draw(gloss).ellipse(
        [-N // 4, -N // 2, N + N // 4, int(N * 0.42)], fill=70
    )
    gloss = gloss.filter(ImageFilter.GaussianBlur(N // 24))
    body = Image.composite(Image.new("RGB", (N, N), WHITE), body, gloss.point(lambda v: v // 3))

    d = ImageDraw.Draw(body)

    # The bevel: one scaled pixel of light along the top arc and dark along the
    # bottom. Drawn as two arcs rather than a full outline so the sides stay clean.
    box = [INSET, INSET, N - 1 - INSET, N - 1 - INSET]
    d.arc(box, start=180, end=360, fill=EDGE_LIGHT, width=F)
    d.arc(box, start=0, end=180, fill=EDGE_DARK, width=F)

    # A paper plane, as two facets: the lit upper surface and the shaded underside
    # fold. Two tones read as a folded sheet at 44px where one tone reads as a
    # triangle, and the fold is what makes the shape recognisable that small.
    def s(x, y):
        return (x * F, y * F)

    d.polygon([s(9, 22), s(35, 11), s(19, 26)], fill=WHITE)
    d.polygon([s(19, 26), s(35, 11), s(26, 34)], fill=PLANE_SHADE)
    # The tail notch: the trailing edge lifts back up, which is the detail that
    # separates a paper plane from an arrowhead.
    d.polygon([s(19, 26), s(26, 34), s(19, 31)], fill=(190, 214, 236))

    icon = Image.new("RGB", (N, N), (0, 0, 0))
    icon.paste(body, (0, 0), rounded_mask(N, INSET, RADIUS))
    icon.resize((S, S), Image.LANCZOS).save(f"{outdir}/telegram_icon.bmp")

    # The mask says which pixels are opaque. 1bpp, so threshold — a grey mask pixel
    # means nothing at this depth, and dithering the corners looks like dirt.
    rounded_mask(N, INSET, RADIUS).resize((S, S), Image.LANCZOS).point(
        lambda v: 255 if v >= 128 else 0
    ).convert("1").save(f"{outdir}/telegram_icon_mask.bmp")

    print(f"{outdir}/telegram_icon.bmp + telegram_icon_mask.bmp ({S}x{S})")


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else ".")
