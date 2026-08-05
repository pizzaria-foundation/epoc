#!/bin/bash
# Build a modern arm-none-symbianelf cross toolchain from upstream sources.
#
# Why upstream instead of the CodeSourcery tarballs GnuPoc documents: GCC still
# carries the arm*-*-symbianelf* target (gcc/config/arm/symbian.h + t-symbian),
# and the 2005/2011 Sourcery sources no longer build against a modern host GCC.
# Sourcery's download portal is also gone.
#
# Produces: $PREFIX/bin/arm-none-symbianelf-{gcc,g++,ld,as,ar,objdump,...}
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
PREFIX="${PREFIX:-$HERE/cross}"
SRC="${SRC:-$HERE/src}"
JOBS="${JOBS:-$(nproc)}"
TARGET=arm-none-symbianelf

BINUTILS_VER=2.45
GCC_VER=15.2.0

mkdir -p "$SRC" "$PREFIX"
cd "$SRC"

fetch() { # url
  local f="${1##*/}"
  [ -f "$f" ] || curl -fL --retry 5 --retry-delay 5 -C - -o "$f" "$1"
}

echo "=== [1/6] fetching sources ==="
fetch "https://ftp.gnu.org/gnu/binutils/binutils-$BINUTILS_VER.tar.xz"
fetch "https://ftp.gnu.org/gnu/gcc/gcc-$GCC_VER/gcc-$GCC_VER.tar.xz"

[ -d "binutils-$BINUTILS_VER" ] || tar xf "binutils-$BINUTILS_VER.tar.xz"
[ -d "gcc-$GCC_VER" ]           || tar xf "gcc-$GCC_VER.tar.xz"

echo "=== [2/6] restore arm-none-symbianelf in binutils ==="
# Upstream retired the triple; GCC still has the target. See
# patches/binutils-restore-symbianelf.patch for the full rationale.
# These edits are idempotent so re-running the script is safe.
bu="binutils-$BINUTILS_VER"
sed -i '/^ arm\*-\*-symbianelf\* | \\$/d' "$bu/bfd/config.bfd"
grep -q 'arm\*-\*-symbianelf\*)' "$bu/bfd/config.bfd" || \
  sed -i 's/^  arm\*-\*-eabi\* | arm-\*-rtems\* | arm\*-\*-uclinuxfdpiceabi)/  arm*-*-eabi* | arm*-*-symbianelf* | arm-*-rtems* | arm*-*-uclinuxfdpiceabi)/' \
    "$bu/bfd/config.bfd"
grep -q 'symbianelf' "$bu/gas/configure.tgt" || \
  sed -i 's/^  arm-\*-eabi\* | arm-\*-rtems\* | arm-\*-genode\*)/  arm-*-eabi* | arm-*-symbianelf* | arm-*-rtems* | arm-*-genode*)/' \
    "$bu/gas/configure.tgt"
grep -q 'symbianelf' "$bu/ld/configure.tgt" || \
  sed -i 's/^arm-\*-elf | arm\*-\*-eabi\* | arm-\*-rtems\* | arm-\*-genode\*)/arm-*-elf | arm*-*-eabi* | arm*-*-symbianelf* | arm-*-rtems* | arm-*-genode*)/' \
    "$bu/ld/configure.tgt"
for f in bfd/config.bfd gas/configure.tgt ld/configure.tgt; do
  grep -q symbianelf "$bu/$f" || { echo "FAILED to patch $bu/$f"; exit 1; }
  echo "  ok: $f"
done

echo "=== [3/6] gcc prerequisites (gmp/mpfr/mpc, in-tree) ==="
( cd "gcc-$GCC_VER" && [ -d gmp ] || ./contrib/download_prerequisites --directory=. ) || \
( cd "gcc-$GCC_VER" && ./contrib/download_prerequisites )

echo "=== [4/6] binutils ==="
if [ -x "$PREFIX/bin/$TARGET-ld" ]; then
  echo "  already installed, skipping"
else
  rm -rf build-binutils && mkdir build-binutils && cd build-binutils
  ../"binutils-$BINUTILS_VER"/configure \
    --target=$TARGET --prefix="$PREFIX" \
    --disable-nls --disable-werror --disable-gdb --disable-sim \
    --disable-libdecnumber --disable-readline \
    --with-sysroot
  make -j"$JOBS"
  make install
  cd "$SRC"
fi

export PATH="$PREFIX/bin:$PATH"

echo "=== [5/6] gcc (freestanding: no libc, Symbian supplies euser/scppnwdl) ==="
# arm-none-symbianelf is the only target in its config.gcc branch that gets no
# stdint header: arm*-*-eabi* gets newlib-stdint.h, rtems and phoenix get it too,
# symbianelf gets nothing. gcc/defaults.h then leaves INTPTR_TYPE and UINTPTR_TYPE
# as NULL, so __INTPTR_TYPE__ and __UINTPTR_TYPE__ are never predefined -- which
# breaks libgcc's coverage driver and libstdc++'s <functional> outright.
#
# newlib-stdint.h just says intptr_t == ptrdiff_t and uintptr_t == size_t, which is
# correct here (both are 32-bit on ARM32), and is exactly what the sibling targets
# use. Idempotent.
# The same omission leaves use_gcc_stdint at its default of "none", so no
# <stdint.h> is installed for the target at all and libsupc++'s new_opa.cc cannot
# compile. arm*-*-eabi* uses "wrap", which defers to a system header; we have no
# libc (--without-headers), so we want "provide" — GCC installs its own complete
# stdint.h from ginclude/stdint-gcc.h.
cfg="gcc-$GCC_VER/gcc/config.gcc"
if ! grep -q 'arm/symbian.h newlib-stdint.h' "$cfg"; then
  sed -i 's|tm_file="${tm_file} arm/symbian.h"|tm_file="${tm_file} arm/symbian.h newlib-stdint.h"\n\t  use_gcc_stdint=provide|' "$cfg"
  grep -q 'arm/symbian.h newlib-stdint.h' "$cfg" || { echo "FAILED to patch $cfg"; exit 1; }
  echo "  patched config.gcc: symbianelf now gets newlib-stdint.h + use_gcc_stdint"
fi
# Applied separately so a tree already carrying the tm_file edit still gets this.
if ! grep -q 'use_gcc_stdint=provide' "$cfg"; then
  sed -i 's|tm_file="${tm_file} arm/symbian.h newlib-stdint.h"|tm_file="${tm_file} arm/symbian.h newlib-stdint.h"\n\t  use_gcc_stdint=provide|' "$cfg"
  grep -q 'use_gcc_stdint=provide' "$cfg" || { echo "FAILED to set use_gcc_stdint in $cfg"; exit 1; }
  echo "  patched config.gcc: use_gcc_stdint=provide"
fi

rm -rf build-gcc && mkdir build-gcc && cd build-gcc
# GCC 15's libcody/libcpp predate GCC 16's default dialect, where u8"" literals
# became char8_t. Pin the host compiler to gnu++17 so the bootstrap builds.
../"gcc-$GCC_VER"/configure \
  CXX="g++ -std=gnu++17" CXX_FOR_BUILD="g++ -std=gnu++17" \
  --target=$TARGET --prefix="$PREFIX" \
  --enable-languages=c,c++ \
  --without-headers --with-newlib \
  --disable-nls --disable-shared --disable-threads \
  --disable-libssp --disable-libgomp --disable-libatomic \
  --disable-libquadmath --disable-libvtv \
  --disable-libmudflap --disable-libsanitizer \
  --disable-hosted-libstdcxx \
  --disable-decimal-float --disable-libffi \
  --with-gnu-as --with-gnu-ld \
  --disable-multilib \
  --with-arch=armv5te --with-float=soft
make -j"$JOBS" all-gcc
make install-gcc

# libgcc gives us the __aeabi_* helpers (idiv, uidiv, ldivmod, dadd, l2d).
#
make -j"$JOBS" all-target-libgcc
make install-target-libgcc
GCCLIBDIR="$PREFIX/lib/gcc/$TARGET/$GCC_VER"

# libsupc++ supplies __gxx_personality_v0. Symbian needs it: on 9.x a
# User::Leave IS a C++ throw and TRAP IS a catch, so the shim must be built with
# exceptions on. Symbian's own drtaeabi.dso covers the __cxa_* and
# __aeabi_unwind_cpp_pr* half of the ABI but not GCC's personality routine —
# which is exactly why the SDK's gcce.mk lists `-lsupc++ -lgcc`.
# --disable-hosted-libstdcxx above keeps this to the freestanding subset.
make -j"$JOBS" all-target-libstdc++-v3 || true
find "$TARGET" -name 'libsupc++.a' -exec cp {} "$GCCLIBDIR/" \;
[ -f "$GCCLIBDIR/libsupc++.a" ] || { echo "libsupc++.a was not produced"; exit 1; }
"$PREFIX/bin/$TARGET-strip" --strip-debug "$GCCLIBDIR/libsupc++.a"
cd "$SRC"

echo "=== [6/6] merge libgcc_eh.a into libgcc.a (GnuPoc fix_csl_gcc_eh) ==="
# Symbian links a single -lgcc; the EH helpers must live in it.
# libgcc_eh is merged into libgcc.a by --disable-shared, so this is usually a no-op.
for lib in $(find "$PREFIX" -name libgcc.a); do
  eh="$(dirname "$lib")/libgcc_eh.a"
  if [ -f "$eh" ]; then
    tmp=$(mktemp -d); ( cd "$tmp" && "$PREFIX/bin/$TARGET-ar" x "$eh" )
    "$PREFIX/bin/$TARGET-ar" r "$lib" "$tmp"/*.o
    rm -rf "$tmp"
    echo "  merged $eh -> $lib"
  fi
done

echo
echo "=== DONE ==="
"$PREFIX/bin/$TARGET-gcc" -v 2>&1 | tail -3
"$PREFIX/bin/$TARGET-gcc" -dM -E -x c /dev/null | grep -Ei 'symbian|__ARM_ARCH|__ELF__' || true
