# Which skin indices a theme actually repaints

Measured on the Nokia E72-2 by applying three themes in turn and reading every index back with
the skin probe. One log, seven runs, two theme switches inside it — so the comparisons below are
the same instrument on the same phone, not two dumps from two builds.

## Why this document exists

`docs/reference/skinprobe.txt` recorded *what colours the phone reports*. It sorted them by luma and
labelled the ones that looked like a page, a chrome, an accent and a warn, and those four became the
seeds `symbian-app`'s `theme_pref` reads.

Then a theme was applied on the phone and **nothing in any of our applications changed**. The code
was correct. The indices were not.

## The two families

The E72 keeps two kinds of entry in these tables, and only one of them belongs to the theme.

- **Saturated entries are platform constants.** `QsnOtherColors[8]` stayed `0x0099cc` and
  `QsnComponentColors[24]` stayed `0x751001` across *both* switches.
- **Neutral entries are the theme.** Twenty indices moved on both switches.

Three of the original four seeds were saturated. Picking seeds by saturation felt like picking the
colours that carry a design, and it was in fact the exact way to read the half of the table a theme
never touches. Under Golden not one of the four moved, so `from_device_seeds` derived a palette
identical to the default one.

## How much each switch moved

| step | indices compared | changed |
|---|---|---|
| default (runs 1–3) | 126 / 150 | — |
| default → Golden | 150 | **21** |
| Golden → pink (runs 6→7) | 126 | **101** |

`run2 → run3` changes **0** while the ruler grows from 126 to 150 indices, which is what rules out
"two skinprobe builds read different colours" as the explanation for the jumps. The jumps are themes.

Golden moves 21 and the pink theme moves 101 — a theme may be as sparse as it likes, so "which
indices move" is a property of the *pair*, and only indices that moved on **both** switches can be
trusted as theme-driven.

## The twenty that moved on both switches

Sorted by their luma under the default theme.

| index | default | Golden | pink | luma d / G / p |
|---|---|---|---|---|
| `QsnOtherColors[5]` | `0x000000` | `0xded3c7` | `0x8f6e75` | 0 / 212 / 117 |
| `QsnComponentColors[5]` | `0x030510` | `0x000000` | `0xffffff` | 5 / 0 / 255 |
| `QsnIconColors[7]` | `0x030510` | `0x000000` | `0xffffff` | 5 / 0 / 255 |
| `QsnTextColors[27]` | `0x281905` | `0x000000` | `0x281905` | 26 / 0 / 26 |
| `QsnOtherColors[9]` | `0x797979` | `0x87796d` | `0xcbaab1` | 121 / 123 / 177 |
| `QsnOtherColors[10]` | `0x797979` | `0x87796d` | `0xcbaab1` | 121 / 123 / 177 |
| `QsnOtherColors[12]` | `0x797979` | `0x87796d` | `0xcbaab1` | 121 / 123 / 177 |
| `QsnComponentColors[16]` | `0xafafaf` | `0xded3c7` | `0x8f6e75` | 175 / 212 / 117 |
| `QsnTextColors[20]` | `0xafafaf` | `0xded3c7` | `0x8f6e75` | 175 / 212 / 117 |
| `QsnLineColors[3]` | `0xafafaf` | `0xded3c7` | `0xfde5ec` | 175 / 212 / 234 |
| `QsnLineColors[4]` | `0xafafaf` | `0xded3c7` | `0xfde5ec` | 175 / 212 / 234 |
| `QsnLineColors[10]` | `0xafafaf` | `0xded3c7` | `0xfde5ec` | 175 / 212 / 234 |
| `QsnLineColors[2]` | `0xc0c0c0` | `0xffffff` | `0xdfc7ca` | 192 / 255 / 204 |
| `QsnLineColors[6]` | `0xc0c0c0` | `0xffffff` | `0xdfc7ca` | 192 / 255 / 204 |
| `QsnOtherColors[13]` | `0xc9c9c9` | `0xd4c9bd` | `0x4b3239` | 201 / 202 / 55 |
| `QsnTextColors[3]` | `0xf6f7f7` | `0xd4c9bd` | `0x8f6e75` | 246 / 202 / 117 |
| `QsnHighlightColors[2]` | `0xfffdff` | `0xffffff` | `0x8d636d` | 253 / 255 / 108 |
| `QsnComponentColors[6]` | `0xffffff` | `0xd4c9bd` | `0x8f6e75` | 255 / 202 / 117 |
| `QsnIconColors[8]` | `0xffffff` | `0xd4c9bd` | `0x8f6e75` | 255 / 202 / 117 |
| `QsnTextColors[29]` | `0xffffff` | `0x000000` | `0xffffff` | 255 / 0 / 255 |

Every one is neutral or lightly tinted. **There is no theme-driven saturated colour among them** —
in particular no red, which is why `warn` stays platform-fixed and `Palette::error` derives a legible
red from the page instead.

## The four seeds now read

| seed | index | why |
|---|---|---|
| page | `QsnComponentColors[5]` | flips to white under the pink theme, so it carries light-vs-dark |
| chrome | `QsnOtherColors[10]` | a theme-driven mid; `[9]` and `[12]` move identically |
| accent | `QsnHighlightColors[2]` | the highlight colour — the accent by role, not by resemblance |
| warn | `QsnComponentColors[24]` | immobile on purpose; a theme must not be able to hide an error |

`QsnComponentColors[5]` reads `0x030510` under the default theme, which is the same value the old
`QsnComponentColors[18]` read. Two independent indices agreeing on the page colour is the
corroboration that `[5]` is a background and not an inverted foreground — only one of the two follows
the theme.

## What the host can now prove

`theme_pref`'s tests carry these nine measured numbers and assert both halves:
`every_measured_phone_theme_derives_a_legible_palette` (all three pass `Palette::check`) and
`a_different_phone_theme_is_a_different_palette` (the three derive three *distinct* pages). The
second is the one that was missing — legibility was never the failure, **distinctness** was. Run with
the old four indices it fails, which is how it was confirmed to have teeth.

## Caveats worth carrying

- The installed `skinprobe` covers `QsnTextColors` only to index 39, so the old `chrome` index
  (`QsnTextColors[62]`) is **unmeasured** under the pink theme. It is no longer read, so this no
  longer blocks anything.
- Three themes is a small sample. A fourth could move an index these twenty do not contain, or
  freeze one they do. The claim here is only about `page`, `chrome` and `accent` moving on two
  independent switches.
