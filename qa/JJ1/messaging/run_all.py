#!/usr/bin/env python3
"""Run the JJ1 messaging scenarios against target/debug/nuggetd (or $NUGGETD).

    python3 qa/JJ1/messaging/run_all.py            # everything
    python3 qa/JJ1/messaging/run_all.py locales tg  # scenario functions whose name contains a word

Prints one PASS/FAIL line per scenario and writes qa/JJ1/messaging/results.json.
Homes and fixtures go under $JJ1_MSG_BASE (default /tmp/jj1/messaging).
"""
import json
import os
import sys
import time
import traceback

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import sc_imessage  # noqa: E402
import sc_telegram  # noqa: E402
import sc_whatsapp  # noqa: E402


def compact(r):
    """Summary for results.json: counts, and failed checks with a short observed value."""
    checks = r.get("checks", [])
    return {"scenario": r["scenario"], "ok": r["ok"], "checks": len(checks),
            "passed": sum(c["ok"] for c in checks),
            "failed": [{"check": c["check"], "observed": json.dumps(c.get("observed"), ensure_ascii=False)[:160]}
                       for c in checks if not c["ok"]] or (["exception"] if r.get("failed") == ["exception"] else []),
            "notes": [n[:200] for n in r.get("notes", [])][:2]}


def main():
    words = sys.argv[1:]
    fns = sc_whatsapp.ALL + sc_telegram.ALL + sc_imessage.ALL
    if words:
        fns = [f for f in fns if any(w in f.__module__ + "." + f.__name__ for w in words)]
    results = []
    for fn in fns:
        t0 = time.time()
        try:
            rs = fn()
        except Exception as e:  # noqa: BLE001
            rs = [{"scenario": fn.__name__, "ok": False, "checks": [], "failed": ["exception"],
                   "notes": [f"{type(e).__name__}: {e}", traceback.format_exc()[-2000:]]}]
        for r in rs:
            r = compact(r)
            r["group"] = fn.__module__ + "." + fn.__name__
            r["secs"] = round(time.time() - t0, 1)
            results.append(r)
            line = f"{'PASS' if r['ok'] else 'FAIL'}  {r['scenario']}"
            if not r["ok"]:
                line += "  -- " + "; ".join(f["check"] if isinstance(f, dict) else f for f in r["failed"])
            print(line, flush=True)
    path = os.path.join(HERE, "results.json")
    old = []
    if words and os.path.exists(path):
        try:
            old = [r for r in json.load(open(path))["results"] if r["scenario"] not in {x["scenario"] for x in results}]
        except Exception:  # noqa: BLE001
            old = []
    allr = old + results
    allr.sort(key=lambda r: r["scenario"])
    with open(path, "w", encoding="utf-8") as f:
        json.dump({"passed": sum(r["ok"] for r in allr), "total": len(allr), "results": allr}, f, indent=0, ensure_ascii=False)
    print(f"{sum(r['ok'] for r in results)}/{len(results)} scenarios passed this run; "
          f"{sum(r['ok'] for r in allr)}/{len(allr)} in results.json")


if __name__ == "__main__":
    main()
