"""Area B: an OpenAI-compatible cloud provider serving embedding, triage and synthesis tiers.
Default: no embeddings and no triage may go to it. With [ingest] allow_cloud_embeddings and
[retrieve] allow_cloud_embedding: non-sensitive chunks and queries only, ledger feature `embed`;
triage still never."""
import json
import os
import sys
import time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import b_common as B  # noqa: E402

NAME = "s25_cloud_embed_triage"
EXTRA_MOCKS = ["mock_schema"]
PROV = """
[[providers]]
id = "cloudllm"
kind = "openai_compatible"
base_url = "https://api.cloudllm.test/v1"
model = "mock-model"
api_key = "env:CLOUDLLM_API_KEY"
tiers = ["triage", "light", "synthesis", "embedding"]
"""
CONFIG = PROV
ENV = {"CLOUDLLM_API_KEY": "sk-GG8CANARY-APIKEY-cloudllm"}


def kinds(reqs):
    return sorted({r["path"].split("?")[0] for r in reqs})


def triage_like(reqs):
    return [r for r in reqs if r["path"].endswith("/chat/completions")]


def run(lab):
    lab.note(f"probe: {lab.call('providers.probe', {'provider': 'cloudllm'}, timeout=120)}")
    t0 = time.time()
    _, corpus = B.add_corpus(lab)
    lab.call("search", {"query": "Acme renewal", "limit": 5})
    time.sleep(20)
    reqs = B.cloud_reqs(lab, t0)
    lab.check("default.no_embeddings", not [r for r in reqs if "embeddings" in r["path"]], kinds(reqs), severity="High")
    lab.check("default.no_triage_or_other", not triage_like(reqs),
              [r.get("body", "")[:300] for r in triage_like(reqs)], severity="High")
    B.save_requests(lab, "default", reqs)
    # opt in
    lab.stop_daemon()
    open(os.path.join(lab.home, "config", "config.toml"), "w").write(
        "[ingest]\nallow_cloud_embeddings = true\nreembed_interval_ms = 5000\n\n[retrieve]\nallow_cloud_embedding = true\n" + PROV)
    lab.start_daemon()
    t1 = time.time()
    time.sleep(30)
    lab.call("search", {"query": "Acme renewal deposit", "limit": 5})
    time.sleep(5)
    reqs = B.cloud_reqs(lab, t1)
    emb = [r for r in reqs if "embeddings" in r["path"]]
    lab.check("optin.embeddings_sent", len(emb) > 0, kinds(reqs))
    B.no_secrets(lab, "optin.embed", emb)
    lab.check("optin.no_triage", not triage_like(reqs),
              [r.get("body", "")[:300] for r in triage_like(reqs)], severity="High")
    B.ledger_covers(lab, "optin", t1, None, corpus)
    rows = lab.ledger()
    lab.note("ledger features: " + json.dumps(sorted({r['feature'] for r in rows})))
    B.save_requests(lab, "optin", reqs)
    B.keys_only_to_own_host(lab, "keys", {"GG8CANARY-APIKEY-cloudllm": "api.cloudllm.test"})
