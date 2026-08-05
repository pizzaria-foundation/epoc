#!/usr/bin/env python3
"""Push a file to a phone over Bluetooth OBEX Object Push.

Fedora ships obexd but none of the old CLI front-ends (obexftp, ussp-push,
bt-obex), so this drives obexd over D-Bus directly. That is also the supported
path now — the legacy tools needed bluetoothd in a compatibility mode that no
longer exists.

    python3 tools/btpush.py <MAC> <file>

The phone must be paired and visible. On an S60 device the file lands in
Messaging as a received item; open it there to install.
"""

import os
import subprocess
import time
import sys

import dbus
import dbus.mainloop.glib
from gi.repository import GLib

BUS_NAME = "org.bluez.obex"
ROOT = "/org/bluez/obex"


def create_session(client, dest, attempts=3):
    """Open an OPP session, waking the phone's SDP first if it has gone quiet.

    The E72 tears down the ACL link after each transfer and stops answering SDP, so
    the *next* push fails with `org.bluez.obex.Error.Failed: Unable to find service
    record` — which reads like the phone has no OBEX at all, when in fact it just is
    not paging. A plain `bluetoothctl connect` re-establishes the link and refills
    the SDP cache, after which the same push succeeds.

    Worth noting the connect itself is allowed to fail: on a phone that is already
    half-awake it returns `br-connection-page-timeout` and the push still works. So
    its result is ignored — it is a nudge, not a precondition.
    """
    last = None
    for i in range(attempts):
        try:
            # D-Bus defaults to a 25s reply timeout, which is not enough:
            # establishing the OBEX channel means an SDP query and an RFCOMM connect
            # to a 2009 phone that may also be waiting on the user to accept.
            return client.CreateSession(dest, {"Target": dbus.String("opp")}, timeout=120)
        except dbus.exceptions.DBusException as e:
            last = e
            if "service record" not in str(e) or i == attempts - 1:
                raise
            print(f"  no SDP record; waking the link (attempt {i + 2}/{attempts}) ...")
            subprocess.run(
                ["bluetoothctl", "connect", dest],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=30,
            )
            time.sleep(2)
    raise last


def main():
    if len(sys.argv) != 3:
        sys.exit(f"usage: {sys.argv[0]} <MAC> <file>")
    dest, path = sys.argv[1], os.path.abspath(sys.argv[2])
    if not os.path.isfile(path):
        sys.exit(f"no such file: {path}")

    dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
    bus = dbus.SessionBus()
    loop = GLib.MainLoop()

    client = dbus.Interface(bus.get_object(BUS_NAME, ROOT), "org.bluez.obex.Client1")

    print(f"opening OPP session to {dest} (accept the prompt on the phone) ...")
    session_path = create_session(client, dest)
    opp = dbus.Interface(
        bus.get_object(BUS_NAME, session_path), "org.bluez.obex.ObjectPush1"
    )

    size = os.path.getsize(path)
    print(f"sending {os.path.basename(path)} ({size} bytes)")
    transfer_path, props = opp.SendFile(path)

    state = {"done": False, "ok": False}

    def on_props(interface, changed, invalidated, p=None):
        if interface != "org.bluez.obex.Transfer1":
            return
        if "Transferred" in changed and size:
            pct = 100 * int(changed["Transferred"]) // size
            sys.stdout.write(f"\r  {pct}%")
            sys.stdout.flush()
        if "Status" in changed:
            status = str(changed["Status"])
            if status == "complete":
                state["ok"] = True
                state["done"] = True
                print("\r  100%  complete")
                loop.quit()
            elif status == "error":
                state["done"] = True
                print("\r  transfer failed")
                loop.quit()

    bus.add_signal_receiver(
        on_props,
        dbus_interface="org.freedesktop.DBus.Properties",
        signal_name="PropertiesChanged",
        path=transfer_path,
        path_keyword="p",
    )

    # The phone may sit on an "accept?" prompt for a while; give it room, but do
    # not hang forever if the user walks away.
    GLib.timeout_add_seconds(180, lambda: (print("\n  timed out"), loop.quit())[1])

    try:
        loop.run()
    except KeyboardInterrupt:
        pass
    finally:
        try:
            client.RemoveSession(session_path)
        except dbus.DBusException:
            pass

    sys.exit(0 if state["ok"] else 1)


if __name__ == "__main__":
    main()
