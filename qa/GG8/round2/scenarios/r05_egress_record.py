"""Round 2: EE5's app-reported ledger rows (egress.record). The desktop app records its update
checks and downloads through this method. Checks: a valid row lands; features other than
update_check/update_download are refused (a local program must not forge rows for Nugget's own
features); the documented rate limit (60 a minute) holds; bad hosts and times are refused."""
import json
import time

NAME = "r05_egress_record"
CONFIG = ""


def attempt(lab, params):
    try:
        lab.call("egress.record", params, timeout=10)
        return "ok"
    except Exception as e:  # noqa: BLE001
        return str(e)[:160]


def run(lab):
    ok = attempt(lab, {"host": "github.com", "feature": "update_check", "bytes_sent": 300, "bytes_received": 4200})
    rows = lab.ledger()
    lab.check("record.valid_row", ok == "ok" and any(r["feature"] == "update_check" and r["host"] == "github.com" for r in rows),
              {"result": ok, "rows": rows[:2]})
    forged = {f: attempt(lab, {"host": "api.anthropic.com", "feature": f, "bytes_sent": 1})
              for f in ("ask", "model_download", "sync:gmail", "draft", "UPDATE_CHECK")}
    lab.check("record.refuses_other_features", all(v != "ok" for v in forged.values()), forged)
    bad = {"url_host": attempt(lab, {"host": "https://github.com/x", "feature": "update_check", "bytes_sent": 1}),
           "future": attempt(lab, {"host": "github.com", "feature": "update_check", "bytes_sent": 1, "at": "2030-01-01T00:00:00Z"}),
           "old": attempt(lab, {"host": "github.com", "feature": "update_check", "bytes_sent": 1, "at": "2020-01-01T00:00:00Z"}),
           "negative": attempt(lab, {"host": "github.com", "feature": "update_check", "bytes_sent": -5}),
           "long_host": attempt(lab, {"host": "a" * 300 + ".com", "feature": "update_check", "bytes_sent": 1})}
    lab.check("record.refuses_bad_values", all(v != "ok" for v in bad.values()), bad)
    results = [attempt(lab, {"host": "github.com", "feature": "update_download", "bytes_sent": 1}) for _ in range(80)]
    accepted = sum(r == "ok" for r in results)
    lab.check("record.rate_limited_to_60_per_minute", accepted <= 60,
              {"accepted": accepted, "first_refusal": next((r for r in results if r != "ok"), None)})
    lab.check("record.no_network", not lab.requests() and not lab.dns(), len(lab.requests()))
    with open(f"{lab.out}/ledger-features.json", "w") as f:
        json.dump(sorted({(r["feature"], r["host"]) for r in lab.ledger(1000)}), f)
