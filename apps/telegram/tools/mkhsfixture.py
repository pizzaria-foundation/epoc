#!/usr/bin/env python3
"""Pack the recorded handshake transcript into a blob the phone can carry.

`apps/telegram/proto/tests/fixtures/handshake.json` proves the handshake on the host. The
handset fails at step two of the same handshake and the host cannot reproduce it, so the
question is whether the *code* differs on ARM or whether the *bytes* differ on the wire.
Running the recorded transcript on the device answers that without a server: same input,
same expected output, on the machine that fails.

Only steps one and two are packed. Step three needs a 2048-bit exponentiation, which on
this handset takes 821 ms on a worker thread and cannot run inside a start-up check —
and step two is where the failure is.

    python3 apps/telegram/tools/mkhsfixture.py

Writes `apps/telegram/src/handshake_fixture.bin`, which `selfcheck.rs` includes.
"""
import json
import os
import struct
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = os.path.join(ROOT, "apps/telegram/proto/tests/fixtures/handshake.json")
DST = os.path.join(ROOT, "apps/telegram/src/handshake_fixture.bin")


def main():
    with open(SRC) as f:
        fx = json.load(f)

    tape = b"".join(bytes.fromhex(fx[k]) for k in ("nonce", "new_nonce", "b", "pad_stream"))
    r0 = bytes.fromhex(fx["received"][0])
    r1 = bytes.fromhex(fx["received"][1])
    # The 20-byte unencrypted header carries a msg_id from the recorder's clock, which this
    # crate does not produce. The body is everything the code is responsible for.
    s0 = bytes.fromhex(fx["sent"][0])[20:]
    s1 = bytes.fromhex(fx["sent"][1])[20:]

    parts = [tape, r0, r1, s0, s1]
    blob = struct.pack("<5I", *(len(p) for p in parts)) + b"".join(parts)
    with open(DST, "wb") as f:
        f.write(blob)

    print("%s  %d bytes" % (os.path.relpath(DST, ROOT), len(blob)))
    for name, p in zip(("tape", "res_pq", "server_DH_params_ok", "req_pq_multi",
                        "req_DH_params"), parts):
        print("  %-22s %d" % (name, len(p)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
