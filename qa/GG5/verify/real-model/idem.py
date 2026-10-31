#!/usr/bin/env python3
"""Re-extraction idempotence, model claims only (confidence 0.70 = the LLM's default here; the
deterministic extractor uses other values), for files present in both snapshots. Includes
retracted claims? no: only active/superseded/user_asserted.
usage: idem.py RUN_DIR A B [--ext .eml,.ics]"""
import json, sys
from collections import defaultdict
from pathlib import Path
run, la, lb = Path(sys.argv[1]), sys.argv[2], sys.argv[3]
exts = tuple(sys.argv[sys.argv.index("--ext") + 1].split(",")) if "--ext" in sys.argv else None
def trip(label, model_only):
    d = json.loads((run / label / "entities.json").read_text())
    out, seen = defaultdict(set), set()
    for e in d["details"].values():
        for c in e.get("claims", []):
            if c["id"] in seen or c["status"] not in ("active", "superseded", "user_asserted"): continue
            seen.add(c["id"])
            is_model = abs(c.get("confidence", 0) - 0.7) < 0.001
            if model_only and not is_model: continue
            o = c["object"]; obj = o["entity"]["name"] if o.get("entity") else o.get("literal")
            for ev in c.get("evidence", []):
                out[ev["item"]["key"][2:]].add((c["subject"]["name"], c["predicate"], obj))
    return out
for model_only in (False, True):
    a, b = trip(la, model_only), trip(lb, model_only)
    keys = sorted(k for k in set(a) | set(b) if (not exts or k.endswith(exts)))
    # only files present (any claim) in A's full set
    full_a = trip(la, False)
    keys = [k for k in keys if k in full_a]
    diff = [k for k in keys if a.get(k, set()) != b.get(k, set())]
    print(f"model_only={model_only}: files {len(keys)}, changed {len(diff)}")
    if model_only:
        for k in diff:
            print(" ", k, f"{len(a.get(k,()))}->{len(b.get(k,()))}", "kept", len(a.get(k,set()) & b.get(k,set())))
