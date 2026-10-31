#!/usr/bin/env python3
"""Verify: adding an alias in R. Chen's page frontmatter only adds an alias (name unchanged).
Reuses the home of a finished run_fixes.py run. Usage: alias_check.py <run_fixes out-dir>"""
import json, sys, time
from pathlib import Path
ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "harness"))
import nugget_qa as q
BIN = str(ROOT.parents[1] / "target" / "debug" / "nuggetd")
out = Path(sys.argv[1]).resolve()
(out / "aliascheck").mkdir(exist_ok=True)
d = q.Daemon(BIN, out / "home", out / "aliascheck")
d.start()
try:
    time.sleep(5)
    rc = [e for e in d.call("knowledge.entities", {"limit": 200, "offset": 0}) if e["name"] == "R. Chen"][0]["id"]
    p = d.vault / "Person" / f"R. Chen ({rc}).md"
    t = p.read_text()
    p.write_text(t.replace('aliases: ["Robert Chen"]', 'aliases: ["Robert Chen", "Bob Chen"]', 1))
    time.sleep(8)
    d.settle("alias", 300, quiet_s=6)
    x = d.call("knowledge.entity", {"id": rc})
    print(json.dumps({"name": x["name"], "aliases": x["aliases"], "vault_path": x.get("vault_path"),
                      "pages": sorted(str(q.relative_to(d.vault)) for q in d.vault.glob("Person/*Chen*"))}))
finally:
    d.stop()
