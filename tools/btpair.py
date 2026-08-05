#!/usr/bin/env python3
"""Pair with a legacy Bluetooth device, supplying the PIN from this side.

The E72 is Bluetooth 2.0, which predates Secure Simple Pairing, so pairing is the
old PIN exchange: the phone prompts for a code and the initiator must present the
same one. bluetoothctl's built-in agent asks interactively, which is no use from a
script, and with no agent at all BlueZ answers the phone's request with
AuthenticationCanceled — which is exactly the failure we were getting.

So: register an agent that answers RequestPinCode with a fixed PIN, pair, then
mark the device trusted so later OBEX pushes need no confirmation at all.

    python3 tools/btpair.py <MAC> [PIN]
"""

import sys

import dbus
import dbus.mainloop.glib
import dbus.service
from gi.repository import GLib

AGENT_PATH = "/rustsdk/agent"
CAPABILITY = "KeyboardDisplay"


class Agent(dbus.service.Object):
    def __init__(self, bus, path, pin):
        super().__init__(bus, path)
        self.pin = pin

    @dbus.service.method("org.bluez.Agent1", in_signature="o", out_signature="s")
    def RequestPinCode(self, device):
        print(f"  phone asked for a PIN -> sending {self.pin}")
        return self.pin

    @dbus.service.method("org.bluez.Agent1", in_signature="o", out_signature="u")
    def RequestPasskey(self, device):
        print(f"  phone asked for a passkey -> sending {self.pin}")
        return dbus.UInt32(int(self.pin))

    @dbus.service.method("org.bluez.Agent1", in_signature="ouq", out_signature="")
    def DisplayPasskey(self, device, passkey, entered):
        print(f"  enter this on the phone: {passkey:06d}")

    @dbus.service.method("org.bluez.Agent1", in_signature="os", out_signature="")
    def DisplayPinCode(self, device, pincode):
        print(f"  enter this on the phone: {pincode}")

    @dbus.service.method("org.bluez.Agent1", in_signature="ou", out_signature="")
    def RequestConfirmation(self, device, passkey):
        print(f"  confirming passkey {passkey:06d}")

    @dbus.service.method("org.bluez.Agent1", in_signature="o", out_signature="")
    def RequestAuthorization(self, device):
        print("  authorising")

    @dbus.service.method("org.bluez.Agent1", in_signature="os", out_signature="")
    def AuthorizeService(self, device, uuid):
        print(f"  authorising service {uuid}")

    @dbus.service.method("org.bluez.Agent1", in_signature="", out_signature="")
    def Cancel(self):
        print("  the phone cancelled")

    @dbus.service.method("org.bluez.Agent1", in_signature="", out_signature="")
    def Release(self):
        pass


def main():
    if len(sys.argv) < 2:
        sys.exit(f"usage: {sys.argv[0]} <MAC> [PIN]")
    mac = sys.argv[1]
    pin = sys.argv[2] if len(sys.argv) > 2 else "0000"

    dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
    bus = dbus.SystemBus()
    loop = GLib.MainLoop()

    agent = Agent(bus, AGENT_PATH, pin)
    mgr = dbus.Interface(
        bus.get_object("org.bluez", "/org/bluez"), "org.bluez.AgentManager1"
    )
    mgr.RegisterAgent(AGENT_PATH, CAPABILITY)
    mgr.RequestDefaultAgent(AGENT_PATH)
    print(f"agent registered (PIN {pin})")

    path = "/org/bluez/hci0/dev_" + mac.replace(":", "_")
    dev = dbus.Interface(bus.get_object("org.bluez", path), "org.bluez.Device1")
    props = dbus.Interface(bus.get_object("org.bluez", path),
                           "org.freedesktop.DBus.Properties")

    def done(ok, msg):
        print(msg)
        if ok:
            try:
                props.Set("org.bluez.Device1", "Trusted", dbus.Boolean(True))
                print("  marked trusted - future pushes will not prompt")
            except dbus.DBusException as e:
                print(f"  could not set Trusted: {e.get_dbus_name()}")
        loop.quit()

    print(f"pairing with {mac} - enter {pin} on the phone if it asks")
    dev.Pair(
        reply_handler=lambda: done(True, "paired"),
        error_handler=lambda e: done(
            props.Get("org.bluez.Device1", "Paired"),
            f"pair returned: {e.get_dbus_name()}",
        ),
        timeout=120,
    )

    GLib.timeout_add_seconds(130, lambda: (print("timed out"), loop.quit())[1])
    try:
        loop.run()
    finally:
        try:
            mgr.UnregisterAgent(AGENT_PATH)
        except dbus.DBusException:
            pass


if __name__ == "__main__":
    main()
