"""A cloud provider (Anthropic kind, at its real default base URL, answered by the sink's mock).
Ask and chat over a corpus with sensitive items. Promise: the question and non-sensitive passages
only; sensitive items withheld and counted; the API key only to api.anthropic.com; every request
in the ledger with feature `ask`/`chat` and the items it carried."""
import json
import shutil
import time

NAME = "s02_cloud_ask"
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
    corpus = f"{lab.home}/Documents"
    shutil.copytree(f"{lab.fmt('{fixtures}')}/corpus", corpus)
    lab.call("sources.add", {"kind": "files", "name": "Documents", "settings": {"roots": [corpus]}})
    lab.wait_idle(seconds=5, timeout=120)
    t0 = time.time()
    lab.mark("ask")
    c = lab.client()
    started, text, done = c.stream("retrieve.ask", {"query": "When is the Acme renewal due and how will it be paid?",
                                                    "budget_tokens": 4000}, timeout=120)
    lab.check("ask.answered", done and not done.get("error"), f"{started.get('provider')} {text[:80]} {done}")
    lab.check("ask.excluded_sensitive>0", started.get("excluded_sensitive", 0) > 0, started.get("excluded_sensitive"))
    reqs = [r for r in lab.requests(since=t0) if r.get("host") == "api.anthropic.com"]
    body = "".join(r.get("body", "") for r in reqs)
    lab.check("ask.no_sensitive_canary", "GG8CANARY-SENS" not in body,
              [x for x in ("SENS-1", "SENS-2", "SENS-3", "SENS-4") if x in body])
    for secret in ("4111 1111 1111 1111", "512-34-6789", "AKIAIOSFODNN7GG8TEST", "GB82 WEST"):
        lab.check(f"ask.no_secret[{secret}]", secret not in body)
    time.sleep(2)
    rows = lab.ledger()
    lab.check("ledger.ask_row", any(r["feature"] == "ask" and r["host"] == "api.anthropic.com" for r in rows),
              json.dumps(rows)[:400])
