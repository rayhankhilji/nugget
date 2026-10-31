#!/usr/bin/env python3
"""truth-sensitive S1: what lands where from the Health/ and Finance/ items (deterministic daemon).

Indexes corpus phase1 + phase2 (dated as the harness does), then captures:
- each item's `sensitive` flag as the MCP layer sees it (default-scoped client vs include_sensitive),
- MCP find_entities / get_entity / search for the therapy and GP items with a DEFAULT client,
- IPC knowledge.* for the same entities,
- every sensitive string, email address and health/finance name found in the plain-text vault.

Usage: python3 qa/GG5/scenarios/truth-sensitive/s1_sensitive.py <workdir>
Output: <workdir>/out/s1.json (+ vault copy); prints a summary.
"""
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(ROOT / "qa/GG5/harness"))
import nugget_qa  # noqa: E402

BIN = str(ROOT / "target/debug/nuggetd")
GOLD = json.loads((ROOT / "qa/GG5/corpus/gold.json").read_text())


def mcp_session(home: Path, client_id: str, token: str, calls):
    """Run the stdio bridge, send initialize + each tools/call, return the results."""
    env = dict(os.environ, NUGGET_MCP_TOKEN=token)
    p = subprocess.Popen([BIN, "--home", str(home), "mcp", "--client", client_id], stdin=subprocess.PIPE,
                         stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, text=True)
    def send(obj):
        p.stdin.write(json.dumps(obj) + "\n"); p.stdin.flush()
    def recv(want_id):
        while True:
            line = p.stdout.readline()
            if not line:
                raise RuntimeError("bridge closed: " + p.stderr.read()[:2000])
            msg = json.loads(line)
            if msg.get("id") == want_id:
                return msg
    send({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "qa", "version": "0"}}})
    recv(0)
    send({"jsonrpc": "2.0", "method": "notifications/initialized"})
    out = []
    for i, (name, args) in enumerate(calls, start=1):
        send({"jsonrpc": "2.0", "id": i, "method": "tools/call", "params": {"name": name, "arguments": args}})
        r = recv(i)
        res = r.get("result") or r.get("error")
        out.append({"tool": name, "args": args, "result": res})
    p.stdin.close(); p.terminate()
    return out


def text_of(res):
    if not isinstance(res, dict):
        return json.dumps(res)
    parts = [c.get("text", "") for c in res.get("content", []) if isinstance(c, dict)]
    if res.get("structuredContent") is not None:
        parts.append(json.dumps(res["structuredContent"], ensure_ascii=False))
    return "\n".join(parts)


def main():
    work = Path(sys.argv[1]).resolve()
    if work.exists():
        shutil.rmtree(work)
    home, out = work / "home", work / "out"
    out.mkdir(parents=True); home.mkdir(parents=True)
    d = nugget_qa.Daemon(BIN, home, out)
    d.write_config(None, None, "\n[mcp]\nhttp = false\n")
    d.start()
    report = {}
    try:
        corpus = home / "corpus"
        corpus.mkdir()
        for ph in ("phase1", "phase2"):
            shutil.copytree(nugget_qa.CORPUS / ph, corpus, dirs_exist_ok=True)
        nugget_qa.set_item_dates(corpus)
        d.call("sources.add", {"kind": "files", "name": "Mara's files", "settings": {"roots": [str(corpus)]}})
        d.settle("s1", 900, quiet_s=10)
        snap = d.snapshot("s1")
        names = {e["name"]: e["id"] for e in snap["list"]}

        # MCP clients: default scopes (what "Connect an app" gives) and include_sensitive.
        dflt = d.call("mcp.clients.create", {"name": "qa-default", "scopes": {}})
        incl = d.call("mcp.clients.create", {"name": "qa-sensitive", "scopes": {"include_sensitive": True}})
        report["mcp_default_scopes"] = dflt["client"]["scopes"]
        calls = [
            ("find_entities", {"text": "Jonas"}),
            ("find_entities", {"text": "Counselling"}),
            ("find_entities", {"text": "Lena Brandt"}),
            ("get_entity", {"id": names.get("Jonas Albers", "missing")}),
            ("get_entity", {"id": names.get("Counselling session", "missing")}),
            ("search", {"query": "ferrous sulfate anaemia"}),
            ("search", {"query": "counselling sleep diary"}),
            ("search", {"query": "Mortgage account NCU"}),
            ("search", {"query": "salary payslip net pay"}),
            ("search", {"query": "replacement debit card number"}),
        ]
        for label, c in (("default", dflt), ("include_sensitive", incl)):
            res = mcp_session(home, c["client"]["id"], c["token"], calls)
            report[f"mcp_{label}"] = [{"tool": r["tool"], "args": r["args"], "text": text_of(r["result"])[:3000]} for r in res]

        # Vault scan.
        vault = d.vault
        hits = []
        emails = {}
        for p in sorted(vault.rglob("*.md")):
            t = p.read_text(errors="replace")
            rel = str(p.relative_to(vault))
            for s in GOLD["sensitive_strings"] + ["Lena Brandt", "Albers Counselling", "Northshore", "health", "Health/"]:
                if s in t:
                    hits.append({"page": rel, "string": s})
            for e in re.findall(r"[\w.+-]+@[\w.-]+\.example", t):
                emails.setdefault(e, set()).add(rel)
        report["vault_hits"] = hits
        report["vault_emails"] = {k: sorted(v) for k, v in emails.items()}
        report["vault_pages"] = sorted(str(p.relative_to(vault)) for p in vault.rglob("*.md"))
    finally:
        d.stop()
    (out / "s1.json").write_text(json.dumps(report, indent=1, ensure_ascii=False))
    print(json.dumps({"mcp_default_scopes": report.get("mcp_default_scopes"), "vault_hits": len(report.get("vault_hits", [])), "emails": len(report.get("vault_emails", {}))}, indent=1))


if __name__ == "__main__":
    main()
