#!/usr/bin/env bash
#
# Read the platform's startup / system-state Publish & Subscribe properties off the handset.
#
# WHY THIS EXISTS
#   Our launcher and daemons are launched during the boot's non-critical phase and behave
#   differently there because the system is not finished: AppArc answers NotReady, the window
#   server may not be serving, and the native Active Idle takes the screen out from under us.
#   Every workaround we have for that is a clock heuristic approximating one number the platform
#   already publishes. This reads that number.
#
# WHERE THE ADDRESSES COME FROM
#   Not from write-ups, and not from the public SDK — it ships no startupdomainpskeys.h (checked:
#   `sdk/epoc32/include` has only startupitem.hrh). They were measured in the emulator's own boot
#   binaries, which are x86 PE and therefore disassemblable on the host:
#
#     sdk/epoc32/release/winscw/udeb/{sysstart,startup,sysap,SPLASHSCREEN}.exe
#     sdk/epoc32/release/winscw/udeb/{aiidleint,aifw,StartupMediatorPlugin}.dll
#
#   aiidleint.dll, at 0x401f5b, constructs a property observer with
#       push 0x41                     <- key
#       push DWORD PTR ds:0x407224    <- category, = 0x101F8766
#   and its callback at 0x4025e0 compares the observed value against 0x68, 0x6d, 0x6e, 0x6f
#   (104, 109, 110, 111) before calling TryBringToForeground. sysstart.exe — the writer — pushes
#   the contiguous range 0x60..0x7f, so the state enum is based at 100, not at 0.
#
#   The key lists below are every key found by cross-referencing all 3729 emulator binaries
#   against each category UID. That is why they are sparse and grouped: the platform groups keys
#   by owner, and we are reading the whole map rather than guessing which one matters.
#
# WHAT AN ANSWER MEANS
#   The previous attempt at this read (docs/device-notes.md, "the key everyone names is not")
#   reported NotFound for `ps 0x101F8767 1` and `ps 0x101F8763 1`. That was the wrong SHAPE, not
#   an absent API: it used a key UID as a category and a small integer as a key. So NotFound from
#   this script is only informative if the controls below succeed — hence the controls.
#
# SAFETY
#   Reads only. `ps` and `cenrep` are RProperty::Get and CRepository::Get; there is no write
#   anywhere in this file, and nothing here can change the phone's state.
#
# USAGE
#   tools/probe-startup.sh                  # read the phone, print the raw transcript
#   tools/probe-startup.sh --dry-run        # print the commands without connecting
#   tools/probe-startup.sh --out FILE       # also save the transcript
#
set -uo pipefail

RSH="${RSH:-$HOME/Codigos/symbian/ADBian/client/rsh.py}"
DRY=0
OUT=""
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) DRY=1; shift ;;
    --out) OUT="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

# ---------------------------------------------------------------------------
# Controls. These must answer. Every one is either documented in a public SDK
# header or already used by shipping code of ours, so a failure here means the
# instrument is broken and nothing below it can be believed.
# ---------------------------------------------------------------------------
CONTROLS=(
  'ping'
  'hal'                             # EMachineUid — confirms which handset answered
  'ps 0x10205041 0x1'               # KHWRMBatteryLevel   (hwrmpowerstatesdkpskeys.h; symbian::device uses it)
  'ps 0x10205041 0x2'               # KHWRMBatteryStatus
  'ps 0x10205041 0x3'               # KHWRMChargingStatus — EChargingStatus* , 0..5
  'ps 0x101f75b6 0x100052c5'        # KUidPhonePwr        (sacls.h; category is KUidSystemCategory)
  'ps 0x101f75b6 0x100052c6'        # KUidSIMStatus
  'ps 0x101f75b6 0x100052c7'        # KUidNetworkStatus
  'ps 0x101f75b6 0x100052c9'        # KUidChargerStatus
  'cenrep 0x101f876c 0x1'           # KCoreAppUIsNetworkConnectionAllowed (CoreApplicationUIsSDKCRKeys.h)
)

# A key nothing on the phone can plausibly have defined. If this ALSO answers, the tool is
# inventing values and the whole run is void. Negative controls are cheap; a lying probe is not.
NEGATIVE=(
  'ps 0x101f8766 0x7ffe'
  'ps 0x101f8767 0x7ffe'
)

# ---------------------------------------------------------------------------
# The target: the startup / system-state category. Key 0x41 is the one aiidleint
# observes and the one most of the platform reads — if only one line of this
# script matters, it is that one.
# ---------------------------------------------------------------------------
STARTUP_KEYS=(0x1 0x2 0x3 0x4 0x10 0x11 0x12 0x18 0x31 0x32 0x33
              0x41 0x42 0x43 0x44 0x45 0x51 0x53 0x64 0x65 0x66 0x301 0x401)

# ---------------------------------------------------------------------------
# SysAp's own category. Included because it is the same measurement for free,
# and because key 0x501 — used by RLock.exe, autolock.exe, UsbWatcher and
# aknoldstylenotif — is the candidate for the autolock status that
# shim/src/shim_keylock.cpp records as unavailable on this handset.
# ---------------------------------------------------------------------------
COREAPP_KEYS=(0x1 0x2 0x3 0x4
              0x101 0x102 0x103 0x104 0x105 0x106 0x107 0x108 0x109
              0x110 0x111 0x112 0x113 0x114 0x115 0x116 0x117
              0x201 0x202 0x203 0x204 0x501)

CMDS=("${CONTROLS[@]}")
for k in "${STARTUP_KEYS[@]}";  do CMDS+=("ps 0x101f8766 $k"); done
for k in "${COREAPP_KEYS[@]}";  do CMDS+=("ps 0x101f8767 $k"); done
CMDS+=('ps 0x101fd657 0x1')        # KPSUidAiInformation key 1 — which UID the phone calls its idle
CMDS+=("${NEGATIVE[@]}")

if [ "$DRY" = 1 ]; then
  printf '%s\n' "${CMDS[@]}"
  echo "# ${#CMDS[@]} commands" >&2
  exit 0
fi

[ -x "$RSH" ] || [ -f "$RSH" ] || { echo "rsh.py not found at $RSH (set RSH=...)" >&2; exit 1; }

if [ -n "$OUT" ]; then
  python3 "$RSH" "${CMDS[@]}" 2>&1 | tee "$OUT"
else
  python3 "$RSH" "${CMDS[@]}"
fi
