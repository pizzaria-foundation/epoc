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

# Noto Sans has no U+2713 CHECK MARK, so the delivery ticks come from Symbols 2.
# The chain is ordered: first font with a given codepoint wins, and the first one
# listed sets the vertical metrics for the whole atlas.
REG=(--font "$FONTS/NotoSans-Regular.ttf" --font "$FONTS/NotoSansSymbols2-Regular.ttf")
BOLD=(--font "$FONTS/NotoSans-Bold.ttf" --font "$FONTS/NotoSansSymbols2-Regular.ttf")

for f in "$FONTS/NotoSans-Regular.ttf" "$FONTS/NotoSans-Bold.ttf" \
         "$FONTS/NotoSansSymbols2-Regular.ttf"; do
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

echo "--> preview-only atlases (host rendering at 2x)"
gen 10 regular ui10
gen 12 regular ui12
gen 12 bold    ui12b
gen 13 bold    ui13b

echo
printf 'device total: %s bytes\n' \
  "$(stat -c%s "$OUT"/ui9.sbf "$OUT"/ui11.sbf "$OUT"/ui11b.sbf | paste -sd+ | bc)"
