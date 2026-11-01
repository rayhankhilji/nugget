"""Shared setup for the JJ1 search scenarios: corpus, daemon home, sources, waits."""
import json
import os
import shutil
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", "..", ".."))
sys.path.insert(0, os.path.join(ROOT, "qa", "JJ1", "harness"))
sys.path.insert(0, HERE)
from nq import Daemon, RpcError  # noqa: E402
import gen_corpus  # noqa: E402

NUGGETD = os.environ.get("NUGGETD") or os.path.join(ROOT, "target", "debug", "nuggetd")
BASE = os.environ.get("JJ1_SEARCH_BASE", "/tmp/jj1/search")


def fresh(name, safe=True):
    """A fresh home under BASE. `safe`: [ingest] assume_unflagged_safe = true, so only rule hits
    (PII etc.) are sensitive and the label is meaningful without a triage model."""
    home = os.path.join(BASE, name)
    shutil.rmtree(home, ignore_errors=True)
    os.makedirs(os.path.join(home, "config"), exist_ok=True)
    if safe:
        with open(os.path.join(home, "config", "config.toml"), "w") as f:
            f.write("[ingest]\nassume_unflagged_safe = true\n")
    return home


def corpus():
    c = os.path.join(BASE, "corpus")
    shutil.rmtree(c, ignore_errors=True)
    truth = gen_corpus.main(c)
    return c, truth


def wait_knowledge(d, timeout=180):
    t = time.time()
    last = None
    stable = 0
    while time.time() - t < timeout:
        st = d.call("knowledge.status")
        cur = (st.get("pending_items"), st.get("entities"), st.get("mentions"))
        stable = stable + 1 if cur == last and not st.get("pending_items") else 0
        last = cur
        if stable >= 3:
            return st
        time.sleep(1)
    return d.call("knowledge.status")


def index(d, c, bulk=True, whatsapp=True):
    srcs = {}
    srcs["files"] = d.call("sources.add", {"kind": "files", "name": "Files", "settings": {"roots": [os.path.join(c, "files")]}})["id"]
    if bulk:
        srcs["bulk"] = d.call("sources.add", {"kind": "files", "name": "Bulk", "settings": {"roots": [os.path.join(c, "bulk")]}})["id"]
    if whatsapp:
        srcs["wa"] = d.call("sources.add", {"kind": "whatsapp", "name": "WA", "settings": {"folder": os.path.join(c, "whatsapp"), "private": False}})["id"]
    d.wait_idle(settle=4, timeout=400)
    wait_knowledge(d)
    return srcs


def key_to_name(key):
    """Files-source item keys look like `0:mail/m01.eml`."""
    return key.split(":", 1)[1] if ":" in key else key


def faceted(d, query, **kw):
    p = {"query": query}
    p.update(kw)
    return d.call("search.faceted", p)


def all_hits(d, query, filters=None, page=7, **kw):
    """Every hit by paging with offset/limit; returns (hits, totals seen)."""
    out, totals, off = [], set(), 0
    while True:
        r = faceted(d, query, filters=filters or {}, limit=page, offset=off, **kw)
        totals.add(r["total"])
        out += r["hits"]
        off += page
        if not r["hits"] or off >= r["total"]:
            break
        if off > 2000:
            break
    return out, totals
