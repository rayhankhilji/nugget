#!/usr/bin/env python3
"""Per item: the latest updated_at among claims citing it, in each snapshot of a run. An item
whose claims were all last written before a phase started was not re-extracted in that phase.

usage: item_times.py RUN_DIR LABEL [LABEL...]"""
import json, sys
from collections import defaultdict
from pathlib import Path

run = Path(sys.argv[1])
rows = defaultdict(dict)
for label in sys.argv[2:]:
    d = json.loads((run / label / "entities.json").read_text())
    for e in d["details"].values():
        for c in e.get("claims", []):
            for ev in c.get("evidence", []):
                k = ev["item"]["key"]
                rows[k][label] = max(rows[k].get(label, ""), c["updated_at"][11:19])
print("item".ljust(56), *[l[:9].ljust(9) for l in sys.argv[2:]])
for k in sorted(rows):
    print(k[2:].ljust(56), *[rows[k].get(l, "-").ljust(9) for l in sys.argv[2:]])
