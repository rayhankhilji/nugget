#!/usr/bin/env python3
"""Print every scenario's checks and observed-vs-ledger comparison under a results directory, as
compact JSON lines between GG8-SUMMARY markers (so they can be read back from the job log)."""
import json
import os
import sys

root = sys.argv[1]
print("GG8-SUMMARY-BEGIN")
for d in sorted(os.listdir(root)) if os.path.isdir(root) else []:
    p = os.path.join(root, d)
    if not os.path.isdir(p):
        continue
    s = os.path.join(p, "summary.json")
    if os.path.exists(s):
        sm = json.load(open(s))
        for c in sm.get("checks", []):
            print(json.dumps({"scenario": d, "check": c["id"], "ok": c["ok"],
                              "detail": str(c.get("detail"))[:1500]}))
        print(json.dumps({"scenario": d, "dns_names": sm.get("dns_names"),
                          "observed_hosts": sm.get("observed_hosts"),
                          "unledgered": sm.get("unledgered_hosts"),
                          "notes": [m for m in sm.get("marks", []) if "note" in m][:30]})[:6000])
        if "per_process" in sm:
            print(json.dumps({"scenario": d, "per_process": sm["per_process"]})[:6000])
    o = os.path.join(p, "observed-vs-ledger.json")
    if os.path.exists(o):
        ov = json.load(open(o))
        rows = [(r["host"], r["feature"], r.get("bytes_sent"), r.get("bytes_received"))
                for r in ov.get("ledger_rows", [])]
        print(json.dumps({"scenario": d, "observed": ov.get("observed"), "ledger": ov.get("ledger"),
                          "unledgered": ov.get("unledgered"), "dns": ov.get("dns"),
                          "ledger_rows": rows[:60]})[:12000])
    for f in sorted(os.listdir(p)):
        if f.endswith("-traffic.json") or f == "error.txt" or f == "mcp.txt":
            txt = open(os.path.join(p, f), errors="replace").read()
            print(json.dumps({"scenario": d, "file": f, "text": txt[:4000]}))
print("GG8-SUMMARY-END")
