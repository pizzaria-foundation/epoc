#!/bin/bash
# Regenerate every .sbf atlas the SDK ships. Run after changing mkfont.py's
# charset or metrics; the atlases are committed because the build embeds them and
# not every machine has the same fonts installed.
#
# Sizes, and what each is for:
#
#   ui9    small   timestamps, delivery state, the unread count in a badge
#   ui11   body    message text and list rows — the size almost everything uses
#   ui11b  strong  sender names, titles, softkey labels, unread rows
#
# And one that is not a size but a script:
#
#   uiemoji11      emoji, chained behind ui11 and ui11b at run time by
#                  symbian_gfx::WithFallback rather than merged into either.
#
# One shared atlas because emoji have no bold: Noto Emoji ships a single weight, so
# merging would store two byte-identical copies of every glyph. It is built at 11px with
# --ascent 12 to match ui11/ui11b, since bearings are measured from the ascent and a
# fallback whose baseline disagrees with its primary sits visibly off the line.
#
# ui9 gets no emoji: it draws timestamps and delivery ticks, which have none.
#
# 9/11 rather than 10/12 because the E72's panel is 320x240 across 2.36 inches:
# ~169 ppi, which is dense for a QVGA screen, so 11px lands at roughly the optical
# size 13px does on a period desktop. It is also what Nokia's own S60 3rd Edition
# layout data uses for the primary list font at this resolution.
#
# The three extra sizes (ui10, ui12, ui12b, ui13b) exist for the host preview at
# 2x and are not linked into the device binary.
set -euo pipefail

SDK="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$SDK/crates/symbian-ui/assets"
FONTS=/usr/share/fonts/google-noto
# The monochrome emoji face, not google-noto-color-emoji: the colour one is CBDT/COLRv1,
# and this rasterizer produces 8-bit coverage that the theme then tints. A colour font
# would come out as a silhouette anyway, via a much slower path.
EMOJI_FONT=/usr/share/fonts/google-noto-emoji-fonts/NotoEmoji-Regular.ttf

# Noto Sans has no U+2713 CHECK MARK, so the delivery ticks come from Symbols 2.
# The chain is ordered: first font with a given codepoint wins, and the first one
# listed sets the vertical metrics for the whole atlas.
REG=(--font "$FONTS/NotoSans-Regular.ttf" --font "$FONTS/NotoSansSymbols2-Regular.ttf")
BOLD=(--font "$FONTS/NotoSans-Bold.ttf" --font "$FONTS/NotoSansSymbols2-Regular.ttf")

for f in "$FONTS/NotoSans-Regular.ttf" "$FONTS/NotoSans-Bold.ttf" \
         "$FONTS/NotoSansSymbols2-Regular.ttf" "$EMOJI_FONT"; do
  [ -f "$f" ] || { echo "missing font: $f" >&2; exit 1; }
done

mkdir -p "$OUT"
gen() { # gen <size> <weight> <name>
  local args=("${REG[@]}")
  [ "$2" = bold ] && args=("${BOLD[@]}")
  python3 "$SDK/tools/mkfont.py" "${args[@]}" --size "$1" --out "$OUT/$3.sbf" \
    | sed 's/^/    /'
}

echo "--> device atlases (linked into every app; keep these small)"
gen 9  regular ui9
gen 11 regular ui11
gen 11 bold    ui11b
python3 "$SDK/tools/mkfont.py" --font "$EMOJI_FONT" --size 11 --ascent 12 \
  --charset emoji --out "$OUT/uiemoji11.sbf" | sed 's/^/    /'

echo "--> preview-only atlases (host rendering at 2x)"
gen 10 regular ui10
gen 12 regular ui12
gen 12 bold    ui12b
gen 13 bold    ui13b
# The 2x preview needs its own emoji atlas or it would draw blanks where the phone draws
# glyphs — a preview that flatters or maligns the device is worse than none. --ascent 13
# to match ui12/ui12b. Host-only, so its size does not matter.
python3 "$SDK/tools/mkfont.py" --font "$EMOJI_FONT" --size 12 --ascent 13 \
  --charset emoji --out "$OUT/uiemoji12.sbf" | sed 's/^/    /'

echo
printf 'device total: %s bytes\n' \
  "$(stat -c%s "$OUT"/ui9.sbf "$OUT"/ui11.sbf "$OUT"/ui11b.sbf "$OUT"/uiemoji11.sbf \
     | paste -sd+ | bc)"
