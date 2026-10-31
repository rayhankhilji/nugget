#!/usr/bin/env python3
"""Per follow-up turn: classified as follow-up (a rewrite request reached the fake model in the
`model` run), the heuristic rewrite (from the `heuristic` run), and retrieval hits."""
import json, re
sent = set()
for l in open("out/fake_model.jsonl"):
    r = json.loads(l)
    if r["kind"] == "rewrite":
        m = re.search(r"Follow-up:\n<<<\n(.*?)\n>>>", r["body"]["messages"][-1]["content"], re.S)
        sent.add(m.group(1))
heur = {(r["conv"], r["turn"]): r for r in map(json.loads, open("out/heuristic.jsonl"))}
model = {(r["conv"], r["turn"]): r for r in map(json.loads, open("out/model.jsonl"))}
print("| turn | follow-up text | looks_like_followup | heuristic query (cloud-only config) | hit heur | hit model(good rewrite) | hit original text |")
print("|---|---|---|---|---|---|---|")
for k, h in heur.items():
    if k[1] == 1:
        continue
    m = model[k]
    f = lambda v: {True: "yes", False: "NO", None: "n/a"}[v]
    print(f"| {k[0]}.{k[1]} | {h['q']} | {'yes' if h['q'] in sent else 'NO'} | {h.get('query_rewrite') or '(unchanged)'} | {f(h['hit'])} | {f(m['hit'])} | {f(h.get('orig_hit'))} |")
