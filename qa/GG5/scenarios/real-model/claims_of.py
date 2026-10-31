#!/usr/bin/env python3
"""Print claims of a snapshot filtered by subject/object name substring (-n) and/or source item
substring (-i), with status, predicate and the first evidence quote.

usage: claims_of.py RUN_DIR LABEL [-n NAME] [-i ITEM] [-p PRED]"""
import argparse, json
from pathlib import Path

ap = argparse.ArgumentParser()
ap.add_argument("run"); ap.add_argument("label")
ap.add_argument("-n"); ap.add_argument("-i"); ap.add_argument("-p")
a = ap.parse_args()
d = json.loads((Path(a.run) / a.label / "entities.json").read_text())
seen = set()
for e in d["details"].values():
    for c in e.get("claims", []):
        if c["id"] in seen:
            continue
        seen.add(c["id"])
        o = c["object"]
        obj = o["entity"]["name"] if o.get("entity") else o.get("literal")
        items = sorted({ev["item"]["key"][2:] for ev in c.get("evidence", [])})
        if a.n and a.n.lower() not in (c["subject"]["name"] + " " + str(obj)).lower():
            continue
        if a.i and not any(a.i in k for k in items):
            continue
        if a.p and c["predicate"] != a.p:
            continue
        sk = d["details"].get(c["subject"]["id"], {}).get("kind")
        q = (c.get("evidence") or [{}])[0].get("quote")
        print(f"[{c['status']:10}] {sk}:{c['subject']['name']} | {c['predicate']} | {obj} <- {[k.split('/')[-1] for k in items]} q={q!r}")
