#!/usr/bin/env python3
"""GG5 vault-edits: user edits of vault pages against a real nuggetd (--no-models), checked
after the edit, after re-extraction of the source items and after a daemon restart.

Usage: python3 qa/GG5/scenarios/vault-edits/run_main.py <out-dir>
Writes <out>/log.jsonl, <out>/checks.json (per stage), snapshots of the vault per stage.
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

BIN = str(ROOT / "target/debug/nuggetd")
OUT = Path(sys.argv[1]).resolve()
if OUT.exists():
    shutil.rmtree(OUT)
OUT.mkdir(parents=True)
HOME = OUT / "home"
WORK = HOME / "corpus"
d = q.Daemon(BIN, HOME, OUT)
CHECKS: dict = {}
REC: dict = {}  # what each scenario touched (claim ids, page entity ids, lines)


def settle(label):
    time.sleep(4)
    d.settle(label, 900, quiet_s=8)


def entity_by_name(name):
    best = None
    for e in d.call("knowledge.entities", {"text": name, "limit": 50}):
        if e["name"] == name and (best is None or e["claims"] > best["claims"]):
            best = e
    if not best:
        return None
    return d.call("knowledge.entity", {"id": best["id"]})


def page_of(eid) -> Path | None:
    e = d.call("knowledge.entity", {"id": eid})
    vp = e.get("vault_path")
    return (d.vault / vp) if vp else None


def claim(eid, cid):
    e = d.call("knowledge.entity", {"id": eid})
    for c in e["claims"]:
        if c["id"] == cid:
            return c
    return None


def claims_by_text(eid, needle):
    e = d.call("knowledge.entity", {"id": eid})
    return [(c["id"], c["status"], c["predicate"], c["text"]) for c in e["claims"] if needle in c["text"]]


def write(page: Path, text: str, what: str, atomic=False):
    before = page.read_text()
    if atomic:
        tmp = page.with_name("." + page.name + ".swp")  # editor temp, then rename over the page
        tmp.write_text(text)
        os.replace(tmp, page)
    else:
        page.write_text(text)
    d.note("edit", page=str(page), what=what, atomic=atomic, diff=q.unified(before, text))


def first_fact(text, pred_heading=None, contains=None):
    for i, m in q.fact_lines(text):
        if contains and contains not in m["text"]:
            continue
        return i, m
    return None, None


def stage(label):
    """Record, for every scenario, the claim statuses and the page lines."""
    out = {}
    st = d.call("knowledge.status")
    out["vault_conflicts"] = st["vault"].get("conflicts")
    for key, r in REC.items():
        row = {}
        eid = r["eid"]
        try:
            ent = d.call("knowledge.entity", {"id": eid})
        except q.RpcError as e:
            row["entity_error"] = str(e)
            out[key] = row
            continue
        row["entity"] = {"id": ent["id"], "name": ent["name"], "aliases": ent["aliases"], "vault_path": ent.get("vault_path")}
        row["claims"] = {str(c): (lambda x: x and {"status": x["status"], "text": x["text"]})(claim(eid, c)) for c in r.get("claims", [])}
        row["user_claims"] = [(c["id"], c["status"], c["predicate"], c["text"]) for c in ent["claims"] if c["status"] == "user_asserted"]
        for needle in r.get("watch_text", []):
            row.setdefault("by_text", {})[needle] = claims_by_text(eid, needle)
        p = (d.vault / ent["vault_path"]) if ent.get("vault_path") else None
        if p and p.exists():
            t = p.read_text()
            row["page_lines"] = {n: [l for l in t.split("\n") if n in l] for n in r.get("page_text", [])}
            row["page_has"] = {n: (n in t) for n in r.get("page_text", [])}
        else:
            row["page"] = f"missing: {p}"
        for extra in r.get("files", []):
            row.setdefault("files", {})[extra] = Path(extra).exists()
        out[key] = row
    CHECKS[label] = out
    (OUT / "checks.json").write_text(json.dumps(CHECKS, indent=1, ensure_ascii=False))
    snap = OUT / label
    if snap.exists():
        shutil.rmtree(snap)
    shutil.copytree(d.vault, snap / "vault")
    d.note("stage", label=label)


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
    shutil.copytree(d.vault, OUT / "phase2" / "vault")

    # ---------------- Harbor page: S1 fix (multi-valued), S2 delete, S3 add x2, S5 notes
    harbor = entity_by_name("Harbor")
    hp = page_of(harbor["id"])
    t = hp.read_text()
    lines = t.split("\n")
    i_fix, m_fix = first_fact(t, contains="part of")
    i_del, m_del = first_fact(t, contains="one-on-one")
    lines[i_fix] = f"- Harbor is a Brightwell Analytics project (fixed by me){m_fix['foot']} <!-- c:{m_fix['id']} -->"
    lines[i_del] = None
    # add under the "### Part of" heading and outside any heading (right under "## Facts")
    for i, l in enumerate(lines):
        if l and l.startswith("### Part of"):
            lines.insert(i + 1, "- Harbor is sponsored by Robert Chen (added under heading by me)")
            break
    for i, l in enumerate(lines):
        if l == "## Facts":
            lines.insert(i + 1, "- Harbor is my top priority this year (added outside headings by me)")
            break
    t2 = "\n".join(l for l in lines if l is not None).rstrip("\n") + "\n\nMy private Harbor note: ask Gus about egress costs. Keep this.\n"
    write(hp, t2, "Harbor: S1 fix part_of line, S2 delete one-on-one line, S3 add 2 bullets, S5 note")
    REC["S1_fix_multi"] = {"eid": harbor["id"], "claims": [int(m_fix["id"])], "page_text": ["fixed by me", "Harbor is part of"], "watch_text": ["Brightwell", "fixed by me"]}
    REC["S2_delete"] = {"eid": harbor["id"], "claims": [int(m_del["id"])], "page_text": ["one-on-one]]"]}
    REC["S3_add"] = {"eid": harbor["id"], "page_text": ["added under heading", "added outside headings"], "watch_text": ["added"]}
    REC["S5_notes"] = {"eid": harbor["id"], "page_text": ["My private Harbor note"]}

    # ---------------- Dana: S1b fix a single-valued (works_at) line, link removed
    dana = entity_by_name("Dana Whitfield")
    dp = page_of(dana["id"])
    t = dp.read_text()
    lines = t.split("\n")
    i, m = first_fact(t, contains="works at")
    lines[i] = f"- Dana Whitfield is Head of Product at Corvid Labs (fixed by me){m['foot']} <!-- c:{m['id']} -->"
    write(dp, "\n".join(lines), "Dana: S1b fix works_at line")
    REC["S1b_fix_single"] = {"eid": dana["id"], "claims": [int(m["id"])], "page_text": ["fixed by me", "works at [["], "watch_text": ["works at", "fixed by me", "Kestrel"]}

    # ---------------- S4 alias: R. Chen with "a", "b" spacing (as a user types YAML), Gus with JSON spacing
    rc = entity_by_name("R. Chen") or entity_by_name("Robert Chen")
    rp = page_of(rc["id"])
    t = rp.read_text()
    t2 = re.sub(r"^aliases: \[(.*?)\]$", lambda mm: f'aliases: [{mm.group(1)}{", " if mm.group(1).strip() else ""}"Bobby"]', t, count=1, flags=re.M)
    write(rp, t2, "R. Chen: S4 alias Bobby, YAML spacing")
    REC["S4_alias_spaced"] = {"eid": rc["id"], "page_text": ["aliases:"], "files": [str(d.vault / ".nugget/conflicts" / rp.relative_to(d.vault))]}
    gus = entity_by_name("Gus Ferreira")
    gp = page_of(gus["id"])
    t = gp.read_text()
    t2 = re.sub(r"^aliases: \[(.*?)\]$", lambda mm: f'aliases: [{mm.group(1)}{"," if mm.group(1).strip() else ""}"Gustavo"]', t, count=1, flags=re.M)
    write(gp, t2, "Gus: S4 alias Gustavo, JSON spacing")
    REC["S4_alias_compact"] = {"eid": gus["id"], "page_text": ["aliases:"], "files": [str(d.vault / ".nugget/conflicts" / gp.relative_to(d.vault))]}

    # ---------------- S8 atomic save (Sam Patel) and marker-stripping retype (Priyanka)
    sp_e = entity_by_name("Sam Patel")
    spp = page_of(sp_e["id"])
    t = spp.read_text()
    lines = t.split("\n")
    for i, l in enumerate(lines):
        if l.startswith("### "):
            lines.insert(i + 1, "- Sam Patel owns the backfill pipeline (atomic save by me)")
            break
    write(spp, "\n".join(lines).rstrip("\n") + "\n\nNote saved atomically.\n", "Sam Patel: S8a atomic save add bullet + note", atomic=True)
    REC["S8a_atomic"] = {"eid": sp_e["id"], "page_text": ["atomic save by me", "Note saved atomically"], "watch_text": ["atomic save"]}
    pr = entity_by_name("Priyanka Rao")
    prp = page_of(pr["id"])
    t = prp.read_text()
    lines = t.split("\n")
    i, m = first_fact(t)
    lines[i] = "- " + m["text"].rstrip(".") + " (retyped by me, no marker)."
    write(prp, "\n".join(lines), "Priyanka: S8b retype a fact line without its marker")
    REC["S8b_retype"] = {"eid": pr["id"], "claims": [int(m["id"])], "page_text": ["retyped by me", f"c:{m['id']} -->"], "watch_text": ["retyped"]}

    # ---------------- S9 rename via title (Theo) and delete a whole page (Jonas Albers)
    th = entity_by_name("Theo Lindqvist")
    thp = page_of(th["id"])
    write(thp, thp.read_text().replace("# Theo Lindqvist", "# Theo L.", 1), "Theo: S9a rename title")
    REC["S9a_rename"] = {"eid": th["id"], "page_text": ["# Theo"]}
    jo = entity_by_name("Jonas Albers")
    jop = page_of(jo["id"])
    d.note("edit", page=str(jop), what="Jonas: S9b delete page file")
    jop.unlink()
    REC["S9b_delete_page"] = {"eid": jo["id"], "page_text": ["# Jonas"], "files": [str(jop)]}

    # ---------------- S6 two edits on one page in quick succession, JSON alias spacing (Ife)
    ife = entity_by_name("Ife Okafor")
    ip = page_of(ife["id"])
    t = ip.read_text()
    lines = t.split("\n")
    i, m = first_fact(t)
    del lines[i]
    write(ip, "\n".join(lines), "Ife: S6 delete first fact line")
    time.sleep(0.3)
    t = ip.read_text()
    t2 = re.sub(r"^aliases: \[(.*?)\]$", lambda mm: f'aliases: [{mm.group(1)}{"," if mm.group(1).strip() else ""}"Ifey"]', t, count=1, flags=re.M)
    write(ip, t2, "Ife: S6 alias Ifey (JSON spacing) 0.3 s later")
    REC["S6_quick"] = {"eid": ife["id"], "claims": [int(m["id"])], "page_text": ["aliases:"], "files": [str(d.vault / ".nugget/conflicts" / ip.relative_to(d.vault))]}

    settle("edits")
    stage("after_edit")

    # ---------------- S4b: a new item mentioning the aliases
    note = WORK / "Notes/Meetings/2026-10-28-coffee-with-Bobby.md"
    note.write_text("---\ndate: 2026-10-28\npeople:\n  - Bobby\n  - Gustavo\n  - Ifey\n---\n# Coffee with Bobby\n\nBobby and Gustavo talked storage pricing; Ifey joined later.\n")
    q.set_item_dates(WORK / "Notes/Meetings")
    settle("alias-item")
    ents = d.call("knowledge.entities", {"limit": 500})
    CHECKS["S4_alias_resolution"] = {
        "entities_named": [(e["id"], e["kind"], e["name"]) for e in ents if e["name"] in ("Bobby", "Gustavo", "Ifey")],
        "rchen_mentions": [m.get("surface") for m in d.call("knowledge.entity", {"id": rc["id"]})["mentions"]],
        "gus_mentions": [m.get("surface") for m in d.call("knowledge.entity", {"id": gus["id"]})["mentions"]],
        "ife_mentions": [m.get("surface") for m in d.call("knowledge.entity", {"id": ife["id"]})["mentions"]],
    }

    # ---------------- re-extraction: re-save the sources of the edited facts
    for rel in ["Notes/Projects/Harbor.md", "Notes/Meetings/2026-06-03-1on1-with-Bob.md", "Notes/People/Dana-Whitfield.md", "Work/2026-09-18-promotion-case-note.md", "Notes/People/Ife.md"]:
        f = WORK / rel
        f.write_text(f.read_text() + "\n\n(re-saved)\n")
    for f in list((WORK / "Mail").rglob("*.eml")) + list((WORK / "Calendar").rglob("*.ics")):
        os.utime(f)
    d.note("sources.touched")
    settle("reextract")
    stage("after_reextract")

    # ---------------- S10 a newer source supersedes the user's edited fact (Dana works at)
    eml = WORK / "Mail/Inbox/2026-11-05-dana-kestrel.eml"
    eml.write_text(
        "From: Dana Whitfield <dana.whitfield@kestrel.example>\nTo: Mara Okafor <mara.okafor@mailbox.example>\n"
        "Subject: Moving again\nDate: Thu, 05 Nov 2026 10:00:00 +0000\nMessage-ID: <kestrel-1@kestrel.example>\n"
        "MIME-Version: 1.0\nContent-Type: text/plain; charset=utf-8\n\nMara,\n\nShort stint at Corvid: from today I'm at Kestrel.\n\nDana\nHead of Product, Kestrel\n"
    )
    settle("s10")
    stage("after_newer_source")

    d.stop()
    d.start()
    settle("restart")
    stage("after_restart")

    # ---------------- S7 edit while the daemon is stopped (Sam Okonkwo / "Sam")
    sam = entity_by_name("Sam") or entity_by_name("Sam Okonkwo")
    samp = page_of(sam["id"])
    d.stop()
    t = samp.read_text()
    lines = t.split("\n")
    i, m = first_fact(t)
    lines[i] = f"- Sam Okonkwo climbs with me on Thursdays (fixed offline){m['foot']} <!-- c:{m['id']} -->"
    t2 = "\n".join(lines).rstrip("\n") + "\n\nOffline note: keep.\n"
    t2 = re.sub(r"^aliases: \[(.*?)\]$", lambda mm: f'aliases: [{mm.group(1)}{"," if mm.group(1).strip() else ""}"Sammy O"]', t2, count=1, flags=re.M)
    write(samp, t2, "Sam: S7 offline fix + note + alias")
    REC["S7_offline"] = {"eid": sam["id"], "claims": [int(m["id"])], "page_text": ["fixed offline", "Offline note"], "watch_text": ["fixed offline"]}
    d.start()
    settle("offline-start")
    stage("after_offline_start")
    d.stop()


try:
    main()
finally:
    d.stop()
