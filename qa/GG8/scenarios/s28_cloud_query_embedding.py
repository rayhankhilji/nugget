"""Area B: only [retrieve] allow_cloud_embedding = true (ingest keeps embeddings local / off).
Docs: queries, and for MMR "passages stored without a vector", go to the cloud embedder. Does a
search then send sensitive passages (stored without a vector) to the cloud?"""
import os
import sys
import time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import b_common as B  # noqa: E402

NAME = "s28_cloud_query_embedding"
EXTRA_MOCKS = ["mock_schema"]
CONFIG = """
[retrieve]
allow_cloud_embedding = true

[[providers]]
id = "cloudllm"
kind = "openai_compatible"
base_url = "https://api.cloudllm.test/v1"
model = "mock-model"
api_key = "env:CLOUDLLM_API_KEY"
tiers = ["embedding"]
"""
ENV = {"CLOUDLLM_API_KEY": "sk-GG8CANARY-APIKEY-cloudllm"}


def run(lab):
    lab.call("providers.probe", {"provider": "cloudllm"}, timeout=120)
    _, corpus = B.add_corpus(lab)
    time.sleep(10)
    t = time.time()
    hits = lab.call("search", {"query": "Acme renewal deposit", "limit": 10})
    time.sleep(3)
    reqs = [r for r in B.cloud_reqs(lab, t) if "embeddings" in r["path"]]
    lab.note(f"search hits: {[h['title'] for h in hits]}")
    lab.check("search.embeds_sent", len(reqs) > 0, f"{len(reqs)} embedding requests")
    B.no_secrets(lab, "search.embed", reqs)
    B.ledger_covers(lab, "search.embed", t, "embed", corpus)
    B.save_requests(lab, "search-embed", reqs)
    # Ask with a local-only answer path is impossible here (no chat model); retrieve.context
    t = time.time()
    ctx = lab.call("retrieve.context", {"query": "card number for the deposit", "budget_tokens": 2000})
    time.sleep(2)
    reqs = [r for r in B.cloud_reqs(lab, t) if "embeddings" in r["path"]]
    B.no_secrets(lab, "context.embed", reqs)
    B.save_requests(lab, "context-embed", reqs)
