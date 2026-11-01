#!/usr/bin/python3.12
"""JJ1 overlay QA: read on-screen UI through AT-SPI (python3.12 has gi + Atspi here).

  atspi.py apps                       -> app names on the bus
  atspi.py dump <frame-title-regex>   -> JSON list of {role,name,text,x,y,w,h,showing} under
                                         every frame whose title matches (all apps)
  atspi.py ext <app-regex> <name>     -> JSON extents of the first node named <name>
"""
import json
import re
import sys

import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi  # noqa: E402

Atspi.init()
desk = Atspi.get_desktop(0)


def kids(n):
    try:
        c = n.get_child_count()
    except Exception:
        return []
    out = []
    for i in range(min(c, 400)):
        try:
            k = n.get_child_at_index(i)
            if k is not None:
                out.append(k)
        except Exception:
            pass
    return out


def info(n):
    d = {}
    try:
        d["role"] = n.get_role_name()
        d["name"] = n.get_name() or ""
    except Exception:
        return None
    try:
        t = n.get_text_iface() if hasattr(n, "get_text_iface") else None
        t = n if t is None else t
        cnt = Atspi.Text.get_character_count(n)
        if cnt:
            d["text"] = Atspi.Text.get_text(n, 0, min(cnt, 4000))
    except Exception:
        pass
    try:
        e = Atspi.Component.get_extents(n, Atspi.CoordType.SCREEN)
        d.update(x=e.x, y=e.y, w=e.width, h=e.height)
    except Exception:
        pass
    try:
        d["showing"] = n.get_state_set().contains(Atspi.StateType.SHOWING)
        d["focused"] = n.get_state_set().contains(Atspi.StateType.FOCUSED)
    except Exception:
        pass
    return d


def walk(n, out, depth=0, maxdepth=60):
    if depth > maxdepth:
        return
    d = info(n)
    if d and (d.get("name") or d.get("text")):
        d["depth"] = depth
        out.append(d)
    for k in kids(n):
        walk(k, out, depth + 1, maxdepth)


def main():
    cmd = sys.argv[1]
    apps = kids(desk)
    if cmd == "apps":
        print(json.dumps([a.get_name() for a in apps]))
        return
    if cmd == "dump":
        rx = re.compile(sys.argv[2])
        out = []
        for a in apps:
            for f in kids(a):
                try:
                    title = f.get_name() or ""
                except Exception:
                    continue
                if rx.search(title):
                    walk(f, out)
        print(json.dumps(out))
        return
    if cmd == "ext":
        rx = re.compile(sys.argv[2])
        for a in apps:
            if not rx.search(a.get_name() or ""):
                continue
            out = []
            walk(a, out)
            for d in out:
                if d.get("name") == sys.argv[3]:
                    print(json.dumps(d))
                    return
        print("null")


main()
