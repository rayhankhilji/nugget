"""Area B: Ask, follow-up Ask with history, and Chat with items, to a cloud Anthropic provider.
Checks: sensitive withheld; rewrite of the follow-up never goes to cloud (exactly one cloud request
per Ask); ledger rows `ask`/`chat` with accurate items; key only to api.anthropic.com; what local
metadata (file paths, user name) the passages carry."""
import os
import sys
import time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import b_common as B  # noqa: E402

NAME = "s20_cloud_ask_chat"
CONFIG = """
[[providers]]
id = "anthropic"
kind = "anthropic"
model = "claude-mock"
api_key = "env:ANTHROPIC_API_KEY"
tiers = ["light", "synthesis", "agentic"]
"""
ENV = {"ANTHROPIC_API_KEY": "sk-ant-GG8CANARY-APIKEY-anthropic"}


def run(lab):
    _, corpus = B.add_corpus(lab)
    # 1. Ask
    t0 = time.time()
    c = lab.client()
    q1 = "When is the Acme renewal due and how will it be paid?"
    st, text, done = c.stream("retrieve.ask", {"query": q1, "budget_tokens": 4000}, timeout=120)
    lab.check("ask.answered", done and not done.get("error"), f"{st.get('provider')} {st.get('locality')} {done}")
    lab.check("ask.excluded_sensitive", st.get("excluded_sensitive", 0) >= 4, st.get("excluded_sensitive"))
    reqs = B.cloud_reqs(lab, t0)
    lab.check("ask.one_request", len(reqs) == 1, len(reqs))
    B.no_secrets(lab, "ask", reqs)
    b = B.body(reqs)
    lab.check("ask.no_local_paths", lab.home not in b and "file://" not in b,
              [ln for ln in b.split("\\n") if "file://" in ln][:3], severity="Low")
    B.ledger_covers(lab, "ask", t0, "ask", corpus)
    B.save_requests(lab, "ask", reqs)
    # 2. Follow-up with history: rewrite must use a local model or heuristic, never cloud
    t1 = time.time()
    hist = [{"role": "user", "text": q1}, {"role": "assistant", "text": text}]
    st2, text2, done2 = c.stream("retrieve.ask", {"query": "And who is the contact there?",
                                                  "budget_tokens": 4000, "history": hist}, timeout=120)
    reqs2 = B.cloud_reqs(lab, t1)
    lab.check("followup.answered", done2 and not done2.get("error"), f"rewrite={st2.get('query_rewrite')} {done2}")
    lab.check("followup.rewrite_not_cloud", len(reqs2) == 1,
              f"{len(reqs2)} cloud requests: {[r['body'][:200] for r in reqs2]}")
    B.no_secrets(lab, "followup", reqs2)
    B.ledger_covers(lab, "followup", t1, "ask", corpus)
    B.save_requests(lab, "followup", reqs2)
    # 3. Chat with items (the app sends item refs for the ledger)
    t2 = time.time()
    msgs = [{"role": "user", "text": "Summarise: Acme renewal due 14 November, contact Priya. GG8CANARY-CHAT-1"}]
    items = [{"source": "files-1", "key": "0:acme-renewal.md"}]
    st3, text3, done3 = c.stream("chat", {"provider": None, "tier": "synthesis", "messages": msgs,
                                          "items": items}, timeout=120)
    reqs3 = B.cloud_reqs(lab, t2)
    lab.check("chat.answered", done3 and not done3.get("error"), f"{st3} {done3}")
    rows, _ = B.ledger_covers(lab, "chat", t2, "chat")
    lab.check("chat.items_in_ledger", any(any(i["key"] == "0:acme-renewal.md" for i in r.get("items", []))
                                          for r in rows if r["feature"] == "chat"), rows)
    B.save_requests(lab, "chat", reqs3)
    # 4. Headers / metadata
    hdrs = {}
    for r in lab.requests():
        for k, v in r.get("headers", []):
            hdrs.setdefault(k.lower(), set()).add(v[:80])
    lab.note("headers seen: " + str({k: sorted(v) for k, v in hdrs.items() if k not in ("content-length",)}))
    B.keys_only_to_own_host(lab, "keys", {"GG8CANARY-APIKEY-anthropic": "api.anthropic.com"})
    c.close()
