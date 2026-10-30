#!/usr/bin/env python3
"""Accept the desktop's global shortcut consent dialog over AT-SPI, as a user clicking it would.

    accept-shortcut-dialog.py [--timeout S] [--dump]

Plasma 6 (xdg-desktop-portal-kde, a Qt Quick window "Global Shortcuts Requested" with OK /
Cancel) and GNOME 48+ (gnome-control-center's global shortcuts provider, a GTK 4 window "Add
Keyboard Shortcuts" with Add / Cancel) ask before binding a new shortcut. This finds that window
in the accessibility tree and activates its accept button (AT-SPI Action "press"/"click"), the
way a screen reader user accepts it. Exits 0 when it pressed the button, 3 when no such dialog
appeared before the timeout, 1 on another failure. Prints the tree with --dump (diagnostics).
"""

import argparse
import sys
import time
import warnings

import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi  # noqa: E402

# Atspi.Action.get_action_name is deprecated but is the one that takes an index everywhere.
warnings.filterwarnings("ignore", category=DeprecationWarning)

TITLES = ("Global Shortcuts Requested", "Add Keyboard Shortcuts")
ACCEPT = ("OK", "Add", "Allow", "_Add", "&OK")
BUTTON_ROLES = (Atspi.Role.PUSH_BUTTON, Atspi.Role.BUTTON) if hasattr(Atspi.Role, "BUTTON") else (
    Atspi.Role.PUSH_BUTTON,
)


def children(node):
    try:
        n = node.get_child_count()
    except Exception:
        return []
    out = []
    for i in range(n):
        try:
            c = node.get_child_at_index(i)
        except Exception:
            c = None
        if c is not None:
            out.append(c)
    return out


def name(node):
    try:
        return node.get_name() or ""
    except Exception:
        return ""


def role(node):
    try:
        return node.get_role()
    except Exception:
        return None


def walk(node, depth=0, limit=40):
    yield node, depth
    if depth >= limit:
        return
    for c in children(node):
        yield from walk(c, depth + 1, limit)


def find_dialog():
    desktop = Atspi.get_desktop(0)
    for app in children(desktop):
        for win in children(app):
            title = name(win)
            if any(t in title for t in TITLES):
                return app, win
    return None, None


def press(button):
    try:
        action = button.get_action_iface()
    except Exception:
        action = None
    if action is None:
        return False
    for i in range(action.get_n_actions()):
        an = (action.get_action_name(i) or "").lower()
        if an in ("press", "click", "activate", "") or i == 0:
            return action.do_action(i)
    return False


def dump():
    desktop = Atspi.get_desktop(0)
    for app in children(desktop):
        print(f"app {name(app)!r}")
        for node, depth in walk(app, 0, 12):
            if depth:
                r = role(node)
                print("  " * depth + f"{r.value_nick if r else '?'} {name(node)!r}")


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--timeout", type=float, default=20.0)
    p.add_argument("--dump", action="store_true")
    a = p.parse_args()
    Atspi.init()
    deadline = time.monotonic() + a.timeout
    while True:
        app, win = find_dialog()
        if win is not None:
            break
        if time.monotonic() > deadline:
            print(f"accept-shortcut-dialog: no consent dialog within {a.timeout:.0f} s", flush=True)
            if a.dump:
                dump()
            sys.exit(3)
        time.sleep(0.25)
    print(f"accept-shortcut-dialog: found {name(win)!r} from {name(app)!r}", flush=True)
    # The buttons may take a moment to be exposed after the window is.
    for _ in range(40):
        for node, _depth in walk(win):
            if role(node) in BUTTON_ROLES and name(node).replace("&", "") in ACCEPT:
                label = name(node)
                if press(node):
                    print(f"accept-shortcut-dialog: pressed {label!r} over AT-SPI", flush=True)
                    return
                print(f"accept-shortcut-dialog: {label!r} has no usable action", flush=True)
        time.sleep(0.25)
    print("accept-shortcut-dialog: the dialog has no accept button I can press", flush=True)
    dump()
    sys.exit(1)


if __name__ == "__main__":
    main()
