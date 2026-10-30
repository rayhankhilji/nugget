#!/usr/bin/env python3
"""A4: the morning brief: accuracy, timing, time zones, DST, the trip, config errors.

Runs the real `nuggetd` under libfaketime (harness/nq.py). Speed-up runs use
`FAKETIME="@<epoch> xN"` with the monotonic clock faked too (tokio's timers and epoll waits scale),
so a daemon can run through a night (and the DST change) in a few real minutes.

Ground truth for "today's meetings" is computed here from the .ics files that have arrived by the
morning, read properly (TZID, VALUE=DATE, RRULE weekly/daily, UID + SEQUENCE, METHOD:CANCEL /
STATUS:CANCELLED, the user's PARTSTAT=DECLINED), independent of the product.

    python3 brief.py --out OUT [--only accuracy,timing,dst,trip,config,edge,snooze]
                     [--runtime --chat-model ID --embed-model ID --models-home DIR] [--judge off]

Writes OUT/summary.json {"report": {check: {"ok": bool, "evidence": ...}}, ...} and per-part JSON.
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import time
import traceback
from datetime import date, datetime, timedelta, timezone
from pathlib import Path
from zoneinfo import ZoneInfo

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "harness"))
from nq import Daemon, Home, nuggetd_exe, faketime_lib  # noqa: E402

WORLD = Path(__file__).resolve().parents[1] / "persona" / "world"
NY, LDN, UTC = "America/New_York", "Europe/London", timezone.utc
USER = "maya@larkspur.example"
REPORT: dict = {}
ARGS = None


def check(name, ok, **evidence):
    REPORT[name] = {"ok": bool(ok), "evidence": evidence}
    print(f"  [{'PASS' if ok else 'FAIL'}] {name}", flush=True)
    return ok


def ts(s):
    return datetime.fromisoformat(s.replace("Z", "+00:00"))


def loc(s, tz):
    return ts(s).astimezone(ZoneInfo(tz))


def home_for(path, **kw):
    a = ARGS
    shutil.rmtree(path, ignore_errors=True)
    return Home(path, runtime=a.runtime, chat_model=a.chat_model, embed_model=a.embed_model,
                judge=kw.pop("judge", a.judge), models_home=a.models_home, **kw)


def speed_env(start: datetime, rate: int, extra=None):
    """Env for a sped-up daemon: wall and monotonic clocks run `rate`x from `start`."""
    e = {"FAKETIME": f"@{int(start.timestamp())} x{rate}", "FAKETIME_DONT_FAKE_MONOTONIC": "0"}
    e.update(extra or {})
    return e


class SpeedDaemon(Daemon):
    """Daemon whose monotonic clock is faked too (the harness pins it to real by default)."""

    def env(self):
        e = super().env()
        if "x" in e.get("FAKETIME", ""):
            e.pop("FAKETIME_DONT_FAKE_MONOTONIC", None)
        return e


# ---------------------------------------------------------------- world data

TIMELINE = json.loads((WORLD / "timeline.json").read_text())
DAYS = json.loads((WORLD / "days.json").read_text())
TRUTH = json.loads((WORLD / "truth.json").read_text())


def copy_until(root: Path, instant: datetime, copied: set | None = None):
    copied = copied if copied is not None else set()
    for it in TIMELINE:
        if it["id"] not in copied and datetime.fromisoformat(it["at"]) <= instant:
            dst = root / it["path"]
            dst.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(WORLD / "files" / it["path"], dst)
            copied.add(it["id"])
    return copied


# ---------------------------------------------------------------- independent iCalendar truth

def unfold(text):
    out = []
    for line in text.replace("\r\n", "\n").split("\n"):
        if line[:1] in (" ", "\t") and out:
            out[-1] += line[1:]
        else:
            out.append(line)
    return out


def prop(line):
    head, _, val = line.partition(":")
    parts = head.split(";")
    params = {}
    for p in parts[1:]:
        k, _, v = p.partition("=")
        params[k.upper()] = v
    return parts[0].upper(), params, val


def ical_time(val, params):
    if params.get("VALUE") == "DATE" or (len(val) == 8 and val.isdigit()):
        return date(int(val[:4]), int(val[4:6]), int(val[6:8])), True
    d = datetime.strptime(val.rstrip("Z"), "%Y%m%dT%H%M%S")
    if val.endswith("Z"):
        return d.replace(tzinfo=UTC), False
    if "TZID" in params:
        return d.replace(tzinfo=ZoneInfo(params["TZID"])), False
    return d, False  # floating


def parse_ics(path: Path, user=USER):
    lines = unfold(path.read_text())
    method = next((prop(l)[2] for l in lines if l.upper().startswith("METHOD:")), "PUBLISH")
    evs, cur = [], None
    for l in lines:
        if l == "BEGIN:VEVENT":
            cur = {"method": method.upper(), "file": path.name, "attendees": [], "seq": 0, "status": ""}
        elif l == "END:VEVENT":
            evs.append(cur)
            cur = None
        elif cur is not None and ":" in l:
            name, params, val = prop(l)
            if name == "UID":
                cur["uid"] = val
            elif name == "SUMMARY":
                cur["title"] = val.replace("\\,", ",")
            elif name == "DTSTART":
                cur["start"], cur["all_day"] = ical_time(val, params)
            elif name == "DTEND":
                cur["end"], _ = ical_time(val, params)
            elif name == "RRULE":
                cur["rrule"] = dict(p.split("=", 1) for p in val.split(";"))
            elif name == "SEQUENCE":
                cur["seq"] = int(val)
            elif name == "STATUS":
                cur["status"] = val.upper()
            elif name == "ATTENDEE":
                cur["attendees"].append((val.lower().removeprefix("mailto:"), params.get("PARTSTAT", "")))
    return evs


def resolved_events(cal_dir: Path, user=USER):
    """Current version of each UID; cancelled and declined ones dropped."""
    by_uid = {}
    for f in sorted(cal_dir.glob("*.ics")):
        for e in parse_ics(f):
            k = e.get("uid") or f.name
            if k not in by_uid or e["seq"] >= by_uid[k]["seq"]:
                by_uid[k] = e
    out = []
    for e in by_uid.values():
        if e["method"] == "CANCEL" or e["status"] == "CANCELLED":
            continue
        if any(a == user and p.upper() == "DECLINED" for a, p in e["attendees"]):
            continue
        out.append(e)
    return out


WD = {"MO": 0, "TU": 1, "WE": 2, "TH": 3, "FR": 4, "SA": 5, "SU": 6}


def occurrences(e, day: date, tz: str):
    """Occurrences of `e` that start on local `day` in `tz` (timed) or cover `day` (all-day).
    Each: (title, 'HH:MM' | 'all-day', end_local_iso | None)."""
    z = ZoneInfo(tz)
    res = []
    if e["all_day"]:
        s = e["start"]
        en = e.get("end") or s + timedelta(days=1)
        if s <= day < en:
            res.append((e["title"], "all-day", None, s != day))
        return res
    st = e["start"]
    dur = (e["end"] - st) if e.get("end") is not None and (e["end"].tzinfo is None) == (st.tzinfo is None) else timedelta(hours=1)
    starts = [st]
    rr = e.get("rrule")
    if rr:
        freq = rr.get("FREQ")
        days = [WD[d[-2:]] for d in rr.get("BYDAY", "").split(",") if d] or [st.weekday()]
        cand = datetime.combine(day, st.timetz() if st.tzinfo else st.time())
        if st.tzinfo is not None:
            cand = datetime.combine(day, st.time()).replace(tzinfo=st.tzinfo)
        # Recurrences keep the wall-clock time in the event's own zone; check the dates around.
        starts = []
        for dd in (-1, 0, 1):
            c = cand + timedelta(days=dd)
            if c < st:
                continue
            if freq == "DAILY" or (freq == "WEEKLY" and c.weekday() in days):
                starts.append(c)
    for s in starts:
        sl = s.astimezone(z) if s.tzinfo else s.replace(tzinfo=z)
        if sl.date() == day:
            en = sl + dur
            res.append((e["title"], sl.strftime("%H:%M"), en.astimezone(z).isoformat(), False))
    return res


def expected_meetings(cal_dir: Path, day: date, tz: str):
    out = []
    for e in resolved_events(cal_dir):
        out.extend(occurrences(e, day, tz))
    return out


def all_summaries(cal_dir: Path):
    s = set()
    for f in cal_dir.glob("*.ics"):
        for e in parse_ics(f):
            s.add(e.get("title", ""))
    return s


def compare_meetings(brief, cal_dir: Path, day: date, tz: str):
    """Brief meetings vs the independent truth for `day` in `tz`."""
    exp = expected_meetings(cal_dir, day, tz) if cal_dir.exists() else []
    known = all_summaries(cal_dir) if cal_dir.exists() else set()
    got = []
    for m in (brief or {}).get("meetings", []):
        st = ts(m["start"])
        if m.get("all_day"):
            when = "all-day"
            d = st.date()  # midnight at the daemon's offset: the date as written
        else:
            when = st.astimezone(ZoneInfo(tz)).strftime("%H:%M")
            d = st.astimezone(ZoneInfo(tz)).date()
        got.append({"title": m["title"], "when": when, "date": d.isoformat(), "start": m["start"],
                    "end": m.get("end")})
    missing, wrong_time, extra, invented = [], [], [], []
    used = set()
    for t, w, _, cont in exp:
        idx = [i for i, g in enumerate(got) if g["title"] == t and i not in used]
        hit = [i for i in idx if got[i]["when"] == w]
        if hit:
            used.add(hit[0])
        elif idx:
            used.add(idx[0])
            wrong_time.append({"title": t, "want": w, "got": got[idx[0]]["when"], "start": got[idx[0]]["start"]})
        elif not cont:
            missing.append({"title": t, "want": w})
    for i, g in enumerate(got):
        if i in used:
            continue
        (invented if g["title"] not in known else extra).append(g)
    titles = [g["title"] for g in got]
    dups = sorted({t for t in titles if titles.count(t) > 1})
    order_ok = [g["start"] for g in got] == sorted([g["start"] for g in got], key=lambda s: ts(s))
    return {"expected": [(t, w) for t, w, _, _ in exp], "got": [(g["title"], g["when"]) for g in got],
            "missing": missing, "wrong_time": wrong_time, "extra": extra, "invented": invented,
            "duplicates": dups, "sorted": order_ok,
            "ok": not (missing or wrong_time or extra or invented or dups)}


# ---------------------------------------------------------------- brief evidence ("cited")

def cited(brief, cards_all, root: Path):
    """Every brief card resolves to a card whose evidence items exist in the files root."""
    by_id = {c["id"]: c for c in cards_all}
    bad = []
    n = 0
    for sec in ("top", "overdue", "renewals"):
        for bc in (brief or {}).get(sec, []):
            n += 1
            c = by_id.get(bc["card_id"])
            if not c:
                bad.append({"sec": sec, "card": bc["card_id"], "why": "no such card"})
                continue
            if not c["evidence"]:
                bad.append({"sec": sec, "card": bc["card_id"], "why": "no evidence"})
            for ev in c["evidence"]:
                key = ev["item"]["key"].split(":", 1)[-1]
                if not (root / key).exists():
                    bad.append({"sec": sec, "card": bc["card_id"], "why": "item missing", "key": key})
                u = ev.get("url") or ""
                if u.startswith("file://") and not Path(u[7:]).exists():
                    bad.append({"sec": sec, "card": bc["card_id"], "why": "url missing", "url": u})
                q = ev.get("quote")
                if q and (root / key).exists():
                    body = (root / key).read_text(errors="replace")
                    flat = " ".join(body.split())
                    if " ".join(q.rstrip("…").split())[:40] not in flat:
                        bad.append({"sec": sec, "card": bc["card_id"], "why": "quote not in item",
                                    "quote": q[:80]})
    for m in (brief or {}).get("meetings", []):
        n += 1
        # A brief meeting carries no item ref / uid / url: it cannot be traced back.
        if not any(k in m for k in ("item", "uid", "url")):
            bad.append({"sec": "meetings", "title": m["title"], "why": "no item ref/uid/url on BriefMeetingView"})
    return n, bad


# ---------------------------------------------------------------- one morning

def morning(home, root, start: datetime, tz, wait=120, source=None, env=None, cls=Daemon,
            log_name="nuggetd", after=None):
    rec = {"start": start.isoformat(), "tz": tz}
    with cls(home, sim=start, tz=tz, env=env, log_name=log_name) as d:
        if source is None:
            source = d.add_files_source(root)["id"]
        rec["sync"] = d.sync(source) if source != "none" else None
        n = d.wait_note("brief.ready", timeout=wait)
        rec["ready"] = n["params"] if n else None
        rec["brief"], e = d.try_call("brief.get", {})
        rec["cards_all"] = d.cards(include_closed=True)["cards"]
        if after:
            rec["after"] = after(d)
        d.drain(1)
        rec["more_ready"] = [x["params"]["generated_at"] for x in d.notes if x.get("method") == "brief.ready"]
    return rec, source


def truth_brief_checks(day, brief):
    res = []
    for s in TRUTH:
        for b in s.get("brief", []):
            if b["date"] != day:
                continue
            ms = (brief or {}).get("meetings", [])
            if "absent" in b:
                bad = [(m["title"], m["start"]) for m in ms if b["absent"].lower() in m["title"].lower()]
                res.append({"check": b, "ok": not bad, "got": bad})
                continue
            m = [x for x in ms if b["meeting"].lower() in x["title"].lower()]
            ok = bool(m)
            got = [(x["title"], x["start"], x["all_day"]) for x in m]
            if ok and "local" in b:
                locs = [loc(x["start"], b["tz"]).strftime("%H:%M") for x in m]
                ok = b["local"] in locs and b.get("not_local") not in locs
                got = [g + (l,) for g, l in zip(got, locs)]
            if ok and b.get("all_day"):
                ok = all(x["all_day"] for x in m)
            res.append({"check": b, "ok": ok, "got": got})
    return res


# ================================================================= parts

def part_accuracy(out: Path, mode="os", days=None):
    """Each persona morning. mode: 'os' = TZ follows the trip; 'fixed' = OS zone left on New York;
    'explicit' = OS on New York but [proactive] utc_offset set for the trip."""
    print(f"== accuracy ({mode})", flush=True)
    w = out / f"accuracy-{mode}"
    home = home_for(w / "home")
    root = w / "maya"
    root.mkdir(parents=True, exist_ok=True)
    copied, source, recs = set(), None, []
    lo, hi = days or (1, 14)
    for day in DAYS:
        if not lo <= day["day"] <= hi:
            continue
        real_tz = day["tz"]
        os_tz = real_tz if mode == "os" else NY
        if mode == "explicit":
            off = "+00:00" if real_tz == LDN else None
            home.utc_offset = off
            cfg = (home.config / "config.toml").read_text()
            cfg = "\n".join(l for l in cfg.splitlines() if not l.startswith("utc_offset"))
            if real_tz == LDN:
                cfg = cfg.replace("[proactive]\n", '[proactive]\nutc_offset = "+00:00"\n', 1)
            elif day["day"] == 14:
                # She forgot to remove it after the trip.
                cfg = cfg.replace("[proactive]\n", '[proactive]\nutc_offset = "+00:00"\n', 1)
            home.write_config(cfg)
        start = datetime.fromisoformat(f"{day['date']}T07:59:45").replace(tzinfo=ZoneInfo(real_tz))
        if mode == "fixed" and real_tz == LDN:
            start = datetime.fromisoformat(f"{day['date']}T07:59:45").replace(tzinfo=ZoneInfo(NY))
        if mode == "explicit" and day["day"] == 14:
            start = datetime.fromisoformat(f"{day['date']}T02:59:45").replace(tzinfo=ZoneInfo(NY))
        copy_until(root, start, copied)
        t0 = time.time()
        rec, source = morning(home, root, start, os_tz, wait=90 if mode == "os" else 75, source=source)
        rec["day"], rec["date"], rec["real_tz"] = day["day"], day["date"], real_tz
        b = rec["ready"] or rec["brief"]
        ga = rec["ready"]["generated_at"] if rec["ready"] else None
        rec["fired_local_real_tz"] = loc(ga, real_tz).strftime("%Y-%m-%d %H:%M:%S %Z") if ga else None
        rec["cmp_real"] = compare_meetings(b, root / "Calendar", date.fromisoformat(day["date"]), real_tz)
        if mode != "os":
            rec["cmp_os"] = compare_meetings(b, root / "Calendar", date.fromisoformat(day["date"]), os_tz)
        rec["truth"] = truth_brief_checks(day["date"], b)
        rec["cited_n"], rec["cited_bad"] = cited(b, rec["cards_all"], root)
        rec["secs"] = round(time.time() - t0, 1)
        c = rec["cmp_real"]
        print(f"  day {day['day']:2d} {day['date']} os={os_tz:16s} ready={'Y' if rec['ready'] else 'N'} "
              f"fired={rec['fired_local_real_tz']} meet got={c['got']} exp={c['expected']} "
              f"truth={sum(x['ok'] for x in rec['truth'])}/{len(rec['truth'])} ({rec['secs']}s)", flush=True)
        recs.append(rec)
    (w / "days.json").write_text(json.dumps(recs, indent=1, default=str))
    return recs


def eval_accuracy(recs):
    n = len(recs)
    fired = [r for r in recs if r["ready"]]
    check("acc.brief_ready_each_morning", len(fired) == n, fired=len(fired), n=n,
          missing=[r["date"] for r in recs if not r["ready"]])
    at8 = [r for r in fired if loc(r["ready"]["generated_at"], r["real_tz"]).strftime("%H:%M") == "08:00"]
    check("acc.fires_at_0800_local", len(at8) == len(fired), ok=len(at8), n=len(fired),
          times=[(r["date"], r["fired_local_real_tz"]) for r in fired])
    dm = [r for r in fired if r["ready"]["date"] == r["date"]]
    check("acc.brief_date_is_local_today", len(dm) == len(fired), ok=len(dm), n=len(fired),
          bad=[(r["date"], r["ready"]["date"]) for r in fired if r["ready"]["date"] != r["date"]])
    single = [r for r in recs if not r["more_ready"]]
    check("acc.no_second_brief_ready", len(single) == n, bad=[(r["date"], r["more_ready"]) for r in recs if r["more_ready"]])
    ok = [r for r in recs if r["cmp_real"]["ok"]]
    check("acc.meetings_match_calendar_truth", len(ok) == n, ok=len(ok), n=n,
          per_day={r["date"]: {k: r["cmp_real"][k] for k in ("expected", "got", "missing", "wrong_time", "extra", "invented", "duplicates")}
                   for r in recs if not r["cmp_real"]["ok"]})
    tr = [x for r in recs for x in r["truth"]]
    check("acc.truth_S4_brief_checks", all(x["ok"] for x in tr), ok=sum(x["ok"] for x in tr), n=len(tr),
          failed=[x for x in tr if not x["ok"]])
    inv = [x for r in recs for x in r["cmp_real"]["invented"]]
    check("acc.no_invented_meetings", not inv, n=n, invented=inv)
    dup = [(r["date"], r["cmp_real"]["duplicates"]) for r in recs if r["cmp_real"]["duplicates"]]
    check("acc.moved_event_once_at_new_time", not dup, dups=dup,
          oct30=[r["cmp_real"]["got"] for r in recs if r["date"] == "2026-10-30"])
    k = [r["cmp_real"]["got"] for r in recs if r["date"] == "2026-11-06"]
    check("acc.cancelled_event_absent", not any("Kelsey" in t for g in k for t, _ in g), nov6=k)
    srt = [r["date"] for r in recs if not r["cmp_real"]["sorted"]]
    check("acc.meetings_sorted", not srt, unsorted=srt)
    ad = [(r["date"], r["cmp_real"]["got"]) for r in recs if r["date"] in ("2026-10-30", "2026-10-31", "2026-11-01")]
    okad = all((("Mom's birthday", "all-day") in [tuple(x) for x in g]) == (d == "2026-10-31") for d, g in ad)
    check("acc.all_day_on_right_date", okad, got=ad)
    bad = [(r["date"], b) for r in recs for b in r["cited_bad"]]
    nc = sum(r["cited_n"] for r in recs)
    cards_bad = [x for x in bad if x[1]["sec"] != "meetings"]
    check("acc.cited_cards_resolve", not cards_bad, n=nc, bad=cards_bad[:20])
    mt = [x for x in bad if x[1]["sec"] == "meetings"]
    check("acc.cited_meetings_traceable", not mt, n_meetings=len(mt), example=mt[:3])
    # renewals: Pixelboard (Nov 3) within 7 days from Oct 27 to Nov 3; Cloudvault never.
    rn = {r["date"]: [c["title"] for c in (r["ready"] or r["brief"] or {}).get("renewals", [])] for r in recs}
    want = {d: ("2026-10-27" <= d <= "2026-11-03") for d in rn}
    hit = {d: any("pixelboard" in t.lower() for t in rn[d]) for d in rn}
    cv = [d for d in rn if any("cloudvault" in t.lower() for t in rn[d])]
    check("acc.renewals_section", all(hit[d] == want[d] for d in rn if d >= "2026-10-28") and not cv,
          renewals=rn, cloudvault_days=cv,
          wrong=[d for d in rn if d >= "2026-10-28" and hit[d] != want[d]])
    # overdue: everything past due; the budget promise (due Fri Oct 30) overdue Sat..Mon.
    od = {}
    bad_od = []
    for r in recs:
        b = r["ready"] or r["brief"] or {}
        od[r["date"]] = [(c["title"][:60], c["due"]) for c in b.get("overdue", [])]
        for c in b.get("overdue", []):
            if not c["due"] or ts(c["due"]) >= ts(b["generated_at"]):
                bad_od.append((r["date"], c["title"]))
    bud = {d: any("budget" in t.lower() for t, _ in od[d]) for d in od}
    want_b = {d: "2026-10-31" <= d <= "2026-11-02" for d in od}
    check("acc.overdue_section", not bad_od and all(bud[d] == want_b[d] for d in od),
          overdue=od, not_past_due=bad_od, budget_wrong=[d for d in od if bud[d] != want_b[d]])
    # top vs renewals overlap (the same card twice in one brief)
    ov = []
    for r in recs:
        b = r["ready"] or r["brief"] or {}
        ids = [c["card_id"] for s in ("top", "overdue", "renewals") for c in b.get(s, [])]
        if len(ids) != len(set(ids)):
            ov.append((r["date"], [c["title"][:40] for c in b.get("top", []) if c["card_id"] in {x["card_id"] for x in b.get("renewals", [])}]))
    check("acc.brief_sections_disjoint", not ov, overlaps=ov)
    # flight: Nov 3 at 19:00 NY, ends 07:00 London (02:00 NY); Nov 4 London morning: still in the air until 07:00
    fl = [(r["date"], m["start"], m.get("end")) for r in recs for m in (r["ready"] or r["brief"] or {}).get("meetings", []) if "Flight" in m["title"]]
    good_end = any(e and ts(e) == datetime(2026, 11, 4, 7, 0, tzinfo=ZoneInfo(LDN)) for _, _, e in fl)
    check("acc.redeye_flight_times", bool(fl) and good_end, flight=fl,
          want_end="2026-11-04T07:00:00+00:00 (= 2026-11-04T02:00:00-05:00)")


def eval_variant(recs, mode):
    rows = [{"date": r["date"], "fired_at_london": r["fired_local_real_tz"],
             "brief_date": (r["ready"] or {}).get("date"), "got": r["cmp_real"]["got"],
             "expected_london": r["cmp_real"]["expected"],
             "missing": r["cmp_real"]["missing"], "wrong_time": r["cmp_real"]["wrong_time"],
             "extra": r["cmp_real"]["extra"], "truth": [(x["check"].get("meeting") or x["check"].get("absent"), x["ok"], x["got"]) for x in r["truth"]]}
            for r in recs]
    ldn = [r for r in rows if r["date"] in ("2026-11-04", "2026-11-05", "2026-11-06", "2026-11-07")]
    at8 = [r for r in ldn if r["fired_at_london"] and r["fired_at_london"][11:16] == "08:00"]
    check(f"trip.{mode}.fires_0800_london", len(at8) == len(ldn), ok=len(at8), n=len(ldn), rows=ldn)
    okm = [r for r in ldn if not (r["missing"] or r["wrong_time"] or r["extra"])]
    check(f"trip.{mode}.meetings_in_london_time", len(okm) == len(ldn), ok=len(okm), n=len(ldn))
    if mode == "explicit":
        back = [r for r in rows if r["date"] == "2026-11-08"]
        check("trip.explicit.after_trip_offset_left_on", bool(back) and back[0]["fired_at_london"] is None, back=back,
              note="utc_offset +00:00 left in config after flying home: started 02:59:45 EST; a brief at 03:00 EST is wrong")


def part_timing(out: Path):
    """Once a day, restart, next day, brief_time changes, catch-up, empty, disabled."""
    print("== timing", flush=True)
    w = out / "timing"
    root = w / "maya"
    root.mkdir(parents=True, exist_ok=True)
    copy_until(root, datetime(2026, 10, 27, 7, 59, tzinfo=ZoneInfo(NY)))
    home = home_for(w / "home")
    ev = {}
    # 1) fires once at 08:00, stays quiet until 08:02
    r1, src = morning(home, root, datetime(2026, 10, 27, 7, 59, 45, tzinfo=ZoneInfo(NY)), NY, wait=60,
                      after=lambda d: (d.wait_note("brief.ready", timeout=60), None)[1])
    ev["day1"] = {"ready_at": r1["ready"] and r1["ready"]["generated_at"], "more": r1["more_ready"]}
    check("time.fires_once_at_brief_time", bool(r1["ready"]) and not r1["more_ready"]
          and loc(r1["ready"]["generated_at"], NY).strftime("%H:%M") == "08:00", **ev["day1"])
    # 2) restart the same day at 08:10: no second brief (store mark)
    r2, _ = morning(home, root, datetime(2026, 10, 27, 8, 10, tzinfo=ZoneInfo(NY)), NY, wait=60, source=src)
    check("time.restart_same_day_no_second_brief", r2["ready"] is None and not r2["more_ready"],
          ready=r2["ready"] and r2["ready"]["generated_at"])
    # 3) restart after midnight-but-before-brief (00:30 next day): nothing until 08:00
    r3, _ = morning(home, root, datetime(2026, 10, 28, 0, 30, tzinfo=ZoneInfo(NY)), NY, wait=40, source=src)
    check("time.after_midnight_waits_for_brief_time", r3["ready"] is None, ready=r3["ready"] and r3["ready"]["generated_at"])
    # 4) next day 07:59:45: fires
    r4, _ = morning(home, root, datetime(2026, 10, 28, 7, 59, 45, tzinfo=ZoneInfo(NY)), NY, wait=60, source=src)
    check("time.fires_next_day", bool(r4["ready"]) and r4["ready"]["date"] == "2026-10-28",
          ready=r4["ready"] and (r4["ready"]["date"], r4["ready"]["generated_at"]))
    # 5) brief_time 09:15: nothing at 08:00, fires at 09:15 (x30 from 07:58)
    cfg = (home.config / "config.toml").read_text()
    home.write_config(cfg.replace('brief_time = "08:00"', 'brief_time = "09:15"'))
    st = datetime(2026, 10, 29, 7, 58, tzinfo=ZoneInfo(NY))
    rec = {}
    with SpeedDaemon(home, sim=st, tz=NY, env=speed_env(st, 30), log_name="speed-bt") as d:
        n = d.wait_note("brief.ready", timeout=200)
        rec["first"] = n and n["params"]["generated_at"]
        rec["now_after"] = d.call("status", {}) and None
    check("time.brief_time_change_0915", rec["first"] is not None and loc(rec["first"], NY).strftime("%H:%M") == "09:15",
          fired=rec["first"] and loc(rec["first"], NY).isoformat())
    # 6) brief_time changed to later the same day after it already fired: no second brief
    home.write_config(cfg.replace('brief_time = "08:00"', 'brief_time = "18:00"'))
    r6, _ = morning(home, root, datetime(2026, 10, 29, 17, 59, 45, tzinfo=ZoneInfo(NY)), NY, wait=45, source=src)
    check("time.brief_time_moved_later_same_day_no_repeat", r6["ready"] is None,
          ready=r6["ready"] and r6["ready"]["generated_at"])
    # 7) catch-up: machine off at 08:00, daemon started 22:30 the same day
    home.write_config(cfg)
    r7, _ = morning(home, root, datetime(2026, 10, 30, 22, 30, tzinfo=ZoneInfo(NY)), NY, wait=40, source=src)
    check("time.catch_up_late_start", True, info="observation", fired=r7["ready"] and loc(r7["ready"]["generated_at"], NY).isoformat(),
          note="a brief that missed 08:00 fires at start, whatever the hour")
    REPORT["time.catch_up_late_start"]["ok"] = r7["ready"] is not None
    # 8) brief_time single digit hour '7:05' accepted
    home.write_config(cfg.replace('brief_time = "08:00"', 'brief_time = "7:05"'))
    r8, _ = morning(home, root, datetime(2026, 10, 31, 7, 4, 45, tzinfo=ZoneInfo(NY)), NY, wait=45, source=src)
    check("time.brief_time_H_MM_accepted", bool(r8["ready"]) and loc(r8["ready"]["generated_at"], NY).strftime("%H:%M") == "07:05",
          fired=r8["ready"] and r8["ready"]["generated_at"])
    home.write_config(cfg)

    # 9) empty home: no brief.ready (by design), mark set; data arriving later that day -> no brief
    he = home_for(w / "empty")
    re_, _ = morning(he, w / "nothing", datetime(2026, 10, 27, 7, 59, 45, tzinfo=ZoneInfo(NY)), NY, wait=45,
                     source="none")
    check("time.empty_brief_not_announced", re_["ready"] is None, brief=re_["brief"])
    root2 = w / "late"
    root2.mkdir(exist_ok=True)
    copy_until(root2, datetime(2026, 10, 27, 7, 59, tzinfo=ZoneInfo(NY)))
    with Daemon(he, sim=datetime(2026, 10, 27, 9, 0, tzinfo=ZoneInfo(NY)), tz=NY, log_name="late") as d:
        s = d.add_files_source(root2)["id"]
        d.sync(s)
        n = d.wait_note("brief.ready", timeout=45)
        b = d.brief()
    check("time.first_data_same_day_gets_a_brief", n is not None,
          ready=n and n["params"]["generated_at"], brief_get_meetings=len(b["meetings"]), brief_get_top=len(b["top"]),
          note="first brief of a fresh install was 'empty' at 08:00 and marked as fired; data synced at 09:00")
    # 10) enabled = false
    hd = home_for(w / "disabled")
    hd.write_config((hd.config / "config.toml").read_text().replace("[proactive]\nenabled = true", "[proactive]\nenabled = false"))
    rd, _ = morning(hd, root, datetime(2026, 10, 28, 7, 59, 45, tzinfo=ZoneInfo(NY)), NY, wait=40)
    check("time.disabled_no_brief", rd["ready"] is None and not rd["brief"]["meetings"] and not rd["brief"]["top"],
          ready=rd["ready"], brief=rd["brief"], cards=len(rd["cards_all"]))


def part_runover(out: Path):
    """A daemon running through a night (x600): one brief per day at 08:00."""
    print("== run over two nights (x600)", flush=True)
    w = out / "runover"
    root = w / "maya"
    root.mkdir(parents=True, exist_ok=True)
    st = datetime(2026, 10, 27, 7, 50, tzinfo=ZoneInfo(NY))
    copy_until(root, datetime(2026, 10, 29, 7, 0, tzinfo=ZoneInfo(NY)))
    home = home_for(w / "home")
    fires = []
    with SpeedDaemon(home, sim=st, tz=NY, env=speed_env(st, 600), log_name="runover") as d:
        s = d.add_files_source(root)["id"]
        d.sync(s)
        t0 = time.time()
        # 50 simulated hours = 300 real seconds
        while time.time() - t0 < 300:
            n = d.wait_note("brief.ready", timeout=5)
            if n:
                fires.append(n["params"]["generated_at"])
    loc_f = [loc(f, NY).strftime("%Y-%m-%d %H:%M:%S") for f in fires]
    days = [f[:10] for f in loc_f]
    ok = days == ["2026-10-27", "2026-10-28", "2026-10-29"] and all(f[11:16] in ("08:00", "08:01") for f in loc_f)
    check("time.runs_through_midnight_one_per_day", ok, fired_local=loc_f,
          want="2026-10-27/28/29 at 08:00 EDT, once each")


def part_dst(out: Path):
    """Daemon running from Sat Oct 31 20:00 EDT through the fall-back to Mon Nov 2 08:30 EST (x600)."""
    print("== DST run (x600)", flush=True)
    w = out / "dst"
    root = w / "maya"
    root.mkdir(parents=True, exist_ok=True)
    st = datetime(2026, 10, 31, 20, 0, tzinfo=ZoneInfo(NY))
    copy_until(root, st)
    home = home_for(w / "home")
    # Fresh install: Saturday's brief has not fired; let it fire on the real clock first.
    r0, src = morning(home, root, datetime(2026, 10, 31, 7, 59, 45, tzinfo=ZoneInfo(NY)), NY, wait=60)
    fires, snooze = [], {}
    with SpeedDaemon(home, sim=st, tz=NY, env=speed_env(st, 600), log_name="dst") as d:
        cards = d.cards()["cards"]
        tgt = next((c for c in cards if c["kind"] != "renewal"), None)
        if tgt:
            v = d.feedback(tgt["id"], "snooze")
            snooze = {"card": tgt["id"], "title": tgt["title"], "snoozed_until": v.get("snoozed_until"),
                      "at": d.call("status", {}) and None}
        t0 = time.time()
        woke = None
        while time.time() - t0 < 230:  # ~38 simulated hours
            n = d.wait_note("brief.ready", timeout=3)
            if n:
                fires.append(n["params"]["generated_at"])
                if tgt and woke is None:
                    cs = {c["id"]: c for c in d.cards(include_closed=True)["cards"]}
                    woke = cs.get(tgt["id"], {}).get("state")
            if len(fires) >= 2:
                break
        snooze["state_at_sunday_brief"] = woke
    loc_f = [loc(f, NY).isoformat() for f in fires]
    sun = [f for f in loc_f if f.startswith("2026-11-01")]
    mon = [f for f in loc_f if f.startswith("2026-11-02")]
    check("dst.sunday_brief_0800_EST", len(sun) == 1 and sun[0][11:16] == "08:00" and sun[0].endswith("-05:00"),
          fired_local=loc_f, sat_fired=r0["ready"] and loc(r0["ready"]["generated_at"], NY).isoformat())
    check("dst.monday_brief_0800_EST", len(mon) == 1 and mon[0][11:16] == "08:00", fired_local=loc_f)
    su = snooze.get("snoozed_until")
    want = datetime(2026, 11, 1, 8, 0, tzinfo=ZoneInfo(NY))
    check("dst.snooze_default_tomorrow_at_brief_time", bool(su) and ts(su) == want, snooze=snooze,
          snoozed_until_local=su and loc(su, NY).isoformat(), want=want.isoformat())
    # Reminders computed before the change for dates after it (renewal reminder, renewal due).
    acts = []
    for c in r0["cards_all"]:
        for a in c["actions"]:
            if a["kind"] == "reminder":
                acts.append((c["title"][:50], a["label"]))
        if c["kind"] == "renewal":
            acts.append((c["title"][:50], "due " + str(c["due"])))
    rem = [x for x in acts if "2026-11-0" in x[1] and "-04:00" in x[1]]
    check("dst.future_times_use_future_offset", not rem, edt_offsets_after_change=rem,
          note="09:00 on Nov 2/3 written as -04:00 is 08:00 EST")
    # Brief time at 01:30 on the fall-back night: 01:30 happens twice.
    w2 = out / "dst-0130"
    h2 = home_for(w2 / "home", brief_time="01:30")
    st2 = datetime(2026, 10, 31, 23, 0, tzinfo=ZoneInfo(NY))
    f2 = []
    with SpeedDaemon(h2, sim=st2, tz=NY, env=speed_env(st2, 300), log_name="dst0130") as d:
        s = d.add_files_source(root)["id"]
        d.sync(s)
        t0 = time.time()
        while time.time() - t0 < 60:  # 5 simulated hours
            n = d.wait_note("brief.ready", timeout=3)
            if n:
                f2.append(loc(n["params"]["generated_at"], NY).isoformat())
    nov1 = [f for f in f2 if f.startswith("2026-11-01")]
    check("dst.brief_at_0130_fires_once_on_fallback_night", len(nov1) == 1, fired=f2)
    # Zone change while running: OS zone file swapped NY -> London on the flight (x600).
    w3 = out / "zone-swap"
    root3 = w3 / "maya"
    root3.mkdir(parents=True, exist_ok=True)
    st3 = datetime(2026, 11, 3, 18, 0, tzinfo=ZoneInfo(NY))
    copy_until(root3, st3)
    h3 = home_for(w3 / "home")
    link = w3 / "localtime"
    zi = Path("/usr/share/zoneinfo")
    link.unlink(missing_ok=True)
    link.symlink_to(zi / NY)
    f3 = []
    with SpeedDaemon(h3, sim=st3, tz=f":{link}", env=speed_env(st3, 600), log_name="zoneswap") as d:
        s = d.add_files_source(root3)["id"]
        d.sync(s)
        # Tuesday's brief already fired? It is 18:00 on a fresh install: it catches up now.
        n = d.wait_note("brief.ready", timeout=30)
        catch = n and n["params"]["generated_at"]
        t0 = time.time()
        swapped = False
        while time.time() - t0 < 140:  # ~23 simulated hours
            el = (time.time() - t0) * 600
            if not swapped and el > 5 * 3600:  # ~23:00 NY: she lands in London's timezone mentally
                tmp = w3 / "localtime.new"
                tmp.unlink(missing_ok=True)
                tmp.symlink_to(zi / LDN)
                os.replace(tmp, link)
                swapped = True
            n = d.wait_note("brief.ready", timeout=2)
            if n:
                f3.append(n["params"])
                break
    got = f3[0] if f3 else None
    check("trip.zone_change_while_running_fires_0800_london",
          bool(got) and loc(got["generated_at"], LDN).strftime("%Y-%m-%d %H:%M") == "2026-11-04 08:00" and got["date"] == "2026-11-04",
          catch_up=catch and loc(catch, NY).isoformat(),
          fired=got and (got["date"], loc(got["generated_at"], LDN).isoformat()),
          meetings=got and [(m["title"], m["start"], m.get("end")) for m in got["meetings"]])


def part_config(out: Path):
    """Invalid [proactive] brief_time / utc_offset: what does the user see?"""
    print("== config errors", flush=True)
    w = out / "config"
    cases = {
        "brief_time=25:00": ('brief_time = "08:00"', 'brief_time = "25:00"'),
        "brief_time=8am": ('brief_time = "08:00"', 'brief_time = "8am"'),
        "brief_time=08:00:00": ('brief_time = "08:00"', 'brief_time = "08:00:00"'),
        "brief_time=empty": ('brief_time = "08:00"', 'brief_time = ""'),
        "brief_time=int": ('brief_time = "08:00"', "brief_time = 8"),
        "utc_offset=EST": ("[proactive]\n", '[proactive]\nutc_offset = "EST"\n'),
        "utc_offset=America/New_York": ("[proactive]\n", '[proactive]\nutc_offset = "America/New_York"\n'),
        "utc_offset=+5": ("[proactive]\n", '[proactive]\nutc_offset = "+5"\n'),
        "utc_offset=+14:00(valid)": ("[proactive]\n", '[proactive]\nutc_offset = "+14:00"\n'),
        "utc_offset=-0500(valid)": ("[proactive]\n", '[proactive]\nutc_offset = "-0500"\n'),
        "timezone=(unknown key)": ("[proactive]\n", '[proactive]\ntimezone = "America/New_York"\n'),
    }
    res = {}
    for name, (a, b) in cases.items():
        h = home_for(w / name.replace("/", "_").replace(":", "").replace("(", "").replace(")", ""))
        h.write_config((h.config / "config.toml").read_text().replace(a, b, 1))
        r = {}
        try:
            with Daemon(h, sim=None, tz=NY, log_name="cfg") as d:
                r["status"] = d.call("status", {})
                _, e = d.try_call("brief.get", {})
                r["brief.get"] = str(e) if e else "ok"
                _, e = d.try_call("cards.list", {})
                r["cards.list"] = str(e) if e else "ok"
            r["exit"] = "running"
        except RuntimeError as e:
            r["exit"] = str(e)[:200]
        logs = sorted((h.data / "logs").glob("*.log")) if (h.data / "logs").exists() else []
        txt = "".join(f.read_text(errors="replace") for f in logs)
        r["log"] = [json.loads(l)["fields"] for l in txt.splitlines()
                    if l.startswith("{") and ("did not start" in l or "[proactive]" in l)][:3]
        res[name] = r
        print(f"  {name}: {r['exit']} brief.get={r['brief.get'] if 'brief.get' in r else '-'} log={r['log'][:1]}", flush=True)
    bad = [k for k in res if "valid" not in k]
    refused = [k for k in bad if res[k]["exit"] != "running"]
    logged = [k for k in bad if any("[proactive]" in json.dumps(x) for x in res[k]["log"])]
    surfaced = [k for k in bad if "[proactive]" in json.dumps(res[k].get("status", {})) or "[proactive]" in res[k].get("brief.get", "")]
    check("config.invalid_values_logged_with_section", len(logged) == len(bad), logged=logged,
          not_logged=[k for k in bad if k not in logged])
    check("config.invalid_values_visible_to_client", len(surfaced) == len(bad), surfaced=surfaced,
          example={k: {"status": res[k].get("status"), "brief.get": res[k].get("brief.get"), "cards.list": res[k].get("cards.list")} for k in bad[:1]})
    check("config.invalid_values_refused_at_start", len(refused) == len(bad), refused=refused,
          not_refused=[k for k in bad if k not in refused], detail=res)
    valid = [k for k in res if "(valid)" in k and res[k].get("brief.get") == "ok"]
    check("config.valid_offsets_accepted", len(valid) == 2, detail={k: res[k] for k in res if "(valid)" in k})


def part_edge(out: Path):
    """Synthetic edge events on Mon Nov 9 2026 (EST): UTC times near midnight, multi-day all-day,
    declined, overnight, tomorrow just after midnight."""
    print("== edge events", flush=True)
    w = out / "edge"
    cal = w / "maya" / "Calendar"
    cal.mkdir(parents=True, exist_ok=True)

    def ics(name, body, method="PUBLISH"):
        (cal / name).write_text("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//GG4 A4//EN\r\n"
                                f"METHOD:{method}\r\nBEGIN:VEVENT\r\nDTSTAMP:20261101T120000Z\r\n"
                                + "\r\n".join(body) + "\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n")
    me = f"ATTENDEE;CN=Maya Okafor;PARTSTAT=ACCEPTED:mailto:{USER}"
    ics("utc-evening.ics", ["UID:e1@a4", "SUMMARY:Call with Tokyo (UTC 23:00)", "DTSTART:20261109T230000Z", "DTEND:20261109T233000Z", me])
    ics("utc-next.ics", ["UID:e2@a4", "SUMMARY:Late call (UTC Nov 10 03:00)", "DTSTART:20261110T030000Z", "DTEND:20261110T040000Z", me])
    ics("utc-prev.ics", ["UID:e3@a4", "SUMMARY:Sunday night call (UTC Nov 9 03:00)", "DTSTART:20261109T030000Z", "DTEND:20261109T040000Z", me])
    ics("conf.ics", ["UID:e4@a4", "SUMMARY:Design conference (3 days)", "DTSTART;VALUE=DATE:20261108", "DTEND;VALUE=DATE:20261111"])
    ics("declined.ics", ["UID:e5@a4", "SUMMARY:Vendor pitch (declined)", "DTSTART;TZID=America/New_York:20261109T110000",
                         "DTEND;TZID=America/New_York:20261109T120000", "ORGANIZER:mailto:sales@vendor.example",
                         f"ATTENDEE;CN=Maya Okafor;PARTSTAT=DECLINED:mailto:{USER}"], "REQUEST")
    ics("overnight.ics", ["UID:e6@a4", "SUMMARY:Overnight deploy", "DTSTART;TZID=America/New_York:20261108T230000",
                          "DTEND;TZID=America/New_York:20261109T020000"])
    ics("tomorrow.ics", ["UID:e7@a4", "SUMMARY:Early tomorrow 00:30", "DTSTART;TZID=America/New_York:20261110T003000",
                         "DTEND;TZID=America/New_York:20261110T010000"])
    ics("lunch.ics", ["UID:e8@a4", "SUMMARY:Lunch with Priya", "DTSTART;TZID=America/New_York:20261109T123000",
                      "DTEND;TZID=America/New_York:20261109T133000", me, "ATTENDEE:mailto:priya@larkspur.example"])
    ics("la.ics", ["UID:e9@a4", "SUMMARY:Call with LA client (10:00 PT)", "DTSTART;TZID=America/Los_Angeles:20261109T100000",
                   "DTEND;TZID=America/Los_Angeles:20261109T110000", me])
    home = home_for(w / "home")
    r, _ = morning(home, w / "maya", datetime(2026, 11, 9, 7, 59, 45, tzinfo=ZoneInfo(NY)), NY, wait=60)
    b = r["ready"] or r["brief"]
    cmp_ = compare_meetings(b, cal, date(2026, 11, 9), NY)
    detail = {k: cmp_[k] for k in ("expected", "got", "missing", "wrong_time", "extra", "invented", "duplicates")}
    print(f"  edge: {detail}", flush=True)
    got = dict(cmp_["got"]) if cmp_["got"] else {}
    titles = [t for t, _ in cmp_["got"]]
    check("edge.utc_event_evening_today", got.get("Call with Tokyo (UTC 23:00)") == "18:00", got=cmp_["got"])
    check("edge.utc_event_next_utc_day_is_today_local", got.get("Late call (UTC Nov 10 03:00)") == "22:00", got=cmp_["got"])
    check("edge.utc_event_prev_local_day_absent", "Sunday night call (UTC Nov 9 03:00)" not in titles, got=cmp_["got"])
    check("edge.multi_day_all_day_shown_each_day", "Design conference (3 days)" in titles, got=cmp_["got"],
          note="all-day Nov 8-10; brief on Nov 9")
    check("edge.declined_absent", "Vendor pitch (declined)" not in titles, got=cmp_["got"])
    check("edge.tomorrow_after_midnight_absent", "Early tomorrow 00:30" not in titles, got=cmp_["got"])
    check("edge.other_zone_tzid_converted", got.get("Call with LA client (10:00 PT)") == "13:00", got=cmp_["got"])
    check("edge.overnight_in_progress_shown", "Overnight deploy" in titles, got=cmp_["got"],
          note="23:00 Sun to 02:00 Mon: optional (still running at midnight, over by the brief)")
    REPORT["edge.overnight_in_progress_shown"]["ok"] = True  # observation only
    (w / "result.json").write_text(json.dumps({"brief": b, "cmp": cmp_}, indent=1, default=str))


def part_snooze(out: Path):
    """Snooze default on an ordinary day (no DST): tomorrow 08:00 local."""
    print("== snooze default (ordinary day)", flush=True)
    w = out / "snooze"
    root = w / "maya"
    root.mkdir(parents=True, exist_ok=True)
    st = datetime(2026, 10, 29, 21, 0, tzinfo=ZoneInfo(NY))
    copy_until(root, st)
    home = home_for(w / "home")
    with Daemon(home, sim=st, tz=NY, log_name="snooze") as d:
        s = d.add_files_source(root)["id"]
        d.sync(s)
        d.refresh()
        c = d.cards()["cards"][0]
        v = d.feedback(c["id"], "snooze")
    su = v.get("snoozed_until")
    check("snooze.default_tomorrow_0800_ordinary_day", bool(su) and ts(su) == datetime(2026, 10, 30, 8, 0, tzinfo=ZoneInfo(NY)),
          snoozed_until=su)


PARTS = ["accuracy", "fixed", "explicit", "timing", "runover", "dst", "config", "edge", "snooze"]


def main():
    global ARGS
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--only", default=",".join(PARTS))
    ap.add_argument("--runtime", action="store_true")
    ap.add_argument("--chat-model")
    ap.add_argument("--embed-model")
    ap.add_argument("--models-home")
    ap.add_argument("--judge", default="off")
    ARGS = a = ap.parse_args()
    if a.runtime and a.judge == "off":
        a.judge = "local"
    out = Path(a.out).resolve()
    out.mkdir(parents=True, exist_ok=True)
    errors = {}
    only = a.only.split(",")
    for p in only:
        t0 = time.time()
        try:
            if p == "accuracy":
                eval_accuracy(part_accuracy(out, "os"))
            elif p == "fixed":
                eval_variant(part_accuracy(out, "fixed", (10, 13)), "fixed")
            elif p == "explicit":
                eval_variant(part_accuracy(out, "explicit", (10, 14)), "explicit")
            elif p == "timing":
                part_timing(out)
            elif p == "runover":
                part_runover(out)
            elif p == "dst":
                part_dst(out)
            elif p == "config":
                part_config(out)
            elif p == "edge":
                part_edge(out)
            elif p == "snooze":
                part_snooze(out)
        except Exception as e:  # keep going; record
            errors[p] = traceback.format_exc()
            print(f"  ERROR in {p}: {e}", flush=True)
        print(f"  ({p}: {time.time() - t0:.0f}s)", flush=True)
        prev = {}
        sp = out / "summary.json"
        if sp.exists():
            try:
                prev = json.loads(sp.read_text()).get("report", {})
            except ValueError:
                prev = {}
        prev.update(REPORT)
        sp.write_text(json.dumps({"report": prev, "errors": errors, "args": vars(a),
                                  "nuggetd": nuggetd_exe(), "faketime": faketime_lib()}, indent=1, default=str))
    rep = json.loads((out / "summary.json").read_text())["report"]
    print(f"\n{sum(v['ok'] for v in rep.values())}/{len(rep)} checks pass; errors: {list(errors)}")


if __name__ == "__main__":
    main()
