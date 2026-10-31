#!/usr/bin/env python3
"""Per item, the current (active/superseded) claims citing it in two snapshots, as
(subject, predicate, object) triples; prints items whose triples differ. Use it on snapshots
before/after the reextract phase for items extracted in both (re-extraction idempotence).

usage: item_claims_diff.py RUN_DIR LABEL_A LABEL_B"""
import json, sys
from collections import defaultdict
from pathlib import Path

run, la, lb = Path(sys.argv[1]), sys.argv[2], sys.argv[3]


def triples(label):
    d = json.loads((run / label / "entities.json").read_text())
    out = defaultdict(set)
    ents = defaultdict(set)
    seen = set()
    for e in d["details"].values():
        for m in e.get("mentions", []):
            ents[m["item"]["key"]].add((e["kind"], e["name"]))
        for c in e.get("claims", []):
            if c["id"] in seen or c["status"] not in ("active", "superseded", "user_asserted"):
                continue
            seen.add(c["id"])
            o = c["object"]
            obj = o["entity"]["name"] if o.get("entity") else json.dumps(o.get("literal"), ensure_ascii=False)
            for ev in c.get("evidence", []):
                out[ev["item"]["key"]].add((c["subject"]["name"], c["predicate"], obj))
    return out, ents


(a, ea), (b, eb) = triples(la), triples(lb)
same = diff = 0
for k in sorted(set(a) & set(b)):
    if a[k] == b[k] and ea[k] == eb[k]:
        same += 1
        continue
    diff += 1
    print(f"\n{k}: claims {len(a[k])} -> {len(b[k])}, entities {len(ea[k])} -> {len(eb[k])}")
    for t in sorted(a[k] - b[k]):
        print("   -", t)
    for t in sorted(b[k] - a[k]):
        print("   +", t)
    for t in sorted(eb[k] - ea[k]):
        print("   + entity", t)
    for t in sorted(ea[k] - eb[k]):
        print("   - entity", t)
print(f"\nitems in both: identical {same}, different {diff}")
