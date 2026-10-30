#!/usr/bin/env python3
"""A small GTK 4 app for the Wayland integration tests (nugget-a11y, nugget-hotkey).

It mirrors crates/nugget-a11y/tests/fixtures/gtk_test_app.py (GTK 3 on X11) with GTK 4, which
has its own AT-SPI implementation (no ATK bridge) and runs natively on Wayland: start it with
GDK_BACKEND=wayland so it never falls back to XWayland.

Window "Nugget Wayland - chat": a message list (three messages, the last one "from me"), a
multi-line composer (GtkTextView), a single-line search entry with a placeholder and a password
entry.

Usage: gtk4_fixture.py <composer|entry|password>   -- which widget takes focus.

Prints "BACKEND <name>" (the GDK display class, so the test can check it is Wayland), "READY"
once the window is mapped and the widget focused, then "COMPOSER <json>" and "ENTRY <json>"
whenever those widgets change: an oracle independent of AT-SPI.
"""
import json
import sys

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Gdk", "4.0")
from gi.repository import Gdk, Gio, GLib, Gtk  # noqa: E402

focus = sys.argv[1] if len(sys.argv) > 1 else "composer"
TITLE = "Nugget Wayland - chat"


def label(widget, text):
    widget.update_property([Gtk.AccessibleProperty.LABEL], [text])


def report(kind, text):
    print(kind, json.dumps(text), flush=True)


def build(app):
    win = Gtk.ApplicationWindow(application=app, title=TITLE)
    win.set_default_size(600, 500)
    box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=6)
    win.set_child(box)

    messages = Gtk.ListBox()
    label(messages, "Messages")
    for author, text, mine in [
        ("Alice", "Are we still on for lunch tomorrow?", False),
        ("Bob", "I can bring the slides.", False),
        ("You", "Yes, noon works for me.", True),
    ]:
        inner = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        inner.append(Gtk.Label(label=author, xalign=0))
        inner.append(Gtk.Label(label=text, xalign=0))
        if mine:
            inner.set_halign(Gtk.Align.END)
        row = Gtk.ListBoxRow()
        row.set_child(inner)
        messages.append(row)
    messages.set_vexpand(True)
    box.append(messages)

    composer = Gtk.TextView()
    label(composer, "Message composer")
    composer.get_buffer().set_text("Hello ")
    composer.set_vexpand(True)
    box.append(composer)

    entry = Gtk.Entry()
    entry.set_placeholder_text("Search messages")
    label(entry, "Search")
    box.append(entry)

    password = Gtk.PasswordEntry()
    password.set_text("hunter2-secret")
    label(password, "Password")
    box.append(password)

    buf = composer.get_buffer()
    buf.connect(
        "changed",
        lambda b: report("COMPOSER", b.get_text(b.get_start_iter(), b.get_end_iter(), True)),
    )
    entry.connect("changed", lambda e: report("ENTRY", e.get_text()))

    target = {"composer": composer, "entry": entry, "password": password}[focus]

    def focus_now():
        win.present()
        target.grab_focus()
        if focus == "composer":
            buf.place_cursor(buf.get_end_iter())
        GLib.timeout_add(300, ready)
        return False

    def ready():
        print("READY", flush=True)
        return False

    print("BACKEND", type(Gdk.Display.get_default()).__name__, flush=True)
    win.present()
    GLib.timeout_add(300, focus_now)


# NON_UNIQUE: the tests run two instances at once (focus switching).
app = Gtk.Application(
    application_id="org.nugget.WaylandFixture", flags=Gio.ApplicationFlags.NON_UNIQUE
)
app.connect("activate", build)
app.run([sys.argv[0]])
