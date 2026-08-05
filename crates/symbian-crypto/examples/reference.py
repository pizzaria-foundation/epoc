#!/usr/bin/env python3
"""Independent reference for the differential check.

    cargo run -q -p symbian-crypto --example dump > /tmp/ours.txt
    python3 crates/symbian-crypto/examples/reference.py > /tmp/theirs.txt
    diff /tmp/ours.txt /tmp/theirs.txt && echo identical

Hashes and HMAC come from `hashlib`/`hmac`, which are OpenSSL's. AES comes from pycrypto,
independently checked against FIPS 197. AES-IGE has no implementation left to borrow —
OpenSSL removed it in 3.0 — so it is computed here from the recurrence, which means the
IGE lines verify the *implementation* while the recurrence itself is verified by being
written out twice, in two languages, from the specification.
"""

import binascii
import hashlib
import hmac as hmaclib

from Crypto.Cipher import AES

h = lambda b: binascii.hexlify(b).decode()


def series(length, seed):
    """xorshift32, matching the Rust side exactly."""
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


def ige_encrypt(key, iv, pt):
    e = AES.new(key, AES.MODE_ECB)
    x = lambda a, b: bytes(p ^ q for p, q in zip(a, b))
    c_prev, m_prev = iv[:16], iv[16:]
    out = b""
    for i in range(0, len(pt), 16):
        m = pt[i:i + 16]
        c = x(e.encrypt(x(m, c_prev)), m_prev)
        out += c
        c_prev, m_prev = c, m
    return out


print("# reference: crates/symbian-crypto/examples/reference.py")

for length in range(0, 301):
    d = series(length, 0x12345678)
    print(f"sha256 {length} {hashlib.sha256(d).hexdigest()}")
    print(f"sha1 {length} {hashlib.sha1(d).hexdigest()}")
    print(f"sha512 {length} {hashlib.sha512(d).hexdigest()}")

for klen in [0, 1, 20, 63, 64, 65, 100, 131]:
    k = series(klen, 0xABCD0001)
    for dlen in [0, 1, 55, 64, 200]:
        d = series(dlen, 0x0000BEEF)
        print(f"hmac256 {klen} {dlen} {hmaclib.new(k, d, hashlib.sha256).hexdigest()}")
        print(f"hmac1 {klen} {dlen} {hmaclib.new(k, d, hashlib.sha1).hexdigest()}")
        print(f"hmac512 {klen} {dlen} {hmaclib.new(k, d, hashlib.sha512).hexdigest()}")

for klen in [16, 24, 32]:
    k = series(klen, 0x55550003)
    e = AES.new(k, AES.MODE_ECB)
    for i in range(8):
        pt = series(16, 0x90000000 + i)
        print(f"aes {klen} {h(pt)} {h(e.encrypt(pt))}")

k = series(32, 0x77770005)
for blocks in range(1, 9):
    pt = series(blocks * 16, 0x22220000 + blocks)
    iv = series(32, 0x33330000)
    print(f"ige {h(k)} {h(iv)} {h(pt)} {h(ige_encrypt(k, iv, pt))}")

# modpow against Python's own pow(), which is exact ground truth rather than another
# implementation of the same idea.
for nbytes in [4, 8, 16, 32, 64, 128, 256]:
    for trial in range(3):
        n = bytearray(series(nbytes, 0xC0DE0000 + nbytes * 16 + trial))
        n[0] |= 0x80
        n[-1] |= 1
        n = bytes(n)
        base = series(nbytes, 0xBA5E0000 + trial)
        exp = series(min(nbytes, 32), 0xE7E70000 + trial)
        r = pow(int.from_bytes(base, "big"), int.from_bytes(exp, "big"),
                int.from_bytes(n, "big"))
        print(f"modpow {h(n)} {h(base)} {h(exp)} {h(r.to_bytes(nbytes, 'big'))}")
