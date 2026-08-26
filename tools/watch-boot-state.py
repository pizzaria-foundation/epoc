#!/usr/bin/env python3
r"""Watch the platform's system-state property across a real reboot of the handset.

    tools/watch-boot-state.py                     # just poll; assumes the phone is booting
    tools/watch-boot-state.py --reboot            # reset the phone first, then poll
    tools/watch-boot-state.py --reboot --minutes 6 --out boot-state.txt

WHAT IT ANSWERS

    `0x101F8766` key `0x41` is the platform's system state (enum based at 100; 109/110/111 are the
    "normal" trio, and the value at which aiidleint brings the native Active Idle to the front).
    We know it reads 109 on a settled phone. What we do not know is **what it reads at the moment
    our own code is born** — the launcher and bootd start from the platform's start-up list at
    around 100 s of uptime, and if the state is already 109 by then, gating our boot behaviour on
    this property buys the launcher nothing (it would still buy the daemons something).

    This is the cheapest instrument that can answer it. A probe binary of ours could not do better:
    it would have to be launched from the same start-up list, so it would be blind over exactly the
    same first ~100 s. Sub-second resolution from the first moments of a boot needs a component the
    platform loads early — the ECom recogniser route — which does not exist yet.

HOW TO READ THE OUTPUT

    Times are host wall-clock seconds since `reboot now` was acknowledged, so they are the boot's
    timeline offset by however long the Bluetooth link takes to come back. That is accurate to a
    second or two, which is the right precision for a 100 s question. The device's own uptimes are
    recovered at the end by pulling the daemons' logs, which stamp `monotonic_us`.

    The headline is the FIRST successful read: that is the earliest moment the phone would talk to
    us at all, and therefore an upper bound on how late the state could still have been changing.

REBOOT COST

    `reboot now` kills the file server. The kernel resets, nothing is flushed, and a daemon halfway
    through a write loses that write. It is the only software reset available on this handset
    (killing SysAp just respawns it; killing the window server powers off). Low risk, not zero.
"""

import argparse
import os
import sys
import time

CLIENT = os.path.expanduser("~/Codigos/symbian/ADBian/client")
sys.path.insert(0, CLIENT)
import btlink  # noqa: E402
from btlink import Link  # noqa: E402

#: The properties to sample, and why each is in the list.
WATCH = [
    ("0x101f8766", "0x41", "system state (aiidleint observes this; 104/109/110/111 = it grabs the screen)"),
    ("0x101f8766", "0x2", "a second state key, 104 on a settled phone"),
    ("0x101f8766", "0x11", "startup phase marker"),
    ("0x101f8766", "0x31", "startup phase marker"),
    ("0x101f8766", "0x42", "startup phase marker"),
    ("0x101f8767", "0x501", "SysAp: autolock-status candidate"),
]

#: Logs to pull at the end, for the device's own uptime stamps.
LOGS = [
    r"C:\Data\_logs\bootd.txt",
    r"C:\Data\_logs\launcher.txt",
]

POLL_S = 1.0
#: How long to keep trying to get the link back before giving up on the boot entirely.
CONNECT_GRACE_S = 300.0


def read_all(link):
    """One sweep. Returns {(cat, key): text}; a failed read is kept as its error text."""
    out = {}
    for cat, key, _ in WATCH:
        ok, text, _ = link.command(f"ps {cat} {key}")
        out[(cat, key)] = text.strip() if ok else f"ERR {text.strip()}"
    return out


def value_of(text):
    """`0x101f8766/0x00000041 = 109 (0x6d)` -> `109`. Anything else comes back whole."""
    if " = " in text:
        return text.split(" = ", 1)[1].split(" ", 1)[0]
    return text


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--reboot", action="store_true", help="reset the phone before watching")
    ap.add_argument("--minutes", type=float, default=5.0)
    ap.add_argument("--out")
    args = ap.parse_args()

    sink = open(args.out, "w") if args.out else None

    def say(line):
        print(line, flush=True)
        if sink:
            sink.write(line + "\n")
            sink.flush()

    for cat, key, why in WATCH:
        say(f"# watching {cat}/{key} — {why}")

    t0 = None
    if args.reboot:
        link = Link.open(None, None)
        before = read_all(link)
        say("# baseline, phone settled:")
        for (cat, key), text in before.items():
            say(f"#   {cat}/{key} = {value_of(text)}")
        ok, text, _ = link.command("reboot now")
        t0 = time.time()
        say(f"# reboot now -> ok={ok} {text.strip()}")
        try:
            link.close()
        except Exception:
            pass
        # The link is gone either way; the phone is on its way down.
        time.sleep(5.0)
    else:
        t0 = time.time()
        say("# no reboot requested; timing is from now")

    # Reconnect as soon as the agent answers again. Every failure here is expected: the phone is
    # down, then booting, then its Bluetooth stack comes up, then rshelld is started from the
    # start-up list. Silence is data — it says the phone is not yet at the phase we can reach.
    link = None
    deadline = t0 + CONNECT_GRACE_S
    attempts = 0
    while time.time() < deadline:
        attempts += 1
        try:
            link = Link.open(None, None)
            break
        except Exception as e:
            if attempts % 5 == 1:
                say(f"# +{time.time() - t0:6.1f}s  no link yet ({type(e).__name__})")
            time.sleep(2.0)
    if link is None:
        say(f"# FAILED: no link within {CONNECT_GRACE_S:.0f}s of the reset")
        return 1

    first = time.time() - t0
    say(f"# link back at +{first:.1f}s after {attempts} attempts — this is the earliest we can see anything")

    # From here, poll until the watch window closes, printing only what changes. A value that
    # never changes is the finding, so the first sweep is always printed in full.
    last = None
    end = t0 + args.minutes * 60
    while time.time() < end:
        try:
            now = read_all(link)
        except Exception as e:
            say(f"# +{time.time() - t0:6.1f}s  link died ({e}); reconnecting")
            try:
                link.reconnect()
            except Exception:
                time.sleep(2.0)
            continue
        t = time.time() - t0
        if last is None:
            for (cat, key), text in now.items():
                say(f"+{t:6.1f}s  {cat}/{key} = {value_of(text)}")
        else:
            for k, text in now.items():
                if text != last[k]:
                    say(f"+{t:6.1f}s  {k[0]}/{k[1]}: {value_of(last[k])} -> {value_of(text)}   <-- CHANGED")
        last = now
        time.sleep(POLL_S)

    say("# window closed; pulling the daemons' logs for their own uptime stamps")
    for path in LOGS:
        ok, text, data = link.command(f"stat {path}")
        if not ok:
            say(f"#   {path}: {text.strip()}")
            continue
        try:
            link.get_file(path, os.path.basename(path).replace(".txt", "-boot.txt"))
            say(f"#   pulled {path}")
        except Exception as e:
            say(f"#   {path}: {e}")

    if sink:
        sink.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
