#!/usr/bin/env python3
"""Tiny GTK3 app for nugget-a11y integration tests (NUGGET_A11Y_IT=1).

Window "Nugget IT - chat": a message list (three messages, the last one "from me"), a
multi-line composer (GtkTextView), a single-line entry with a placeholder and a password entry.
Usage: gtk_test_app.py <composer|entry|password>  -- which widget takes focus.
Prints "READY" on stdout once the window is mapped and focused, then "COMPOSER <json>" and
"ENTRY <json>" whenever those widgets change, as an oracle independent of AT-SPI.
"""
import json
import sys

import gi

gi.require_version("Gtk", "3.0")
from gi.repository import GLib, Gtk  # noqa: E402

focus = sys.argv[1] if len(sys.argv) > 1 else "composer"

win = Gtk.Window(title="Nugget IT - chat")
win.set_default_size(600, 500)
box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=6)
win.add(box)

messages = Gtk.ListBox()
messages.get_accessible().set_name("Messages")
for author, text, mine in [
    ("Alice", "Are we still on for lunch tomorrow?", False),
    ("Bob", "I can bring the slides.", False),
    ("You", "Yes, noon works for me.", True),
]:
    row = Gtk.ListBoxRow()
    inner = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
    inner.pack_start(Gtk.Label(label=author, xalign=0), False, False, 0)
    inner.pack_start(Gtk.Label(label=text, xalign=0), False, False, 0)
    if mine:
        inner.set_halign(Gtk.Align.END)
    row.add(inner)
    messages.add(row)
box.pack_start(messages, True, True, 0)

composer = Gtk.TextView()
composer.get_accessible().set_name("Message composer")
composer.get_buffer().set_text("Hello ")
box.pack_start(composer, True, True, 0)

entry = Gtk.Entry()
entry.set_placeholder_text("Search messages")
entry.get_accessible().set_name("Search")
box.pack_start(entry, False, False, 0)

password = Gtk.Entry()
password.set_visibility(False)
password.set_text("hunter2-secret")
password.get_accessible().set_name("Password")
box.pack_start(password, False, False, 0)

def report(kind, text):
    print(kind, json.dumps(text), flush=True)


composer.get_buffer().connect(
    "changed", lambda b: report("COMPOSER", b.get_text(b.get_start_iter(), b.get_end_iter(), True))
)
entry.connect("changed", lambda e: report("ENTRY", e.get_text()))

win.connect("destroy", Gtk.main_quit)
win.show_all()

target = {"composer": composer, "entry": entry, "password": password}[focus]


def focus_now():
    gdk = win.get_window()
    if gdk is not None:
        gdk.focus(0)
    win.present()
    target.grab_focus()
    if focus == "composer":
        buf = composer.get_buffer()
        buf.place_cursor(buf.get_end_iter())
    GLib.timeout_add(300, ready)
    return False


def ready():
    print("READY", flush=True)
    return False


GLib.timeout_add(300, focus_now)
Gtk.main()
