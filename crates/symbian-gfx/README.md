# symbian-gfx

The rasterizer. `no_std`, `forbid(unsafe_code)`, no allocation on any drawing path.

Everything is drawn into a 16-bit-per-pixel buffer that the caller owns — a `Vec<u16>`
on the host, the shim's staging buffer on a device. The crate knows nothing about
Symbian; the only concession to it is an explicit stride, because Symbian aligns
bitmap scanlines to 4 bytes and a 320-pixel-wide bitmap is not guaranteed to have
`stride == width`.

## Why 16bpp when the E72's screen is 32bpp

The device reports `EColor16MU` — 32 bits per pixel — but the canvas stays RGB565 and
is expanded once at present time.

A UI overdraws: a list row is painted, then a highlight over it, then text over that.
Drawing at 16bpp halves the memory traffic for every one of those passes, and on a
600 MHz ARM1136 with no write-combining to speak of, memory traffic is the budget. One
conversion of 76,800 pixels at the end costs less than doubling the cost of everything
before it.

`present::rgb565_to_xrgb8888` is that conversion, and `ScreenFormat::from_display_mode`
is what decides whether it is needed at all.

## Modules

| | |
|---|---|
| `geom` | `Point`, `Size`, `Rect`, `Edges`. Rects are **half-open**: `x1` and `y1` are one past the last pixel, so `width() == x1 - x0` with no off-by-one anywhere |
| `color` | `Color` (8-bit RGBA), `Rgb565`, and `blend565` |
| `canvas` | The drawing surface: clip and origin stack, fills, rounded rects, blits, text |
| `font` | The `.sbf` atlas format, the `Font` trait, `fit()` with ellipsis, `wrap()` |
| `present` | RGB565 → XRGB8888 for the final blit |

## Two things worth knowing before using it

**Clipping only ever shrinks.** `Canvas::with(rect, f)` clips to `rect` and moves the
origin to its top-left, so a widget draws from `(0, 0)` and *cannot* escape the box it
was given even if its arithmetic is wrong. There is no way for a child to widen its own
clip. On a platform where a stray write into the window server's bitmap is a reboot,
that is worth more than the convenience it costs.

**No floating point, anywhere.** `core` has no `f32::sqrt` — it lives in `std`, via
libm — and soft-float on this target is expensive enough to avoid on principle. So
rounded corners are computed with an integer square root in doubled coordinates
(`(2dx)² = (2rad)² - (2dy)²`, which clears the half-pixel), and gradients interpolate
with `Color::lerp` on 8-bit channels.

`blend565` is the one piece written for speed rather than clarity: it interleaves red
and blue into one 32-bit lane and green into another, so an alpha blend is two
multiply-adds instead of six.

## Fonts

`tools/mkfont.py` rasterizes a TTF into an `.sbf` atlas. Coverage is read from the
font's cmap via fontTools, **not** by asking Pillow whether it drew anything — for a
missing codepoint most fonts happily return the `.notdef` box, which has ink and sails
into the atlas as a tofu glyph. It did, the first time, and the delivery ticks rendered
as ▯▯.

`Font` is a trait so the atlas is not the only possible source. On device Symbian's own
`CFont` would give real hinted glyphs and full UCS-2 coverage for zero shipped bytes;
the atlas is what makes the host preview possible, and what guarantees coverage
regardless of which fonts a given handset shipped with.

## Seeing it without a device

```
cargo test -p symbian-gfx     # 39 tests
cargo run -p preview          # writes preview-out/*.png at 2x
```
