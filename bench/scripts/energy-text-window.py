#!/usr/bin/env python3
"""A GTK 3 window with a focused text field, for `nugget-bench energy`'s `shell_focus` (Linux).

Usage: energy-text-window.py TITLE TEXT

Window TITLE: a short message thread (two labels) above a multi-line reply field (GtkTextView)
holding TEXT, with the cursor at its end. Prints "READY" on stdout once the window is mapped and
the field focused. Each window of the scenario has its own title and text, so the shell's
read-ahead sees three different windows and three different contexts. Close it with SIGTERM.
"""
import signal
import sys

import gi

gi.require_version("Gtk", "3.0")
from gi.repository import GLib, Gtk  # noqa: E402

title = sys.argv[1] if len(sys.argv) > 1 else "Nugget bench"
text = sys.argv[2] if len(sys.argv) > 2 else "Hello "

win = Gtk.Window(title=title)
win.set_default_size(560, 360)
box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=6)
win.add(box)
for line in (f"Alex: about {title}, can you send an update today?", "You: Sure, give me a minute."):
    box.pack_start(Gtk.Label(label=line, xalign=0), False, False, 0)

field = Gtk.TextView()
field.get_accessible().set_name("Reply")
field.get_buffer().set_text(text)
box.pack_start(field, True, True, 0)

win.connect("destroy", Gtk.main_quit)
GLib.unix_signal_add(GLib.PRIORITY_DEFAULT, signal.SIGTERM, Gtk.main_quit)
win.show_all()


def focus_now():
    gdk = win.get_window()
    if gdk is not None:
        gdk.focus(0)
    win.present()
    field.grab_focus()
    buf = field.get_buffer()
    buf.place_cursor(buf.get_end_iter())
    GLib.timeout_add(300, ready)
    return False


def ready():
    print("READY", flush=True)
    return False


GLib.timeout_add(300, focus_now)
Gtk.main()
