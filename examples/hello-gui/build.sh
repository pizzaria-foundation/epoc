#!/bin/bash
# Build the GUI hello-world: sources and resources -> installable hello.sis.
#
# This is the verified recipe, kept as a script because every flag in it was
# arrived at by hitting the failure it prevents. tools/symbuild should absorb it
# once it grows resource and icon handling.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
SDK="$(cd "$HERE/../.." && pwd)"
EPOCROOT="$SDK/sdk"
HOST="$SDK/toolchain/host/bin"
CROSS="$SDK/toolchain/cross/bin"
TC=arm-none-symbianelf
LIB="$EPOCROOT/epoc32/release/armv5/lib"
STAT="$EPOCROOT/epoc32/release/armv5/urel"
BUILD="$HERE/build"

NAME=hello
UID3=0xE1234568

export PATH="$HOST:$PATH"
mkdir -p "$BUILD"
cd "$HERE"

# --------------------------------------------------------------- 1. resources
# epocrc is just cpp followed by rcomp, so we do both directly and need no Perl.
# hello.rss must come first: hello_reg.rss #includes the hello.rsg it generates.
echo "--> resources"
for rss in data/$NAME.rss data/${NAME}_reg.rss; do
  base="$(basename "$rss" .rss)"
  "$CROSS/$TC-g++" -E -P -x c++ -nostdinc \
    -I"$EPOCROOT/epoc32/include" -I data \
    -D_UNICODE -DLANGUAGE_SC "$rss" -o "$BUILD/$base.rpp"
  # -u for a Unicode resource binary; -h emits the .rsg of symbol ids.
  ( cd data && "$HOST/rcomp" -u -o"$BUILD/$base.rsc" -h"$base.rsg" -s"$BUILD/$base.rpp" )
  echo "    $base.rsc"
done

# ------------------------------------------------------------------- 2. icon
# 44x44 colour bitmap plus a 1bpp mask.
#
# Two bmconv quirks, both learned the hard way:
#   - the depth option is glued to the filename (/c24foo.bmp), not a separate arg;
#   - it parses DOS-style options, so it mistakes ANY argument beginning with `/`
#     for an option — an absolute Unix path silently becomes garbage. Everything
#     it touches has to be a relative path.
echo "--> icon"
if [ ! -f data/hello_icon.bmp ]; then
  python3 "$HERE/mkicon.py" data
fi
( cd data && "$HOST/bmconv" /q "../build/${NAME}_icon.mbm" \
    /c24hello_icon.bmp /1hello_icon_mask.bmp )
echo "    ${NAME}_icon.mbm"

# ------------------------------------------------------------------ 3. compile
# Flags mirror epoc32/tools/compilation_config/gcce.mk. The two non-obvious ones:
#
#   -D__SUPPORT_CPP_EXCEPTIONS__ / -fexceptions
#       symbian_os_v9.3.hrh switches __LEAVE_EQUALS_THROW__ off unless this macro
#       (which "the tools" are expected to define) is present. Without it TRAPD
#       falls back to the legacy TTrap mechanism, whose implementation is in no
#       .dso and no .lib in the public SDK, and the link fails on TTrap::Trap.
#
#   -Wno-narrowing
#       The SDK's own vwsdef.h brace-initialises a TUid from a value that does not
#       fit TInt32, which C++11 makes an error rather than a warning.
echo "--> compile"
"$CROSS/$TC-g++" -c -O2 -march=armv5t -msoft-float -mapcs -nostdinc \
  -include "$EPOCROOT/epoc32/include/gcce/gcce.h" \
  -I"$EPOCROOT/epoc32/include" -I"$EPOCROOT/epoc32/include/variant" \
  -Iinc -Idata \
  -D__SYMBIAN32__ -D__GCCE__ -D__EPOC32__ -D__MARM__ -D__EABI__ -D__MARM_ARMV5__ \
  -D__PRODUCT_INCLUDE__='<variant/symbian_os_v9.3.hrh>' \
  -D_UNICODE -DNDEBUG -D__EXE__ -D__SUPPORT_CPP_EXCEPTIONS__ \
  -fexceptions -fno-rtti \
  -Wall -Wno-narrowing -Wno-ctor-dtor-privacy -Wno-unknown-pragmas -Wno-attributes \
  src/$NAME.cpp -o "$BUILD/$NAME.o"

# --------------------------------------------------------------------- 4. link
# eexe.lib carries _E32Startup; usrt2_2.lib the user-side runtime. Both live in
# release/armv5/urel, not in lib/.
#
# The runtime import libraries must stay in this order: scppnwdl before
# drtrvct2_2, per the SDK's own gcce.mk, or operator new resolves to the wrong
# implementation.
#
# -lsupc++ supplies __gxx_personality_v0. Symbian's drtaeabi.dso covers the
# __cxa_* and __aeabi_unwind_cpp_pr* half of the ARM C++ ABI but not GCC's
# personality routine, and with exceptions on every TRAP needs it.
#
# No -Ttext: it fights -shared, relocating only .text and stranding everything
# else at its natural address. The linker script sets the base.
echo "--> link"
LG="$(dirname "$("$CROSS/$TC-gcc" -print-libgcc-file-name)")"
"$CROSS/$TC-ld" -shared --target1-abs --no-undefined -nostdlib \
  -u _E32Startup --default-symver \
  -T "$SDK/toolchain/symbian-exe.lds" \
  "$STAT/eexe.lib" "$BUILD/$NAME.o" \
  --start-group "$STAT/usrt2_2.lib" --end-group \
  -L"$LIB" \
  -l:euser.dso -l:apparc.dso -l:cone.dso -l:eikcore.dso -l:avkon.dso \
  -l:ws32.dso -l:fbscli.dso -l:bitgdi.dso -l:gdi.dso -l:fntstr.dso \
  -l:efsrv.dso -l:estor.dso \
  -l:dfpaeabi.dso -l:dfprvct2_2.dso -l:drtaeabi.dso -l:scppnwdl.dso -l:drtrvct2_2.dso \
  -L"$LG" -lsupc++ -lgcc \
  -o "$BUILD/$NAME.elf" 2>&1 | grep -v 'string table' || true

"$CROSS/$TC-readelf" -l "$BUILD/$NAME.elf" | grep -E '^  LOAD' | sed 's/^/    /'

# ------------------------------------------------- 4b. prepare the ELF for elf2e32
# Clears SHF_ALLOC on the link-time-only sections and zeroes the GOT, by editing
# the section header table in place. See tools/e32prep.py for why this is not
# objcopy: objcopy reclassified .rel.plt and threw away its 82 R_ARM_JUMP_SLOT
# relocations, losing 82 imports without a word of complaint.
echo "--> prepare ELF"
python3 "$SDK/tools/e32prep.py" "$BUILD/$NAME.elf"

# ------------------------------------------------------------------ 5. elf2e32
# uid1 = KExecutableImageUid, uid2 = KUidApp (a GUI application), uid3 = ours.
echo "--> elf2e32"
"$HOST/elf2e32" --uncompressed \
  --sid=$UID3 --uid1=0x1000007a --uid2=0x100039ce --uid3=$UID3 --vid=0x00000000 \
  --capability=none --fpu=softvfp --targettype=EXE \
  --linkas="$NAME{000a0000}.exe" --libpath="$LIB/" \
  --elfinput="$BUILD/$NAME.elf" --output="$BUILD/$NAME.exe" \
  --heap=0x1000,0x400000 --stack=0x8000
echo "    $NAME.exe $(stat -c%s "$BUILD/$NAME.exe") bytes"

# --------------------------------------------------------------- 5b. validate
# Nothing downstream of here can tell you the image is wrong. The phone refuses a
# malformed E32 by doing nothing at all — no error, no panic, no log — so an
# invalid header costs a full install round trip to discover. Fail here instead.
python3 "$SDK/tools/e32dump.py" "$BUILD/$NAME.exe" --quiet
echo "    header validated"

# ------------------------------------------------------------------- 6. makesis
echo "--> package"
cat > "$BUILD/$NAME.pkg" <<EOF
; Generated by build.sh. Unsigned: the dev handset runs a patched installserver.
&EN
#{"Rust Hello"},($UID3),1,0,0,TYPE=SISAPP
%{"RustSdkDev"}
:"RustSdkDev"
[0x101F7961],0,0,0,{"Series60ProductID"}
"$NAME.exe"        -"!:\\sys\\bin\\$NAME.exe"
; 10003a3f is AppArc's own SID and \`import\` is its writable drop-box. This exact
; path is what makes the app appear in the menu; without \`import\` the install
; succeeds and nothing ever shows up.
"${NAME}_reg.rsc"  -"!:\\private\\10003a3f\\import\\apps\\${NAME}_reg.rsc"
"$NAME.rsc"        -"!:\\resource\\apps\\$NAME.rsc"
"${NAME}_icon.mbm" -"!:\\resource\\apps\\${NAME}_icon.mbm"
EOF
( cd "$BUILD" && "$HOST/makesis" "$NAME.pkg" "$NAME.sis" )

mkdir -p "$SDK/out"
cp "$BUILD/$NAME.sis" "$SDK/out/hello-gui.sis"
echo
echo "==> $SDK/out/hello-gui.sis  ($(stat -c%s "$BUILD/$NAME.sis") bytes)"
echo "    Copy to the phone and open it from the file manager."
