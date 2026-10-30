#!/usr/bin/env python3
"""Verifier: minimal independent checks for truth-sensitive findings 1, 4 and 5.

Corpus (written here, nothing from the GG5 corpus):
- Mail/2026-03-04-cc-only.eml   Robert -> Mara, Cc Dana (Dana never writes to Mara anywhere)
- Notes/2026-06-10-vendor-call.md  untyped note, people: [Quinn Vendor], project: Harbor
- Health/2026-05-12-gp.md   a GP note with a diagnosis, no card/SSN/IBAN/key
Captures every claim, the vault, and a default MCP client's view of the GP note.
Usage: python3 qa/GG5/verify/truth-sensitive/v1_minimal.py <workdir>
"""
import json, re, shutil, sys
from pathlib import Path
ROOT = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(ROOT / "qa/GG5/harness"))
sys.path.insert(0, str(ROOT / "qa/GG5/scenarios/truth-sensitive"))
import nugget_qa
from s1_sensitive import BIN, mcp_session, text_of

FILES = {
 "Mail/2026-03-04-cc-only.eml": "From: Robert Chen <robert.chen@brightwell.example>\r\nTo: Mara Okafor <mara.okafor@brightwell.example>\r\nCc: Dana Whitfield <dana.whitfield@brightwell.example>\r\nSubject: Harbor kickoff\r\nDate: Wed, 04 Mar 2026 09:12:00 +0000\r\nMessage-ID: <cc-only@qa.example>\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nMara, you are leading Harbor.\r\n",
 "Notes/2026-06-10-vendor-call.md": "---\ndate: 2026-06-10\npeople:\n  - Quinn Vendor\nproject: Harbor\n---\n# Sales call\n\nQuinn pitched their monitoring product. Not buying.\n",
 "Health/2026-05-12-gp.md": "---\ndate: 2026-05-12\n---\n# GP visit\n\nDr Brandt: iron-deficiency anaemia. Prescription: ferrous sulfate 200 mg.\n",
}

def main():
    work = Path(sys.argv[1]).resolve()
    if work.exists(): shutil.rmtree(work)
    home, out = work / "home", work / "out"
    out.mkdir(parents=True); home.mkdir(parents=True)
    d = nugget_qa.Daemon(BIN, home, out)
    d.write_config(None, None, "\n[mcp]\nhttp = false\n")
    d.start(); rep = {}
    try:
        corpus = home / "corpus"
        for rel, t in FILES.items():
            f = corpus / rel; f.parent.mkdir(parents=True, exist_ok=True); f.write_text(t)
        nugget_qa.set_item_dates(corpus)
        d.call("sources.add", {"kind": "files", "name": "f", "settings": {"roots": [str(corpus)]}})
        d.settle("v1", 600, quiet_s=8)
        snap = d.snapshot("v1")
        seen, claims = set(), []
        for e in snap["details"].values():
            for c in e.get("claims", []):
                if c["id"] not in seen:
                    seen.add(c["id"]); claims.append((c["status"], c["text"], [ev.get("quote") for ev in c["evidence"]]))
        rep["claims"] = claims
        c = d.call("mcp.clients.create", {"name": "dflt", "scopes": {}})
        rep["mcp_default"] = [text_of(r["result"])[:600] for r in mcp_session(home, c["client"]["id"], c["token"], [("search", {"query": "ferrous sulfate anaemia"})])]
        rep["vault_pages"] = sorted(str(p.relative_to(d.vault)) for p in d.vault.glob("*/*.md"))
    finally:
        d.stop()
    (out / "v1.json").write_text(json.dumps(rep, indent=1, ensure_ascii=False))
    print(json.dumps(rep, indent=1, ensure_ascii=False))

if __name__ == "__main__":
    main()
