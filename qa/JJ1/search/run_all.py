"""JJ1 search QA: run every scenario, print pass/fail, write qa/JJ1/search/results.json.

    python3 qa/JJ1/search/run_all.py            # daemon + desktop
    python3 qa/JJ1/search/run_all.py daemon     # daemon only
    python3 qa/JJ1/search/run_all.py desktop    # desktop only

Needs target/debug/nuggetd (NUGGETD=...) and, for desktop, apps/desktop/dist + node_modules.
Homes live under /tmp/jj1/search (JJ1_SEARCH_BASE)."""
import json
import os
import subprocess
import sys
import time

from common import HERE, ROOT, BASE, NUGGETD, Daemon, corpus, fresh, index
import daemon_scenarios


def desktop():
    out = []
    c_dir, _ = corpus()
    home = fresh("desktop")
    storage = os.path.join(BASE, "desktop-storage.json")
    if os.path.exists(storage):
        os.remove(storage)
    port = str(47000 + os.getpid() % 1000)
    with Daemon(home, nuggetd=NUGGETD) as d:
        index(d, c_dir, bulk=False)
        sock = os.path.join(home, "data", "ipc", "nuggetd.sock")
        for phase in ("a", "b"):
            if phase == "b":
                d.restart()
            res_path = os.path.join(BASE, f"desktop-{phase}.json")
            print(f"desktop phase {phase}")
            p = subprocess.run(["node", os.path.join(HERE, "desktop.mjs"), phase, sock, port, storage, res_path],
                               cwd=ROOT, timeout=900)
            try:
                r = json.load(open(res_path))
            except Exception as e:
                out.append({"scenario": f"UI phase {phase}", "pass": False, "checks": [{"check": "runner", "ok": False, "detail": f"rc={p.returncode} {e}"}]})
                continue
            for c in r["results"]:
                out.append({"scenario": "UI " + c["check"], "pass": c["ok"], "checks": [c]})
    return out


def main():
    which = sys.argv[1:] or ["daemon", "desktop"]
    results = []
    t0 = time.time()
    if "daemon" in which:
        results += daemon_scenarios.run()
    if "desktop" in which:
        results += desktop()
    passed = sum(r["pass"] for r in results)
    print(f"\n{passed}/{len(results)} scenarios pass ({time.time() - t0:.0f}s)")
    for r in results:
        print(f"{'PASS' if r['pass'] else 'FAIL'}  {r['scenario']}")
    path = os.path.join(HERE, "results.json")
    prev = {}
    if os.path.exists(path) and len(which) == 1:
        prev = {r["scenario"]: r for r in json.load(open(path)).get("scenarios", [])}
    merged = {**prev, **{r["scenario"]: r for r in results}}
    with open(path, "w") as f:
        json.dump({"nuggetd": NUGGETD, "ran_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                   "passed": sum(r["pass"] for r in merged.values()), "total": len(merged),
                   "scenarios": list(merged.values())}, f, indent=1, ensure_ascii=False)


if __name__ == "__main__":
    main()
