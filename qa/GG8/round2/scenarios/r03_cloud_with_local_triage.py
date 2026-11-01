"""Round 2: real local models (qwen3-1.7b-q8 chat/triage, nomic embedding, managed llama-server)
plus a cloud provider (Anthropic kind at api.anthropic.com, answered by the sink), knowledge
cloud_sources and the cloud Today judge. Corpus: notes (4 with card/SSN/IBAN/keys) and WhatsApp +
Telegram imports (private by default). Promise: before triage sorts an item it counts as sensitive;
confirmed-sensitive and private-chat items never reach the cloud (Ask, quick ask + handover,
knowledge, judge); every request is ledgered; read-ahead stays local."""
import json
import time

import os
import sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import r2common as C  # noqa: E402

NAME = "r03_cloud_with_local_triage"
AUTOSTART = False
CONFIG = C.RUNTIME + """
[knowledge.llm]
cloud_sources = ["files-1", "whatsapp-1", "telegram-1"]

[proactive]
judge = "cloud"

[[providers]]
id = "anthropic"
kind = "anthropic"
model = "claude-mock"
api_key = "env:ANTHROPIC_API_KEY"
tiers = ["light", "synthesis", "agentic"]
"""
ENV = {"ANTHROPIC_API_KEY": "sk-ant-GG8CANARY-APIKEY-r03"}
SINK_ENV = {"GG8_EXTRA_MOCKS": "mock_schema"}
EXTRA_MOCKS = ["mock_schema"]
NORMAL = ["acme-renewal", "lisbon-trip", "weekly-notes"]
FORBIDDEN = ("GG8CANARY-SENS", "GG8CANARY-WA", "GG8CANARY-TG", "4111 1111 1111 1111", "512-34-6789",
             "GB82 WEST", "AKIAIOSFODNN7GG8TEST")


def leaks(text):
    return [f for f in FORBIDDEN if f in text]


def run(lab):
    t_start = time.time()
    C.seed(lab)
    rs = C.wait_runtime(lab)
    lab.check("runtime.ready", all(r["state"] == "ready" for r in rs if r["role"] in ("chat", "embedding")), rs)
    lab.wait_idle(seconds=5, timeout=180)
    flags0 = C.sensitivity(lab)
    lab.note("sensitivity before triage " + json.dumps(flags0))
    # 1) Before triage has sorted anything: a cloud Ask must not carry unsorted items.
    c = lab.client()
    t0 = time.time()
    try:
        st, text, done = c.stream("retrieve.ask", {"query": "When is the Acme renewal due?", "budget_tokens": 3000,
                                                   "provider": "anthropic"}, timeout=180)
        sent = "".join(r.get("body", "") for r in C.bodies(lab, t0))
        unsorted = [t for t, s in flags0.items() if s]
        lab.check("pre_triage.ask_sends_no_unsorted", not C.canaries(sent) or all(
            f not in sent for f in FORBIDDEN), {"excluded": st.get("excluded_sensitive"), "canaries": C.canaries(sent),
                                                 "unsorted_titles": unsorted})
    except Exception as e:  # noqa: BLE001
        lab.note(f"pre-triage ask: {e}")
    ok, flags = C.wait_triage(lab, NORMAL, timeout=900)
    lab.check("triage.sorted_normal_items", ok, flags)
    lab.note("sensitivity after triage " + json.dumps(flags))
    # 2) Cloud Ask after triage.
    t1 = time.time()
    st, text, done = c.stream("retrieve.ask", {"query": "Acme renewal: when is it due, how is it paid, and when is the dinner?",
                                               "budget_tokens": 4000, "provider": "anthropic"}, timeout=180)
    sent = "".join(r.get("body", "") for r in C.bodies(lab, t1))
    lab.check("ask.cloud.no_sensitive_or_private", not leaks(sent), {"leaks": leaks(sent), "canaries": C.canaries(sent),
                                                                      "excluded": st.get("excluded_sensitive")})
    lab.check("ask.cloud.sends_sorted_normal_items", any(f"GG8CANARY-NORMAL" in x for x in C.canaries(sent)), C.canaries(sent))
    # 3) Quick ask (provider null: local first) then the Chat handover follow-up to the cloud.
    t2 = time.time()
    st, local_answer, done = c.stream("retrieve.ask", {"query": "How will the Acme renewal be paid? Include the card details.",
                                                       "budget_tokens": 3000, "provider": None, "tier": "synthesis",
                                                       "utc_offset_minutes": 60}, timeout=600)
    lab.note(f"quick ask: provider={st.get('provider')} locality={st.get('locality')} answer={local_answer[:300]!r}")
    lab.check("quick_ask.local_first.nothing_sent", not C.bodies(lab, t2), len(C.bodies(lab, t2)))
    t3 = time.time()
    st, text, done = c.stream("retrieve.ask", {"query": "And the wire details?", "budget_tokens": 3000, "provider": "anthropic",
                                               "history": [{"role": "user", "text": "How will the Acme renewal be paid? Include the card details."},
                                                           {"role": "assistant", "text": local_answer}]}, timeout=180)
    sent = "".join(r.get("body", "") for r in C.bodies(lab, t3))
    lab.check("handover.followup.no_sensitive", not leaks(sent),
              {"leaks": leaks(sent), "local_answer_had": leaks(local_answer)})
    # 4) Read-ahead stays local.
    t4 = time.time()
    ctx = {"app": {"id": "org.mozilla.thunderbird", "name": "Thunderbird"},
           "field": {"role": "text_area", "label": "Message body", "value": "Hi Dana,"},
           "thread": [{"author": "Dana Kim", "text": "Can you send the Acme renewal deck?", "from_me": False}],
           "recipients": ["dana@example.test"], "subject": "Deck"}
    ra = lab.call("draft.read_ahead", {"context": ctx}, timeout=60)
    time.sleep(20)
    lab.check("read_ahead.nothing_sent", not C.bodies(lab, t4), {"result": ra, "requests": len(C.bodies(lab, t4))})
    # 5) Knowledge extraction (cloud_sources) and the cloud judge.
    t5 = time.time()
    lab.call("proactive.refresh", timeout=300)
    time.sleep(90)
    lab.wait_idle(seconds=15, timeout=600)
    later = [r for r in lab.requests(since=t5) if r.get("host") == "api.anthropic.com" and "method" in r]
    allbody = "".join(r.get("body", "") for r in later)
    lab.check("background.cloud.no_sensitive_or_private", not leaks(allbody),
              {"requests": len(later), "leaks": leaks(allbody), "canaries": C.canaries(allbody)})
    # 6) Ledger completeness.
    rows = lab.ledger()
    reqs = [r for r in lab.requests() if "method" in r]
    by_host = {}
    for r in rows:
        by_host.setdefault(r["host"], []).append(r["feature"])
    lab.check("ledger.one_row_per_request", len([r for r in rows if r["host"] == "api.anthropic.com"]) == len(
        [r for r in reqs if r.get("host") == "api.anthropic.com"]),
        {"requests": len(reqs), "rows": {h: sorted(set(f)) + [len(f)] for h, f in by_host.items()}})
    empty_items = [(r["feature"], r["bytes_sent"]) for r in rows if not r["items"] and r["feature"] not in ("probe",)]
    lab.check("ledger.items_listed", not empty_items, empty_items[:10])
    C.save(lab, "ledger-rows.json", rows)
