#!/usr/bin/env python3
"""Summarise a drive.py output: per follow-up turn, rewrite and hit vs original-query hit."""
import json, sys
rows = [json.loads(l) for l in open(sys.argv[1])]
fu = [r for r in rows if r["turn"] > 1 and r["support"]]
rw = [r for r in fu if r.get("query_rewrite")]
print(f"follow-up turns with support: {len(fu)}; rewritten: {len(rw)}")
print(f"hit with daemon query (rewrite if any): {sum(bool(r['hit']) for r in fu)}/{len(fu)}")
print(f"hit with original text only:           {sum(bool(r['orig_hit']) for r in fu)}/{len(fu)}")
ms = sorted(r["started_ms"] for r in rows if r["turn"] > 1 and "started_ms" in r)
if ms:
    print(f"started_ms follow-ups: min {ms[0]} median {ms[len(ms)//2]} max {ms[-1]}")
for r in fu:
    if r["hit"] != r["orig_hit"]:
        print(f"  {r['conv']}.{r['turn']} {r['q']!r} -> {r.get('query_rewrite')!r}: hit={r['hit']} orig_hit={r['orig_hit']}")
