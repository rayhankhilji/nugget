#!/usr/bin/env python3
"""Claims and entity names carrying gold.json sensitive strings (plus finance figures), with the
entity kind, predicate, status and source item, per snapshot; and the vault pages that show them.

usage: sensitive_claims.py GOLD RUN_DIR LABEL"""
import json, sys
from pathlib import Path

gold = json.loads(Path(sys.argv[1]).read_text())
run, label = Path(sys.argv[2]), sys.argv[3]
sens = gold["sensitive_strings"] + ["210,437.55", "214,880.12", "5,312", "NCU-", "Brandt", "ferrous", "anaemia"]
d = json.loads((run / label / "entities.json").read_text())
seen = set()
for e in d["details"].values():
    for c in e.get("claims", []):
        blob = (c["text"] + json.dumps(c["object"], ensure_ascii=False) + c["subject"]["name"]).lower()
        hit = [s for s in sens if s.lower() in blob]
        if hit and c["id"] not in seen:
            seen.add(c["id"])
            subj = d["details"].get(c["subject"]["id"], {})
            items = sorted({ev["item"]["key"][2:] for ev in c.get("evidence", [])})
            quotes = [ev.get("quote") for ev in c.get("evidence", [])][:1]
            print(f"[{c['status']}] {subj.get('kind')}:{c['subject']['name']!r} {c['predicate']} {c['object'].get('literal') or c['object'].get('entity',{}).get('name')!r} <- {items} quote={quotes}")
v = run / label / "vault"
for p in sorted(v.rglob("*.md")):
    if ".nugget" in p.parts:
        continue
    t = p.read_text(errors="replace")
    hit = sorted({s for s in sens if s.lower() in t.lower()})
    if hit:
        print("VAULT", p.relative_to(v), hit)
