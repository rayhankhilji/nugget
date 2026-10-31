"""Area B: Today cards with a cloud provider. [proactive] judge = "cloud": card evidence may go to
the cloud judge, never sensitive evidence, ledger feature `proactive_judge`. judge = "local" and
"off": nothing to the cloud."""
import json
import os
import sys
import time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import b_common as B  # noqa: E402

NAME = "s27_cloud_proactive_judge"
EXTRA_MOCKS = ["mock_schema"]
PROV = """
[[providers]]
id = "anthropic"
kind = "anthropic"
model = "claude-mock"
api_key = "env:ANTHROPIC_API_KEY"
tiers = ["light", "synthesis", "agentic"]
"""


def cfg(judge):
    return f"""
[knowledge.me]
name = "Sam Taylor"
emails = ["sam@example.com"]

[proactive]
judge = "{judge}"
refresh_minutes = 1
debounce_secs = 2

[proactive.engine]
user_addresses = ["sam@example.com"]
""" + PROV


CONFIG = cfg("off")
ENV = {"ANTHROPIC_API_KEY": "sk-ant-GG8CANARY-APIKEY-anthropic"}


def phase(lab, judge, corpus_added):
    lab.stop_daemon()
    open(os.path.join(lab.home, "config", "config.toml"), "w").write(cfg(judge))
    lab.start_daemon()
    lab.call("providers.probe", {"provider": "anthropic"}, timeout=120)
    t = time.time()
    if not corpus_added:
        B.add_corpus(lab, "mail", "Mail")
        B.add_corpus(lab, "corpus", "Documents")
    lab.call("proactive.refresh", timeout=300)
    time.sleep(10)
    cards = lab.call("cards.list", {"include_closed": False})
    reqs = B.cloud_reqs(lab, t)
    return t, cards, reqs


def run(lab):
    out = {}
    first = True
    for judge in ("off", "local", "cloud"):
        t, cards, reqs = phase(lab, judge, not first)
        first = False
        cl = cards.get("cards") if isinstance(cards, dict) else cards
        titles = [c.get("title") for c in (cl or [])]
        out[judge] = titles
        lab.note(f"judge={judge}: {len(cl or [])} cards {titles}; {len(reqs)} cloud requests")
        B.save_requests(lab, f"judge-{judge}", reqs)
        if judge in ("off", "local"):
            lab.check(f"judge_{judge}.nothing_sent", not reqs,
                      [r.get("body", "")[:300] for r in reqs], severity="High")
        else:
            lab.check("judge_cloud.sent", len(reqs) > 0, f"{len(reqs)} requests; cards={titles}")
            B.no_secrets(lab, "judge_cloud", reqs)
            b = B.body(reqs)
            lab.check("judge_cloud.no_sensitive_mail", "MAILSENS" not in b and "4111" not in b, "", severity="High")
            rows, _ = B.ledger_covers(lab, "judge_cloud", t, "proactive_judge")
    lab.check("cards.generated", any(out.values()), json.dumps(out)[:500])
