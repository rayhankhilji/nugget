"""Onboarding with a local model only, on the real internet (lab `internet` mode).

Fresh home; models.list; models.install of the manifest's smallest chat and embedding models; the
first request then installs and starts the pinned llama-server. Every host reached (DNS names, TLS
SNI, remote addresses, bytes) is compared with the egress ledger, which should hold one
`model_download` / `runtime_download` row per download, naming where the bytes came from."""
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import common  # noqa: E402

NAME = "s60_onboarding"
MODE = "internet"
CONFIG = common.LOCAL_CONFIG


def run(lab):
    t0 = time.time()
    models = lab.call("models.list")
    lab.check("models.list.offline", len(lab.dns()) == 0,
              f"{len(models)} models; DNS after models.list: {[d.get('qname') for d in lab.dns()]}")
    for mid in (common.EMBED_MODEL, common.CHAT_MODEL):
        lab.mark(f"install {mid}")
        last, secs = common.install_model(lab, mid)
        lab.check(f"install.{mid}", last and last.get("phase") == "installed",
                  f"{last} in {secs:.0f}s")
    lab.mark("runtime start")
    # The first request installs the runtime and starts both servers.
    try:
        started, text, done = common.stream(lab, "chat", {
            "provider": None, "tier": "synthesis", "items": [],
            "messages": [{"role": "user", "text": "Say hello in five words."}]}, timeout=900)
        lab.check("first.chat", done and not done.get("error"), f"{text[:120]!r} {done}")
    except Exception as e:  # noqa: BLE001
        lab.check("first.chat", False, repr(e))
    st = common.wait_runtime(lab)
    lab.check("runtime.ready", st and all(r.get("state") == "ready" for r in st), st)
    time.sleep(5)
    res = common.compare_with_ledger(lab, since=t0)
    feats = {f for h in res["ledger"].values() for f in h["features"]}
    lab.check("ledger.has_model_download", "model_download" in feats, sorted(feats))
    lab.check("ledger.has_runtime_download", "runtime_download" in feats, sorted(feats))
    lab.check("ledger.only_download_features",
              feats <= {"model_download", "runtime_download"}, sorted(feats))
    lab.check("every_observed_host_ledgered", not res["unledgered"],
              {"unledgered": res["unledgered"], "observed": list(res["observed"]),
               "ledger": list(res["ledger"]), "dns": res["dns"]}, severity="High")
    for h, o in res["observed"].items():
        led = res["ledger"].get(h)
        if led and o["bytes_down"] > 1_000_000:
            ratio = led["bytes_received"] / max(1, o["bytes_down"])
            lab.check(f"bytes.{h}", 0.9 <= ratio <= 1.1,
                      f"ledger {led['bytes_received']} vs tcp payload {o['bytes_down']} ({ratio:.3f})")
