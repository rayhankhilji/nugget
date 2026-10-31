#!/usr/bin/env python3
"""Rough quality of model-only claims (predicates no deterministic extractor emits) in a
snapshot: how many, how many are has_title, how many has_title objects are not job titles
(dates, times, names, numbers, more than 5 words), and how many quotes do not contain the object.

usage: llm_claim_quality.py RUN_DIR LABEL"""
import json, re, sys
from pathlib import Path

LLM_ONLY = {"has_title", "due_on", "committed_to", "prefers", "dislikes", "owes", "lives_in", "located_in",
            "sibling_of", "married_to", "friend_of", "client_of", "held_at", "manages", "studied_at",
            "founded", "born_on"}
TITLE_WORDS = re.compile(r"engineer|lead|manager|head|director|vp|partner|nurse|officer|counsellor|designer|analyst|chief|principal|staff|president|gp|doctor", re.I)
run, label = Path(sys.argv[1]), sys.argv[2]
d = json.loads((run / label / "entities.json").read_text())
seen, n, ht, ht_bad, obj_missing, nonperson_ht = set(), 0, 0, 0, 0, 0
bad_examples = []
for e in d["details"].values():
    for c in e.get("claims", []):
        if c["id"] in seen or c["status"] != "active" or c["predicate"] not in LLM_ONLY:
            continue
        seen.add(c["id"])
        n += 1
        o = c["object"]
        obj = o["entity"]["name"] if o.get("entity") else str(o.get("literal"))
        q = " ".join(ev.get("quote") or "" for ev in c.get("evidence", []))
        if obj.lower() not in q.lower():
            obj_missing += 1
        if c["predicate"] == "has_title":
            ht += 1
            subj_kind = d["details"].get(c["subject"]["id"], {}).get("kind")
            if subj_kind != "person":
                nonperson_ht += 1
            if subj_kind != "person" or not TITLE_WORDS.search(obj) or len(obj.split()) > 5:
                ht_bad += 1
                bad_examples.append(f"{c['subject']['name']} | {obj}")
print(f"{run.name} {label}: model-only active claims {n}; has_title {ht} (non-title or non-person subject {ht_bad}, subject not a person {nonperson_ht}); object not in quote {obj_missing}")
for x in bad_examples[:12]:
    print("   ", x)
