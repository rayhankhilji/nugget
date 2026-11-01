"""Round 2: GG8-2 and GG8-6 once items are triaged. Real local chat model (qwen3-1.7b-q8) does
triage; there is NO local embedder, so embeddings go to an opted-in cloud embedding provider
(api.cloudllm.test); the Today judge runs on the cloud (api.anthropic.com). Corpus: notes (4 with
card/SSN/IBAN/keys) and mail (.eml, one with a card, one with an API key). Promise: sensitive and
unsorted items are never embedded or judged in the cloud; ledger rows list their items."""
import json
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import r2common as C  # noqa: E402

NAME = "r04_cloud_embed_judge_triaged"
AUTOSTART = False
CONFIG = """
[runtime]
chat_model = "qwen3-1.7b-q8"

[ingest]
allow_cloud_embeddings = true
reembed_interval_ms = 20000

[retrieve]
allow_cloud_embedding = true

[knowledge.me]
name = "Sam Taylor"
emails = ["sam@example.com"]

[proactive]
judge = "cloud"
refresh_minutes = 1
debounce_secs = 2

[proactive.engine]
user_addresses = ["sam@example.com"]

[[providers]]
id = "cloudembed"
kind = "openai_compatible"
base_url = "https://api.cloudllm.test/v1"
model = "embed-mock"
api_key = "env:CLOUDLLM_API_KEY"
tiers = ["embedding"]

[[providers]]
id = "anthropic"
kind = "anthropic"
model = "claude-mock"
api_key = "env:ANTHROPIC_API_KEY"
tiers = ["synthesis", "agentic"]
"""
ENV = {"ANTHROPIC_API_KEY": "sk-ant-GG8CANARY-APIKEY-r04", "CLOUDLLM_API_KEY": "GG8CANARY-APIKEY-cloudllm"}
EXTRA_MOCKS = ["mock_schema"]
FORBIDDEN = ("GG8CANARY-SENS", "MAILSENS", "4111 1111 1111 1111", "4111111111111111", "512-34-6789",
             "GB82 WEST", "AKIAIOSFODNN7GG8TEST", "sk-ant-api03-GG8")


def leaks(text):
    return [f for f in FORBIDDEN if f in text]


def run(lab):
    os.makedirs(lab.data, exist_ok=True)
    subprocess.run(["cp", "-al", os.path.join(C.MODELS_HOME, "data", "models"), os.path.join(lab.data, "models")], check=True)
    slots = os.path.join(lab.data, "models", "slots.json")
    s = json.load(open(slots))
    s["models"].pop("nomic-embed-text-v1.5-q8", None)
    tmp = slots + ".tmp"
    json.dump(s, open(tmp, "w"), indent=2)
    os.replace(tmp, slots)  # break the hard link to the shared copy
    lab.start_daemon()
    import shutil
    shutil.copytree(f"{lab.fmt('{fixtures}')}/corpus", f"{lab.home}/Documents")
    shutil.copytree(f"{lab.fmt('{fixtures}')}/mail", f"{lab.home}/Mail")
    lab.call("sources.add", {"kind": "files", "name": "Documents", "settings": {"roots": [f"{lab.home}/Documents"]}})
    lab.call("sources.add", {"kind": "files", "name": "Mail", "settings": {"roots": [f"{lab.home}/Mail"]}})
    rs = C.wait_runtime(lab)
    lab.note("runtime " + json.dumps(rs))
    ok, flags = C.wait_triage(lab, ["acme-renewal", "lisbon-trip"], timeout=900)
    lab.note("sensitivity " + json.dumps(flags))
    lab.check("triage.sorted", ok, flags)
    time.sleep(45)  # a re-embed pass for newly sorted items
    t = time.time()
    for q in ("Acme renewal payment card", "wire instructions IBAN", "integration credentials", "Lisbon trip"):
        lab.call("search", {"query": q, "limit": 10})
        lab.call("retrieve.context", {"query": q, "budget_tokens": 2000})
    time.sleep(5)
    emb = [r for r in lab.requests() if r.get("host") == "api.cloudllm.test" and r.get("path", "").endswith("/embeddings")]
    allb = "".join(r.get("body", "") for r in emb)
    lab.check("embed.cloud_used", bool(emb), len(emb))
    lab.check("embed.no_sensitive_ever", not leaks(allb), {"leaks": leaks(allb), "requests": len(emb)})
    after = "".join(r.get("body", "") for r in emb if r["t"] >= t)
    lab.check("embed.search_time.no_sensitive", not leaks(after), {"leaks": leaks(after)})
    lab.note("embed canaries " + json.dumps(C.canaries(allb)))
    t2 = time.time()
    lab.call("proactive.refresh", timeout=300)
    time.sleep(20)
    cards = lab.call("cards.list", {"include_closed": False})
    judge = [r for r in lab.requests(since=t2) if r.get("host") == "api.anthropic.com" and "method" in r]
    jb = "".join(r.get("body", "") for r in judge)
    lab.note(f"cards {[c.get('title') for c in cards.get('cards', [])]}; judge requests {len(judge)}")
    lab.check("judge.no_sensitive", not leaks(jb), leaks(jb))
    rows = lab.ledger()
    C.save(lab, "ledger-rows.json", rows)
    for feat in ("embed", "proactive_judge"):
        fr = [r for r in rows if r["feature"] == feat]
        lab.check(f"ledger.{feat}.items_listed", fr and all(r["items"] for r in fr if r["bytes_sent"] > 200),
                  [(r["bytes_sent"], len(r["items"])) for r in fr][:12])
    reqs = [r for r in lab.requests() if "method" in r]
    lab.check("ledger.row_per_request", len(rows) >= len(reqs), {"requests": len(reqs), "rows": len(rows)})
