#!/usr/bin/env python3
"""Diff the reextract and restart snapshots of a GG5 run: which claims vanished or changed
status, from which items, and which items were re-extracted at restart (claims whose
updated_at is at/after the restart's knowledge run).

usage: restart_diff.py RUN_DIR [RESTART_RUN_ISO]   (stdlib only)"""
import json, sys
from collections import Counter, defaultdict
from pathlib import Path

run = Path(sys.argv[1])
cut = sys.argv[2] if len(sys.argv) > 2 else None


def claims(label):
    d = json.loads((run / label / "entities.json").read_text())
    out = {}
    for e in d["details"].values():
        for c in e.get("claims", []):
            out[c["id"]] = c
    return out


a, b = claims("reextract"), claims("restart")
items = lambda c: sorted({ev["item"]["key"] for ev in c.get("evidence", [])})
missing = [c for i, c in a.items() if i not in b]
changed = [(a[i], b[i]) for i in a if i in b and a[i]["status"] != b[i]["status"]]
new = [c for i, c in b.items() if i not in a]
print(f"reextract claims {len(a)} restart {len(b)}; missing {len(missing)}; status changed {len(changed)}; new {len(new)}")
print("status transitions:", Counter((x['status'], y['status']) for x, y in changed))
by_item = defaultdict(list)
for c in missing:
    for k in items(c) or ["(no evidence)"]:
        by_item[k].append(("missing", c["status"], c["text"]))
for x, y in changed:
    for k in items(x) or ["(no evidence)"]:
        by_item[k].append((f"{x['status']}->{y['status']}", "", x["text"]))
for k in sorted(by_item):
    print(f"\n{k}")
    for r in by_item[k]:
        print("   ", r)
if cut:
    touched = sorted({k for c in b.values() if c["updated_at"] >= cut for k in items(c)})
    print(f"\nitems with claims written at/after {cut}: {len(touched)}")
    for k in touched:
        print("   ", k)
