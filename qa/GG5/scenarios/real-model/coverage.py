#!/usr/bin/env python3
"""Items with any mention or claim evidence in each snapshot of a run, vs files in the corpus
at that point (phase1: 23, later: 42).

usage: coverage.py RUN_DIR"""
import json, sys
from pathlib import Path

run = Path(sys.argv[1])
for label in ["phase1", "phase2", "edits", "reextract", "restart"]:
    p = run / label / "entities.json"
    if not p.exists():
        continue
    d = json.loads(p.read_text())
    items = set()
    for e in d["details"].values():
        for m in e.get("mentions", []):
            items.add(m["item"]["key"])
        for c in e.get("claims", []):
            for ev in c.get("evidence", []):
                items.add(ev["item"]["key"])
    total = 23 if label == "phase1" else 42
    print(f"{label:10} items with graph data {len(items):2}/{total}")
    if label == "phase1":
        print("   ", sorted(k[2:] for k in items))
