#!/usr/bin/env python3
"""GG5 vault-edits, second run (fresh home, phase1+phase2, --no-models):
S11  edit a fact line but keep its [[link]] (the usual "fix the wording" edit), then re-extract;
S10  edit Dana's works_at line, then a newer source (her typed person note, re-saved now with a
     new org) says something else;
S4c  a two-word alias ("Bobby Chen") added in the frontmatter, then a new item naming it.

Usage: python3 qa/GG5/scenarios/vault-edits/run_s10_s11.py <out-dir>
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


def page(eid):
    return d.vault / d.call("knowledge.entity", {"id": eid})["vault_path"]


def look(label, eid, needles):
    e = d.call("knowledge.entity", {"id": eid})
    t = page(eid).read_text()
    RES.setdefault(label, {})[e["name"] + " " + eid] = {
        "claims": [(c["id"], c["status"], c["predicate"], c["text"]) for c in e["claims"] if any(n in c["text"] for n in needles)],
        "page": [l for l in t.split("\n") if any(n in l for n in needles)],
        "aliases": e["aliases"],
    }
    (OUT / "results.json").write_text(json.dumps(RES, indent=1, ensure_ascii=False))


def edit(p: Path, fn, what):
    a = p.read_text()
    b = fn(a)
    assert a != b, what
    p.write_text(b)
    d.note("edit", page=str(p), what=what, diff=q.unified(a, b))


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

    rc = ent("R. Chen")
    dana = ent("Dana Whitfield")

    # S11: reword Robert's works_at line, keeping the link and the marker.
    def s11(t):
        out = []
        for line in t.split("\n"):
            m = q.FACT_RE.match(line)
            if m and "works at" in m["text"]:
                link = re.search(r"\[\[[^\]]+\]\]", m["text"]).group(0)
                line = f"- Robert Chen is VP Engineering at {link} (reworded by me){m['foot']} <!-- c:{m['id']} -->"
            out.append(line)
        return "\n".join(out)
    edit(page(rc["id"]), s11, "S11 reword works_at keeping the link")

    # S10: Dana's works_at line edited (link removed).
    def s10(t):
        out = []
        for line in t.split("\n"):
            m = q.FACT_RE.match(line)
            if m and "works at" in m["text"]:
                line = f"- Dana Whitfield is Head of Product at Corvid Labs (fixed by me){m['foot']} <!-- c:{m['id']} -->"
            out.append(line)
        return "\n".join(out)
    edit(page(dana["id"]), s10, "S10 fix Dana works_at")

    # S4c: two-word alias on R. Chen, compact JSON spacing.
    edit(page(rc["id"]), lambda t: re.sub(r"^aliases: \[(.*?)\]$", lambda m: f'aliases: [{m.group(1)},"Bobby Chen"]', t, count=1, flags=re.M), "S4c alias Bobby Chen")
    settle("edits")
    look("after_edit", rc["id"], ["works at", "VP Engineering"])
    look("after_edit", dana["id"], ["works at", "fixed by me", "Kestrel"])

    # re-extract everything (as touching all sources would)
    for f in WORK.rglob("*"):
        if f.is_file():
            if f.suffix == ".md":
                f.write_text(f.read_text() + "\n\n(re-saved)\n")
            else:
                os.utime(f)
    settle("reextract")
    look("after_reextract", rc["id"], ["works at", "VP Engineering"])
    look("after_reextract", dana["id"], ["works at", "fixed by me", "Kestrel"])

    # S10 newer source: Dana's own person note now says Kestrel Robotics (saved now).
    n = WORK / "Notes/People/Dana-Whitfield.md"
    n.write_text(n.read_text().replace("org: Corvid Labs", "org: Kestrel Robotics").replace("date: 2026-08-17", "date: 2026-10-30"))
    settle("newer")
    look("after_newer_source", dana["id"], ["works at", "fixed by me", "Kestrel", "Corvid"])

    # S4c resolution: a new note and a new mail naming "Bobby Chen".
    (WORK / "Notes/Meetings/2026-10-29-lunch-with-Bobby-Chen.md").write_text(
        "---\ndate: 2026-10-29\npeople:\n  - Bobby Chen\n---\n# Lunch with Bobby Chen\n\nTalked about the launch.\n")
    settle("alias-item")
    ents = d.call("knowledge.entities", {"limit": 500})
    RES["S4c_alias_resolution"] = {
        "entities_named_bobby": [(e["id"], e["name"]) for e in ents if "Bobby" in e["name"]],
        "rchen_mention_surfaces": sorted({m["surface"] for m in d.call("knowledge.entity", {"id": rc["id"]})["mentions"]}),
    }
    (OUT / "results.json").write_text(json.dumps(RES, indent=1, ensure_ascii=False))

    d.stop()
    d.start()
    settle("restart")
    look("after_restart", rc["id"], ["works at", "VP Engineering"])
    look("after_restart", dana["id"], ["works at", "fixed by me", "Kestrel", "Corvid"])


try:
    main()
finally:
    d.stop()
