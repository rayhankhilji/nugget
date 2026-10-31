#!/usr/bin/env python3
"""truth-sensitive S3: notes are dated by file mtime, not by their own `date:`.

Full corpus (phase1+phase2, dated as the harness does), plus one old typed person note written in
March, before Dana left Brightwell:

    Notes/People/Dana (Brightwell).md   date: 2026-03-01, org: Brightwell Analytics

Variant `dated`:  the note's mtime is 2026-03-01 (what the harness does for every file).
Variant `copied`: the note's mtime is "now", as after `cp`, a git clone, a sync client or a
                  restore from backup; its frontmatter still says 2026-03-01.
Captures Dana's works_at claims (status, observed_at of each evidence) in both variants.

Usage: python3 qa/GG5/scenarios/truth-sensitive/s3_copied_note_dates.py <workdir>
"""
import json
import os
import shutil
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]
sys.path.insert(0, str(ROOT / "qa/GG5/harness"))
sys.path.insert(0, str(HERE))
import nugget_qa  # noqa: E402
from s1_sensitive import BIN  # noqa: E402

OLD_NOTE = """---
type: person
date: 2026-03-01
email: dana.whitfield@brightwell.example
org: Brightwell Analytics
role: Design Lead
---
# Dana Whitfield

Design Lead at Brightwell Analytics; owns the Harbor dashboards.
"""


def run(work: Path, copied: bool):
    if work.exists():
        shutil.rmtree(work)
    home, out = work / "home", work / "out"
    out.mkdir(parents=True); home.mkdir(parents=True)
    d = nugget_qa.Daemon(BIN, home, out)
    d.write_config(None, None, "\n[mcp]\nhttp = false\n")
    d.start()
    try:
        corpus = home / "corpus"
        corpus.mkdir()
        for ph in ("phase1", "phase2"):
            shutil.copytree(nugget_qa.CORPUS / ph, corpus, dirs_exist_ok=True)
        note = corpus / "Notes/People/Dana (Brightwell).md"
        note.write_text(OLD_NOTE)
        nugget_qa.set_item_dates(corpus)
        if copied:
            t = time.time()
            os.utime(note, (t, t))
        d.call("sources.add", {"kind": "files", "name": "Mara's files", "settings": {"roots": [str(corpus)]}})
        d.settle("s3", 900, quiet_s=10)
        snap = d.snapshot("s3")
    finally:
        d.stop()
    rows = []
    seen = set()
    for e in snap["details"].values():
        for c in e.get("claims", []):
            if c["predicate"] != "works_at" or "Dana" not in c["text"] or c["id"] in seen:
                continue
            seen.add(c["id"])
            rows.append({"id": c["id"], "status": c["status"], "text": c["text"],
                         "evidence": [(ev["item"]["key"], ev.get("observed_at"), ev.get("quote")) for ev in c["evidence"]]})
    page = nugget_qa.find_page(snap and (home / "data/vault"), "Dana Whitfield")
    return rows


def main():
    work = Path(sys.argv[1]).resolve()
    rep = {"dated": run(work / "dated", False), "copied": run(work / "copied", True)}
    (work / "s3.json").write_text(json.dumps(rep, indent=1, ensure_ascii=False))
    print(json.dumps(rep, indent=1, ensure_ascii=False))


if __name__ == "__main__":
    main()
