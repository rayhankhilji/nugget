#!/usr/bin/env python3
"""GG5 app scenario: populate a home with the persona corpus (phase1+phase2, no models), settle,
snapshot, then leave the daemon running for the browser UI. Writes the daemon pid and socket path
to <out>/daemon.json. Stop it with `kill <pid>` (SIGTERM) when done.

Usage: python3 qa/GG5/scenarios/app/populate.py <home> <out>
"""
import json
import shutil
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "harness"))
import nugget_qa  # noqa: E402

home = Path(sys.argv[1]).resolve()
out = Path(sys.argv[2]).resolve()
home.mkdir(parents=True, exist_ok=True)
out.mkdir(parents=True, exist_ok=True)
d = nugget_qa.Daemon("/home/user/nugget/target/debug/nuggetd", home, out)
d.write_config(None, None)
d.start()
work = home / "corpus"
if work.exists():
    shutil.rmtree(work)
work.mkdir()
shutil.copytree(nugget_qa.CORPUS / "phase1", work, dirs_exist_ok=True)
nugget_qa.set_item_dates(work)
d.call("sources.add", {"kind": "files", "name": "Mara's files", "settings": {"roots": [str(work)]}})
d.settle("phase1", 1800)
shutil.copytree(nugget_qa.CORPUS / "phase2", work, dirs_exist_ok=True)
nugget_qa.set_item_dates(work)
time.sleep(5)
d.settle("phase2", 1800)
d.snapshot("phase2")
(out / "daemon.json").write_text(json.dumps({"pid": d.proc.pid, "socket": d.socket_path(), "home": str(home)}))
d.client.close()
print("READY", d.proc.pid, d.socket_path(), flush=True)
