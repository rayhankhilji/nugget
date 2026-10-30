#!/usr/bin/env python3
"""Replay Maya's two weeks through the real daemon, one simulated morning at a time.

For each day in persona/world/days.json: copy every file that has arrived by the morning into the
files source's root, start `nuggetd` under libfaketime at `--start` (local, default 07:59:00) in
the day's zone (`TZ`), sync, refresh, wait for `brief.ready` at 08:00, then record `cards.list`,
`brief.get`, `proactive.learning` and `proactive.metrics`. With `--user` a simulated user then
accepts cards that match the truth's expectations and dismisses the ones the truth forbids
(reason `not_relevant`, or `already_handled` when the truth says it was answered).

Output: OUT/day-NN.json per morning and OUT/summary.json (expectation hits and misses, forbidden
cards seen, duplicates, sensitive flags, brief checks).

    python3 replay.py --out OUT [--user] [--runtime --chat-model ID] [--days 1-14] [--judge off]
"""
import argparse
import json
import os
import shutil
import sys
import time
from datetime import datetime, timedelta
from pathlib import Path
from zoneinfo import ZoneInfo

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "harness"))
from nq import Daemon, Home  # noqa: E402

WORLD = Path(__file__).resolve().parents[1] / "persona" / "world"


def text_of(c):
    return " ".join([c.get("title", ""), c.get("detail", ""), c.get("summary") or "",
                     " ".join(c.get("people", []))]).lower()


def kind_ok(want, got):
    return want in ("any", got)


def in_window(date, e):
    return (e.get("first") or "0000") <= date <= (e.get("last") or "9999")


def check_day(date, cards, brief, truth, tz):
    hits, misses, forbidden, early_late = [], [], [], []
    open_cards = [c for c in cards if c.get("state") in ("new", "seen")]
    for s in truth:
        for e in s["expect"]:
            m = [c for c in open_cards if e["match"] in text_of(c) and kind_ok(e["kind"], c["kind"])]
            if in_window(date, e):
                (hits if m else misses).append({"scenario": s["id"], "expect": e,
                                                "cards": [c["id"] for c in m]})
            elif m and not e.get("optional"):
                early_late.append({"scenario": s["id"], "expect": e,
                                   "cards": [(c["id"], c["kind"], c["title"]) for c in m]})
        for f in s["forbid"]:
            if not ((f.get("from") or "0000") <= date <= (f.get("to") or "9999")):
                continue
            m = [c for c in open_cards if f["match"] in text_of(c) and kind_ok(f.get("kind", "any"), c["kind"])]
            for c in m:
                forbidden.append({"scenario": s["id"], "why": f["why"], "card": c["id"], "kind": c["kind"],
                                  "title": c["title"]})
    # duplicates: two open cards of the same kind about the same normalised title
    seen, dups = {}, []
    for c in open_cards:
        k = (c["kind"], c["title"].lower().removeprefix("re: ").strip())
        if k in seen:
            dups.append([seen[k], c["id"], c["title"]])
        seen[k] = c["id"]
    # brief checks
    bchecks = []
    for s in truth:
        for b in s.get("brief", []):
            if b["date"] != date:
                continue
            ms = brief.get("meetings", []) if brief else []
            if "absent" in b:
                bad = [m for m in ms if b["absent"].lower() in m["title"].lower()]
                bchecks.append({"check": b, "ok": not bad, "got": [(m["title"], m["start"]) for m in bad]})
                continue
            m = [m for m in ms if b["meeting"].lower() in m["title"].lower()]
            ok = bool(m)
            got = [(x["title"], x["start"], x.get("all_day")) for x in m]
            if ok and "local" in b:
                want = b["local"]
                loc = [datetime.fromisoformat(x["start"].replace("Z", "+00:00")).astimezone(ZoneInfo(b["tz"])).strftime("%H:%M") for x in m]
                ok = want in loc and not (b.get("not_local") in loc)
                got = [g + (l,) for g, l in zip(got, loc)]
            bchecks.append({"check": b, "ok": ok, "got": got})
    return {"hits": hits, "misses": misses, "forbidden": forbidden, "out_of_window": early_late,
            "duplicates": dups, "brief_checks": bchecks}


def user_acts(d, cards, truth, date, log):
    """A plausible user: accept what the truth expects today, dismiss what it forbids."""
    for c in [c for c in cards if c.get("state") in ("new", "seen")]:
        t = text_of(c)
        verdict = None
        for s in truth:
            for f in s["forbid"]:
                if f["match"] in t and kind_ok(f.get("kind", "any"), c["kind"]) and \
                        (f.get("from") or "0000") <= date <= (f.get("to") or "9999"):
                    verdict = ("dismiss", "already_handled" if any(w in f["why"] for w in ("answered", "replied", "sent", "delivered")) else "not_relevant", s["id"])
            for e in s["expect"]:
                if e["match"] in t and kind_ok(e["kind"], c["kind"]) and in_window(date, e) and not verdict:
                    verdict = ("accept", None, s["id"])
        if verdict is None and c["rank"] < 0.5:
            verdict = ("dismiss", "not_important", "low-rank")
        if verdict is None:
            continue
        r, err = d.try_call("cards.feedback", {"card_id": c["id"], "feedback": verdict[0],
                                                **({"reason": verdict[1]} if verdict[1] else {})})
        log.append({"card": c["id"], "title": c["title"], "kind": c["kind"], "action": verdict[0],
                    "reason": verdict[1], "because": verdict[2], "error": str(err) if err else None})


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--user", action="store_true", help="simulate accept/dismiss")
    ap.add_argument("--runtime", action="store_true", help="[runtime] on (real local models)")
    ap.add_argument("--chat-model")
    ap.add_argument("--embed-model")
    ap.add_argument("--models-home")
    ap.add_argument("--judge", default="local")
    ap.add_argument("--days", default="1-14")
    ap.add_argument("--start", default="07:59:00")
    ap.add_argument("--brief-wait", type=float, default=150)
    ap.add_argument("--fixed-tz", help="never move TZ (e.g. the user did not change the OS zone)")
    a = ap.parse_args()
    out = Path(a.out).resolve()
    out.mkdir(parents=True, exist_ok=True)
    days = json.loads((WORLD / "days.json").read_text())
    timeline = json.loads((WORLD / "timeline.json").read_text())
    truth = json.loads((WORLD / "truth.json").read_text())
    lo, hi = (int(x) for x in a.days.split("-"))
    home = Home(out / "home", runtime=a.runtime, chat_model=a.chat_model, embed_model=a.embed_model,
                judge=a.judge, models_home=a.models_home)
    root = out / "maya"
    root.mkdir(exist_ok=True)
    copied = set()
    source = None
    summary = {"days": [], "args": vars(a)}
    for day in days:
        if not lo <= day["day"] <= hi:
            continue
        tz = a.fixed_tz or day["tz"]
        start = datetime.fromisoformat(f"{day['date']}T{a.start}").replace(tzinfo=ZoneInfo(tz))
        for it in timeline:
            if it["id"] not in copied and datetime.fromisoformat(it["at"]) <= start:
                dst = root / it["path"]
                dst.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(WORLD / "files" / it["path"], dst)
                ts = datetime.fromisoformat(it["at"]).timestamp()
                os.utime(dst, (ts, ts))  # the file's time is its arrival, not today
                copied.add(it["id"])
        rec = {"day": day["day"], "date": day["date"], "tz": tz, "start": start.isoformat(),
               "files": len(copied)}
        t0 = time.time()
        with Daemon(home, sim=start, tz=tz, log_name="nuggetd") as d:
            if source is None:
                source = d.add_files_source(root)["id"]
            rec["sync"] = d.sync(source)
            d.drain(3)
            r, err = d.try_call("proactive.refresh", {}, timeout=900)
            rec["refresh"] = r if r is not None else {"error": str(err)}
            brief_note = d.wait_note("brief.ready", timeout=a.brief_wait)
            rec["brief_ready"] = brief_note["params"] if brief_note else None
            rec["cards"] = d.cards()
            rec["cards_all"] = d.cards(include_closed=True)["cards"]
            rec["brief"], e = d.try_call("brief.get", {})
            rec["learning"], _ = d.try_call("proactive.learning", {})
            rec["metrics"], _ = d.try_call("proactive.metrics", {})
            rec["check"] = check_day(day["date"], rec["cards"]["cards"], rec["brief"], truth, tz)
            if a.user:
                rec["user"] = []
                user_acts(d, rec["cards"]["cards"], truth, day["date"], rec["user"])
                rec["learning_after"], _ = d.try_call("proactive.learning", {})
            rec["notes"] = [n for n in d.notes if n.get("method") != "ingest.progress"][:50]
        rec["secs"] = round(time.time() - t0, 1)
        (out / f"day-{day['day']:02d}.json").write_text(json.dumps(rec, indent=1, default=str))
        c = rec["check"]
        line = (f"day {day['day']:2d} {day['date']} {tz:16s} open={len(rec['cards']['cards']):2d} "
                f"hits={len(c['hits'])} misses={len(c['misses'])} forbidden={len(c['forbidden'])} "
                f"dups={len(c['duplicates'])} brief_ok={sum(b['ok'] for b in c['brief_checks'])}/{len(c['brief_checks'])} "
                f"brief_ready={'yes' if rec['brief_ready'] else 'NO'} ({rec['secs']}s)")
        print(line, flush=True)
        summary["days"].append({"day": day["day"], "date": day["date"], "line": line, "check": c,
                                "titles": [(x["kind"], x["title"], round(x["rank"], 2), x["sensitive"]) for x in rec["cards"]["cards"]]})
    (out / "summary.json").write_text(json.dumps(summary, indent=1, default=str))


if __name__ == "__main__":
    main()
