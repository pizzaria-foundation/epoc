#!/usr/bin/env python3
"""Carve E32 images out of a .sis file.

Why brute force rather than a proper SIS parser: all we want is a reference
binary built by the real Symbian toolchain, to diff our own E32 header against.
A full SISController/SISData walker is a few hundred lines and we would throw it
away afterwards. The payloads are raw-deflate streams, so trying to inflate at
every offset and keeping whatever comes out with 'EPOC' at 0x10 finds them in
well under a second on a file this size.

    python3 tools/sisextract.py <file.sis> [outdir]
"""

import os
import struct
import sys
import zlib


def looks_like_e32(b):
    return len(b) > 0x9c and b[0x10:0x14] == b"EPOC"


def main():
    if len(sys.argv) < 2:
        sys.exit(f"usage: {sys.argv[0]} <file.sis> [outdir]")
    path = sys.argv[1]
    outdir = sys.argv[2] if len(sys.argv) > 2 else "."
    data = open(path, "rb").read()

    found = []
    seen = set()
    for off in range(len(data) - 4):
        # Raw deflate (no zlib header): the SIS payloads are stored this way.
        try:
            out = zlib.decompressobj(-15).decompress(data[off:])
        except zlib.error:
            continue
        if not looks_like_e32(out):
            continue
        uid3 = struct.unpack_from("<I", out, 8)[0]
        key = (uid3, len(out))
        if key in seen:
            continue
        seen.add(key)
        found.append((off, out))

    # Uncompressed payloads, in case a SIS stored one verbatim.
    pos = 0
    while True:
        i = data.find(b"EPOC", pos)
        if i < 0:
            break
        start = i - 0x10
        if start >= 0 and looks_like_e32(data[start:]):
            uid3 = struct.unpack_from("<I", data, start + 8)[0]
            key = (uid3, -1)
            if key not in seen:
                seen.add(key)
                found.append((start, data[start:]))
        pos = i + 4

    if not found:
        print("no E32 image found")
        return 1

    os.makedirs(outdir, exist_ok=True)
    base = os.path.splitext(os.path.basename(path))[0]
    for n, (off, blob) in enumerate(found):
        uid1, uid2, uid3 = struct.unpack_from("<3I", blob, 0)
        kind = "exe" if uid1 == 0x1000007A else "dll" if uid1 == 0x10000079 else "bin"
        name = f"{base}-{n}-{uid3:08x}.{kind}"
        with open(os.path.join(outdir, name), "wb") as f:
            f.write(blob)
        print(f"  {name}  ({len(blob)} bytes, from offset 0x{off:x})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
