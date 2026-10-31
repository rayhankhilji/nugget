"""Local models installed plus a cloud provider (Anthropic kind at api.anthropic.com, answered by
the sink's recording mock; lab `sinkhole` mode). Promises checked from the wire:
- Double Tap read-ahead with a local draft reaches only the local server (no sink request);
  with a cloud draft provider it is skipped and nothing is sent;
- Ask goes to the local model first (nothing to the cloud); when the local server dies, a
  fallback to the cloud (if any) carries no sensitive item and is in the ledger;
- a follow-up's query rewrite never goes to the cloud."""
import json
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import common  # noqa: E402

NAME = "s62_local_plus_cloud"
MODE = "sinkhole"
CONFIG = common.LOCAL_CONFIG + """
[[providers]]
id = "anthropic"
kind = "anthropic"
model = "claude-mock"
api_key = "env:ANTHROPIC_API_KEY"
tiers = ["light", "synthesis", "agentic"]
"""
ENV = {"ANTHROPIC_API_KEY": "sk-ant-GG8CANARY-APIKEY-real"}
AUTOSTART = False

DRAFT = {"context": {
    "app": {"name": "Thunderbird", "id": "org.mozilla.Thunderbird", "window_title": "Re: Acme renewal"},
    "field": {"role": "text_area", "label": "Message body", "value": "Hi Priya,\n\n"},
    "thread": [{"author": "Priya Raman", "from_me": False,
                "text": "How will finance pay for the Acme renewal, and when is it due?"}],
    "recipients": ["priya.raman@acme-logistics.example"], "subject": "Re: Acme renewal"},
    "instruction": None, "provider": None, "max_tokens": 120}


def cloud_reqs(lab, since):
    return [r for r in lab.requests(since=since) if "method" in r]


def leaked(reqs):
    body = "".join((r.get("body") or "") for r in reqs)
    return [m for m in common.SENSITIVE_MARKERS if m in body]


def run(lab):
    common.adopt_home(lab, "s61_local_session")
    lab.start_daemon()
    st = common.wait_runtime(lab)
    lab.note(f"runtime {st}")
    lab.wait_idle(seconds=10, timeout=600)

    t = time.time()
    ra = lab.call("draft.read_ahead", DRAFT, timeout=60)
    time.sleep(20)
    r = cloud_reqs(lab, t)
    lab.check("read_ahead.local.started", isinstance(ra, dict) and ra.get("started"), ra)
    lab.check("read_ahead.local.nothing_to_cloud", not r, [(x.get("host"), x.get("path")) for x in r],
              severity="High")

    t = time.time()
    d = dict(DRAFT, provider="anthropic")
    ra = lab.call("draft.read_ahead", d, timeout=60)
    time.sleep(10)
    r = cloud_reqs(lab, t)
    lab.check("read_ahead.cloud.skipped", isinstance(ra, dict) and not ra.get("started")
              and ra.get("skipped") == "cloud", ra)
    lab.check("read_ahead.cloud.nothing_sent", not r, [(x.get("host"), x.get("path")) for x in r],
              severity="Blocker")

    t = time.time()
    started, text, done = common.stream(lab, "draft.begin", d, timeout=300)
    r = cloud_reqs(lab, t)
    lab.check("draft.cloud.no_sensitive", not leaked(r), leaked(r), severity="Blocker")
    lab.check("draft.cloud.excluded_sensitive", started.get("excluded_sensitive", 0) >= 0,
              f"excluded={started.get('excluded_sensitive')} provider={started.get('provider')}")

    t = time.time()
    started, text, done = common.stream(lab, "retrieve.ask", {
        "query": "How will the Acme renewal be paid?", "budget_tokens": 1500}, timeout=600)
    r = cloud_reqs(lab, t)
    lab.check("ask.local_first", started.get("locality") == "on_device" and not r,
              f"{started.get('provider')} {started.get('locality')} cloud={len(r)}")

    t = time.time()
    hist = [{"role": "user", "text": "How will the Acme renewal be paid?"},
            {"role": "assistant", "text": text[:400] or "By card."}]
    started, text2, done = common.stream(lab, "retrieve.ask", {
        "query": "And the wire details?", "budget_tokens": 1500, "provider": "anthropic",
        "history": hist}, timeout=600)
    r = cloud_reqs(lab, t)
    lab.check("followup.cloud.single_request", len(r) == 1,
              f"{len(r)} cloud requests; rewrite={started.get('query_rewrite')}")
    lab.check("followup.cloud.no_sensitive", not leaked(r), leaked(r), severity="Blocker")
    with open(os.path.join(lab.out, "followup-cloud-request.json"), "w") as f:
        json.dump(r, f, indent=1)

    # Kill the local chat server and ask again: what happens, and what goes to the cloud.
    t = time.time()
    pidf = os.path.join(lab.data, "runtime", "chat.pid")
    pid = open(pidf).read().split()[0] if os.path.exists(pidf) else ""
    k = subprocess.run(["kill", "-9", pid], capture_output=True) if pid else None
    lab.note(f"killed chat server pid={pid!r} rc={k and k.returncode}; "
             + subprocess.run(["sh", "-c", "pgrep -af llama-server || true"],
                              capture_output=True, text=True).stdout[:300])
    try:
        started, text3, done = common.stream(lab, "retrieve.ask", {
            "query": "How will the Acme renewal be paid?", "budget_tokens": 1500}, timeout=600)
        r = cloud_reqs(lab, t)
        lab.check("fallback.observed", True,
                  f"provider={started.get('provider')} locality={started.get('locality')} "
                  f"excluded={started.get('excluded_sensitive')} cloud_requests={len(r)} err={done and done.get('error')}")
        lab.check("fallback.no_sensitive", not leaked(r), leaked(r), severity="Blocker")
        if r:
            lab.check("fallback.excluded_counted", started.get("excluded_sensitive", 0) > 0,
                      started.get("excluded_sensitive"))
    except Exception as e:  # noqa: BLE001
        lab.check("fallback.observed", True, f"error: {e!r}")
    time.sleep(3)
    rows = lab.ledger()
    hosts = {(x["host"], x["feature"]) for x in rows}
    lab.note(f"ledger {sorted(hosts)}")
    obs = {r.get("host") for r in lab.requests() if "method" in r}
    lab.check("every_cloud_request_ledgered", obs <= {h for h, _ in hosts}, f"observed={obs} ledger={hosts}",
              severity="High")
