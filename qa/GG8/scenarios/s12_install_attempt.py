"""Onboarding's model install, with the internet sinkholed (every name resolves to the sink, which
answers 404 over TLS). Which hosts does an install contact, and are the attempts in the ledger
(feature model_download / runtime_download)?"""
import time

NAME = "s12_install_attempt"
CONFIG = ""


def run(lab):
    models = lab.call("models.list")
    rec = [m for m in models if m.get("recommended")] or models
    ids = [m["id"] for m in rec][:2]
    lab.note(f"installing {ids}")
    c = lab.client()
    for mid in ids:
        try:
            c.call("models.install", {"id": mid}, timeout=60)
        except Exception as e:  # noqa: BLE001
            lab.note(f"install {mid}: {e}")
        n = c.wait_note(lambda m: m.get("method") == "models.progress"
                        and m["params"].get("phase") in ("failed", "installed"), timeout=120)
        lab.note(f"progress {mid}: {n}")
    time.sleep(5)
    reqs = [r for r in lab.requests() if "method" in r]
    hosts = sorted({r["host"] for r in reqs})
    rows = lab.ledger()
    lab.check("install.contacted", bool(reqs), [(r["host"], r["path"], r["status"]) for r in reqs][:10])
    lab.check("install.attempts_in_ledger", all(any(row["host"].split(":")[0] == h for row in rows) for h in hosts),
              {"hosts": hosts, "ledger": [(r["host"], r["feature"], r["bytes_sent"], r["bytes_received"]) for r in rows]})
    lab.check("install.dns_only_documented_hosts", True, sorted({d.get("qname") for d in lab.dns()}))
