#!/usr/bin/env python3
"""Press keys in a Wayland session through the compositor's virtual input device.

    virtual-keyboard.py [--compositor kwin|gnome] [--hold-ms N] CHORD [CHORD...]

Each CHORD is keys joined by '+' (ctrl+alt+space, Return, Escape): pressed in order and released
in reverse, as a person does. The key events enter the compositor as a real input device's do,
so they go through its keyboard and global shortcut handling (nothing is sent to an app):

- kwin: the org_kde_kwin_fake_input Wayland protocol (what KDE Connect and remote input use),
  spoken directly on the Wayland socket. KWin 6 offers it to any unsandboxed client and
  `authenticate` is granted at once; its events come from a FakeInputDevice that KWin's input
  redirection treats like any keyboard (global shortcuts included).
- gnome: Mutter's org.gnome.Mutter.RemoteDesktop D-Bus API (CreateSession, Start,
  NotifyKeyboardKeycode on one connection): a Clutter virtual keyboard on the seat, whose events
  go through Mutter's key bindings and GNOME Shell's accelerator grabs.

Keys are Linux evdev key codes (both APIs take them). The compositor comes from
NUGGET_WAYLAND_COMPOSITOR when not given. Prints one line per chord; exits non-zero when the
device cannot be created.
"""

import argparse
import os
import socket
import struct
import sys
import time

KEYS = {
    "esc": 1, "escape": 1, "tab": 15, "return": 28, "enter": 28, "ctrl": 29, "control": 29,
    "shift": 42, "alt": 56, "space": 57, "super": 125, "meta": 125, "logo": 125,
    "a": 30, "s": 31, "d": 32, "f": 33, "q": 16, "w": 17, "e": 18, "r": 19, "t": 20,
}


def parse(chord):
    codes = []
    for name in chord.split("+"):
        key = name.strip().lower()
        if key not in KEYS:
            raise SystemExit(f"virtual-keyboard: unknown key {name!r} (known: {sorted(KEYS)})")
        codes.append(KEYS[key])
    return codes


# --- KWin: org_kde_kwin_fake_input over the raw Wayland wire protocol --------------------------


class Wayland:
    """The few Wayland messages needed, without a client library."""

    def __init__(self):
        runtime = os.environ["XDG_RUNTIME_DIR"]
        display = os.environ.get("WAYLAND_DISPLAY", "wayland-0")
        path = display if display.startswith("/") else os.path.join(runtime, display)
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(path)
        self.next_id = 2  # 1 is wl_display
        self.buf = b""
        self.globals = {}
        self.done = set()
        self.registry = None

    def new_id(self):
        i = self.next_id
        self.next_id += 1
        return i

    def send(self, obj, opcode, payload=b""):
        size = 8 + len(payload)
        self.sock.sendall(struct.pack("<II", obj, (size << 16) | opcode) + payload)

    @staticmethod
    def string(s):
        b = s.encode() + b"\0"
        pad = (4 - len(b) % 4) % 4
        return struct.pack("<I", len(b)) + b + b"\0" * pad

    def roundtrip(self, timeout=5.0):
        cb = self.new_id()
        self.send(1, 0, struct.pack("<I", cb))  # wl_display.sync
        deadline = time.monotonic() + timeout
        while cb not in self.done:
            left = deadline - time.monotonic()
            if left <= 0:
                raise SystemExit("virtual-keyboard: the compositor did not answer wl_display.sync")
            self.sock.settimeout(left)
            data = self.sock.recv(65536)
            if not data:
                raise SystemExit("virtual-keyboard: the compositor closed the connection")
            self.buf += data
            self._dispatch()

    def _dispatch(self):
        while len(self.buf) >= 8:
            obj, word = struct.unpack_from("<II", self.buf)
            size, opcode = word >> 16, word & 0xFFFF
            if len(self.buf) < size:
                return
            body = self.buf[8:size]
            self.buf = self.buf[size:]
            if obj == 1 and opcode == 0:  # wl_display.error(object, code, message)
                _, code = struct.unpack_from("<II", body)
                (n,) = struct.unpack_from("<I", body, 8)
                msg = body[12 : 12 + n - 1].decode(errors="replace")
                raise SystemExit(f"virtual-keyboard: Wayland protocol error {code}: {msg}")
            if obj == self.registry and opcode == 0:  # wl_registry.global(name, interface, version)
                (name,) = struct.unpack_from("<I", body)
                (n,) = struct.unpack_from("<I", body, 4)
                iface = body[8 : 8 + n - 1].decode()
                off = 8 + ((n + 3) & ~3)
                (version,) = struct.unpack_from("<I", body, off)
                self.globals[iface] = (name, version)
            elif opcode == 0:  # wl_callback.done
                self.done.add(obj)


def kwin_press(chords, hold_ms):
    wl = Wayland()
    wl.registry = wl.new_id()
    wl.send(1, 1, struct.pack("<I", wl.registry))  # wl_display.get_registry
    wl.roundtrip()
    if "org_kde_kwin_fake_input" not in wl.globals:
        raise SystemExit(
            "virtual-keyboard: KWin does not offer org_kde_kwin_fake_input to this client "
            f"(globals: {sorted(wl.globals)})"
        )
    name, version = wl.globals["org_kde_kwin_fake_input"]
    if version < 4:
        raise SystemExit(f"virtual-keyboard: org_kde_kwin_fake_input v{version} has no keyboard_key")
    fake = wl.new_id()
    bind_version = min(version, 6)
    wl.send(
        wl.registry,
        0,
        struct.pack("<I", name)
        + Wayland.string("org_kde_kwin_fake_input")
        + struct.pack("<II", bind_version, fake),
    )
    # authenticate(application, reason): KWin grants it to unsandboxed clients.
    wl.send(fake, 0, Wayland.string("Nugget CI") + Wayland.string("press the shortcut under test"))
    wl.roundtrip()
    # The device is added to KWin's input redirection when bound; let it settle.
    time.sleep(0.3)
    for chord in chords:
        codes = parse(chord)
        for c in codes:
            wl.send(fake, 10, struct.pack("<II", c, 1))  # keyboard_key(key, pressed)
            wl.roundtrip()
            time.sleep(0.03)
        time.sleep(hold_ms / 1000)
        for c in reversed(codes):
            wl.send(fake, 10, struct.pack("<II", c, 0))
            wl.roundtrip()
            time.sleep(0.03)
        print(f"virtual-keyboard: kwin fake_input pressed {chord} (evdev {codes})", flush=True)
        time.sleep(0.2)
    if bind_version >= 5:
        wl.send(fake, 11)  # destroy
    wl.roundtrip()
    wl.sock.close()


# --- GNOME: Mutter's RemoteDesktop D-Bus API -----------------------------------------------------


def gnome_press(chords, hold_ms):
    import gi

    gi.require_version("Gio", "2.0")
    from gi.repository import Gio, GLib

    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)

    def call(path, iface, method, args=None, reply=None):
        return bus.call_sync(
            "org.gnome.Mutter.RemoteDesktop", path, iface, method, args,
            GLib.VariantType(reply) if reply else None, Gio.DBusCallFlags.NONE, 10000, None,
        )

    (session,) = call(
        "/org/gnome/Mutter/RemoteDesktop", "org.gnome.Mutter.RemoteDesktop", "CreateSession",
        reply="(o)",
    ).unpack()
    iface = "org.gnome.Mutter.RemoteDesktop.Session"
    call(session, iface, "Start")
    time.sleep(0.3)
    try:
        for chord in chords:
            codes = parse(chord)
            for c in codes:
                call(session, iface, "NotifyKeyboardKeycode", GLib.Variant("(ub)", (c, True)))
                time.sleep(0.03)
            time.sleep(hold_ms / 1000)
            for c in reversed(codes):
                call(session, iface, "NotifyKeyboardKeycode", GLib.Variant("(ub)", (c, False)))
                time.sleep(0.03)
            print(
                f"virtual-keyboard: mutter RemoteDesktop pressed {chord} (evdev {codes})",
                flush=True,
            )
            time.sleep(0.2)
    finally:
        time.sleep(0.2)
        call(session, iface, "Stop")


def main():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--compositor", default=os.environ.get("NUGGET_WAYLAND_COMPOSITOR", ""))
    p.add_argument("--hold-ms", type=int, default=80)
    p.add_argument("chords", nargs="+")
    a = p.parse_args()
    for chord in a.chords:
        parse(chord)
    if a.compositor == "kwin":
        kwin_press(a.chords, a.hold_ms)
    elif a.compositor == "gnome":
        gnome_press(a.chords, a.hold_ms)
    else:
        sys.exit(f"virtual-keyboard: no virtual keyboard for compositor {a.compositor!r} (kwin, gnome)")


if __name__ == "__main__":
    main()
