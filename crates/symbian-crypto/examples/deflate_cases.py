#!/usr/bin/env python3
"""Emits compressed test cases for the inflate differential check.

    python3 crates/symbian-crypto/examples/deflate_cases.py \
      | cargo run -q -p symbian-crypto --example dump \
      | grep '^inflate' | grep -v ' ok$'

Silence means every case decompressed to exactly what went in.

The cases are chosen to reach each of DEFLATE's three block types and the awkward corners of
the back-reference encoder:

  level 0        stored blocks, which take the byte-aligned path
  level 1        fixed Huffman on small inputs
  level 9        dynamic Huffman, with a code-length tree of its own
  >64 KB         more than one block, so the loop has to continue
  long runs      a distance shorter than the length, which is a copy that reads bytes the
                 same loop is writing — the case any bulk memmove gets wrong
  far matches    a 32 KB distance, the maximum the format allows
"""

import binascii
import gzip
import zlib

# "-" for empty rather than an empty field: a zero-length line would split into two
# whitespace-separated fields instead of three, and the consumer would skip it in silence.
# That happened, and nine cases — every empty-input one — went unchecked while the run
# reported no failures.
def h(b):
    return binascii.hexlify(b).decode() or "-"


def series(length, seed):
    s = seed | 1
    out = bytearray()
    for _ in range(length):
        s ^= (s << 13) & 0xFFFFFFFF
        s &= 0xFFFFFFFF
        s ^= s >> 17
        s ^= (s << 5) & 0xFFFFFFFF
        s &= 0xFFFFFFFF
        out.append(s & 0xFF)
    return bytes(out)


cases = [
    ("empty", b""),
    ("one", b"a"),
    ("hello", b"hello, world"),
    # A long run: the encoder emits a back-reference whose distance is 1 and whose length is
    # far greater, so decoding has to read bytes it is in the middle of writing.
    ("run", b"a" * 5000),
    ("run2", b"ab" * 4000),
    # Incompressible, which forces stored blocks even at high levels.
    ("random", series(20000, 0xDEADBEEF)),
    # Compressible with real structure.
    ("text", (b"the quick brown fox jumps over the lazy dog. " * 400)),
    # Larger than one deflate block.
    ("big", series(3000, 1) * 30),
    # A match at the maximum distance the format allows.
    ("far", b"MARKER" + series(32000, 7) + b"MARKER"),
]

for name, data in cases:
    for level in (0, 1, 6, 9):
        print(f"zlib-{name}-{level} {h(data)} {h(zlib.compress(data, level))}")
        print(f"gzip-{name}-{level} {h(data)} {h(gzip.compress(data, level))}")
    # Raw deflate, no wrapper — the third thing inflate_any has to recognise.
    c = zlib.compressobj(6, zlib.DEFLATED, -zlib.MAX_WBITS)
    raw = c.compress(data) + c.flush()
    print(f"raw-{name} {h(data)} {h(raw)}")
