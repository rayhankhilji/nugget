#!/usr/bin/env python3
"""Grade a GG5 harness run (nugget_qa.py --out DIR) against qa/GG5/corpus/gold.json.

Checks, per snapshot (phase1, phase2, edits, reextract, restart):
- people and organisations: each gold entity maps to exactly one graph entity (split = more than
  one; a graph entity matching two gold entities that must not merge = wrong merge);
- facts: gold current facts present and current; superseded facts not current; forbidden facts
  (`must_not_be_current`) not current;
- citations: every active claim has evidence, and its quote occurs verbatim in the source file;
- sensitive strings: where they appear (vault pages, entity names, claim texts, graph);
- after the user's edits (log.jsonl): the edit's effect, and that it survives re-extraction and a
  restart.

Prints a readable summary and writes JSON. Standard library only.
"""

from __future__ import annotations

import argparse
import json
import re
import unicodedata
from pathlib import Path

CURRENT = {"active", "user_asserted"}
LABELS = ["phase1", "phase2", "edits", "reextract", "restart", "restart-later"]


def norm(s: str) -> str:
    s = unicodedata.normalize("NFKC", s or "").lower()
    return re.sub(r"\s+", " ", re.sub(r"[^\w@.\- ]", " ", s)).strip()


def ws(s: str) -> str:
    return re.sub(r"\s+", " ", s or "").strip()


class Snap:
    def __init__(self, d: Path):
        self.dir = d
        e = json.loads((d / "entities.json").read_text())
        self.list = e["list"]
        self.details = {k: v for k, v in e["details"].items() if "error" not in v}
        self.graph = json.loads((d / "graph.json").read_text()) if (d / "graph.json").exists() else {}
        self.status = json.loads((d / "status.json").read_text())
        self.vault = d / "vault"

    def claims(self):
        seen = set()
        for ent in self.details.values():
            for c in ent.get("claims", []):
                if c["id"] not in seen:
                    seen.add(c["id"])
                    yield c

    def pages(self):
        if not self.vault.exists():
            return []
        return [p for p in self.vault.rglob("*.md") if ".nugget" not in p.parts]


def match_entity(gold_e: dict, ent: dict) -> bool:
    names = {norm(gold_e["name"])} | {norm(a) for a in gold_e.get("aliases", [])}
    emails = {e.lower() for e in gold_e.get("emails", [])}
    if norm(ent["name"]) in names:
        return True
    if any(norm(a) == norm(gold_e["name"]) for a in ent.get("aliases", [])):
        return True
    for scheme, val in ent.get("identifiers", []):
        if scheme == "email" and val.lower() in emails:
            return True
    return False


def resolve(snap: Snap, gold: dict):
    """gold key -> list of graph entity ids it maps to.

    An entity carrying a gold person's mail address maps to that person only (a calendar
    attendee shown as "Sam" with Sam Okonkwo's address is Sam Okonkwo). Otherwise names and
    aliases match, except aliases two gold entities share (a bare "Sam")."""
    from collections import Counter

    alias_count = Counter(norm(a) for g in gold["entities"] for a in {g["name"], *g.get("aliases", [])})
    out = {g["key"]: [] for g in gold["entities"]}
    for eid, ent in snap.details.items():
        emails = {v.lower() for sch, v in ent.get("identifiers", []) if sch == "email"}
        by_mail = [g for g in gold["entities"] if emails & {e.lower() for e in g.get("emails", [])}]
        if by_mail:
            for g in by_mail:
                out[g["key"]].append(eid)
            continue
        for g in gold["entities"]:
            names = {norm(x) for x in {g["name"], *g.get("aliases", [])} if alias_count[norm(x)] == 1}
            ent_names = {norm(ent["name"])} | {norm(a) for a in ent.get("aliases", [])}
            if norm(ent["name"]) in names or norm(g["name"]) in ent_names:
                out[g["key"]].append(eid)
    return out


def obj_matches(claim: dict, obj, keymap: dict, gold_by_key: dict) -> bool:
    o = claim["object"]
    if isinstance(obj, str) and obj in gold_by_key:
        if o.get("entity"):
            return o["entity"]["id"] in keymap.get(obj, []) or norm(o["entity"].get("name", "")) in (
                {norm(gold_by_key[obj]["name"])} | {norm(a) for a in gold_by_key[obj].get("aliases", [])})
        lit = o.get("literal")
        return lit is not None and norm(gold_by_key[obj]["name"]) in norm(json.dumps(lit))
    lit = o.get("literal")
    want = norm(str(obj))
    if lit is not None:
        return want in norm(json.dumps(lit, ensure_ascii=False)) or want in norm(claim.get("text", ""))
    if o.get("entity"):
        return want in norm(o["entity"].get("name", ""))
    return False


def find_claims(snap: Snap, fact: dict, keymap: dict, gold_by_key: dict):
    subj_ids = set(keymap.get(fact["subject"], []))
    preds = fact.get("predicate_any") or [fact.get("predicate", "*")]
    hits = []
    for c in snap.claims():
        if c["subject"]["id"] not in subj_ids:
            continue
        if preds != ["*"] and c["predicate"] not in preds:
            continue
        objs = fact.get("object_any") or ([fact["object"]] if fact.get("object") is not None else [])
        if objs and not any(obj_matches(c, o, keymap, gold_by_key) for o in objs):
            continue
        hits.append(c)
    return hits


def source_text(corpus: Path, item_key: str) -> str | None:
    rel = item_key.split(":", 1)[1] if ":" in item_key else item_key
    for phase in ("phase1", "phase2"):
        p = corpus / phase / rel
        if p.exists():
            return p.read_text(errors="replace")
    return None


def grade_snap(snap: Snap, gold: dict, corpus: Path) -> dict:
    gold_by_key = {g["key"]: g for g in gold["entities"]}
    keymap = resolve(snap, gold)
    r: dict = {"label": snap.dir.name, "knowledge": snap.status.get("knowledge")}
    phase_n = {"phase1": 1}.get(snap.dir.name, 2)

    # Entities.
    ents = []
    for g in gold["entities"]:
        if g.get("phase", 1) > phase_n:
            continue
        ids = keymap[g["key"]]
        kinds = sorted({snap.details[i]["kind"] for i in ids})
        ents.append({"key": g["key"], "ids": ids, "names": [snap.details[i]["name"] for i in ids], "kinds": kinds,
                     "ok": len(ids) == 1 and (not g.get("kind") or kinds == [g["kind"]])})
    r["entities"] = ents
    r["entity_ok"] = sum(e["ok"] for e in ents)
    r["entity_total"] = len(ents)
    wrong_merges = []
    for a, b in gold.get("must_not_merge", []):
        both = set(keymap.get(a, [])) & set(keymap.get(b, []))
        if both:
            wrong_merges.append({"pair": [a, b], "ids": sorted(both)})
    r["wrong_merges"] = wrong_merges
    splits = []
    for m in gold.get("must_merge", []):
        ids = set(keymap.get(m["entity"], []))
        # Any surface form shown as its own entity is a split.
        for eid, ent in snap.details.items():
            if eid in ids:
                continue
            if any(norm(ent["name"]) == norm(s) for s in m.get("surface", [])):
                splits.append({"entity": m["entity"], "extra": {"id": eid, "name": ent["name"], "kind": ent["kind"]}})
    r["splits"] = splits

    # Facts.
    facts = []
    for f in gold.get("facts", []):
        if f.get("current_after_phase", 1) > phase_n:
            continue
        hits = find_claims(snap, f, keymap, gold_by_key)
        cur = [c for c in hits if c["status"] in CURRENT]
        facts.append({"id": f["id"], "desc": f"{f['subject']} {f.get('predicate') or f.get('predicate_any')} {f.get('object')}",
                      "current": bool(cur), "found": bool(hits),
                      "claims": [{"id": c["id"], "status": c["status"], "text": c["text"]} for c in hits]})
    r["facts"] = facts
    r["fact_recall"] = (sum(f["current"] for f in facts), len(facts))
    stale = []
    for s in gold.get("superseded", []):
        if phase_n < 2:
            continue
        for c in find_claims(snap, s, keymap, gold_by_key):
            if c["status"] in CURRENT:
                stale.append({"id": s["id"], "claim": c["id"], "text": c["text"], "status": c["status"]})
    r["stale_current"] = stale
    forbidden = []
    for s in gold.get("must_not_be_current", []):
        if s.get("after_phase", 1) > phase_n:
            continue
        for c in find_claims(snap, s, keymap, gold_by_key):
            if c["status"] in CURRENT:
                forbidden.append({"desc": s.get("desc"), "claim": c["id"], "text": c["text"], "status": c["status"]})
    r["forbidden_current"] = forbidden

    # Citations.
    uncited, bad_quote, total_active = [], [], 0
    for c in snap.claims():
        if c["status"] != "active":
            continue
        total_active += 1
        if not c.get("evidence"):
            uncited.append({"claim": c["id"], "text": c["text"]})
            continue
        ok = False
        for ev in c["evidence"]:
            q = ev.get("quote")
            src = source_text(corpus, ev["item"]["key"])
            if q and src and ws(q).lower() in ws(src).lower():
                ok = True
        if not ok:
            bad_quote.append({"claim": c["id"], "text": c["text"], "evidence": c["evidence"][:2]})
    r["active_claims"] = total_active
    r["uncited"] = uncited
    r["quote_not_in_source"] = bad_quote

    # Sensitive strings.
    leaks = []
    sens = gold.get("sensitive_strings", [])
    for p in snap.pages():
        t = p.read_text(errors="replace")
        for s in sens:
            if s.lower() in t.lower():
                leaks.append({"where": "vault", "page": str(p.relative_to(snap.vault)), "string": s})
    for eid, ent in snap.details.items():
        for s in sens:
            if s.lower() in ent["name"].lower():
                leaks.append({"where": "entity_name", "id": eid, "string": s})
        for c in ent.get("claims", []):
            for s in sens:
                if s.lower() in (c.get("text") or "").lower() or s.lower() in json.dumps(c["object"]).lower():
                    leaks.append({"where": "claim", "entity": ent["name"], "claim": c["id"], "status": c["status"], "string": s})
    for n in snap.graph.get("overview", {}).get("nodes", []):
        for s in sens:
            if s.lower() in n["name"].lower():
                leaks.append({"where": "graph_node", "id": n["id"], "string": s})
    r["sensitive_hits"] = leaks

    # Vault health.
    v = snap.status.get("knowledge", {}).get("vault", {})
    r["vault"] = {"pages": v.get("pages"), "conflicts": v.get("conflicts"), "invalid": v.get("invalid"), "files": len(snap.pages())}
    # Every graph entity has a page and vice versa.
    page_ids = set()
    for p in snap.pages():
        m = re.search(r'^nugget_id: "([^"]+)"', p.read_text(errors="replace"), re.M)
        if m:
            page_ids.add(m.group(1))
    r["entities_without_page"] = sorted(set(snap.details) - page_ids)
    return r


def grade_edits(run: Path, snaps: dict) -> list:
    """Follow every edit the harness logged through the later snapshots."""
    log = [json.loads(l) for l in (run / "log.jsonl").read_text().splitlines() if l.strip()]
    edits = [e for e in log if e["step"] == "edit"]
    out = []
    for e in edits:
        rec = {"what": e["what"], "page": e["page"], "after": {}}
        removed = [l[1:] for l in e["diff"].splitlines() if l.startswith("-") and not l.startswith("---")]
        added = [l[1:] for l in e["diff"].splitlines() if l.startswith("+") and not l.startswith("+++")]
        ids = [int(m) for l in removed for m in re.findall(r"<!-- c:(\d+) -->", l)]
        for label in ("edits", "reextract", "restart", "restart-later"):
            s = snaps.get(label)
            if not s:
                continue
            st = {}
            claims = {c["id"]: c for c in s.claims()}
            for cid in ids:
                st[f"claim {cid}"] = claims.get(cid, {}).get("status", "missing")
            page = s.vault / e["page"]
            text = page.read_text(errors="replace") if page.exists() else None
            if text is None:
                # The page may have been renamed; find by name stem.
                stem = Path(e["page"]).name.split(" (")[0]
                cands = list(s.vault.glob(f"*/{stem} (*).md"))
                text = cands[0].read_text(errors="replace") if cands else ""
            st["added_lines_still_on_page"] = [a for a in added if a.strip() and ws(re.sub(r"<!--.*?-->|\[\^\d+\]", "", a)) in ws(re.sub(r"<!--.*?-->|\[\^\d+\]", "", text))]
            st["removed_lines_back_on_page"] = [r_ for r_ in removed if r_.strip() and ws(re.sub(r"<!--.*?-->|\[\^\d+\]", "", r_)) in ws(re.sub(r"<!--.*?-->|\[\^\d+\]", "", text))]
            user = [c for c in claims.values() if c["status"] == "user_asserted"]
            st["user_asserted"] = [{"id": c["id"], "subject": c["subject"]["name"], "predicate": c["predicate"], "text": c["text"]} for c in user]
            if "alias" in e["what"]:
                st["robert_aliases"] = sorted({a for d in s.details.values() if d["name"] == "Robert Chen" for a in d["aliases"]})
            rec["after"][label] = st
        out.append(rec)
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--gold", required=True)
    ap.add_argument("--run", required=True)
    ap.add_argument("--corpus", default=str(Path(__file__).resolve().parent.parent / "corpus"))
    ap.add_argument("--out")
    a = ap.parse_args()
    gold = json.loads(Path(a.gold).read_text())
    run = Path(a.run)
    snaps = {l: Snap(run / l) for l in LABELS if (run / l / "entities.json").exists()}
    res = {"snapshots": {l: grade_snap(s, gold, Path(a.corpus)) for l, s in snaps.items()}}
    if (run / "log.jsonl").exists():
        res["edits"] = grade_edits(run, snaps)
    if a.out:
        Path(a.out).write_text(json.dumps(res, indent=1, ensure_ascii=False))
    for l, r in res["snapshots"].items():
        k = r["knowledge"] or {}
        print(f"== {l}: {k.get('entities')} entities, {k.get('claims')} claims, extractor {k.get('extractor_provider')}, vault {r['vault']}")
        print(f"   entities resolved 1:1 {r['entity_ok']}/{r['entity_total']}; wrong merges {len(r['wrong_merges'])}; split surfaces {len(r['splits'])}")
        for e in r["entities"]:
            if not e["ok"]:
                print(f"     - {e['key']}: {list(zip(e['ids'], e['names'], [None]*len(e['ids'])))} kinds {e['kinds']}")
        for w in r["wrong_merges"]:
            print(f"     ! wrong merge {w}")
        for s in r["splits"]:
            print(f"     ! split {s}")
        fr = r["fact_recall"]
        print(f"   gold facts current {fr[0]}/{fr[1]}; stale shown as current {len(r['stale_current'])}; forbidden current {len(r['forbidden_current'])}")
        for f in r["facts"]:
            if not f["current"]:
                print(f"     - missing {f['id']} {f['desc']} found={[ (c['status'], c['text']) for c in f['claims']]}")
        for s in r["stale_current"] + r["forbidden_current"]:
            print(f"     ! {s}")
        print(f"   active claims {r['active_claims']}; uncited {len(r['uncited'])}; quote not in source {len(r['quote_not_in_source'])}")
        print(f"   sensitive strings found {len(r['sensitive_hits'])}: {sorted({(h['where'], h['string']) for h in r['sensitive_hits']})}")
        if r["entities_without_page"]:
            print(f"   entities without a vault page: {len(r['entities_without_page'])}")
    for e in res.get("edits", []):
        print(f"== edit {e['what']} ({e['page']})")
        for l, st in e["after"].items():
            print(f"   {l}: " + json.dumps({k: v for k, v in st.items() if k != 'user_asserted'}, ensure_ascii=False)[:600])


if __name__ == "__main__":
    main()
