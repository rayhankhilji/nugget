"""A realistic local-only session after onboarding, with the real llama-server, on the real
internet (so a leak would really leave): files, search, Ask, chat, Double Tap read-ahead and
draft, knowledge extraction, Today, the agent, an MCP client over HTTP, then 5 minutes idle.
Promise: not one packet leaves the namespace and the ledger gains no row."""
import json
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import common  # noqa: E402

NAME = "s61_local_session"
MODE = "internet"
CONFIG = common.LOCAL_CONFIG
AUTOSTART = False
IDLE = int(os.environ.get("GG8_IDLE_SECS", "300"))

MCP_CLIENT = r'''
import json, sys, urllib.request
tok = sys.argv[1]
def rpc(i, method, params):
    req = urllib.request.Request("http://127.0.0.1:7319/mcp", method="POST",
        data=json.dumps({"jsonrpc": "2.0", "id": i, "method": method, "params": params}).encode(),
        headers={"Authorization": "Bearer " + tok, "Content-Type": "application/json",
                 "Accept": "application/json, text/event-stream",
                 "MCP-Protocol-Version": "2025-06-18"})
    with urllib.request.urlopen(req, timeout=120) as r:
        return r.read().decode()
print(rpc(1, "initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                            "clientInfo": {"name": "gg8", "version": "1"}}))
print(rpc(2, "tools/call", {"name": "search", "arguments": {"query": "Acme renewal card"}}))
print(rpc(3, "tools/call", {"name": "get_context", "arguments": {"query": "Acme renewal payment"}}))
'''

DRAFT = {"context": {
    "app": {"name": "Thunderbird", "id": "org.mozilla.Thunderbird",
            "window_title": "Re: Acme renewal"},
    "field": {"role": "text_area", "label": "Message body", "value": "Hi Priya,\n\n"},
    "thread": [{"author": "Priya Raman", "from_me": False,
                "text": "Can you confirm the renewal date and the term we agreed?"}],
    "recipients": ["priya.raman@acme-logistics.example"], "subject": "Re: Acme renewal"},
    "instruction": None, "provider": None, "max_tokens": 120}


def run(lab):
    common.adopt_home(lab, "s60_onboarding")
    lab.start_daemon()
    t0 = time.time()
    lab.mark("session start")
    before = len(lab.ledger())
    corpus = common.copy_corpus(lab)
    src = lab.call("sources.add", {"kind": "files", "name": "Documents",
                                   "settings": {"roots": [corpus]}})
    lab.wait_idle(seconds=20, timeout=900)
    st = lab.call("ingest.status")
    lab.note(f"ingest {json.dumps(st)[:600]}")
    hits = lab.call("search", {"query": "When is the Acme renewal due?", "limit": 10})
    lab.check("search", len(hits) > 0, [h["title"] for h in hits])
    started, text, done = common.stream(lab, "retrieve.ask", {
        "query": "When is the Acme renewal due?", "budget_tokens": 1500})
    lab.check("ask.local", done and not done.get("error") and started.get("locality") == "on_device",
              f"{started.get('provider')} {started.get('locality')} {text[:200]!r}")
    lab.check("ask.local.no_withholding", started.get("excluded_sensitive", 0) == 0,
              started.get("excluded_sensitive"))
    started, text, done = common.stream(lab, "chat", {
        "provider": None, "tier": "light", "items": [],
        "messages": [{"role": "user", "text": "Give me a one-line packing list for Lisbon."}]})
    lab.check("chat.local", done and not done.get("error"), f"{started} {text[:120]!r}")
    ra = lab.call("draft.read_ahead", DRAFT, timeout=60)
    lab.check("read_ahead.local.started", isinstance(ra, dict) and ra.get("started"), ra)
    time.sleep(15)
    started, text, done = common.stream(lab, "draft.begin", DRAFT)
    lab.check("draft.local", done and not done.get("error") and started.get("locality") == "on_device",
              f"{started.get('provider')} {text[:200]!r}")
    # Knowledge: wait for the extractor to drain (local LLM extraction is slow on CPU).
    end = time.time() + 900
    ks = None
    while time.time() < end:
        ks = lab.call("knowledge.status")
        if ks.get("pending_items", 1) == 0 and ks.get("last_run"):
            break
        time.sleep(10)
    lab.check("knowledge.ran", ks and ks.get("entities", 0) > 0, json.dumps(ks)[:400])
    try:
        lab.call("proactive.refresh", timeout=600)
        cards = lab.call("cards.list", {"include_closed": False})
        lab.check("today.cards", True, json.dumps(cards)[:300])
    except Exception as e:  # noqa: BLE001
        lab.check("today.cards", False, repr(e))
    try:
        c = lab.client(timeout=900)
        ag = c.call("agent.run", {"goal": "Look up in my notes when the Acme renewal is due and tell me.",
                                  "budgets": {"max_steps": 4, "deadline_secs": 400}})
        tid = ag["task_id"]
        n = c.wait_note(lambda m: m.get("method") == "agent.event"
                        and m["params"].get("task_id") == tid
                        and str(m["params"].get("kind", m["params"].get("type", ""))).lower()
                        in ("finished", "done", "failed", "cancelled", "completed"), timeout=450)
        c.close()
        task = lab.call("agent.get", {"task_id": tid})
        lab.check("agent.ran", True, json.dumps(task)[:500])
    except Exception as e:  # noqa: BLE001
        lab.check("agent.ran", False, repr(e))
    try:
        mc = lab.call("mcp.clients.create", {"name": "gg8-real",
                                             "scopes": {"sources": None, "tools": [],
                                                        "include_sensitive": False}})
        r = lab.ns_run(["python3", "-c", MCP_CLIENT, mc["token"]], capture_output=True,
                       text=True, timeout=300)
        out = r.stdout + r.stderr
        with open(os.path.join(lab.out, "mcp.txt"), "w") as f:
            f.write(out)
        lab.check("mcp.http.works", "Acme" in out, out[-300:])
        lab.check("mcp.no_sensitive", not any(m in out for m in common.SENSITIVE_MARKERS),
                  [m for m in common.SENSITIVE_MARKERS if m in out])
    except Exception as e:  # noqa: BLE001
        lab.check("mcp.http.works", False, repr(e))
    lab.mark("idle")
    time.sleep(IDLE)
    after = lab.ledger()
    lab.check("ledger.no_new_rows", len(after) == before, after[: len(after) - before])
    pk = common.offns_packets_since(lab, t0)
    lab.check("no_packet_left_namespace", len(pk) == 0, pk[:20], severity="Blocker")
    dns = [d.get("qname") for d in lab.dns() if d["t"] >= t0]
    lab.check("no_dns", len(dns) == 0, dns[:20])
    common.compare_with_ledger(lab, since=t0)
