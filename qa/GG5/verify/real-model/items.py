#!/usr/bin/env python3
"""Per snapshot, which corpus files have any mention or claim evidence in entities.json.
usage: items.py CORPUS_DIR RUN_DIR [LABEL...]"""
import json, sys
from pathlib import Path
corpus, run = Path(sys.argv[1]), Path(sys.argv[2])
labels = sys.argv[3:] or ["phase1", "phase2", "edits", "reextract", "restart", "restart-later"]
p1 = sorted(str(p.relative_to(corpus / "phase1")) for p in (corpus / "phase1").rglob("*") if p.is_file())
p2 = sorted(str(p.relative_to(corpus / "phase2")) for p in (corpus / "phase2").rglob("*") if p.is_file())
for label in labels:
    f = run / label / "entities.json"
    if not f.exists():
        continue
    d = json.loads(f.read_text())
    items = set()
    for e in d["details"].values():
        for m in e.get("mentions", []):
            items.add(m["item"]["key"][2:])
        for c in e.get("claims", []):
            for ev in c.get("evidence", []):
                items.add(ev["item"]["key"][2:])
    m1 = [x for x in p1 if x not in items]; m2 = [x for x in p2 if x not in items]
    extra = sorted(items - set(p1) - set(p2))
    print(f"{label:13} phase1 {len(p1)-len(m1)}/{len(p1)}  phase2 {len(p2)-len(m2)}/{len(p2)}  total {len(items)}  extra={extra}")
    print("   missing p1:", m1)
    print("   missing p2:", m2)
