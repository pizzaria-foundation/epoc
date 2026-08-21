#!/usr/bin/env python3
"""Accept files pushed from the phone over Bluetooth OBEX.

    python3 tools/btrecv.py [directory]        # default: ./inbox

The receiving half of OBEX, and the only half left: Fedora ships
obexd but no CLI front-end for it, so the receiving side has to be driven over D-Bus.

The piece that is easy to miss: obexd will not accept an incoming push unless an *agent*
is registered to authorise it, and an agent only exists while the process that registered
it is running. Without one the phone reports "sending failed" and nothing appears here —
which reads like a Bluetooth problem and is not one.

Files land in `directory`, and every transfer is announced with its size so a truncated
push is visible rather than silent.
"""

import os
import sys

import dbus
import dbus.mainloop.glib
import dbus.service
from gi.repository import GLib

BUS_NAME = "org.bluez.obex"
ROOT = "/org/bluez/obex"
AGENT_PATH = "/rustsdk/obexagent"

DEST = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else "inbox")


class Agent(dbus.service.Object):
    @dbus.service.method("org.bluez.obex.Agent1", in_signature="o", out_signature="s")
    def AuthorizePush(self, path):
        """Accept the transfer and say what to call the file.

        obexd resolves the returned name against its own root, so this hands back a bare
        filename and the root is set below. Returning an absolute path here is ignored,
        which is a confusing hour if you assume otherwise.
        """
        transfer = dbus.Interface(
            bus.get_object(BUS_NAME, path), "org.freedesktop.DBus.Properties"
        )
        props = transfer.GetAll("org.bluez.obex.Transfer1")
        name = str(props.get("Name", "received.bin"))
        size = int(props.get("Size", 0))
        print(f"incoming: {name}  ({size} bytes)", flush=True)
        # Track it so completion can be reported with the final size.
        watched[path] = name
        return name

    @dbus.service.method("org.bluez.obex.Agent1", in_signature="", out_signature="")
    def Cancel(self):
        print("  the phone cancelled", flush=True)

    @dbus.service.method("org.bluez.obex.Agent1", in_signature="", out_signature="")
    def Release(self):
        pass


watched = {}


def on_props(interface, changed, invalidated, path=None):
    if interface != "org.bluez.obex.Transfer1" or path not in watched:
        return
    status = changed.get("Status")
    if status == "complete":
        name = watched.pop(path)
        full = os.path.join(DEST, name)
        try:
            size = os.path.getsize(full)
        except OSError:
            size = -1
        print(f"received: {full}  ({size} bytes)", flush=True)
    elif status == "error":
        name = watched.pop(path, "?")
        print(f"failed:   {name}", flush=True)


def main():
    global bus
    os.makedirs(DEST, exist_ok=True)

    dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
    bus = dbus.SessionBus()

    agent = Agent(bus, AGENT_PATH)
    mgr = dbus.Interface(bus.get_object(BUS_NAME, ROOT), "org.bluez.obex.AgentManager1")
    mgr.RegisterAgent(AGENT_PATH)

    bus.add_signal_receiver(
        on_props,
        dbus_interface="org.freedesktop.DBus.Properties",
        signal_name="PropertiesChanged",
        path_keyword="path",
    )

    print(f"ready to receive into {DEST}")
    print("  send the file from the phone now")
    print("  Ctrl-C to stop")
    loop = GLib.MainLoop()
    try:
        loop.run()
    except KeyboardInterrupt:
        pass
    finally:
        try:
            mgr.UnregisterAgent(AGENT_PATH)
        except dbus.DBusException:
            pass


if __name__ == "__main__":
    main()
