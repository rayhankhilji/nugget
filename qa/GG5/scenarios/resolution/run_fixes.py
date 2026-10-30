#!/usr/bin/env python3
"""GG5 resolution scenario A: index the whole persona corpus (deterministic extractors only),
then use the user's manual tools over IPC and check they hold.

Steps (each captured under <out>/):
  base        phase1+phase2 indexed, settled
  merge       knowledge.merge Gus (tessellate) -> Gus (lumenfold), Tessellate -> Lumenfold
  wrongmerge  the user merges Sam Patel into "Sam" (Okonkwo) by mistake
  split       knowledge.split moves Sam Patel's facts + email back out
  header      the user edits the "# R. Chen" heading of Robert's page to "# Robert Chen"
  reextract   re-save every source naming Gus/Sam/Tessellate/Lumenfold/Chen (mtime / append)
  restart     daemon restart

Usage: python3 run_fixes.py <out-dir>
"""
import json
import os
import re
import shutil
import sys
import time
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
R = {}


def ents():
    res, off = [], 0
    while True:
        page = d.call("knowledge.entities", {"limit": 200, "offset": off})
        res += page
        if len(page) < 200:
            return res
        off += 200


def by_name(name, kind=None):
    return [e for e in ents() if e["name"] == name and (kind is None or e["kind"] == kind)]


def detail(i):
    try:
        return d.call("knowledge.entity", {"id": i})
    except q.RpcError as e:
        return {"error": str(e)}


def brief(x):
    if "error" in x:
        return x
    return {"id": x["id"], "name": x["name"], "aliases": x["aliases"], "identifiers": x["identifiers"],
            "vault_path": x.get("vault_path"),
            "claims": [f'{c["subject"]["name"]} {c["predicate"]} {(c["object"].get("entity") or {}).get("name") or c["object"].get("literal")} [{c["status"]}] #{c["id"]}' for c in x["claims"]]}


def people_orgs(label):
    rows = [(e["id"], e["kind"], e["name"]) for e in ents() if e["kind"] in ("person", "organization")]
    R[label + ".people_orgs"] = rows
    return rows


def vault_pages(pattern):
    return sorted(str(p.relative_to(d.vault)) for p in d.vault.glob(pattern))


def save():
    (out / "result.json").write_text(json.dumps(R, indent=1, ensure_ascii=False))


try:
    work = home / "corpus"
    work.mkdir()
    shutil.copytree(q.CORPUS / "phase1", work, dirs_exist_ok=True)
    shutil.copytree(q.CORPUS / "phase2", work, dirs_exist_ok=True)
    q.set_item_dates(work)
    d.call("sources.add", {"kind": "files", "name": "Mara's files", "settings": {"roots": [str(work)]}})
    d.settle("base", 900, quiet_s=10)
    d.snapshot("base")
    people_orgs("base")

    gus = by_name("Gus Ferreira", "person")
    gus_d = {g["id"]: detail(g["id"]) for g in gus}
    g_new = next(i for i, x in gus_d.items() if ["email", "gus@lumenfold.example"] in x["identifiers"])
    g_old = next(i for i, x in gus_d.items() if i != g_new)
    tess = by_name("Tessellate", "organization")[0]["id"]
    lum = by_name("Lumenfold", "organization")[0]["id"]
    R["ids"] = {"gus_new": g_new, "gus_old": g_old, "tessellate": tess, "lumenfold": lum}

    # --- merge
    d.call("knowledge.merge", {"into": g_new, "from": g_old})
    d.call("knowledge.merge", {"into": lum, "from": tess})
    time.sleep(3)
    R["merge.gus_old_lookup"] = brief(detail(g_old))
    R["merge.gus_new"] = brief(detail(g_new))
    R["merge.tess_old_lookup"] = brief(detail(tess))
    R["merge.lumenfold"] = brief(detail(lum))
    R["merge.entities_named"] = {n: [(e["id"], e["kind"]) for e in by_name(n)] for n in ("Gus Ferreira", "Tessellate", "Lumenfold")}
    R["merge.vault_person_gus"] = vault_pages("Person/Gus*")
    R["merge.vault_org"] = vault_pages("Organization/*")
    old_page = d.vault / "Person" / f"Gus Ferreira ({g_old}).md"
    R["merge.gus_old_page"] = old_page.read_text() if old_page.exists() else None
    tp = d.vault / "Organization" / f"Tessellate ({tess}).md"
    R["merge.tess_old_page"] = tp.read_text() if tp.exists() else None
    # Links to the old ids anywhere in the vault?
    R["merge.links_to_old_ids"] = sorted({str(p.relative_to(d.vault)) for p in d.vault.rglob("*.md")
                                          if (g_old in p.read_text() or tess in p.read_text()) and p.name not in (old_page.name, tp.name)})
    d.snapshot("merge")
    save()

    # --- wrong merge, then split
    sam = by_name("Sam", "person")[0]["id"]
    sp = by_name("Sam Patel", "person")[0]["id"]
    R["ids"].update({"sam_okonkwo": sam, "sam_patel": sp})
    sp_before = detail(sp)
    R["wrongmerge.sam_patel_before"] = brief(sp_before)
    d.call("knowledge.merge", {"into": sam, "from": sp})
    time.sleep(2)
    merged = detail(sam)
    R["wrongmerge.sam"] = brief(merged)
    # The user picks Sam Patel's facts: every claim whose evidence names sam.patel or Harbor membership
    patel_claims = [c["id"] for c in merged["claims"] if any("sam.patel" in ev["quote"] or "Sam Patel" in ev["quote"] for ev in c["evidence"])
                    and not any("sokonkwo" in ev["quote"] for ev in c["evidence"])]
    R["split.claims_chosen"] = patel_claims
    new = d.call("knowledge.split", {"entity": sam, "claims": patel_claims, "name": "Sam Patel",
                                       "aliases": ["Sam Patel"], "identifiers": [["email", "sam.patel@brightwell.example"]]})
    time.sleep(3)
    R["ids"]["sam_patel_split"] = new["id"]
    R["split.new"] = brief(detail(new["id"]))
    R["split.sam_after"] = brief(detail(sam))
    R["split.old_sam_patel_lookup"] = brief(detail(sp))
    R["split.vault_person_sam"] = vault_pages("Person/Sam*")
    R["split.links_to_old_sp"] = sorted({str(p.relative_to(d.vault)) for p in d.vault.rglob("*.md") if sp in p.read_text()})
    oldsp = d.vault / "Person" / f"Sam Patel ({sp}).md"
    R["split.old_sp_page"] = oldsp.read_text() if oldsp.exists() else None
    d.snapshot("split")
    save()

    # --- header edit: rename "R. Chen" by editing the page heading
    rc = by_name("R. Chen", "person")
    R["header.rc_found"] = rc
    if rc:
        rcid = rc[0]["id"]
        R["ids"]["robert"] = rcid
        p = d.vault / "Person" / f"R. Chen ({rcid}).md"
        t = p.read_text()
        p.write_text(t.replace("# R. Chen\n", "# Robert Chen\n", 1))
        time.sleep(8)
        d.settle("header", 300, quiet_s=6)
        R["header.robert"] = brief(detail(rcid))
        R["header.vault"] = vault_pages("Person/R*") + vault_pages("Person/Robert*")
        R["header.conflicts"] = d.call("knowledge.status")["vault"]
        # second attempt: frontmatter alias is the documented route; does an alias change the name?
    d.snapshot("header")
    save()

    # --- re-extraction: re-save sources
    touched = q.touch_sources(d, work, ["Gus", "Tessellate", "Lumenfold", "Sam", "Chen"])
    R["reextract.touched"] = touched
    time.sleep(5)
    d.settle("reextract", 900, quiet_s=10)
    people_orgs("reextract")
    for k, v in R["ids"].items():
        R[f"reextract.{k}"] = brief(detail(v))
    R["reextract.entities_named"] = {n: [(e["id"], e["kind"]) for e in by_name(n)] for n in ("Gus Ferreira", "Tessellate", "Lumenfold", "Sam", "Sam Patel", "R. Chen", "Robert Chen")}
    d.snapshot("reextract")
    save()

    # --- restart
    d.stop()
    d.start()
    time.sleep(8)
    d.settle("restart", 900, quiet_s=10)
    people_orgs("restart")
    for k, v in R["ids"].items():
        R[f"restart.{k}"] = brief(detail(v))
    R["restart.vault_person"] = vault_pages("Person/*")
    R["restart.vault_org"] = vault_pages("Organization/*")
    d.snapshot("restart")
    save()
finally:
    save()
    d.stop()
