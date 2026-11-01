#!/usr/bin/env python3
"""HH2 deterministic sensitivity rules, no model: index the triage corpus and check each planted
item's `sensitive` flag (search.faceted hit) against truth.jsonl. Also: junk items stay searchable.
Usage: rules_check.py --corpus DIR --home DIR [--nuggetd BIN] --out results.json"""
import argparse, json, os, sys, time
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "harness"))
from nq import Daemon, dump

ap = argparse.ArgumentParser()
ap.add_argument("--corpus", required=True); ap.add_argument("--home", required=True)
ap.add_argument("--nuggetd"); ap.add_argument("--out", required=True)
ap.add_argument("--assume-safe", action="store_true", help="[ingest] assume_unflagged_safe = true, so only rules mark items")
a = ap.parse_args()
truth = [json.loads(l) for l in open(os.path.join(a.corpus, "truth.jsonl"), encoding="utf-8")]
with Daemon(a.home, nuggetd=a.nuggetd) as d:
    if a.assume_safe:
        d.stop()
        with open(d.config_path(), "a") as f:
            f.write("\n[ingest]\nassume_unflagged_safe = true\n")
        d.start()
    t0 = time.time()
    d.call("sources.add", {"kind": "files", "name": "Mail", "settings": {"roots": [os.path.abspath(os.path.join(a.corpus, "mail"))]}})
    st = d.wait_idle(min_items=len(truth), timeout=900)
    t_idx = time.time() - t0
    rows = []
    for r in truth:
        if not r.get("probe") or r["class"] == "normal" and r["lang"] is None:
            continue
        res = d.call("search.faceted", {"query": f'"{r["probe"]}"' if " " in r["probe"] else r["probe"], "limit": 20, "offset": 0,
                                        "filters": {}, "utc_offset_minutes": 0})
        hits = res.get("hits", [])
        mine = [h for h in hits if r["path"].split("/")[-1] in json.dumps(h)]
        h = mine[0] if mine else None
        rows.append({"path": r["path"], "lang": r["lang"], "class": r["class"], "kind": r["kind"], "probe": r["probe"],
                     "found": h is not None, "sensitive": (h or {}).get("sensitive"), "nhits": len(hits)})
    out = {"t_indexed_s": round(t_idx, 1), "ingest": st, "rows": rows, "sample_hit": hits[:1]}
    dump(out, a.out)
sens = [r for r in rows if r["class"] == "sensitive"]
caught = [r for r in sens if r["sensitive"]]
print(f"indexed {len(truth)} in {t_idx:.1f}s; sensitive caught {len(caught)}/{len(sens)}")
for lang in ("en", "de", "es", "fr"):
    s = [r for r in sens if r["lang"] == lang]
    print(f"  {lang}: {sum(1 for r in s if r['sensitive'])}/{len(s)} missed: {[r['kind']+':'+r['probe'] for r in s if not r['sensitive']]}")
norm = [r for r in rows if r["class"] != "sensitive"]
print(f"non-sensitive flagged: {[r['probe'] for r in norm if r['sensitive']]} of {len(norm)}; not found: {[r['probe'] for r in rows if not r['found']]}")
