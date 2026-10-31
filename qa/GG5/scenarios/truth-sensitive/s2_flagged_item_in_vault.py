#!/usr/bin/env python3
"""truth-sensitive S2: an item Nugget itself marks sensitive still feeds the vault and graph.

Input: only the counselling invite, with an IBAN added to its DESCRIPTION (a counsellor's invoice
line), so the deterministic PII scan marks the item `sensitive: true` without any model.
Captures: MCP default client (sensitive hidden) vs IPC knowledge.entity / knowledge.graph and the
plain-text vault.

Usage: python3 qa/GG5/scenarios/truth-sensitive/s2_flagged_item_in_vault.py <workdir>
"""
import json
import shutil
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]
sys.path.insert(0, str(ROOT / "qa/GG5/harness"))
sys.path.insert(0, str(HERE))
import nugget_qa  # noqa: E402
from s1_sensitive import BIN, mcp_session, text_of  # noqa: E402

ICS = (ROOT / "qa/GG5/corpus/phase1/Health/2026-06-17-counselling.ics").read_text()
ICS = ICS.replace(
    "DESCRIPTION:Weekly session with Jonas Albers, counsellor. Bring the sleep diary.",
    "DESCRIPTION:Weekly session with Jonas Albers, counsellor. Bring the sleep diary. Invoice: pay to IBAN GB82 WEST 1234 5698 7654 32.",
)


def main():
    work = Path(sys.argv[1]).resolve()
    if work.exists():
        shutil.rmtree(work)
    home, out = work / "home", work / "out"
    out.mkdir(parents=True); home.mkdir(parents=True)
    d = nugget_qa.Daemon(BIN, home, out)
    d.write_config(None, None, "\n[mcp]\nhttp = false\n")
    d.start()
    rep = {}
    try:
        corpus = home / "corpus" / "Health"
        corpus.mkdir(parents=True)
        (corpus / "2026-06-17-counselling.ics").write_text(ICS)
        nugget_qa.set_item_dates(home / "corpus")
        d.call("sources.add", {"kind": "files", "name": "Mara's files", "settings": {"roots": [str(home / "corpus")]}})
        d.settle("s2", 600, quiet_s=8)
        snap = d.snapshot("s2")
        ids = {e["name"]: e["id"] for e in snap["list"]}
        rep["ipc_entities"] = [(e["name"], e["kind"], e["claims"]) for e in snap["list"]]
        rep["ipc_jonas_claims"] = [c["text"] for c in snap["details"][ids["Jonas Albers"]]["claims"]]
        rep["ipc_graph_overview_nodes"] = [n.get("name") for n in d.call("knowledge.graph", {"center": None, "depth": 1, "limit": 200, "kinds": []})["nodes"]]
        c = d.call("mcp.clients.create", {"name": "qa-default", "scopes": {}})
        res = mcp_session(home, c["client"]["id"], c["token"], [
            ("search", {"query": "counselling sleep diary"}),
            ("find_entities", {"text": "Jonas"}),
            ("find_entities", {"text": ""}),
        ])
        rep["mcp_default"] = [{"tool": r["tool"], "args": r["args"], "text": text_of(r["result"])[:1500]} for r in res]
        rep["vault_pages"] = sorted(str(p.relative_to(d.vault)) for p in d.vault.glob("*/*.md"))
        rep["vault_jonas"] = nugget_qa.find_page(d.vault, "Jonas Albers").read_text()
    finally:
        d.stop()
    (out / "s2.json").write_text(json.dumps(rep, indent=1, ensure_ascii=False))
    print(json.dumps(rep, indent=1, ensure_ascii=False)[:6000])


if __name__ == "__main__":
    main()
