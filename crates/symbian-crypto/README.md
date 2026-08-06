# symbian-crypto

Hashes and ciphers, written because the platform does not have them.

`no_std`, `forbid(unsafe_code)`, no allocation on any hot path, no floating point.

## What the platform actually provides

Measured against the public S60 3rd Edition FP2 SDK, not recalled:

| | |
|---|---|
| SHA-1, MD5 | `hash.dso` |
| SHA-256 | **nowhere** |
| AES, RSA, bignum | not in the public SDK — `crypto.dso` exports certificates and signatures, not primitives |
| random | `random.dso` |

Open C, **if a given handset has it**, adds OpenSSL 0.9.8a: `AES_encrypt`,
`RSA_public_encrypt`, `BN_mod_exp`, `RAND_bytes`, `HMAC`. Not SHA-256 — 0.9.8a is from 2005
and predates it; its `sha.h` does not mention SHA-256 at all. And whether the Open C runtime
is installed is a property of the phone, not of the SDK, which is what `examples/libprobe`
exists to answer.

So SHA-256 has to be written no matter what, AES-IGE has to be written no matter what
because no OpenSSL ever had it, and the rest is written here rather than bet on.

## Why this is the least risky part of the project

Everything here is integer arithmetic with **published test vectors**. Unlike the platform
work — where every guess turned out wrong and every answer needed a probe built for it — a
hash either matches FIPS 180-4 or it does not.

The tests are the specification:

| | |
|---|---|
| SHA-256 | FIPS 180-4, plus the million-`a` long-message vector |
| SHA-512 | FIPS 180-4, plus the million-`a` vector. Constants **derived** from the definition, not transcribed |
| SHA-1 | FIPS 180-4, plus the million-`a` vector |
| AES-128/192/256 | FIPS 197 appendix C, plus SP 800-38A block vectors |
| HMAC-SHA-256 | RFC 4231 cases 1, 2, 3 and 6 |
| HMAC-SHA-1 | RFC 2202 |
| HMAC-SHA-512 | RFC 4231, including the 131-byte over-block key |
| inflate | 81 blobs from real zlib and gzip, every level, every block type |
| AES-IGE | vectors generated from an independent AES driving the recurrence |
| modpow | `pow(b, e, m)` from Python, which is exact ground truth rather than another implementation |

Plus a differential check: `examples/dump` and `examples/reference.py` produce **1077 lines**
of digests, ciphertexts and modular exponentiations across every input length from 0 to 300
and every modulus size from 32 to 2048 bits, and they are byte-identical.
The reference side uses OpenSSL through `hashlib` and pycrypto for AES. Fixed vectors pin the
algorithm at a handful of lengths; the differential is what catches a partial-block or
padding bug that only appears at one.

```
cargo run -q -p symbian-crypto --example dump > /tmp/ours.txt
python3 crates/symbian-crypto/examples/reference.py > /tmp/theirs.txt
diff /tmp/ours.txt /tmp/theirs.txt && echo identical
```

## AES-IGE

The mode MTProto encrypts every message with, and the one piece here that cannot be borrowed
from anywhere: it existed as `AES_ige_encrypt` in OpenSSL 0.9.8 and 1.x, was deprecated, and
was **removed in 3.0**. The host's own OpenSSL 3.5 has no trace of it.

```text
encrypt:   c[i] = E(m[i] ^ c[i-1]) ^ m[i-1]
decrypt:   m[i] = D(c[i] ^ m[i-1]) ^ c[i-1]
```

with `c[-1] = iv[0..16]` and `m[-1] = iv[16..32]` — the split `AES_ige_encrypt` used and the
one Telegram's `msg_key`-derived IV assumes.

A round-trip test is not enough for a mode. Swap the two XOR operands and you get something
that round-trips against itself perfectly and agrees with nobody, and everything works until
you talk to a peer. That is why the recurrence is written out above, written a second time in
`reference.py`, and pinned by vectors from an AES that is not this one.

## Threat model, stated rather than assumed

These are **not constant-time**. AES uses S-box table lookups, which leak through cache
timing where an attacker can run code alongside you.

On this device that is not the exposure. An ARM1136 at 600 MHz has no cache shared with
another tenant, Symbian has no multi-user model, and anyone who can run code on the phone
already has the file the key is in. The real risks are a wrong implementation and a leaked
key file — which the vectors and `symbian::fs::private_path` address respectively.

`ct_eq` is provided and should be used for any MAC comparison: `a == b` on slices stops at
the first differing byte, which tells an attacker who can time it how many leading bytes of
a forged tag were right — enough to find a valid one byte at a time. That one *is* worth
getting right even here, because it costs nothing.

If this code is reused somewhere with real co-tenancy, the S-box needs revisiting first.

## Size and speed

| | `.rodata` |
|---|---|
| two S-boxes | 512 B |
| forward round table | 1 KB |
| inverse round table | 1 KB |

### The paragraph that used to be here was wrong

It said a table-driven AES "would be perhaps 30% faster and cost 8 KB", that the trade was
not worth making, and that "the network is four orders of magnitude slower than the cipher".
The device self test measured the cipher for the first time and every part of that was
false:

| | E72, measured |
|---|---|
| SHA-256 | 8 MB/s |
| AES-256, byte at a time | **169 KB/s** |

Slower than the Wi-Fi it was supposed to be four orders of magnitude faster than, and 48x
off SHA-256 for maybe 3x the work. It was 30% of the frame budget, not 30% of a speedup.

The estimate failed in a specific way worth naming: it was a guess about a ratio, written
with the same confidence as the measured facts around it, and nothing in the file marked it
as one. A number nobody measured should read like a number nobody measured.

### What replaced it

T-tables, generated by a `const fn` from the S-box rather than transcribed — the same
reasoning as the SHA-512 constants. **One** table per direction rather than the classic
four, because the other three are byte rotations of it and ARM's barrel shifter makes
`eor r1, r1, r0, ror #8` a single instruction: 2 KB instead of 8 KB for the same work.

Measured with `cargo run --release --example bench`, which times the table version against a
byte-at-a-time reference kept in the benchmark for exactly this purpose:

```
AES-256, host:
  tables:       102916 KB/s
  byte-wise:      4764 KB/s
  ratio:          21.6x
```

A host ratio is not a device ratio — different cache, different codegen — so the E72 number
comes from the self test rather than from multiplying. What the host ratio does establish is
that the win is a factor of twenty, not a third.

The byte-at-a-time cipher also survives as `#[cfg(test)]` code in `aes.rs`, and
`the_tables_agree_with_the_reference` runs both over 3,072 blocks across all three key
lengths. That test exists because a T-table with one wrong entry produces a cipher that is
perfectly self-consistent — it round-trips its own output and agrees with nothing else in
the world, so a round-trip test would pass it.

## Bignum

2048-bit modular exponentiation, enough for RSA-2048 and MTProto's Diffie-Hellman. Nothing
more — no division, no general modular inverse, no primality testing, because those are what
a *key generator* needs and this only consumes keys the server sends.

32-bit limbs, because the ARM1136 has `umull` (32×32→64) and that is exactly one limb
product. Fixed size on the stack, no allocation: an exponentiation that fails for lack of
memory halfway through a login is worse than one that cannot be attempted.

Montgomery multiplication by the CIOS method, which replaces a division per multiply — the
one operation this word size makes genuinely expensive — with a multiply and a shift.

### Timing

Exponentiation is a **Montgomery ladder**: one squaring and one multiply per exponent bit,
always, with the operands swapped by a masked select rather than a branch. Plain
square-and-multiply multiplies only when a bit is set, which makes the running time a direct
readout of the exponent's Hamming weight — and in Diffie-Hellman the exponent is the secret.

It costs about a third more. Unlike the AES S-box concern above, this leak needs no cache
access to exploit, just a clock, so it is worth paying for.

### Cost

**Measured 37 ms** on an aarch64 host — `cargo run --release --example bench`. On the E72
expect **roughly 0.4 to 0.6 s**: the host figure scaled by 10–15×, which is an estimate and
not a measurement (5× the clock, plus an in-order pipeline and a non-pipelined `umull`).

An earlier version of these docs claimed a quarter of a second. That was the arithmetic done
hopefully rather than carefully, and the bench exists so the number is not a guess again.

A login runs two exponentiations, so budget about a second. `rust_step` must return in
milliseconds, so it **cannot** be called from the event pump — it needs a second thread or to
be split across steps.

## inflate

DEFLATE decompression with the zlib and gzip wrappers, plus `inflate_any` which sniffs
between the three. MTProto wraps large responses in `gzip_packed`, so a client that cannot
inflate cannot read a dialog list.

Canonical Huffman decoded bit by bit against the code-length counts, the way zlib's own
`puff.c` reference does, rather than by building lookup tables. Slower per symbol, and the
right trade twice: the payloads are kilobytes against a network measured in hundreds of
milliseconds, and the table-building step is where an inflate implementation's subtle bugs
live. There is no table to get wrong.

`max_out` is **required**, not optional. A compressed stream is attacker-controlled and
DEFLATE's ratio is unbounded — a few hundred bytes can expand to gigabytes, which on a device
with 45 MB free is a denial of service by way of one message.

### The test, and a silent skip it caught

`examples/deflate_cases.py` compresses with real zlib and gzip at levels 0, 1, 6 and 9 plus
raw deflate, over inputs chosen to reach every corner: stored blocks, fixed Huffman, dynamic
Huffman, multi-block streams, a run whose back-reference distance is *shorter* than its length
(a copy that reads bytes the same loop is writing — what any bulk memmove gets wrong), and a
match at the 32 KB maximum distance.

```
python3 crates/symbian-crypto/examples/deflate_cases.py \
  | cargo run -q --release -p symbian-crypto --example dump \
  | grep '^inflate'
```

81 cases, all passing. The first run reported 72 with zero failures, because the nine
empty-input cases hex-encoded to an empty field, which made the line two columns instead of
three, and the consumer skipped short lines in silence. The consumer now reports a malformed
line as a failure and prints its total, because a check that silently examines less than it
claims is worse than no check.

## Still missing, for MTProto

- **`RSA_PAD`**, MTProto 2.0's padding scheme around the raw RSA primitive. Protocol work
  rather than crypto work.
- **PBKDF2**, twenty lines over `hmac_sha512`, for when 2FA passwords matter.
