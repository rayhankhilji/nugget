"""Shared helpers for the JJ1 messaging scenarios."""
import json
import os
import shutil
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", "..", ".."))
sys.path.insert(0, os.path.join(ROOT, "qa", "JJ1", "harness"))
from nq import Daemon, RpcError  # noqa: E402

NUGGETD = os.environ.get("NUGGETD") or os.path.join(ROOT, "target", "debug", "nuggetd")
BASE = os.environ.get("JJ1_MSG_BASE", "/tmp/jj1/messaging")


def fresh(name):
    d = os.path.join(BASE, name)
    shutil.rmtree(d, ignore_errors=True)
    os.makedirs(d)
    return d


def daemon(base, safe=False):
    """A daemon on `base`/home. `safe`: assume_unflagged_safe, so that items nobody judged count
    as not sensitive (no triage model exists here), which isolates the connector's own flag."""
    home = os.path.join(base, "home")
    if safe:
        os.makedirs(os.path.join(home, "config"), exist_ok=True)
        with open(os.path.join(home, "config", "config.toml"), "w") as f:
            f.write("[ingest]\nassume_unflagged_safe = true\n")
    return Daemon(home, nuggetd=NUGGETD)


def add(d, kind, settings, name=None, wait=True, min_items=0):
    s = d.call("sources.add", {"kind": kind, "name": name or kind, "settings": settings})
    if wait:
        d.wait_idle(source=s["id"], timeout=180, min_items=min_items)
    return s["id"]


def source(d, sid):
    for s in d.call("sources.list"):
        if s["id"] == sid:
            return s
    return None


def hits(d, q, sid=None, limit=100):
    p = {"query": q, "limit": limit}
    if sid:
        p["filters"] = {"sources": [sid]}
    return d.call("search.faceted", p)


def preview(d, h):
    return d.call("search.preview", {"item": h["item"], "chunk_id": h.get("chunk_id")})


def docs_for(d, q, sid):
    """{key: preview} for every hit of `q` in source `sid`."""
    out = {}
    for h in hits(d, q, sid)["hits"]:
        k = h["item"]["key"]
        if k not in out:
            p = preview(d, h)
            p["_hit"] = h
            out[k] = p
    return out


def wait_for(fn, timeout=30, step=1.0):
    end = time.time() + timeout
    v = None
    while time.time() < end:
        v = fn()
        if v:
            return v
        time.sleep(step)
    return v


class Check:
    """Collects expectation results for one scenario."""

    def __init__(self, name):
        self.name = name
        self.items = []
        self.notes = []

    def ok(self, cond, what, observed=None):
        self.items.append({"check": what, "ok": bool(cond), "observed": observed})
        return bool(cond)

    def note(self, s):
        self.notes.append(s)

    def result(self):
        failed = [i for i in self.items if not i["ok"]]
        return {"scenario": self.name, "ok": not failed and bool(self.items), "checks": self.items,
                "failed": [f["check"] for f in failed], "notes": self.notes}


def short(o, n=400):
    s = o if isinstance(o, str) else json.dumps(o, ensure_ascii=False)
    return s if len(s) <= n else s[:n] + "…"
