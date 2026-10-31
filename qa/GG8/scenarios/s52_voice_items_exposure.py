"""Double Tap's sent text (draft.feedback final_text, kept for the voice profile) becomes an item.
Does it then flow to MCP clients with default scopes and into a cloud Ask's context? Is the source
visible to the user in sources.list?"""
import json
import shutil
import time

NAME = "s52_voice_items_exposure"
CONFIG = """
[[providers]]
id = "anthropic"
kind = "anthropic"
model = "claude-mock"
api_key = "env:ANTHROPIC_API_KEY"
tiers = ["light", "synthesis", "agentic"]
"""
ENV = {"ANTHROPIC_API_KEY": "sk-ant-GG8CANARY-APIKEY-voice"}


def mcp_http(lab, token, body):
    return lab.ns_run(["curl", "-s", "-m", "20", "-X", "POST", "http://127.0.0.1:7319/mcp",
                       "-H", f"Authorization: Bearer {token}", "-H", "Content-Type: application/json",
                       "-H", "Accept: application/json, text/event-stream", "-d", json.dumps(body)],
                      capture_output=True, text=True).stdout


def run(lab):
    corpus = f"{lab.home}/Documents"
    shutil.copytree(f"{lab.fmt('{fixtures}')}/corpus", corpus)
    lab.call("sources.add", {"kind": "files", "name": "Documents", "settings": {"roots": [corpus]}})
    lab.wait_idle(seconds=5, timeout=120)
    c = lab.client()
    ctx = {"app": {"id": "org.mozilla.thunderbird", "name": "Thunderbird"},
           "field": {"role": "text_area", "label": "Message body", "value": ""},
           "thread": [{"author": "Dr. Okafor", "text": "Your biopsy results are back, please call the clinic.", "from_me": False}],
           "recipients": ["clinic@example.test"], "subject": "Results"}
    started, text, done = c.stream("draft.begin", {"context": ctx}, timeout=120)
    c.call("draft.feedback", {"stream_id": started["stream_id"], "outcome": "edited", "insert_verified": True,
                              "final_text": "Thanks doctor, I will call about the biopsy and the Acme renewal leave. GG8CANARY-VOICE-final"})
    time.sleep(15)
    srcs = lab.call("sources.list")
    lab.note("sources: " + json.dumps([(s["id"], s["kind"], s["items"]) for s in srcs]))
    lab.check("voice.source_visible_in_sources_list", any("voice" in s["id"] for s in srcs),
              [s["id"] for s in srcs])
    tok = lab.call("mcp.clients.create", {"name": "default-scopes", "scopes": {}})["token"]
    out = mcp_http(lab, tok, {"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
        "name": "search", "arguments": {"query": "biopsy Acme renewal leave"}}})
    lab.check("mcp.default_scope_hides_sent_drafts", "GG8CANARY-VOICE" not in out,
              out[out.find("nugget-voice") - 200: out.find("nugget-voice") + 400] if "nugget-voice" in out else "")
    t0 = time.time()
    started, text, done = c.stream("retrieve.ask", {"query": "What did I say about the biopsy?", "budget_tokens": 3000}, timeout=120)
    body = "".join(r.get("body", "") for r in lab.requests(since=t0, host="api.anthropic.com"))
    lab.check("cloud_ask.no_sent_drafts", "GG8CANARY-VOICE" not in body,
              [ci["item"] for ci in started.get("citations", [])])
    rows = [r for r in lab.ledger() if r["feature"] == "ask"]
    lab.note("ask ledger: " + json.dumps(rows))
