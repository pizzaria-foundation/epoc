#!/bin/bash
# Build two minimal probes into one SIS, to answer two questions in one install.
#
#   probe1 - linked the way hello-gui is: all the ELF dynamic metadata (.hash,
#            .dynsym, .dynstr, .rel.dyn, ...) is SHF_ALLOC, so elf2e32 folds ~18 KB
#            of it into the E32 code segment.
#
#   probe2 - identical, but those sections have ALLOC cleared before elf2e32 runs,
#            so the code segment holds only .text/.rodata/.ARM.*/.got — which is
#            what a real Symbian image contains. A genuine euser.dso has ER_RO as
#            its only allocated section; ours had eight.
#
# Whichever writes its file tells us which of the two is the blocker. Neither
# touches Avkon, so a failure of both points at the E32 format itself rather than
# at the GUI code.
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
LG="$("$CROSS/$TC-gcc" -print-libgcc-file-name)"; LG="$(dirname "$LG")"

export PATH="$HOST:$PATH"
mkdir -p "$BUILD"
cd "$HERE"

# Sections that exist only so the linker and elf2e32 can do their work. They must
# stay in the file (elf2e32 reads .dynsym and .rel.* to build the import and
# relocation tables) but must not be allocated, or they end up inside the code.
NOALLOC=(.hash .gnu.hash .dynsym .dynstr .gnu.version .gnu.version_d
         .gnu.version_r .rel.dyn .rel.plt .dynamic)

build_one() { # name uid3 strip_metadata
  local name=$1 uid3=$2 strip=$3
  echo "--> $name (metadata alloc: $([ "$strip" = yes ] && echo no || echo yes))"

  "$CROSS/$TC-g++" -c -O2 -march=armv5t -msoft-float -mapcs -nostdinc \
    -include "$EPOCROOT/epoc32/include/gcce/gcce.h" \
    -I"$EPOCROOT/epoc32/include" -I"$EPOCROOT/epoc32/include/variant" \
    -D__SYMBIAN32__ -D__GCCE__ -D__EPOC32__ -D__MARM__ -D__EABI__ -D__MARM_ARMV5__ \
    -D__PRODUCT_INCLUDE__='<variant/symbian_os_v9.3.hrh>' \
    -D_UNICODE -DNDEBUG -D__EXE__ -D__SUPPORT_CPP_EXCEPTIONS__ \
    -DKOutPath="_L(\"C:\\\\Data\\\\$name.txt\")" \
    -fexceptions -fno-rtti \
    -Wno-narrowing -Wno-ctor-dtor-privacy -Wno-unknown-pragmas -Wno-attributes \
    src/probe.cpp -o "$BUILD/$name.o"

  "$CROSS/$TC-ld" -shared --target1-abs --no-undefined -nostdlib \
    -u _E32Startup --default-symver \
    -T "$SDK/toolchain/symbian-exe.lds" \
    "$STAT/eexe.lib" "$BUILD/$name.o" \
    --start-group "$STAT/usrt2_2.lib" --end-group \
    -L"$LIB" -l:euser.dso -l:efsrv.dso \
    -l:dfpaeabi.dso -l:dfprvct2_2.dso -l:drtaeabi.dso -l:scppnwdl.dso -l:drtrvct2_2.dso \
    -L"$LG" -lsupc++ -lgcc \
    -o "$BUILD/$name.elf" 2>&1 | grep -v 'string table' || true

  if [ "$strip" = yes ]; then
    local args=()
    for s in "${NOALLOC[@]}"; do args+=(--set-section-flags "$s=readonly,contents"); done
    "$CROSS/$TC-objcopy" "${args[@]}" "$BUILD/$name.elf" "$BUILD/$name.stripped.elf"
    mv "$BUILD/$name.stripped.elf" "$BUILD/$name.elf"
  fi

  "$HOST/elf2e32" --uncompressed \
    --sid=$uid3 --uid1=0x1000007a --uid2=0x100039ce --uid3=$uid3 --vid=0x00000000 \
    --capability=none --fpu=softvfp --targettype=EXE \
    --linkas="$name{000a0000}.exe" --libpath="$LIB/" \
    --elfinput="$BUILD/$name.elf" --output="$BUILD/$name.exe" \
    --heap=0x1000,0x100000 --stack=0x4000
  echo "    codeSize $(python3 "$SDK/tools/e32dump.py" "$BUILD/$name.exe" | awk '/codeSize/{print $2}')"

  # Registration, so the app shows up in the menu and can be launched.
  cat > "$BUILD/${name}_reg.rss" <<RSS
#include <appinfo.rh>
UID2 KUidAppRegistrationResourceFile
UID3 $uid3
RESOURCE APP_REGISTRATION_INFO
    {
    app_file = "$name";
    hidden = KAppNotHidden;
    embeddability = KAppNotEmbeddable;
    newfile = KAppDoesNotSupportNewFile;
    launch = KAppLaunchInForeground;
    }
RSS
  "$CROSS/$TC-g++" -E -P -x c++ -nostdinc -I"$EPOCROOT/epoc32/include" \
    -D_UNICODE -DLANGUAGE_SC "$BUILD/${name}_reg.rss" -o "$BUILD/${name}_reg.rpp"
  "$HOST/rcomp" -u -o"$BUILD/${name}_reg.rsc" -s"$BUILD/${name}_reg.rpp"
}

build_one probe1 0xE1234570 no
build_one probe2 0xE1234571 yes

cat > "$BUILD/probe.pkg" <<'EOF'
&EN
#{"Rust Probe"},(0xE1234570),1,0,0,TYPE=SISAPP
%{"RustSdkDev"}
:"RustSdkDev"
[0x101F7961],0,0,0,{"Series60ProductID"}
"probe1.exe"      -"!:\sys\bin\probe1.exe"
"probe1_reg.rsc"  -"!:\private\10003a3f\import\apps\probe1_reg.rsc"
"probe2.exe"      -"!:\sys\bin\probe2.exe"
"probe2_reg.rsc"  -"!:\private\10003a3f\import\apps\probe2_reg.rsc"
EOF
( cd "$BUILD" && "$HOST/makesis" probe.pkg probe.sis )
cp "$BUILD/probe.sis" "$SDK/out/probe.sis"
echo
echo "==> $SDK/out/probe.sis  ($(stat -c%s "$BUILD/probe.sis") bytes)"
