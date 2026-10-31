#!/usr/bin/env python3
"""Independent minimal repro for GG5 vault-edits findings 1-5, on a tiny synthetic corpus
(five typed person notes, nothing else). Usage: python3 mini.py <out-dir>"""
import json, re, shutil, sys, time
from pathlib import Path
ROOT = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(ROOT / "qa/GG5/harness"))
import nugget_qa as q

OUT = Path(sys.argv[1]).resolve()
shutil.rmtree(OUT, ignore_errors=True); OUT.mkdir(parents=True)
HOME = OUT / "home"; WORK = HOME / "corpus"
d = q.Daemon(str(ROOT / "target/debug/nuggetd"), HOME, OUT)
R = {}
PEOPLE = {"Zoe Fixer": "Acme Widgets", "Yuri Retyper": "Beta Systems", "Xena Aliasing": "Gamma Labs",
          "Walt Newer": "Delta Foods", "Vera Stripper": "Epsilon Tools"}

def note(name, org, date="2026-08-01", extra=""):
    p = WORK / "Notes/People" / (name.replace(" ", "-") + ".md")
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(f"---\ntype: person\ndate: {date}\nemail: {name.split()[0].lower()}@mail{name.split()[0].lower()}.example\norg: {org}\n---\n# {name}\n\nWorks at {org}.{extra}\n")
    return p

def settle(l):
    time.sleep(4); d.settle(l, 600, quiet_s=6)

def ent(name):
    best = None
    for e in d.call("knowledge.entities", {"text": name.replace(" ", "-"), "limit": 50}):
        if e["name"] == name.replace(" ", "-") and (best is None or e["claims"] > best["claims"]):
            best = e
    return d.call("knowledge.entity", {"id": best["id"]})

def page(e):
    if not e.get("vault_path"): print("NOPAGE", json.dumps({k: v for k, v in e.items() if k not in ("mentions",)})[:1500]); raise SystemExit(2)
    return d.vault / e["vault_path"]

def snap(label, name, needles):
    e = ent(name); t = page(e).read_text()
    R.setdefault(label, {})[name] = {
        "claims": [(c["id"], c["status"], c["predicate"], c["text"]) for c in e["claims"]],
        "aliases": e["aliases"],
        "page": [l for l in t.split("\n") if any(n in l for n in needles)]}
    R[label]["conflicts"] = d.call("knowledge.status")["vault"]["conflicts"]
    (OUT / "results.json").write_text(json.dumps(R, indent=1, ensure_ascii=False))

def sub_fact(t, contains, fn):
    out = []
    for line in t.split("\n"):
        m = q.FACT_RE.match(line)
        if m and contains in m["text"]:
            line = fn(m)
        out.append(line)
    return "\n".join(out)

def main():
    d.write_config(None, None); d.start()
    for n, o in PEOPLE.items(): note(n, o)
    q.set_item_dates(WORK)
    d.call("sources.add", {"kind": "files", "name": "mini", "settings": {"roots": [str(WORK)]}})
    settle("index")
    for n in PEOPLE: snap("indexed", n, ["works at", "aliases", "#"])
    (OUT / "indexed_pages").mkdir(); 
    for n in PEOPLE: shutil.copy(page(ent(n)), OUT / "indexed_pages" / (n + ".md"))

    # F1: edit Zoe's works_at line, link removed, marker kept
    p = page(ent("Zoe Fixer"))
    p.write_text(sub_fact(p.read_text(), "works at", lambda m: f"- Zoe Fixer is CTO at Acme (fixed by me){m['foot']} <!-- c:{m['id']} -->"))
    # F2: retype Yuri's line, link kept, marker+footnote dropped
    p = page(ent("Yuri Retyper"))
    p.write_text(sub_fact(p.read_text(), "works at", lambda m: "- " + m["text"].rstrip(".") + " (retyped by me)."))
    # F3: spaced alias on Xena
    p = page(ent("Xena Aliasing"))
    p.write_text(re.sub(r"^aliases: \[(.*?)\]$", lambda m: f'aliases: [{m.group(1)}{", " if m.group(1).strip() else ""}"Xena A", "Xena Alias"]', p.read_text(), count=1, flags=re.M))
    # F4: edit Walt's line (link removed)
    p = page(ent("Walt Newer"))
    p.write_text(sub_fact(p.read_text(), "works at", lambda m: f"- Walt Newer is chef at Delta (fixed by me){m['foot']} <!-- c:{m['id']} -->"))
    p.write_text(re.sub(r"^aliases: \[\]$", 'aliases: ["Walt N","Walt Nw"]', p.read_text(), count=1, flags=re.M))  # F3 control: compact
    # F5: strip comments on Vera + reword
    p = page(ent("Vera Stripper")); t = re.sub(r" ?<!--.*?-->", "", p.read_text())
    p.write_text(t.replace("Vera Stripper works at", "Vera Stripper is boss at", 1) + "\nMy note.\n")
    settle("edits")
    for n in PEOPLE: snap("after_edit", n, ["works at", "fixed", "retyped", "aliases", "boss", "# Vera", "## Facts"])
    (OUT / "after_edit_vera.md").write_text(page(ent("Vera Stripper")).read_text())

    # re-extract: re-save every note (unchanged facts)
    for f in (WORK / "Notes/People").glob("*.md"):
        f.write_text(f.read_text() + "\n(re-saved)\n")
    settle("reextract")
    for n in PEOPLE: snap("after_reextract", n, ["works at", "fixed", "retyped", "aliases", "boss", "# Vera", "## Facts"])
    (OUT / "after_reextract_vera.md").write_text(page(ent("Vera Stripper")).read_text())

    # F4: newer source for Walt (new org, newer date)
    note("Walt Newer", "Zeta Motors", date="2026-10-30")
    settle("newer")
    snap("after_newer", "Walt Newer", ["works at", "fixed", "Zeta", "Delta"])
    d.stop(); d.start(); settle("restart")
    for n in PEOPLE: snap("after_restart", n, ["works at", "fixed", "retyped", "aliases", "Zeta"])
    d.stop()


try:
    main()
finally:
    d.stop()
