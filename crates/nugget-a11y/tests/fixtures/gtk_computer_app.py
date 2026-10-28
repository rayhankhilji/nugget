#!/usr/bin/env python3
"""GTK3 app for the nugget-a11y computer-use integration test (NUGGET_A11Y_IT=1).

Window "Nugget IT - controls": a status label and an "Apply" button that changes it, a
"Notifications" check button, a "Name" entry, a "Password" entry, a "Colour" combo box, a list of
fruit, and an "Advanced" expander. Prints "READY" once mapped and focused, then
"<WIDGET> <json>" whenever a widget changes, as an oracle independent of AT-SPI.
"""
import json

import gi

gi.require_version("Gtk", "3.0")
from gi.repository import GLib, Gtk  # noqa: E402


def report(kind, value):
    print(kind, json.dumps(value), flush=True)


win = Gtk.Window(title="Nugget IT - controls")
win.set_default_size(500, 600)
box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=6)
win.add(box)

status = Gtk.Label(label="Status: idle")
box.pack_start(status, False, False, 0)

apply = Gtk.Button(label="Apply")


def on_apply(_b):
    status.set_text("Status: applied")
    report("STATUS", status.get_text())


apply.connect("clicked", on_apply)
box.pack_start(apply, False, False, 0)

check = Gtk.CheckButton(label="Notifications")
check.connect("toggled", lambda c: report("CHECK", c.get_active()))
box.pack_start(check, False, False, 0)

name = Gtk.Entry()
name.get_accessible().set_name("Name")
name.connect("changed", lambda e: report("NAME", e.get_text()))
box.pack_start(name, False, False, 0)

password = Gtk.Entry()
password.set_visibility(False)
password.set_text("hunter2-secret")
password.get_accessible().set_name("Password")
box.pack_start(password, False, False, 0)

combo = Gtk.ComboBoxText()
for c in ["Red", "Green", "Blue"]:
    combo.append_text(c)
combo.set_active(0)
combo.get_accessible().set_name("Colour")
combo.connect("changed", lambda c: report("COMBO", c.get_active_text()))
box.pack_start(combo, False, False, 0)

fruit = Gtk.ListBox()
fruit.get_accessible().set_name("Fruit")
for f in ["Apple", "Banana", "Cherry"]:
    fruit.add(Gtk.Label(label=f, xalign=0))
fruit.connect(
    "row-selected",
    lambda _l, row: report("FRUIT", row.get_child().get_text() if row else None),
)
box.pack_start(fruit, False, False, 0)

expander = Gtk.Expander(label="Advanced")
expander.add(Gtk.Label(label="Hidden details"))
expander.connect("notify::expanded", lambda e, _p: report("EXPANDED", e.get_expanded()))
box.pack_start(expander, False, False, 0)

win.connect("destroy", Gtk.main_quit)
win.show_all()


def focus_now():
    gdk = win.get_window()
    if gdk is not None:
        gdk.focus(0)
    win.present()
    name.grab_focus()
    GLib.timeout_add(300, ready)
    return False


def ready():
    print("READY", flush=True)
    return False


GLib.timeout_add(300, focus_now)
Gtk.main()
