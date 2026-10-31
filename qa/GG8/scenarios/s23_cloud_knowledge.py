"""Area B: knowledge extraction with a cloud synthesis provider. Without [knowledge.llm]
cloud_sources nothing may be sent; with cloud_sources = [the folder], only non-sensitive items,
ledger feature `knowledge` with the right items."""
import os
import sys
import time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import b_common as B  # noqa: E402

NAME = "s23_cloud_knowledge"
EXTRA_MOCKS = ["mock_schema"]
BASE = """
[knowledge]
debounce_ms = 500
interval_minutes = 1

[[providers]]
id = "anthropic"
kind = "anthropic"
model = "claude-mock"
api_key = "env:ANTHROPIC_API_KEY"
tiers = ["light", "synthesis", "agentic"]
"""
CONFIG = BASE
ENV = {"ANTHROPIC_API_KEY": "sk-ant-GG8CANARY-APIKEY-anthropic"}


def run(lab):
    _, corpus = B.add_corpus(lab)
    t0 = time.time()
    time.sleep(45)
    ks = lab.call("knowledge.status")
    reqs = B.cloud_reqs(lab, t0)
    lab.check("no_cloud_sources.nothing_sent", not reqs,
              f"status={ks} requests={[(r['path'], r.get('body','')[:200]) for r in reqs]}", severity="High")
    lab.stop_daemon()
    cfgp = os.path.join(lab.home, "config", "config.toml")
    cfg = BASE.replace("interval_minutes = 1", "interval_minutes = 1\n\n[knowledge.llm]\ncloud_sources = [\"files-1\", \"files-2\"]")
    open(cfgp, "w").write(cfg)
    lab.start_daemon()
    try:
        lab.note(f"probe: {lab.call('providers.probe', {'provider': 'anthropic'}, timeout=120)}")
    except Exception as e:  # noqa: BLE001
        lab.note(f"probe failed: {e}")
    # re-index from scratch so every item is pending for extraction again
    lab.call("sources.remove", {"id": "files-1", "purge": True})
    t1 = time.time()
    lab.call("sources.add", {"kind": "files", "name": "Documents", "settings": {"roots": [corpus]}})
    end = time.time() + 120
    while time.time() < end:
        time.sleep(5)
        if B.cloud_reqs(lab, t1) and time.time() - max(r["t"] for r in B.cloud_reqs(lab, t1)) > 15:
            break
    ks = lab.call("knowledge.status")
    reqs = B.cloud_reqs(lab, t1)
    lab.check("cloud_sources.sent", len(reqs) > 0, f"{len(reqs)} requests; status={ks}")
    B.no_secrets(lab, "knowledge", reqs)
    B.ledger_covers(lab, "knowledge", t1, "knowledge", corpus)
    B.save_requests(lab, "knowledge", reqs)
    lab.check("knowledge.no_local_paths", not any("file://" in r.get("body", "") or lab.home in r.get("body", "")
                                                  for r in reqs), "", severity="Low")
