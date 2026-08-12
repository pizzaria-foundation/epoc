#!/bin/bash
# Build the whole fleet, in the order the package needs.
#
# `symbuild` checks that every path in EXTRA_EXES and EXTRA_DLLS exists before it makes the
# .sis, so the launcher cannot be built until the probes and the DLL are. Doing that by hand
# is eight commands in a specific order, and getting it wrong reports a missing file rather
# than a stale one — which is the better failure, but still a failure that costs a minute
# every time.
#
#   apps/devdump/build.sh            build everything
#   apps/devdump/build.sh clean      and start over
#
# The result is apps/devdump/build/devdump.sis: the launcher, seven probes and one
# polymorphic DLL, in one package.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
SDK="$(cd "$HERE/../.." && pwd)"
ACTION="${1:-build}"

# The DLL first: the `dll` probe loads it at runtime, and the launcher packages it.
# Everything else is order-independent among itself, but all of it precedes the launcher.
PARTS=(
  "apps/dlltest"
  "apps/mtmdemo"
  "apps/devdump/probes/system"
  "apps/devdump/probes/libsweep"
  "apps/devdump/probes/caps"
  "apps/devdump/probes/dll"
  "apps/devdump/probes/net"
  "apps/devdump/probes/fs"
  "apps/devdump/probes/msg"
  "apps/devdump/probes/mtm"
  "apps/devdump/probes/ncn"
  "apps/devdump/probes/msvev"
  "apps/devdump"
)

for part in "${PARTS[@]}"; do
  echo
  echo "################ $part"
  "$SDK/tools/symbuild" "$SDK/$part" "$ACTION"
done

if [ "$ACTION" = build ]; then
  echo
  echo "==> $HERE/build/devdump.sis"
  echo
  echo "Install it, open Device dump, press Select, and wait. Then:"
  echo "    epoc db pull \"C:\\Data\\dump\\99-merged.txt\" ./dump.txt"
  echo "or take the whole C:\\Data\\dump\\ directory off over USB or Bluetooth."
fi
