"""A model server on the local network (openai_compatible at http://<lan>:8080/v1, the sink's
mock) serving triage, synthesis and embedding. Promise (privacy.md): local-network requests stay off
the internet but ARE recorded in the ledger, with their feature (triage, embed, knowledge, ask)."""
import json
import shutil
import time

NAME = "s11_lan_provider"
CONFIG = """
[[providers]]
id = "workstation"
kind = "openai_compatible"
base_url = "http://{lan_ip}:8080/v1"
model = "lan-model"
tiers = ["triage", "light", "synthesis", "embedding"]
"""


def run(lab):
    corpus = f"{lab.home}/Documents"
    shutil.copytree(f"{lab.fmt('{fixtures}')}/corpus", corpus)
    lab.call("sources.add", {"kind": "files", "name": "Documents", "settings": {"roots": [corpus]}})
    lab.wait_idle(seconds=10, timeout=300)
    c = lab.client()
    started, text, done = c.stream("retrieve.ask", {"query": "When is the Acme renewal due?",
                                                    "budget_tokens": 3000}, timeout=120)
    lab.check("ask.answered", done and not done.get("error"), f"{started.get('provider')} {started.get('locality')} {done}")
    time.sleep(40)  # knowledge debounce and run
    lab.wait_idle(seconds=10, timeout=300)
    reqs = [r for r in lab.requests() if "method" in r]
    rows = lab.ledger()
    paths = {}
    for r in reqs:
        paths[r["path"]] = paths.get(r["path"], 0) + 1
    feats = {}
    for row in rows:
        feats[row["feature"]] = feats.get(row["feature"], 0) + 1
    lab.check("lan.requests_seen", bool(reqs), paths)
    lab.check("ledger.rows_for_lan", bool(rows), feats)
    lab.check("ledger.row_count_matches_requests", len(rows) == len(reqs),
              f"{len(reqs)} requests vs {len(rows)} ledger rows; paths={paths} features={feats}")
    lab.check("ledger.has_ask", "ask" in feats, feats)
    lab.check("ledger.has_embed", any(p.endswith("/embeddings") for p in paths) <= ("embed" in feats), feats)
    lab.check("ledger.has_triage", any(
        "triage" in (r.get("body") or "").lower() for r in reqs) <= ("triage" in feats), feats)
    with open(f"{lab.out}/ledger-features.json", "w") as f:
        json.dump({"paths": paths, "features": feats}, f, indent=1)
