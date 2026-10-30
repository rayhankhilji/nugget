#!/usr/bin/env python3
"""GG5 vault-edits, third run (fresh home, phase1+phase2, --no-models):
S8c  an editor/formatter that drops every HTML comment (all region and claim markers) while the
     user also rewords a fact line on the Sam Patel page;
S9b' delete a page file, then re-extract its entity's source: does the page come back, and how?

Usage: python3 qa/GG5/scenarios/vault-edits/run_strip.py <out-dir>
"""
from __future__ import annotations

import json
import os
import re
import shutil
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(ROOT / "qa/GG5/harness"))
import nugget_qa as q  # noqa: E402

OUT = Path(sys.argv[1]).resolve()
if OUT.exists():
    shutil.rmtree(OUT)
OUT.mkdir(parents=True)
HOME = OUT / "home"
WORK = HOME / "corpus"
d = q.Daemon(str(ROOT / "target/debug/nuggetd"), HOME, OUT)
RES: dict = {}


def settle(label):
    time.sleep(4)
    d.settle(label, 900, quiet_s=8)


def ent(name):
    best = None
    for e in d.call("knowledge.entities", {"text": name, "limit": 50}):
        if e["name"] == name and (best is None or e["claims"] > best["claims"]):
            best = e
    return d.call("knowledge.entity", {"id": best["id"]})


def main():
    d.write_config(None, None)
    d.start()
    WORK.mkdir(parents=True)
    shutil.copytree(q.CORPUS / "phase1", WORK, dirs_exist_ok=True)
    q.set_item_dates(WORK)
    d.call("sources.add", {"kind": "files", "name": "Mara's files", "settings": {"roots": [str(WORK)]}})
    settle("phase1")
    shutil.copytree(q.CORPUS / "phase2", WORK, dirs_exist_ok=True)
    q.set_item_dates(WORK)
    settle("phase2")

    sp = ent("Sam Patel")
    p = d.vault / sp["vault_path"]
    before = p.read_text()
    t = re.sub(r" ?<!--.*?-->", "", before)  # every comment gone, inline and on its own line
    t = t.replace("Sam Patel works at", "Sam Patel is a senior data engineer at", 1)
    t = t.rstrip("\n") + "\n\nMy note on Sam Patel.\n"
    p.write_text(t)
    d.note("edit", page=str(p), what="S8c strip all comments + reword works_at", diff=q.unified(before, t))
    (OUT / "sam_patel_user.md").write_text(t)

    jo = ent("Jonas Albers")
    jp = d.vault / jo["vault_path"]
    jtext = jp.read_text()
    jp.unlink()
    d.note("edit", page=str(jp), what="S9b delete page")
    settle("edits")
    e = d.call("knowledge.entity", {"id": sp["id"]})
    RES["after_edit"] = {
        "sam_claims": [(c["id"], c["status"], c["text"]) for c in e["claims"]],
        "sam_page": p.read_text(),
        "jonas_page_exists": jp.exists(),
        "conflicts": d.call("knowledge.status")["vault"]["conflicts"],
    }
    # re-extract every source
    for f in WORK.rglob("*"):
        if f.is_file():
            if f.suffix == ".md":
                f.write_text(f.read_text() + "\n\n(re-saved)\n")
            else:
                os.utime(f)
    settle("reextract")
    e = d.call("knowledge.entity", {"id": sp["id"]})
    RES["after_reextract"] = {
        "sam_claims": [(c["id"], c["status"], c["text"]) for c in e["claims"]],
        "sam_page": p.read_text(),
        "jonas_page_exists": jp.exists(),
        "jonas_page": jp.read_text() if jp.exists() else None,
        "conflicts": d.call("knowledge.status")["vault"]["conflicts"],
    }
    (OUT / "results.json").write_text(json.dumps(RES, indent=1, ensure_ascii=False))


try:
    main()
finally:
    d.stop()
