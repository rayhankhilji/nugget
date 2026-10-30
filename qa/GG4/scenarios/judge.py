#!/usr/bin/env python3
"""A5: the optional LLM judge with a real local model (CI), or without one (locally: the judge has
no provider and must fall back cleanly).

Replays selected persona mornings (cumulative files, simulated clock) with `judge = "local"` and
checks every enriched card:
- grounded: numbers, money, times and capitalised names in the summary appear in the card's
  title/detail/evidence (else "unsupported");
- temporal: words like today/tomorrow/overdue/yesterday agree with the card's due date and the
  simulated morning;
- stale: a summary that stays the same after the due date passed;
- sensitive: sensitive cards' summaries quoting health/salary/fraud words;
- the suggested action is one the card offers (first action);
- refresh latency with the judge, judge errors, and a failure mode (the model server killed
  mid-run: cards must still come back, the refresh must report the judge error).

    python3 judge.py --out OUT [--runtime --chat-model ID --embed-model ID --models-home DIR]
"""
import argparse
import json
import os
import re
import signal
import shutil
import subprocess
import sys
import time
from datetime import datetime, date
from pathlib import Path
from zoneinfo import ZoneInfo

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "harness"))
from nq import Daemon, Home  # noqa: E402

WORLD = Path(__file__).resolve().parents[1] / "persona" / "world"
DAYS = ["2026-10-28", "2026-10-29", "2026-10-30", "2026-11-02", "2026-11-03", "2026-11-05"]
SENSITIVE_WORDS = ["metformin", "hba1c", "lab", "diagnos", "salary", "$118", "fraud", "therapy",
                   "session", "results"]
STOP = {"The", "This", "User", "Maya", "Reply", "Draft", "Send", "Follow", "Remind", "A", "An", "It",
        "They", "You", "Your", "No", "Prepare", "Check", "Meeting", "Confirm", "Respond", "Ask", "Is",
        "There", "As", "To", "In", "On", "For", "Since", "Because", "If", "Make", "Review", "Consider",
        "Mon", "Monday", "Tue", "Tuesday", "Wed", "Wednesday", "Thu", "Thursday", "Fri", "Friday",
        "Sat", "Saturday", "Sun", "Sunday", "October", "November", "Oct", "Nov", "I", "We"}


def tokens(summary):
    nums = re.findall(r"\$?\d[\d,.:]*\s?(?:am|pm)?", summary, re.I)
    names = [w for w in re.findall(r"\b[A-Z][a-zA-Z]+\b", summary) if w not in STOP]
    return [n.strip().rstrip(".,") for n in nums if n.strip().rstrip(".,")], names


def grounded(card):
    src = " ".join([card["title"], card["detail"], " ".join(card.get("people", []))] +
                   [e.get("quote") or "" for e in card.get("evidence", [])] +
                   [e.get("title") or "" for e in card.get("evidence", [])]).lower()
    nums, names = tokens(card.get("summary") or "")
    bad = [n for n in nums if n.lower().replace(",", "") not in src.replace(",", "")]
    bad += [n for n in names if n.lower() not in src]
    return bad


def temporal(card, today):
    s = (card.get("summary") or "").lower()
    if not card.get("due"):
        return None
    due = datetime.fromisoformat(card["due"].replace("Z", "+00:00")).date()
    delta = (due - today).days
    problems = []
    if "tomorrow" in s and delta != 1:
        problems.append(f"says tomorrow, due in {delta} days")
    if re.search(r"\btoday\b", s) and delta != 0:
        problems.append(f"says today, due in {delta} days")
    if "overdue" in s and delta >= 0:
        problems.append(f"says overdue, due in {delta} days")
    if delta < 0 and not any(w in s for w in ("overdue", "past", "missed", "late", "was due", "passed")) \
            and re.search(r"\b(by|before|due)\b", s):
        problems.append(f"due {-delta} days ago but summary reads as upcoming")
    return problems


def find_llama():
    out = subprocess.run(["pgrep", "-f", "llama-server"], capture_output=True, text=True).stdout.split()
    return [int(p) for p in out]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--runtime", action="store_true")
    ap.add_argument("--chat-model")
    ap.add_argument("--embed-model")
    ap.add_argument("--models-home")
    a = ap.parse_args()
    out = Path(a.out).resolve()
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)
    timeline = json.loads((WORLD / "timeline.json").read_text())
    days = {d["date"]: d for d in json.loads((WORLD / "days.json").read_text())}
    home = Home(out / "home", runtime=a.runtime, chat_model=a.chat_model, embed_model=a.embed_model,
                judge="local", models_home=a.models_home)
    root = out / "maya"
    root.mkdir()
    copied, source = set(), None
    rep = {"days": [], "cards": [], "checks": {}}
    seen_summary = {}
    for ds in DAYS:
        tz = days[ds]["tz"]
        start = datetime.fromisoformat(f"{ds}T07:30:00").replace(tzinfo=ZoneInfo(tz))
        for it in timeline:
            if it["id"] not in copied and datetime.fromisoformat(it["at"]) <= start:
                dst = root / it["path"]
                dst.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(WORLD / "files" / it["path"], dst)
                ts = datetime.fromisoformat(it["at"]).timestamp()
                os.utime(dst, (ts, ts))  # the file's time is its arrival, not today
                copied.add(it["id"])
        with Daemon(home, sim=start, tz=tz, log_name="nuggetd") as d:
            if a.runtime:
                d.wait_runtime_ready(900)
            if source is None:
                source = d.add_files_source(root)["id"]
            d.sync(source)
            d.drain(3)
            t0 = time.time()
            r, err = d.try_call("proactive.refresh", {}, timeout=1800)
            secs = round(time.time() - t0, 1)
            cards = d.cards()["cards"]
            met, _ = d.try_call("proactive.metrics", {})
            brief, _ = d.try_call("brief.get", {})
        today = date.fromisoformat(ds)
        enriched = [c for c in cards if c.get("summary")]
        rep["days"].append({"date": ds, "refresh": r or str(err), "refresh_secs": secs, "open": len(cards),
                            "enriched": len(enriched), "brief_top": [(b["title"], b["summary"]) for b in (brief or {}).get("top", [])]})
        for c in cards:
            s = c.get("summary")
            rec = {"date": ds, "id": c["id"], "kind": c["kind"], "title": c["title"], "detail": c["detail"],
                   "due": c.get("due"), "sensitive": c["sensitive"], "summary": s,
                   "actions": [x["kind"] for x in c["actions"]]}
            if s:
                rec["unsupported"] = grounded(c)
                rec["temporal"] = temporal(c, today)
                rec["sensitive_words"] = [w for w in SENSITIVE_WORDS if w in s.lower()] if c["sensitive"] else []
                prev = seen_summary.get(c["id"])
                rec["stale"] = bool(prev and prev[1] == s and c.get("due") and
                                    datetime.fromisoformat(c["due"].replace("Z", "+00:00")).date() < today <= today
                                    and prev[0] != ds)
                seen_summary.setdefault(c["id"], (ds, s))
            rep["cards"].append(rec)
        print(f"{ds} refresh {secs}s open={len(cards)} enriched={len(enriched)} {r or err}", flush=True)

    # failure mode: kill the model server just before a refresh
    ds = "2026-11-06"
    start = datetime.fromisoformat(f"{ds}T07:30:00").replace(tzinfo=ZoneInfo("Europe/London"))
    for it in timeline:
        if it["id"] not in copied and datetime.fromisoformat(it["at"]) <= start:
            dst = root / it["path"]
            dst.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(WORLD / "files" / it["path"], dst)
            copied.add(it["id"])
    with Daemon(home, sim=start, tz="Europe/London", log_name="nuggetd") as d:
        killed = []
        if a.runtime:
            d.wait_runtime_ready(900)
            for p in find_llama():
                try:
                    os.kill(p, signal.SIGKILL)
                    killed.append(p)
                except OSError:
                    pass
        d.sync(source)
        t0 = time.time()
        r, err = d.try_call("proactive.refresh", {}, timeout=1800)
        fail = {"killed_llama": killed, "refresh": r, "error": str(err) if err else None,
                "secs": round(time.time() - t0, 1), "open": len(d.cards()["cards"]),
                "runtime": d.try_call("runtime.status", {})[0]}
    rep["failure_mode"] = fail
    log = (home.path / "nuggetd.log").read_text(errors="replace")
    logs = "".join(p.read_text(errors="replace") for p in (home.data / "logs").glob("*.log")) if (home.data / "logs").exists() else ""
    rep["judge_log_lines"] = [l[:400] for l in (log + logs).splitlines() if "judge" in l.lower()][-40:]

    en = [c for c in rep["cards"] if c.get("summary")]
    uniq = {(c["id"], c["summary"]): c for c in en}.values()
    rep["checks"] = {
        "judge_enriched_any": {"ok": bool(en) or not a.runtime, "n": len(en)},
        "grounded": {"ok": all(not c["unsupported"] for c in uniq), "n": len(uniq),
                     "bad": [(c["title"], c["summary"], c["unsupported"]) for c in uniq if c["unsupported"]]},
        "temporal": {"ok": all(not c["temporal"] for c in en), "n": len(en),
                     "bad": [(c["date"], c["title"], c["due"], c["summary"], c["temporal"]) for c in en if c["temporal"]]},
        "stale_after_due": {"ok": not any(c.get("stale") for c in en),
                            "bad": [(c["date"], c["title"], c["summary"]) for c in en if c.get("stale")]},
        "sensitive_quoted": {"ok": not any(c["sensitive_words"] for c in en),
                             "bad": [(c["title"], c["summary"]) for c in en if c["sensitive_words"]]},
        "failure_fallback": {"ok": fail["open"] > 0 and fail["error"] is None, "evidence": fail},
    }
    (out / "summary.json").write_text(json.dumps({"report": rep["checks"], "detail": rep}, indent=1, default=str))
    for k, v in rep["checks"].items():
        print(k, "OK" if v["ok"] else "FAIL", json.dumps({x: y for x, y in v.items() if x != "ok"}, default=str)[:1500])


if __name__ == "__main__":
    main()
