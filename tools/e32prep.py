#!/usr/bin/env python3
"""Prepare a linked ELF for elf2e32, by editing bytes rather than rewriting it.

Two adjustments are needed to turn GNU ld's output into something elf2e32 reads
the way Symbian's own toolchain output is read. Both were first attempted with
objcopy, which quietly made things worse: `--set-section-flags .rel.plt=...`
reclassified the section and dropped all 82 of its R_ARM_JUMP_SLOT relocations,
silently losing 82 imports. Patching the section header table in place changes
exactly the fields named and nothing else.

1. Clear SHF_ALLOC on the link-time-only sections.

   elf2e32 concatenates every allocated section into the E32 code segment,
   skipping only .data / ER_RW / ER_ZI. GNU ld allocates the whole dynamic
   apparatus, so .hash, .dynsym and a 9.5 KB .dynstr were being baked into the
   executable image — 18 KB of it, ahead of 10 KB of real code. A genuine
   Symbian binary has ER_RO as its only allocated section. The sections must
   stay in the file, because elf2e32 reads .dynsym and .rel.* to build the
   import and relocation tables; only the ALLOC bit goes.

2. Zero the .got contents.

   Symbian encodes an import as (addend << 16) | ordinal, taking the addend from
   whatever word already sits at the import site. RVCT leaves 0 there. GNU ld
   pre-fills each GOT entry with the address of its PLT stub for lazy binding, so
   elf2e32 was folding in the low half of a PLT address as the addend, producing
   imports like 0xa3ac000e. Symbian resolves everything at load and never uses
   lazy binding, so the pre-filled values are dead weight; zeroing them makes the
   encoding come out clean and the loader writes the real address into the slot
   the stub reads.

    python3 tools/e32prep.py <file.elf>
"""

import struct
import sys

SHF_ALLOC = 0x2

# Everything the linker emits for dynamic loading, which Symbian does not use:
# imports come from the E32 import table, not from ELF dynamic linking.
NOALLOC = {
    ".hash", ".gnu.hash", ".dynsym", ".dynstr", ".gnu.version",
    ".gnu.version_d", ".gnu.version_r", ".rel.dyn", ".rel.plt", ".dynamic",
}


def main(path):
    data = bytearray(open(path, "rb").read())
    if data[:4] != b"\x7fELF" or data[4] != 1 or data[5] != 1:
        sys.exit("not a 32-bit little-endian ELF")

    e_shoff, = struct.unpack_from("<I", data, 0x20)
    e_shentsize, e_shnum, e_shstrndx = struct.unpack_from("<3H", data, 0x2E)

    # Section header string table, so we can match sections by name.
    strtab_off, = struct.unpack_from("<I", data, e_shoff + e_shstrndx * e_shentsize + 0x10)

    def name_of(i):
        n, = struct.unpack_from("<I", data, e_shoff + i * e_shentsize)
        end = data.index(b"\0", strtab_off + n)
        return data[strtab_off + n:end].decode("latin1")

    cleared, zeroed = [], None
    for i in range(e_shnum):
        base = e_shoff + i * e_shentsize
        name = name_of(i)
        flags_off = base + 0x08
        sh_flags, sh_addr, sh_offset, sh_size = struct.unpack_from("<4I", data, flags_off)

        if name in NOALLOC and (sh_flags & SHF_ALLOC):
            struct.pack_into("<I", data, flags_off, sh_flags & ~SHF_ALLOC)
            cleared.append(name)

        if name == ".got" and sh_size:
            data[sh_offset:sh_offset + sh_size] = b"\0" * sh_size
            zeroed = sh_size

    open(path, "wb").write(data)
    print(f"    de-allocated: {' '.join(cleared) if cleared else '(none)'}")
    print(f"    zeroed .got: {zeroed if zeroed else 0} bytes")


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else sys.exit("usage: e32prep.py <file.elf>"))
