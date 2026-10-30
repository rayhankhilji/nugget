#!/usr/bin/env python3
"""GG5 resolution scenario B: is a person's display name order-dependent?

Same corpus (phase1+phase2), same dates, only the calendar folder is renamed so it sorts after
Mail/ and Notes/ ("zz-Calendar"). Prints the person/organisation entities. Compare with the
plain run (scenario A base, or the reference run o1/phase2), where Calendar/ sorts first.

Usage: python3 run_order.py <out-dir>
"""
import json
import shutil
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "harness"))
import nugget_qa as q  # noqa: E402

BIN = str(ROOT.parents[1] / "target" / "debug" / "nuggetd")
out = Path(sys.argv[1]).resolve()
if out.exists():
    shutil.rmtree(out)
out.mkdir(parents=True)
home = out / "home"
home.mkdir()
d = q.Daemon(BIN, home, out)
d.write_config(None, None)
d.start()
try:
    work = home / "corpus"
    work.mkdir()
    for ph in ("phase1", "phase2"):
        for sub in (q.CORPUS / ph).iterdir():
            dst = work / ("zz-Calendar" if sub.name == "Calendar" else sub.name)
            shutil.copytree(sub, dst, dirs_exist_ok=True)
    q.set_item_dates(work)
    d.call("sources.add", {"kind": "files", "name": "Mara's files", "settings": {"roots": [str(work)]}})
    d.settle("order", 900, quiet_s=10)
    snap = d.snapshot("order")
    rows = []
    for e in snap["list"]:
        if e["kind"] in ("person", "organization"):
            x = snap["details"][e["id"]]
            rows.append({"kind": e["kind"], "name": e["name"], "aliases": x["aliases"], "identifiers": x["identifiers"]})
    (out / "result.json").write_text(json.dumps(rows, indent=1, ensure_ascii=False))
    for r in rows:
        print(json.dumps(r, ensure_ascii=False))
finally:
    d.stop()
