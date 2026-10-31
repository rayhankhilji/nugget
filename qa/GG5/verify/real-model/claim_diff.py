#!/usr/bin/env python3
"""Compare claim ids/status between two snapshots of a run.
usage: claim_diff.py RUN_DIR A B [-v]"""
import json, sys
from pathlib import Path
from collections import Counter
run, a, b = Path(sys.argv[1]), sys.argv[2], sys.argv[3]
verbose = "-v" in sys.argv
def load(label):
    d = json.loads((run / label / "entities.json").read_text())
    out = {}
    for e in d["details"].values():
        for c in e.get("claims", []):
            out[c["id"]] = c
    return out, d
A, dA = load(a); B, dB = load(b)
def show(c):
    o = c["object"].get("literal") or (c["object"].get("entity") or {}).get("name")
    items = sorted({ev["item"]["key"][2:] for ev in c.get("evidence", [])})
    return f"{c['subject']['name']!r} {c['predicate']} {o!r} conf={c.get('confidence',0):.2f} {items}"
gone = [i for i in A if i not in B]
new = [i for i in B if i not in A]
trans = Counter((A[i]["status"], B[i]["status"]) for i in A if i in B and A[i]["status"] != B[i]["status"])
print(f"{a}: {len(A)} claims ({Counter(c['status'] for c in A.values())}); {b}: {len(B)} ({Counter(c['status'] for c in B.values())})")
print(f"gone: {len(gone)} by status {Counter(A[i]['status'] for i in gone)}; new: {len(new)}; transitions: {dict(trans)}")
if verbose:
    for i in gone: print("  GONE", A[i]["status"], show(A[i]))
    for i in A:
        if i in B and A[i]["status"] != B[i]["status"]: print(f"  {A[i]['status']}->{B[i]['status']}", show(A[i]))
    for i in new: print("  NEW", B[i]["status"], show(B[i]))
