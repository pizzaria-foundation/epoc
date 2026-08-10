# symbian-ui

The widget toolkit and the design system. `no_std`, `forbid(unsafe_code)`, 84 tests.

Lists, text fields, the screen chrome, 20 icons, five palettes, and a full-screen image
viewer - everything that decides what a 320x240 screen looks like.

## Why there is no widget tree

There is no retained tree here and no `Box<dyn View>`. A screen is a plain struct that
owns its state, handles a key event, and draws.

That is a trade, not an oversight. A widget tree buys composition and costs allocation,
indirection through trait objects, and a focus-traversal system. On a 320×240 screen
showing five rows at a time with one D-pad driving everything, composition is not the
problem — **arithmetic** is. Splitting the arithmetic out and unit-testing it catches
the bugs that actually happen: a scrollbar thumb one pixel past its track, a caret
landing inside a Cyrillic character, a list that scrolls to a row that no longer exists.

It also matches the platform. Avkon owns `CActiveScheduler::Start()`, so Rust is always
a callee — `handle_key` and `draw`, called from the shim, is the shape the platform
forces anyway.

## The design system

Three modules, in the order they depend on each other.

### `tokens` — the vocabulary

The visual unit of the S60 era was not a colour, it was a **band**: a shallow vertical
gradient with one pixel of lighter colour along its top edge and one of darker along its
bottom. Every title bar, softkey bar, highlight row and button was built from that one
shape, and it is easy to read as decoration and drop.

It was not decoration. S60 themes were user-installable and could put *any* background
behind a widget, so a flat fill could land on a wallpaper of the same lightness and
vanish. The light-top/dark-bottom pair is self-contrast: whatever the background does,
one of the two edges differs from it. That is why the era's icons all had a bevel too.

So `Surface` carries four colours, and `Surface::raised(base, strength)` derives all
four from one — which is what lets a theme be authored from a handful of colours rather
than four times as many, the same way `.attheme` and the S60 skin format both worked.

The colour *roles* come from Nokia's own skin table (`aknsconstants.h` in the S60 SDK),
which names ~60 entries by job rather than by hue: "navi pane texts", "left softkey
text", "list highlight text". That indirection is what let a theme swap without any
widget knowing. This module keeps the idea and trims it to what a chat client needs.

### `paint` — the primitives

`band`, `band_round`, `separator_for`, `frame_raised`/`frame_sunken`, `highlight`,
`scrollbar`, `pill`. Each is a handful of `fill_rect`/`hline` calls, kept in one place so
that "how a raised band looks" is decided once and every component inherits it.

Two decisions in here are deliberate and easy to undo by accident:

- **The selection highlight is full-bleed and square-cornered.** With no pointer, that
  band is the only thing telling you where you are, so it should be the loudest object
  on screen. A rounded inset pill reads as a *button you press* rather than as a cursor.
- **The scrollbar is always drawn**, never faded out. On a screen showing five rows of a
  fifty-row list, "where am I and how much is left" is not incidental information, and a
  bar that appears only while scrolling answers the question exactly when you have
  stopped needing it. A full-height thumb says "this is all of it", which is an answer.

### `icon` — 20 shapes, drawn as geometry

No emoji and no glyph font. Emoji are colour images a theme cannot recolour, ~60 KB of
atlas each, and at 11px a four-colour smudge. A glyph font is better but still
anti-aliased, so a 9px chevron arrives as three rows of grey. The era's icons were
hand-pixelled precisely because nothing else is sharp at this size.

So every shape is built from axis-aligned runs and 45° diagonals — the two things that
are exactly crisp on a pixel grid at any size — and takes a colour, so the theme owns
the appearance and there is nothing to ship.

Six of the twenty were redrawn after looking at them magnified, because the tests proved
containment and symmetry and said nothing about legibility:

| was | is | why |
|---|---|---|
| clock | hourglass | a 9px clock face has a 3px interior — no room for hands, so it rendered as a donut |
| pushpin | bookmark | a head, a neck and a needle in 9 rows reads as a dagger |
| bell + slash | speaker + cross | a bell is a dome over a flared body; at 9px both taper into the same triangle. And a slash drawn *across* the body merges with it, since there is only one colour |
| two figures | two heads, one body | two overlapping bodies merge into one lumpy blob |
| paperclip | page with folded corner | a clip is two nested U-turns and the inner one closes up |

`cargo run -p preview` writes `preview-out/21-icons-zoom.png`, which is the sheet that
found those.

## Themes

`Palette::ALL` is five: `DARK`, `LIGHT`, `S60`, `IRC` (monochrome green, flat on purpose),
and `HIGH_CONTRAST` (pure black and white, which exists as a test as much as a theme —
every widget must stay legible when the palette has no room for a subtle distinction).

The theme tests are the ones worth keeping. `every_palette_keeps_text_legible_on_its_own_surfaces`
checks luma contrast for every text-on-surface pair in every palette; nothing else in the
build would catch a transposed hex digit in a hand-authored constant table.

## Widgets

| | |
|---|---|
| `list` | `ListState`: selection clamping, `ensure_visible` that writes the clamp back, `for_visible`, proportional scrollbar geometry |
| `edit` | `TextField`: char-boundary-safe insert and delete, so a caret cannot land inside a multi-byte character |
| `chrome` | `Frame::split`, title bar, three-slot softkey bar, avatar, badge, placeholder |
| `input` | `Key`, `Softkey`, `Modifiers`, `Handled` |

## Metrics

Absolute pixels. 320×240 is the only target, there is no DPI to adapt to, and a scale
factor would add arithmetic that never pays off. An 18px title plus a 17px softkey bar
leaves 205px, which is five 38px rows and change — and there is a test asserting exactly
that, because "the list looks empty" is the failure mode of getting it wrong.

---

Part of [epoc](../../README.md), a Rust SDK for Symbian S60 3rd Edition. MIT licensed; see
`LICENSE` at the repository root. `symbian` in this crate's name is descriptive, not a claim
on somebody else's trademark - the repository README says more. Written with AI assistance,
and every hardware claim in it was measured rather than reasoned about.
