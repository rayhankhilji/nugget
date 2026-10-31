#!/usr/bin/env python3
"""Flatten entities.json claims of a snapshot: one row per unique claim id.
usage: claims.py RUN_DIR LABEL [--pred P] [--status S] [--name SUBSTR] [--item SUBSTR] [--json]"""
import json, sys, argparse
from pathlib import Path
ap = argparse.ArgumentParser()
ap.add_argument("run"); ap.add_argument("label")
ap.add_argument("--pred"); ap.add_argument("--status"); ap.add_argument("--name"); ap.add_argument("--item")
ap.add_argument("--count", action="store_true")
a = ap.parse_args()
d = json.loads((Path(a.run) / a.label / "entities.json").read_text())
rows = {}
for e in d["details"].values():
    for c in e.get("claims", []):
        rows[c["id"]] = c
n = 0
for cid, c in sorted(rows.items()):
    if a.pred and c["predicate"] != a.pred: continue
    if a.status and c["status"] != a.status: continue
    obj = c["object"].get("literal") or (c["object"].get("entity") or {}).get("name")
    if a.name and a.name.lower() not in (c["subject"]["name"] + " " + str(obj)).lower(): continue
    items = sorted({ev["item"]["key"][2:] for ev in c.get("evidence", [])})
    if a.item and not any(a.item in i for i in items): continue
    n += 1
    if a.count: continue
    subj = d["details"].get(c["subject"]["id"], {})
    q = [ev.get("quote") for ev in c.get("evidence", [])][:1]
    print(f"{cid}\t[{c['status']}] {subj.get('kind')}:{c['subject']['name']!r} {c['predicate']} {obj!r} conf={c.get('confidence'):.2f} <- {items} q={q}")
if a.count: print(n)
