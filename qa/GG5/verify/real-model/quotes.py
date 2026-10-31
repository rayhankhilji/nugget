#!/usr/bin/env python3
"""Every evidence quote of every claim in a snapshot, checked verbatim (whitespace- and
case-normalised) against the raw corpus file. Prints the misses that are not .ics files.
usage: quotes.py CORPUS_DIR RUN_DIR LABEL"""
import json, re, sys
from pathlib import Path
corpus, run, label = Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3]
def norm(s): return re.sub(r"\s+", " ", s).strip().lower()
files = {}
for ph in ("phase1", "phase2"):
    for p in (corpus / ph).rglob("*"):
        if p.is_file(): files[str(p.relative_to(corpus / ph))] = norm(p.read_text(errors="replace"))
d = json.loads((run / label / "entities.json").read_text())
seen, n, miss_ics, other = set(), 0, 0, []
for e in d["details"].values():
    for c in e.get("claims", []):
        if c["id"] in seen: continue
        seen.add(c["id"])
        for ev in c.get("evidence", []):
            q = ev.get("quote")
            if not q: continue
            n += 1
            k = ev["item"]["key"][2:]
            if norm(q) in files.get(k, ""): continue
            if k.endswith(".ics"): miss_ics += 1; continue
            other.append((c["status"], round(c.get("confidence", 0), 2), k, q[:90], c["text"][:80]))
print(f"{label}: quotes {n}, not verbatim: .ics {miss_ics}, other {len(other)}")
for o in other: print("  ", o)
