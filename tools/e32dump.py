#!/usr/bin/env python3
"""Decode an E32Image header, and verify it against what Symbian's own tools emit.

Written because Martin's elf2e32 replacement does not implement --e32input (the
dump mode), so there was no way to see what we had actually produced. Field layout
from tools/elf2e32/e32image.h; flag meanings from f32image.h.

Run with --quiet to use it as a build gate: it prints nothing on success and exits
non-zero the moment a field is inconsistent. Every check here exists because the
corresponding mistake actually happened and cost a round trip to the device, which
gives no diagnostic at all when it refuses an image — it simply does nothing.

The expected values come from a real armv5 binary carved out of the SDK's own
example SIS files with tools/sisextract.py, not from documentation.

    python3 tools/e32dump.py <file.exe> [--quiet]
"""

import struct
import sys
import zlib

# Symbian's conventional code base. Verified against SimulationPsy_ARMV5, built by
# the official toolchain. Ours drifted to 0xc748 once unallocated metadata was
# still consuming address space ahead of .text, and the loader silently refused it.
EXPECTED_CODE_BASE = 0x8000
# elf2e32 seeds the CRC field with this, CRCs the first 0x9c bytes, writes it back.
CRC_INITIALISER = 0xC90FDAA2
CRC_SIZE = 0x9C

HDR = "<12I 6i 3I i I i i 5I HH"
HDR_SIZE = struct.calcsize(HDR)

FLAGS = [
    (0x00000001, "KImageDll"),
    (0x00000002, "KImageNoCallEntryPoint"),
    (0x00000008, "KImageABI_EABI"),
    (0x00000020, "KImageEpt_Eka2"),
    (0x00000040, "KImageAllowDllData"),
    (0x00000080, "KImageOldJDiff"),
    (0x00000100, "KImageOldElfDiff"),
    (0x00100000, "KImageHWFloat_VFPv2"),
    (0x01000000, "KImageHdrFmt_J"),
    (0x02000000, "KImageHdrFmt_V"),
    (0x10000000, "KImageImpFmt_ELF"),
    (0x20000000, "KImageImpFmt_PE_Elf"),
]

UID1 = {0x1000007A: "KExecutableImageUid (EXE)",
        0x10000079: "KDynamicLibraryUid (DLL)"}
UID2 = {0x100039CE: "KUidApp (GUI application)",
        0x1000008D: "KSharedLibraryUid",
        0x00000000: "(none)"}
CPU = {0x2000: "ECpuArmV4", 0x2001: "ECpuArmV5", 0x2002: "ECpuArmV6"}


def uid_checksum(uid1, uid2, uid3):
    """Symbian's UID checksum: two CRC-CCITTs over the even and odd bytes.

    Reimplemented rather than shelled out to the SDK's uidcrc so the check has no
    external dependency and can run inside the build.
    """
    raw = struct.pack("<3I", uid1, uid2, uid3)
    even = bytes(raw[0::2])
    odd = bytes(raw[1::2])

    def ccitt(buf):
        crc = 0
        for b in buf:
            crc ^= b << 8
            for _ in range(8):
                crc = ((crc << 1) ^ 0x1021) & 0xFFFF if crc & 0x8000 else (crc << 1) & 0xFFFF
        return crc

    return (ccitt(odd) << 16) | ccitt(even)


def main(path, quiet=False):
    data = open(path, "rb").read()
    out = []
    _p = (lambda t="": out.append(t)) if quiet else (lambda t="": print(t))
    global print_
    print_ = _p
    if len(data) < HDR_SIZE:
        sys.exit("file too short to be an E32Image")

    f = struct.unpack_from(HDR, data, 0)
    (uid1, uid2, uid3, uidchk, sig, crc, modver, comp, tools,
     tlo, thi, flags, codeSize, dataSize, heapMin, heapMax, stackSize, bssSize,
     entry, codeBase, dataBase, dllRefs, expOff, expCount, textSize,
     codeOff, dataOff, impOff, codeRelocOff, dataRelocOff, prio, cpu) = f

    problems = []

    print_(f"{path}  ({len(data)} bytes)")
    print_(f"  uid1              0x{uid1:08x}  {UID1.get(uid1, '?? UNKNOWN')}")
    print_(f"  uid2              0x{uid2:08x}  {UID2.get(uid2, '?? UNKNOWN')}")
    print_(f"  uid3              0x{uid3:08x}")
    want_chk = uid_checksum(uid1, uid2, uid3)
    print_(f"  uid checksum      0x{uidchk:08x}  {'ok' if uidchk == want_chk else f'?? EXPECTED 0x{want_chk:08x}'}")
    if uidchk != want_chk:
        problems.append(f"uid checksum is 0x{uidchk:08x}, should be 0x{want_chk:08x}")
    sig_s = struct.pack("<I", sig).decode("latin1")
    print_(f"  signature         {sig_s!r}  {'ok' if sig_s == 'EPOC' else '?? EXPECTED EPOC'}")
    if sig_s != "EPOC":
        problems.append("signature is not 'EPOC'")
    # The loader validates this before anything else, so a wrong CRC is refused
    # with no message at all.
    probe = bytearray(data[:CRC_SIZE])
    struct.pack_into("<I", probe, 0x14, CRC_INITIALISER)
    want_crc = (~zlib.crc32(bytes(probe), 0xFFFFFFFF)) & 0xFFFFFFFF
    print_(f"  header crc        0x{crc:08x}  {'ok' if crc == want_crc else f'?? EXPECTED 0x{want_crc:08x}'}")
    if crc != want_crc:
        problems.append(f"header CRC is 0x{crc:08x}, should be 0x{want_crc:08x}")
    print_(f"  module version    0x{modver:08x}")
    print_(f"  compression       0x{comp:08x}  {'uncompressed' if comp == 0 else 'compressed'}")
    print_(f"  tools version     0x{tools:08x}")

    names = [n for bit, n in FLAGS if flags & bit]
    print_(f"  flags             0x{flags:08x}  {' | '.join(names) if names else '(none)'}")
    for required in ("KImageEpt_Eka2", "KImageABI_EABI", "KImageHdrFmt_V", "KImageImpFmt_ELF"):
        if required not in names:
            problems.append(f"flag {required} is not set")
    if "KImageDll" in names:
        problems.append("KImageDll set on what should be an EXE")

    print_(f"  cpu               0x{cpu:04x}      {CPU.get(cpu, '?? UNKNOWN')}")
    print_()
    print_(f"  codeSize          {codeSize} (0x{codeSize:x})")
    print_(f"  textSize          {textSize} (0x{textSize:x})")
    print_(f"  dataSize          {dataSize}")
    print_(f"  bssSize           {bssSize}")
    print_(f"  codeBase          0x{codeBase:08x}"
           f"{'' if codeBase == EXPECTED_CODE_BASE else f'  ?? EXPECTED 0x{EXPECTED_CODE_BASE:08x}'}")
    if codeBase != EXPECTED_CODE_BASE:
        problems.append(
            f"codeBase is 0x{codeBase:08x}, not the conventional 0x{EXPECTED_CODE_BASE:08x} — "
            "usually means something is still consuming address space ahead of .text")
    print_(f"  dataBase          0x{dataBase:08x}")
    print_(f"  entryPoint        0x{entry:08x}   (offset into code)")
    print_(f"  heap              min 0x{heapMin:x}  max 0x{heapMax:x}")
    print_(f"  stack             0x{stackSize:x}")
    print_(f"  dllRefTableCount  {dllRefs}")
    print_(f"  exportDirCount    {expCount}")
    print_()
    print_(f"  codeOffset        0x{codeOff:08x}")
    print_(f"  dataOffset        0x{dataOff:08x}")
    print_(f"  importOffset      0x{impOff:08x}")
    print_(f"  codeRelocOffset   0x{codeRelocOff:08x}")
    print_(f"  dataRelocOffset   0x{dataRelocOff:08x}")

    if entry >= codeSize:
        problems.append(f"entryPoint 0x{entry:x} lies outside codeSize 0x{codeSize:x}")
    if codeOff + codeSize > len(data):
        problems.append("code segment runs past the end of the file")
    for name, off in (("import", impOff), ("codeReloc", codeRelocOff),
                      ("dataReloc", dataRelocOff)):
        if off and off > len(data):
            problems.append(f"{name}Offset 0x{off:x} is past the end of the file")

    # The V-format header follows the base header plus E32ImageHeaderComp.
    # uncompressedSize is written unconditionally, compressed or not, so the V
    # header always starts at 0x80 — not at 0x7c as the struct layout suggests.
    if flags & 0x02000000:
        vfmt = "<4I I I H B"
        off = HDR_SIZE + 4
        (uncompressed,) = struct.unpack_from("<I", data, HDR_SIZE)
        print_(f"  uncompressedSize  {uncompressed}")
        (sid, vid, cap0, cap1, exc, spare2, edSize, edType) = struct.unpack_from(vfmt, data, off)
        print_()
        print_(f"  secureId          0x{sid:08x}  {'ok' if sid == uid3 else '?? USUALLY EQUALS UID3'}")
        print_(f"  vendorId          0x{vid:08x}")
        print_(f"  capabilities      0x{cap1:08x}{cap0:08x}")
        print_(f"  exceptionDescr    0x{exc:08x}")
        if exc:
            if not exc & 1:
                problems.append("exceptionDescriptor lacks its low bit; "
                                "elf2e32 is meant to set it")
            if (exc & ~1) >= codeSize:
                problems.append(f"exceptionDescriptor 0x{exc:x} lies outside the code segment")
        print_(f"  exportDescType    0x{edType:02x}  size {edSize}")

    # Import encoding. Symbian packs an import as (addend << 16) | ordinal, taking
    # the addend from the word already at the site. A large addend means something
    # non-zero was sitting there — for us it was GNU ld's GOT pre-filled with PLT
    # stub addresses, which made the loader resolve to garbage. Real addends are
    # small vtable or member offsets.
    if impOff and impOff < len(data) and dllRefs > 0:
        q = impOff + 4
        n_imports = 0
        suspect = []
        try:
            for _ in range(dllRefs):
                _nameOff, count = struct.unpack_from("<II", data, q)
                q += 8
                for off in struct.unpack_from(f"<{count}I", data, q):
                    w = struct.unpack_from("<I", data, codeOff + off)[0]
                    n_imports += 1
                    if (w >> 16) > 0x100:
                        suspect.append((off, w))
                q += 4 * count
        except struct.error:
            problems.append("import table runs past the end of the file")
        print_()
        print_(f"  imports           {n_imports} across {dllRefs} DLLs")
        if suspect:
            problems.append(
                f"{len(suspect)} imports carry an implausible addend, first "
                f"code+0x{suspect[0][0]:x} = 0x{suspect[0][1]:08x} — the word at the "
                "import site was not zero (an unzeroed GOT does this)")

    print_()
    if problems:
        # In quiet mode nothing has been shown yet; dump the whole report so the
        # failure arrives with its context rather than as a bare complaint.
        if quiet:
            for line in out:
                print(line)
        print("PROBLEMS:")
        for p in problems:
            print(f"  - {p}")
        sys.exit(1)
    print_("header is internally consistent")


if __name__ == "__main__":
    args = [a for a in sys.argv[1:] if not a.startswith("-")]
    if not args:
        sys.exit("usage: e32dump.py <file.exe> [--quiet]")
    main(args[0], quiet="--quiet" in sys.argv)
