"""Secrets and private text vs. logs, the audit log and plaintext files, at NUGGET_LOG=debug.
Session: a cloud Ask, a Double Tap draft (thread, field text and final text carry canaries), a
CalDAV password and an MCP query with canaries. Then the store is opened with its key and every
non-content table is searched. Promise (privacy.md): audit entries never hold secret values, MCP
arguments are summarised, drafts are audited without their content; logs hold no content."""
import json
import os
import shutil
import subprocess
import time

NAME = "s50_logs_audit"
LOG = "debug"
CONFIG = """
[[providers]]
id = "anthropic"
kind = "anthropic"
model = "claude-mock"
api_key = "env:ANTHROPIC_API_KEY"
tiers = ["light", "synthesis", "agentic"]
"""
ENV = {"ANTHROPIC_API_KEY": "sk-ant-GG8CANARY-APIKEY-logs"}
# Tables whose job is to hold indexed content (expected to contain corpus canaries).
CONTENT_TABLES = ("items", "chunks", "item_text", "fts", "vec", "chunk", "kg_", "evidence",
                  "mention", "claim", "entit", "voice")


def mcp_http(lab, token, body):
    return lab.ns_run(["curl", "-s", "-m", "20", "-X", "POST", "http://127.0.0.1:7319/mcp",
                       "-H", f"Authorization: Bearer {token}", "-H", "Content-Type: application/json",
                       "-H", "Accept: application/json, text/event-stream", "-d", json.dumps(body)],
                      capture_output=True, text=True).stdout


def dump_store(lab):
    key = open(os.path.join(lab.data, "store.key")).read().strip()
    db = os.path.join(lab.data, "nugget.db")
    q = f"PRAGMA key = \"x'{key}'\";\n.tables\n"
    tables = subprocess.run(["sqlcipher", db], input=q, capture_output=True, text=True).stdout.split()
    out = {}
    for t in tables:
        r = subprocess.run(["sqlcipher", "-json", db],
                           input=f"PRAGMA key = \"x'{key}'\";\nSELECT * FROM \"{t}\";\n",
                           capture_output=True, text=True)
        out[t] = r.stdout[:3_000_000]
    return out


def run(lab):
    corpus = f"{lab.home}/Documents"
    shutil.copytree(f"{lab.fmt('{fixtures}')}/corpus", corpus)
    lab.call("sources.add", {"kind": "files", "name": "Documents", "settings": {"roots": [corpus]}})
    lab.wait_idle(seconds=5, timeout=120)
    c = lab.client()
    c.stream("retrieve.ask", {"query": "GG8CANARY-QUERY-ask Acme renewal due?", "budget_tokens": 2000}, timeout=120)
    ctx = {"app": {"id": "org.mozilla.thunderbird", "name": "Thunderbird", "window_title": "Re: GG8CANARY-DRAFT-title"},
           "field": {"role": "text_area", "label": "Message body", "value": "Hi Priya, GG8CANARY-DRAFT-field"},
           "thread": [{"author": "Priya Raman", "text": "Can you confirm the Acme renewal date? GG8CANARY-DRAFT-thread",
                       "from_me": False}],
           "recipients": ["priya@acme.example"], "subject": "Acme renewal GG8CANARY-DRAFT-subject"}
    try:
        started, text, done = c.stream("draft.begin", {"context": ctx, "instruction": "GG8CANARY-DRAFT-instruction"}, timeout=120)
        lab.note(f"draft: {started} {text[:80]} {done}")
        c.call("draft.feedback", {"stream_id": started["stream_id"], "outcome": "edited",
                                  "final_text": "Confirmed, 14 November. GG8CANARY-DRAFT-final", "insert_verified": True})
    except Exception as e:  # noqa: BLE001
        lab.note(f"draft failed: {e}")
    src = lab.call("sources.add", {"kind": "caldav", "name": "Cal", "settings": {
        "url": "https://dav.example.test/cal/", "username": "gg8user"}})
    lab.call("sources.secret.set", {"id": src["id"], "secret": "GG8CANARY-PASSWORD-caldav"})
    created = lab.call("mcp.clients.create", {"name": "gg8-mcp", "scopes": {}})
    tok = created["token"]
    mcp_http(lab, tok, {"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
        "name": "search", "arguments": {"query": "GG8CANARY-QUERY-mcp Acme renewal"}}})
    time.sleep(10)
    lab.stop_daemon()
    tables = dump_store(lab)
    with open(os.path.join(lab.out, "store-tables.json"), "w") as f:
        json.dump({t: v[:200000] for t, v in tables.items()}, f)
    hits = {}
    for t, v in tables.items():
        for needle in ("GG8CANARY", tok, "sk-ant-GG8"):
            if needle in v:
                i = 0
                while (i := v.find(needle, i)) >= 0:
                    j = i
                    while j < len(v) and (v[j].isalnum() or v[j] in "-_"):
                        j += 1
                    hits.setdefault(t, set()).add(v[i:j][:60])
                    i = j
    hits = {t: sorted(s) for t, s in hits.items()}
    lab.note("store hits: " + json.dumps(hits))
    noncontent = {t: s for t, s in hits.items() if not any(k in t.lower() for k in CONTENT_TABLES)}
    lab.check("store.noncontent_tables_clean", not noncontent, noncontent)
    logs = ""
    for p in [os.path.join(lab.out, "daemon.stdout")] + [os.path.join(lab.data, "logs", x) for x in os.listdir(os.path.join(lab.data, "logs"))]:
        logs += open(p, errors="replace").read()
    for needle in ("GG8CANARY-DRAFT", "GG8CANARY-QUERY", "GG8CANARY-PASSWORD", "GG8CANARY-APIKEY",
                   "GG8CANARY-NORMAL", "GG8CANARY-SENS", tok):
        lab.check(f"logs.no[{needle[:20]}]", needle not in logs,
                  [ln[:300] for ln in logs.splitlines() if needle in ln][:3])
    modes = []
    for root, dirs, files in os.walk(lab.home):
        if "/Documents" in root:
            continue
        for fn in files + dirs:
            p = os.path.join(root, fn)
            m = os.stat(p).st_mode & 0o777
            if m & 0o077:
                modes.append(f"{oct(m)} {os.path.relpath(p, lab.home)}")
    lab.check("files.private_modes", not modes, modes[:20])
