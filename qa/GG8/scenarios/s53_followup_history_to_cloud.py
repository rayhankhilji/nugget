"""Ask follow-ups with `history` and a cloud provider. A first answer made by a LOCAL model can
quote a sensitive item (here: the card number, as the real local model did in run 41117729320,
s62). The app then sends that answer back as history with the follow-up. Promise (privacy.md):
sensitive items never go to a cloud model. Is the history checked before it goes to the cloud?"""
import shutil
import time

NAME = "s53_followup_history_to_cloud"
CONFIG = """
[[providers]]
id = "anthropic"
kind = "anthropic"
model = "claude-mock"
api_key = "env:ANTHROPIC_API_KEY"
tiers = ["light", "synthesis", "agentic"]
"""
ENV = {"ANTHROPIC_API_KEY": "sk-ant-GG8CANARY-APIKEY-followup"}
# Verbatim the assistant turn the real local model (qwen3-1.7b-q8) wrote in run 41117729320.
LOCAL_ANSWER = ("The Acme renewal will be paid by card. The card number is 4111 1111 1111 1111, "
                "expiry 09/29, and the billing contact is Priya Raman. The reference is GG8CANARY-SENS-1. [1]")


def run(lab):
    corpus = f"{lab.home}/Documents"
    shutil.copytree(f"{lab.fmt('{fixtures}')}/corpus", corpus)
    lab.call("sources.add", {"kind": "files", "name": "Documents", "settings": {"roots": [corpus]}})
    lab.wait_idle(seconds=5, timeout=120)
    c = lab.client()
    t0 = time.time()
    started, text, done = c.stream("retrieve.ask", {
        "query": "And the wire details?", "budget_tokens": 3000, "provider": "anthropic",
        "history": [{"role": "user", "text": "How will the Acme renewal be paid?"},
                    {"role": "assistant", "text": LOCAL_ANSWER}]}, timeout=120)
    lab.check("followup.answered", done and not done.get("error"), f"{started.get('provider')} excluded={started.get('excluded_sensitive')}")
    body = "".join(r.get("body", "") for r in lab.requests(since=t0, host="api.anthropic.com"))
    leaked = [s for s in ("4111 1111 1111 1111", "GG8CANARY-SENS-1") if s in body]
    lab.check("followup.history_to_cloud_has_no_sensitive", not leaked, leaked)
    rows = [r for r in lab.ledger() if r["feature"] == "ask"]
    lab.check("followup.ledger_lists_source_item", any(
        i["key"].endswith("acme-payment.md") for r in rows for i in r["items"]) or not leaked,
        [[i["key"] for i in r["items"]] for r in rows])
