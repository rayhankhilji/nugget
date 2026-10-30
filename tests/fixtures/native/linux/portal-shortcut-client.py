#!/usr/bin/env python3
"""A minimal GlobalShortcuts portal client, for checking a session's portal path without Nugget.

    portal-shortcut-client.py [--app-id dev.nugget.desktop] [--id double-tap] [--seconds 120]

Registers the host app id (org.freedesktop.host.portal.Registry), creates a GlobalShortcuts
session, binds one shortcut (preferred trigger CTRL+ALT+space) and prints one line per step and
per Activated signal, flushing each, until --seconds pass. Lines start with "PORTAL ".
"""

import argparse
import sys

import gi

gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib  # noqa: E402

DEST = "org.freedesktop.portal.Desktop"
PATH = "/org/freedesktop/portal/desktop"
IFACE = "org.freedesktop.portal.GlobalShortcuts"


def say(*a):
    print("PORTAL", *a, flush=True)


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--app-id", default="dev.nugget.desktop")
    p.add_argument("--id", default="double-tap")
    p.add_argument("--trigger", default="CTRL+ALT+space")
    p.add_argument("--seconds", type=int, default=120)
    a = p.parse_args()

    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    sender = bus.get_unique_name()[1:].replace(".", "_")
    loop = GLib.MainLoop()
    state = {"n": 0}

    try:
        bus.call_sync(DEST, PATH, "org.freedesktop.host.portal.Registry", "Register",
                      GLib.Variant("(sa{sv})", (a.app_id, {})), None, Gio.DBusCallFlags.NONE,
                      10000, None)
        say(f"registered app id {a.app_id}")
    except GLib.Error as e:
        say(f"register failed: {e.message}")

    def request(method, args_fmt, args, on_response):
        state["n"] += 1
        token = f"nuggetci{state['n']}"
        path = f"/org/freedesktop/portal/desktop/request/{sender}/{token}"
        sub = [None]

        def cb(_c, _s, _p, _i, _sig, params):
            bus.signal_unsubscribe(sub[0])
            code, results = params.unpack()
            on_response(code, results)

        sub[0] = bus.signal_subscribe(DEST, "org.freedesktop.portal.Request", "Response", path,
                                      None, Gio.DBusSignalFlags.NONE, cb)
        args = list(args)
        args[-1] = dict(args[-1], handle_token=GLib.Variant("s", token))
        try:
            bus.call_sync(DEST, PATH, IFACE, method, GLib.Variant(args_fmt, tuple(args)), None,
                          Gio.DBusCallFlags.NONE, 30000, None)
        except GLib.Error as e:
            say(f"{method} failed: {e.message}")
            loop.quit()

    def activated(_c, _s, _p, _i, _sig, params):
        session, sid, ts, _opts = params.unpack()
        say(f"activated {sid} ts={ts}")

    def deactivated(_c, _s, _p, _i, _sig, params):
        say(f"deactivated {params.unpack()[1]}")

    bus.signal_subscribe(DEST, IFACE, "Activated", PATH, None, Gio.DBusSignalFlags.NONE, activated)
    bus.signal_subscribe(DEST, IFACE, "Deactivated", PATH, None, Gio.DBusSignalFlags.NONE,
                         deactivated)

    def on_bound(code, results):
        say(f"bind response {code} {results}")

    def on_session(code, results):
        if code != 0:
            say(f"session refused: response {code} {results}")
            loop.quit()
            return
        session = results["session_handle"]
        say(f"session created {session}")
        shortcut = (a.id, {"description": GLib.Variant("s", "Nugget: draft a reply"),
                           "preferred_trigger": GLib.Variant("s", a.trigger)})
        request("BindShortcuts", "(oa(sa{sv})sa{sv})", (session, [shortcut], "", {}), on_bound)

    request("CreateSession", "(a{sv})",
            ({"session_handle_token": GLib.Variant("s", "nuggetcisession")},), on_session)
    GLib.timeout_add_seconds(a.seconds, loop.quit)
    loop.run()
    say("done")


if __name__ == "__main__":
    sys.exit(main())
