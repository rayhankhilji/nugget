#!/usr/bin/env python3
"""GG4 A3: card actions and their dry-run diffs, through the real daemon.

A small synthetic mailbox (written here, not the persona's) gives cards of every mail-based kind:
an unanswered question from a client (draft_reply + archive_thread), Maya's dated promise
(draft_reply + reminder), Maya's unanswered question (draft_reply + reminder), a plan renewal
(reminder), a dated to-do in a note (reminder + create_event) and an external meeting .ics
(meeting_prep -> research_note, if it fires). For every offered action the scenario previews it,
executes it with the preview's digest and compares the dry-run diff with what happened (the
.ics file, `reminders.list`, the vault note, the artifact). Then it tries to break the approval
binding (wrong / foreign / reused / post-restart / >1 h old digests, closed cards, an index the
card does not offer, concurrent executes, the kill switch) and checks confirmation rules,
learning, `reminder.due` timing and time zones (America/New_York, Europe/London).

Without a model, draft_reply is driven through a fake OpenAI-compatible server
(harness/fake_llm.py) and the no-model behaviour is recorded too. With `--runtime` (CI) the
real local model drafts and the fake server is not used.

    python3 qa/GG4/scenarios/actions.py --out OUT [--runtime --chat-model ID --embed-model ID --models-home DIR]

Writes OUT/summary.json {"report": {check: {"ok": bool, "evidence": ...}}} and OUT/*.json.
All data is synthetic.
"""
from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import re
import shutil
import socket
import sys
import threading
import time
from pathlib import Path
from zoneinfo import ZoneInfo

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "harness"))
from nq import Daemon, Home, local  # noqa: E402
from fake_llm import MARK, FakeLLM  # noqa: E402

ME = ("Maya Okafor", "maya@larkspur.example")
LENA = ("Lena Fischer", "lena@cobaltworks.example")
RAJ = ("Raj Patel", "raj@cobaltworks.example")
OWEN = ("Owen Hart", "owen@larkspur.example")
IRIS = ("Iris Chen", "iris@quarry-labs.example")
INK = ("Inkwell Studio", "billing@inkwell.example")
BEA = ("Bea Novak", "bea@harbourline.example")


# ---------------------------------------------------------------- corpus

def eml(mid, at, frm, to, subject, body, cc=(), reply_to=None):
    h = [f"From: {frm[0]} <{frm[1]}>", "To: " + ", ".join(f"{n} <{e}>" for n, e in to)]
    if cc:
        h.append("Cc: " + ", ".join(f"{n} <{e}>" for n, e in cc))
    h += [f"Subject: {subject}", f"Date: {at.strftime('%a, %d %b %Y %H:%M:%S %z')}",
          f"Message-ID: <{mid}@mail.a3.example>"]
    if reply_to:
        h += [f"In-Reply-To: <{reply_to}@mail.a3.example>", f"References: <{reply_to}@mail.a3.example>"]
    h += ["MIME-Version: 1.0", "Content-Type: text/plain; charset=utf-8"]
    return "\r\n".join(h) + "\r\n\r\n" + body.strip().replace("\n", "\r\n") + "\r\n"


def write_corpus(root: Path, now: dt.datetime, tz: str):
    """Files relative to `now` (aware). Returns a dict of what each scenario expects."""
    z = ZoneInfo(tz)
    d = lambda days, h, m=0: (now - dt.timedelta(days=days)).astimezone(z).replace(hour=h, minute=m, second=0, microsecond=0)  # noqa: E731
    files = {}
    # U1: a client's question, 2 days old, unanswered -> unanswered_thread
    files["Mail/Inbox/u1-lena-ask.eml"] = eml(
        "u1-lena-ask", d(2, 10, 15), LENA, [ME], "Packaging dieline questions",
        "Hi Maya,\n\nCould you confirm the bleed size for the carton dieline? Our printer needs it "
        "before they can quote.\n\nThanks,\nLena", cc=[RAJ])
    # F1: Maya promised something by Friday (dated) -> follow_up (draft_reply + reminder)
    files["Mail/Inbox/f1-owen-ask.eml"] = eml(
        "f1-owen-ask", d(2, 10, 40), OWEN, [ME], "Vendor shortlist",
        "Hey Maya, can you pull together the vendor shortlist for the print run?\nOwen")
    fri = now.astimezone(z)
    while fri.weekday() != 4:
        fri += dt.timedelta(days=1)
    files["Mail/Sent/f1-maya-promise.eml"] = eml(
        "f1-maya-promise", d(2, 11, 5), ME, [OWEN], "Re: Vendor shortlist",
        "Sure, I'll send the vendor shortlist by Friday.\nMaya", reply_to="f1-owen-ask")
    # F2: Maya asked Iris something 4 days ago, no answer -> follow_up (awaiting reply)
    files["Mail/Sent/f2-maya-question.eml"] = eml(
        "f2-maya-question", d(4, 15, 30), ME, [IRIS], "Usability study results",
        "Hi Iris,\n\nCould you share the usability study results from last week's sessions? "
        "I'd like to fold them into the checkout redesign.\n\nThanks,\nMaya")
    # R1: renewal notice -> renewal (reminder)
    renew = (now + dt.timedelta(days=5)).astimezone(z)
    files["Mail/Inbox/r1-inkwell-renewal.eml"] = eml(
        "r1-inkwell-renewal", d(1, 6, 0), INK, [ME], "Your Inkwell Studio plan renews soon",
        f"Hi Maya,\n\nYour Inkwell Studio Team plan renews on {renew.strftime('%B')} {renew.day}, "
        f"{renew.year} for $96.00.\nTo change your plan, visit your account settings.\n\nInkwell Studio")
    files["Mail/Inbox/r1-inkwell-receipt.eml"] = eml(
        "r1-inkwell-receipt", d(25, 6, 0), INK, [ME], "Your Inkwell Studio receipt",
        "Thanks for your payment of $96.00 for Inkwell Studio Team (monthly).\nReceipt #INK-20931\n\nInkwell Studio")
    # T1: a dated to-do in a note -> commitment (reminder + create_event)
    due = (now + dt.timedelta(days=1)).astimezone(z).date()
    files["Documents/todo.md"] = (
        "# To do\n\n"
        f"- [ ] Submit the October expense report (due {due.isoformat()})\n"
        "- [x] Order new sketchbooks\n")
    # M1: an external meeting tomorrow afternoon -> meeting_prep (research_note), if it fires
    ms = (now + dt.timedelta(days=1)).astimezone(z).replace(hour=14, minute=0, second=0, microsecond=0)
    me_ = ms + dt.timedelta(hours=1)
    u = lambda t: t.astimezone(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")  # noqa: E731
    files["Calendar/m1-harbourline.ics"] = "\r\n".join([
        "BEGIN:VCALENDAR", "VERSION:2.0", "PRODID:-//GG4 A3//EN", "BEGIN:VEVENT",
        "UID:m1-harbourline@a3.example", f"DTSTAMP:{u(now)}", f"DTSTART:{u(ms)}", f"DTEND:{u(me_)}",
        "SUMMARY:Harbourline brand review", "LOCATION:Video call",
        f"ORGANIZER;CN=Bea Novak:mailto:{BEA[1]}",
        f"ATTENDEE;CN=Bea Novak:mailto:{BEA[1]}", f"ATTENDEE;CN=Maya Okafor:mailto:{ME[1]}",
        "END:VEVENT", "END:VCALENDAR", ""])
    for p, t in files.items():
        f = root / p
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_text(t)
        # The file's mtime is its date for notes: keep it in the simulated past (a real-clock
        # mtime would be in the simulated future and the note ignored).
        ts = (now - dt.timedelta(days=1)).timestamp()
        os.utime(f, (ts, ts))
    return {"friday": fri.date().isoformat(), "renew": renew.date().isoformat(), "todo_due": due.isoformat(),
            "meeting_start": ms.isoformat()}


# ---------------------------------------------------------------- helpers

class Report:
    def __init__(self):
        self.r = {}

    def add(self, name, ok, **evidence):
        prev = self.r.get(name)
        if prev:  # repeated check: count runs
            prev["n"] += 1
            prev["passes"] += int(bool(ok))
            prev["ok"] = prev["ok"] and bool(ok)
            prev.setdefault("more", []).append(evidence)
        else:
            self.r[name] = {"ok": bool(ok), "n": 1, "passes": int(bool(ok)), "evidence": evidence}
        print(f"  [{'PASS' if ok else 'FAIL'}] {name}", flush=True)


def card_of(cards, kind=None, text=None):
    for c in cards:
        if (kind is None or c["kind"] == kind) and (text is None or text.lower() in (c["title"] + " " + c["detail"]).lower()):
            return c
    return None


def action_idx(card, kind):
    for a in card.get("actions", []):
        if a["kind"] == kind:
            return a["index"]
    return None


def err(e):
    return None if e is None else {"code": e.code, "message": e.message}


def setup(d, root):
    src = d.add_files_source(root)["id"]
    d.sync(src)
    d.drain(2)
    d.call("proactive.refresh", {}, timeout=900)
    return src


def open_cards(d):
    return d.call("cards.list", {"include_closed": False})["cards"]


def all_cards(d):
    return d.call("cards.list", {"include_closed": True})["cards"]


def ics_fields(text):
    out = {}
    unf = re.sub(r"\r\n[ \t]", "", text)
    for line in unf.split("\r\n"):
        if ":" in line:
            k, v = line.split(":", 1)
            out.setdefault(k.split(";")[0], []).append((k, v))
    return out


def parse_rfc3339(s):
    return dt.datetime.fromisoformat(s.replace("Z", "+00:00"))


def ics_time(v):
    return dt.datetime.strptime(v, "%Y%m%dT%H%M%SZ").replace(tzinfo=dt.timezone.utc)


def make_home(path, a, fake=None, judge="off", extra=""):
    home = Home(path, runtime=a.runtime, chat_model=a.chat_model, embed_model=a.embed_model,
                judge=judge, models_home=a.models_home)
    cfg = (home.config / "config.toml").read_text()
    if fake is not None:
        cfg = fake.provider_toml() + "\n" + cfg
    if extra:
        cfg += "\n" + extra
    home.write_config(cfg)
    return home


class FtDaemon(Daemon):
    """A daemon whose fake clock can jump: libfaketime reads FAKETIME_TIMESTAMP_FILE on every
    call (FAKETIME_NO_CACHE), so `jump(t)` moves the daemon's wall clock to `t` (local time in its
    zone) and lets it tick on from there. Monotonic time stays real."""

    def __init__(self, home, sim, tz, ft_path, **kw):
        self.ft = Path(ft_path)
        super().__init__(home, sim=sim, tz=tz, **kw)
        self.jump(self.sim)

    def jump(self, t):
        # libfaketime's "@T" from the file means T + (time since the process started), also
        # after a re-read, so subtract the daemon's uptime to land on `t`.
        loc = t.astimezone(ZoneInfo(self.tz))
        up = time.time() - self.t_start if getattr(self, "t_start", None) else 0.0
        self.ft.write_text("@" + (loc - dt.timedelta(seconds=up)).strftime("%Y-%m-%d %H:%M:%S") + "\n")
        self.jumped = (loc, time.time())

    def start(self):
        self.t_start = time.time()
        self.jump(self.sim)
        return super().start()

    def sim_now(self):
        loc, t0 = self.jumped
        return loc + dt.timedelta(seconds=time.time() - t0)

    def env(self):
        e = super().env()
        e.pop("FAKETIME", None)
        e.pop("FAKETIME_FMT", None)
        e.update(FAKETIME_TIMESTAMP_FILE=str(self.ft), FAKETIME_NO_CACHE="1")
        return e


def raw_call(sock_path, method, params, timeout=120):
    """One request on a fresh connection (for concurrent calls)."""
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    s.connect(str(sock_path))
    s.sendall(json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode() + b"\n")
    buf = b""
    while True:
        while b"\n" not in buf:
            ch = s.recv(1 << 20)
            if not ch:
                s.close()
                return {"error": {"message": "closed"}}
            buf += ch
        line, buf = buf.split(b"\n", 1)
        m = json.loads(line)
        if m.get("id") == 1:
            s.close()
            return m


def diff_texts(p, op="add"):
    return [l["text"] for l in p["diff"] if l["op"] == op]


def parse_mailto(url):
    from urllib.parse import parse_qs, unquote, urlsplit
    u = urlsplit(url)
    q = {k: v[0] for k, v in parse_qs(u.query, keep_blank_values=True).items()}
    return {"to": [unquote(x) for x in u.path.split(",") if x], "cc": [x for x in q.get("cc", "").split(",") if x],
            "subject": q.get("subject"), "body": (q.get("body") or "").replace("\r\n", "\n")}


def card_state(d, cid):
    for c in all_cards(d):
        if c["id"] == cid:
            return c["state"]
    return None


def exercise(d, home, card, idx, tz, rep, tag, fake_used):
    """Preview action `idx` of `card`, execute it (unconfirmed first), compare with the effect."""
    act = card["actions"][idx]
    kind = act["kind"]
    rec = {"card": card["id"], "card_kind": card["kind"], "title": card["title"], "action": act}
    p, e = d.try_call("cards.preview", {"card_id": card["id"], "action": idx}, timeout=300)
    rec["preview"], rec["preview_error"] = p, err(e)
    if p is None:
        rep.add(f"{tag}:{kind}:preview", False, rec=rec)
        return rec
    rep.add(f"{tag}:{kind}:label_matches_preview_summary", act["label"] == p["summary"] and act["risk"] == p["risk"],
            label=act["label"], summary=p["summary"], card_risk=act["risk"], preview_risk=p["risk"])
    before_rem = d.call("reminders.list", {})
    events_dir = home.data / "proactive" / "events"
    before_ics = set(events_dir.glob("*.ics")) if events_dir.exists() else set()
    r1, e1 = d.try_call("cards.execute", {"card_id": card["id"], "action": idx, "digest": p["digest"], "confirmed": False})
    rec["exec_unconfirmed"], rec["exec_unconfirmed_error"] = r1, err(e1)
    final, fe = r1, e1
    if r1 and r1["outcome"] == "needs_confirmation":
        # nothing may have happened yet
        same = d.call("reminders.list", {}) == before_rem and (set(events_dir.glob("*.ics")) if events_dir.exists() else set()) == before_ics
        rep.add(f"{tag}:{kind}:needs_confirmation_changes_nothing", same and card_state(d, card["id"]) in ("new", "seen"),
                reason=r1.get("detail"), state=card_state(d, card["id"]))
        final, fe = d.try_call("cards.execute", {"card_id": card["id"], "action": idx, "digest": p["digest"], "confirmed": True})
        rec["exec_confirmed"], rec["exec_confirmed_error"] = final, err(fe)
    rec["confirmation_asked"] = bool(r1 and r1["outcome"] == "needs_confirmation")
    rec["state_after"] = card_state(d, card["id"])
    executed = bool(final and final["outcome"] == "executed")
    rec["executed"] = executed
    art = (final or {}).get("artifact") or {}
    adds = diff_texts(p)
    if kind == "reminder":
        m = re.match(r"Reminder at (\S+): (.*)$", adds[0]) if adds else None
        want_at, want_text = (m.group(1), m.group(2)) if m else (None, None)
        after = d.call("reminders.list", {})
        new = [x for x in after if x not in before_rem]
        ok = (executed and len(new) == 1 and want_at is not None and parse_rfc3339(new[0]["at"]) == parse_rfc3339(want_at)
              and new[0]["text"] == want_text and art.get("kind") == "reminder"
              and parse_rfc3339(art["at"]) == parse_rfc3339(want_at) and art.get("text") == want_text)
        rec["reminder_stored"] = new
        rep.add(f"{tag}:reminder:stored_matches_diff", ok, diff=adds, stored=new, artifact=art)
        rep.add(f"{tag}:reminder:stored_has_card_id", bool(new) and new[0].get("card_id") == card["id"],
                stored_card_id=new[0].get("card_id") if new else None, card=card["id"])
        loc = parse_rfc3339(want_at).astimezone(ZoneInfo(tz)) if want_at else None
        rec["at_local"] = loc.isoformat() if loc else None
        rep.add(f"{tag}:reminder:label_readable", bool(want_at) and want_at.endswith(("Z", "+00:00")) is False and "." not in want_at,
                label=p["summary"], at=want_at, at_local=rec["at_local"])
        rep.add(f"{tag}:reminder:offset_is_local_offset_at_that_instant",
                bool(loc) and parse_rfc3339(want_at).utcoffset() == loc.utcoffset(),
                at=want_at, local=rec["at_local"], tz=tz)
    elif kind == "create_event":
        title = next((t[len("Event: "):] for t in adds if t.startswith("Event: ")), None)
        when = next((t[len("When: "):] for t in adds if t.startswith("When: ")), "")
        s, _, en = when.partition(" – ")
        new = sorted(set(events_dir.glob("*.ics")) - before_ics) if events_dir.exists() else []
        rec["ics_files"] = [str(x) for x in new]
        ok = executed and len(new) == 1 and art.get("kind") == "file" and art.get("path") == str(new[0])
        f = {}
        if new:
            text = new[0].read_bytes().decode()
            rec["ics_text"] = text
            f = ics_fields(text)
            ds, de = f.get("DTSTART", [[None, None]])[0][1], f.get("DTEND", [[None, None]])[0][1]
            ok = ok and f.get("SUMMARY", [[0, None]])[0][1] == title and ds and ics_time(ds) == parse_rfc3339(s) \
                and de and ics_time(de) == parse_rfc3339(en) and "ATTENDEE" not in f
            desc_lines = [t for t in adds if not t.startswith(("Event: ", "When: ", "Invite: "))]
            got_desc = f.get("DESCRIPTION", [[0, ""]])[0][1].replace("\\n", "\n").replace("\\,", ",").replace("\\;", ";")
            ok = ok and got_desc == "\n".join(desc_lines)
            rec["ics_local_start"] = ics_time(ds).astimezone(ZoneInfo(tz)).isoformat() if ds else None
        rep.add(f"{tag}:create_event:ics_matches_diff", ok, diff=adds, artifact=art, ics=rec.get("ics_text"))
        rep.add(f"{tag}:create_event:diff_times_whole_minutes", bool(s) and parse_rfc3339(s).second == 0 and "." not in s,
                when=when, local_start=rec.get("ics_local_start"))
    elif kind == "draft_reply":
        to = next((t[4:] for t in adds if t.startswith("To: ")), "")
        cc = next((t[4:] for t in adds if t.startswith("Cc: ")), "")
        subj = next((t[9:] for t in adds if t.startswith("Subject: ")), None)
        i = adds.index("") if "" in adds else len(adds)
        body = "\n".join(adds[i + 1:])
        mt = parse_mailto(art["url"]) if art.get("url") else {}
        rec["mailto"] = mt
        ok = (executed and art.get("kind") == "draft" and art.get("text") == body and mt.get("body") == body
              and mt.get("to") == [x for x in to.split(", ") if x] and mt.get("cc") == [x for x in cc.split(", ") if x]
              and mt.get("subject") == subj)
        rep.add(f"{tag}:draft_reply:artifact_matches_diff", ok, diff=adds, artifact=art, mailto=mt)
        rep.add(f"{tag}:draft_reply:nothing_sent_risk_edit", p["risk"] == "edit" and p["target"] == "mail:compose"
                and any("Nothing is sent" in l["text"] for l in p["diff"]), risk=p["risk"], target=p["target"])
        if fake_used:
            rep.add(f"{tag}:draft_reply:body_from_model", MARK in body, body=body)
    elif kind == "archive_thread":
        rec["archive"] = {"supported": act["supported"], "unconfirmed": r1, "confirmed": final, "confirmed_error": err(fe)}
    if kind != "archive_thread":
        rep.add(f"{tag}:{kind}:executed", executed, outcome=final, error=err(fe))
        rep.add(f"{tag}:{kind}:card_done_after_execute", rec["state_after"] == "done", state=rec["state_after"])
    return rec


# ---------------------------------------------------------------- phases

def phase_inventory_and_execute(a, out, rep, tz, sim, tag, fake):
    """Every offered action: preview, execute, compare. Executing closes a card, so variant v
    executes action v of each card (or its last), in a fresh home each."""
    recs = []
    covered = set()
    inventory = None
    for v in (0, 1):
        work = out / f"{tag}-v{v}"
        shutil.rmtree(work, ignore_errors=True)
        root = work / "maya"
        now = local(sim, tz)
        exp = write_corpus(root, now, tz)
        home = make_home(work / "home", a, fake=fake)
        with Daemon(home, sim=now, tz=tz) as d:
            setup(d, root)
            cards = open_cards(d)
            if inventory is None:
                inventory = [{"id": c["id"], "kind": c["kind"], "title": c["title"],
                              "actions": c["actions"]} for c in cards]
                kinds = {c["kind"] for c in cards}
                offered = {x["kind"] for c in cards for x in c["actions"]}
                rep.add(f"{tag}:inventory:mail_card_kinds", {"unanswered_thread", "follow_up", "renewal", "commitment"} <= kinds,
                        kinds=sorted(kinds), expected=exp)
                rep.add(f"{tag}:inventory:research_note_offered", "research_note" in offered,
                        offered=sorted(offered), note="needs a meeting_prep card; the .ics meeting has an external attendee")
                arch = [x for c in cards for x in c["actions"] if x["kind"] == "archive_thread"]
                rep.add(f"{tag}:archive_thread:files_source_supported_false", bool(arch) and all(not x["supported"] for x in arch),
                        archive_actions=arch)
            learn0 = d.call("proactive.learning", {})
            for c in cards:
                i = min(v, len(c["actions"]) - 1)
                key = (c["kind"], c["title"], i)
                if key in covered:
                    continue
                covered.add(key)
                r = exercise(d, home, c, i, tz, rep, tag, fake is not None and not a.runtime)
                recs.append(r)
            if v == 0:
                learn1 = d.call("proactive.learning", {})
                met = d.call("proactive.metrics", {})
                rep.add(f"{tag}:learning:executes_teach", learn1 != learn0, before=learn0, after=learn1, metrics=met)
            # archive confirmation rule
            for r in recs:
                if r["action"]["kind"] == "archive_thread" and "archive" in r and "archive_checked" not in r:
                    r["archive_checked"] = True
                    ar = r["archive"]
                    rep.add(f"{tag}:archive_thread:always_needs_confirmation",
                            (ar["unconfirmed"] or {}).get("outcome") == "needs_confirmation",
                            unconfirmed=ar["unconfirmed"])
                    rep.add(f"{tag}:archive_thread:unsupported_not_offered_for_confirmation",
                            not ar["supported"] and (ar["unconfirmed"] or {}).get("outcome") != "needs_confirmation",
                            supported=ar["supported"], unconfirmed=ar["unconfirmed"], confirmed=ar["confirmed"],
                            confirmed_error=ar["confirmed_error"], state_after=r["state_after"])
                    rep.add(f"{tag}:archive_thread:failed_archive_leaves_card_open", r["state_after"] in ("new", "seen"),
                            state=r["state_after"])
            # policy-raised confirmations on local actions
            for r in recs:
                if r.get("confirmation_asked") and r["action"]["kind"] in ("reminder", "create_event", "draft_reply") \
                        and not r.get("policy_checked"):
                    r["policy_checked"] = True
                    rep.add(f"{tag}:policy:local_action_confirmation_reason_accurate",
                            not re.search(r"send something|spend money", (r["exec_unconfirmed"] or {}).get("detail") or ""),
                            action=r["action"], preview_risk=(r["preview"] or {}).get("risk"),
                            reason=(r["exec_unconfirmed"] or {}).get("detail"))
            (out / f"{tag}-v{v}-audit.json").write_text(json.dumps(d.try_call("policy.log.export", {})[0], indent=1, default=str)[:200000])
    (out / f"{tag}-inventory.json").write_text(json.dumps(inventory, indent=1))
    (out / f"{tag}-exercise.json").write_text(json.dumps(recs, indent=1, default=str))
    return recs


def phase_binding(a, out, rep, tz, sim, fake):
    """Try to break the approval binding."""
    tag = "binding"
    work = out / tag
    shutil.rmtree(work, ignore_errors=True)
    root = work / "maya"
    now = local(sim, tz)
    write_corpus(root, now, tz)
    home = make_home(work / "home", a, fake=fake)
    ev = {}
    d = FtDaemon(home, now, tz, work / "faketime.txt").start()
    try:
        setup(d, root)
        cards = open_cards(d)
        R = card_of(cards, "renewal")
        T = card_of(cards, "commitment")
        F1 = card_of(cards, "follow_up", "shortlist")
        F2 = card_of(cards, "follow_up", "Iris")
        U = card_of(cards, "unanswered_thread")
        ev["cards"] = {k: (c or {}).get("id") for k, c in dict(R=R, T=T, F1=F1, F2=F2, U=U).items()}
        ex = lambda cid, i, dg, conf=True: d.try_call("cards.execute", {"card_id": cid, "action": i, "digest": dg, "confirmed": conf})  # noqa: E731
        pv = lambda cid, i: d.try_call("cards.preview", {"card_id": cid, "action": i}, timeout=300)  # noqa: E731

        # wrong digest
        r, e = ex(R["id"], 0, "0" * 64)
        rep.add("binding:wrong_digest_refused", r is None and e is not None, result=r, error=err(e))
        pR, _ = pv(R["id"], 0)
        pT0, _ = pv(T["id"], 0)
        pT1, _ = pv(T["id"], 1)
        # digest of another card
        r, e = ex(T["id"], 0, pR["digest"])
        rep.add("binding:digest_of_other_card_refused", r is None and e is not None, result=r, error=err(e))
        # digest of another action of the same card
        r, e = ex(T["id"], 0, pT1["digest"])
        rep.add("binding:digest_of_other_action_refused", r is None and e is not None, result=r, error=err(e))
        # an index the card does not offer
        r, e = ex(T["id"], 7, pT0["digest"])
        p7, e7 = pv(T["id"], 7)
        rep.add("binding:unoffered_index_refused", r is None and e is not None and p7 is None and e7 is not None,
                execute_error=err(e), preview_error=err(e7))
        # malformed digest / negative index
        r, e = ex(T["id"], -1, pT0["digest"])
        rep.add("binding:negative_index_refused", r is None and e is not None, error=err(e))
        rem0 = d.call("reminders.list", {})
        rep.add("binding:refusals_changed_nothing", rem0 == [] and card_state(d, T["id"]) in ("new", "seen"),
                reminders=rem0, state=card_state(d, T["id"]))

        # concurrent executes of the same approval (two connections)
        sp = home.socket_path()
        res = [None, None]

        def go(k, cid, i, dg):
            res[k] = raw_call(sp, "cards.execute", {"card_id": cid, "action": i, "digest": dg, "confirmed": True})
        th = [threading.Thread(target=go, args=(k, R["id"], 0, pR["digest"])) for k in range(2)]
        [t.start() for t in th]
        [t.join() for t in th]
        execd = [x for x in res if (x.get("result") or {}).get("outcome") == "executed"]
        rems = [x for x in d.call("reminders.list", {}) if x["text"].startswith("Renewal:")]
        rep.add("binding:concurrent_same_digest_runs_once", len(execd) == 1 and len(rems) == 1, results=res, reminders=rems)
        # reuse after executed
        r, e = ex(R["id"], 0, pR["digest"])
        rep.add("binding:reuse_after_execute_refused", r is None and e is not None, result=r, error=err(e))
        p2, e2 = pv(R["id"], 0)
        rep.add("binding:preview_on_done_card_refused", p2 is None and e2 is not None, error=err(e2))

        # concurrent executes of two different actions of one card
        res = [None, None]
        th = [threading.Thread(target=go, args=(0, T["id"], 0, pT0["digest"])),
              threading.Thread(target=go, args=(1, T["id"], 1, pT1["digest"]))]
        [t.start() for t in th]
        [t.join() for t in th]
        execd = [x for x in res if (x.get("result") or {}).get("outcome") == "executed"]
        evdir = home.data / "proactive" / "events"
        n_ics = len(list(evdir.glob("*.ics"))) if evdir.exists() else 0
        n_rem = len([x for x in d.call("reminders.list", {}) if "expense" in x["text"]])
        rep.add("binding:concurrent_two_actions_one_card", len(execd) == 1 and n_ics + n_rem == 1,
                results=res, ics_files=n_ics, reminders=n_rem)

        # a dismissed card
        pF1d, _ = pv(F1["id"], 0)
        d.call("cards.feedback", {"card_id": F1["id"], "feedback": "dismiss", "reason": "not_relevant"})
        r, e = ex(F1["id"], 0, pF1d["digest"])
        rep.add("binding:execute_on_dismissed_card_refused", r is None and e is not None, result=r, error=err(e))

        # a refresh between preview and execute (F2's reminder is "in two hours")
        pF2, _ = pv(F2["id"], 1)
        time.sleep(1.5)
        d.call("proactive.refresh", {})
        after = card_of(open_cards(d), "follow_up", "Iris")
        r, e = ex(F2["id"], 1, pF2["digest"])
        rep.add("binding:preview_survives_refresh", r is not None and r.get("outcome") == "executed",
                preview_label=pF2["summary"], label_after_refresh=(after or {"actions": [{}, {}]})["actions"][1].get("label"),
                result=r, error=err(e))
        ev["refresh_case"] = {"preview": pF2, "after": after}

        # kill switch
        pU, eU = pv(U["id"], 0)
        ev["U_preview"] = pU or err(eU)
        if pU:
            k = d.call("policy.kill", {})
            r, e = ex(U["id"], 0, pU["digest"])
            st = card_state(d, U["id"])
            rep.add("binding:kill_switch_halts", r is not None and r.get("outcome") == "halted" and not r.get("artifact")
                    and st in ("new", "seen"), kill=k, result=r, error=err(e), state=st)
            pK, eK = pv(U["id"], 0)
            rep.add("binding:preview_while_halted", True, note="recorded only", preview_ok=pK is not None, error=err(eK),
                    llm_called=None)
            d.call("policy.resume", {})
            r, e = ex(U["id"], 0, pU["digest"])
            rep.add("binding:after_resume_same_digest_executes", r is not None and r.get("outcome") == "executed",
                    result=r, error=err(e))
    finally:
        d.stop()
    (out / "binding.json").write_text(json.dumps(ev, indent=1, default=str))
    return ev


def wait_due(d, timeout):
    n = d.wait_note("reminder.due", timeout=timeout)
    return n, d.sim_now()


def phase_ttl_restart_due(a, out, rep, tz, sim, fake):
    """Previews across a restart and past the 1 h TTL; reminders firing at their time."""
    work = out / "ttl"
    shutil.rmtree(work, ignore_errors=True)
    root = work / "maya"
    now = local(sim, tz)
    write_corpus(root, now, tz)
    home = make_home(work / "home", a, fake=fake)
    ft = work / "faketime.txt"
    ev = {}
    d = FtDaemon(home, now, tz, ft).start()
    try:
        setup(d, root)
        F1 = card_of(open_cards(d), "follow_up", "shortlist")
        p0 = d.call("cards.preview", {"card_id": F1["id"], "action": 1})
    finally:
        d.stop()
    d = FtDaemon(home, now + dt.timedelta(minutes=2), tz, ft, log_name="nuggetd2").start()
    try:
        r, e = d.try_call("cards.execute", {"card_id": F1["id"], "action": 1, "digest": p0["digest"], "confirmed": True})
        rep.add("ttl:digest_after_restart_refused", r is None and e is not None, result=r, error=err(e))
        # older than an hour
        p1 = d.call("cards.preview", {"card_id": F1["id"], "action": 1})
        t1 = d.sim_now()
        d.jump(t1 + dt.timedelta(minutes=61))
        time.sleep(0.5)
        r, e = d.try_call("cards.execute", {"card_id": F1["id"], "action": 1, "digest": p1["digest"], "confirmed": True})
        rep.add("ttl:digest_older_than_1h_refused", r is None and e is not None, result=r, error=err(e))
        # just under an hour
        p2 = d.call("cards.preview", {"card_id": F1["id"], "action": 1})
        t2 = d.sim_now()
        d.jump(t2 + dt.timedelta(minutes=58))
        time.sleep(0.5)
        r, e = d.try_call("cards.execute", {"card_id": F1["id"], "action": 1, "digest": p2["digest"], "confirmed": True})
        rep.add("ttl:digest_58min_old_executes", r is not None and r.get("outcome") == "executed", result=r, error=err(e))
        ev["F1_preview"] = p2
        # renewal reminder too
        R = card_of(open_cards(d), "renewal")
        if R:
            pR = d.call("cards.preview", {"card_id": R["id"], "action": 0})
            rr, _ = d.try_call("cards.execute", {"card_id": R["id"], "action": 0, "digest": pR["digest"], "confirmed": True})
            ev["R_preview"], ev["R_exec"] = pR, rr
        rems = d.call("reminders.list", {})
        ev["reminders"] = rems
        d.drain(1)
        d.notes = [n for n in d.notes if n.get("method") != "reminder.due"]
        for rem in rems:
            at = parse_rfc3339(rem["at"])
            # not early: 2 minutes before, nothing within 70 s
            d.jump(at - dt.timedelta(minutes=3))
            early, _ = wait_due(d, 70)
            d.jump(at - dt.timedelta(seconds=20))
            n, when = wait_due(d, 150)
            lag = (when - at).total_seconds() if n else None
            loc = at.astimezone(ZoneInfo(tz))
            rep.add("due:reminder_fires_on_time", early is None and n is not None and n["params"]["id"] == rem["id"]
                    and -1 <= lag <= 90, reminder=rem, early=early, note=n, sim_at_note=when.isoformat(),
                    lag_secs=lag, at_local=loc.strftime("%Y-%m-%d %H:%M %Z"))
            left = [x["id"] for x in d.call("reminders.list", {})]
            rep.add("due:fired_reminder_leaves_pending_list", rem["id"] not in left, pending=left)
            ev.setdefault("fired", []).append({"rem": rem, "note": n, "sim_at_note": when.isoformat(),
                                               "at_local": loc.isoformat()})
    finally:
        d.stop()
    (out / "ttl.json").write_text(json.dumps(ev, indent=1, default=str))
    return ev


def phase_no_model(a, out, rep, tz, sim):
    """No provider and no runtime: what does draft_reply look like?"""
    work = out / "nomodel"
    shutil.rmtree(work, ignore_errors=True)
    root = work / "maya"
    now = local(sim, tz)
    write_corpus(root, now, tz)
    home = make_home(work / "home", a)
    with Daemon(home, sim=now, tz=tz) as d:
        setup(d, root)
        U = card_of(open_cards(d), "unanswered_thread")
        act = U["actions"][0]
        t0 = time.time()
        p, e = d.try_call("cards.preview", {"card_id": U["id"], "action": 0}, timeout=300)
        secs = round(time.time() - t0, 1)
        msg = (e.message if e else "") or ""
        rep.add("nomodel:draft_reply_supported_flag_honest", act["supported"] is False or p is not None,
                action=act, preview=p, error=err(e))
        rep.add("nomodel:draft_reply_failure_is_clear", p is None and e is not None and secs < 30
                and re.search(r"model|provider|install|runtime", msg, re.I) is not None,
                error=err(e), secs=secs)
        # the card's other action still works
        p2, e2 = d.try_call("cards.preview", {"card_id": U["id"], "action": 1})
        rep.add("nomodel:card_usable_without_model", p2 is not None, error=err(e2))


def phase_tz(a, out, rep, fake):
    """Times in the dry-run diffs and in the files, in New York (across DST) and London."""
    res = {}
    for tz, sim in (("Europe/London", "2026-11-04T09:00"), ("America/New_York", "2026-10-29T09:00")):
        tag = "tz-" + tz.split("/")[1]
        work = out / tag
        shutil.rmtree(work, ignore_errors=True)
        root = work / "maya"
        now = local(sim, tz)
        exp = write_corpus(root, now, tz)
        home = make_home(work / "home", a, fake=fake)
        with Daemon(home, sim=now, tz=tz) as d:
            setup(d, root)
            cards = open_cards(d)
            T = card_of(cards, "commitment")
            R = card_of(cards, "renewal")
            F1 = card_of(cards, "follow_up", "shortlist")
            out_ = {"expected": exp}
            if R:
                p = d.call("cards.preview", {"card_id": R["id"], "action": 0})
                at = parse_rfc3339(re.match(r"Reminder at (\S+):", diff_texts(p)[0]).group(1))
                loc = at.astimezone(ZoneInfo(tz))
                want = dt.datetime.fromisoformat(exp["renew"]) - dt.timedelta(days=1)
                rep.add(f"{tag}:renewal_reminder_9am_local_day_before", loc.hour == 9 and loc.date() == want.date(),
                        preview=diff_texts(p), local=loc.strftime("%Y-%m-%d %H:%M %Z"), renew=exp["renew"])
            if F1:
                p = d.call("cards.preview", {"card_id": F1["id"], "action": 1})
                at = parse_rfc3339(re.match(r"Reminder at (\S+):", diff_texts(p)[0]).group(1))
                loc = at.astimezone(ZoneInfo(tz))
                rep.add(f"{tag}:promise_reminder_at_deadline_local", loc.hour == 17 and loc.date().isoformat() == exp["friday"],
                        preview=diff_texts(p), local=loc.strftime("%Y-%m-%d %H:%M %Z"), friday=exp["friday"])
            if T:
                p = d.call("cards.preview", {"card_id": T["id"], "action": 1})
                r = d.call("cards.execute", {"card_id": T["id"], "action": 1, "digest": p["digest"], "confirmed": True})
                path = ((r or {}).get("artifact") or {}).get("path")
                text = Path(path).read_bytes().decode() if path else ""
                f = ics_fields(text)
                ds = f.get("DTSTART", [[None, None]])[0][1]
                loc = ics_time(ds).astimezone(ZoneInfo(tz)) if ds else None
                rep.add(f"{tag}:focus_event_ics_utc_matches_diff", bool(ds) and any(ics_time(ds) == parse_rfc3339(x.split(" – ")[0][6:])
                                                                                      for x in diff_texts(p) if x.startswith("When: ")),
                        diff=diff_texts(p), dtstart=ds, local=loc.isoformat() if loc else None, ics=text)
                out_["focus_local"] = loc.isoformat() if loc else None
            res[tz] = out_
    (out / "tz.json").write_text(json.dumps(res, indent=1, default=str))
    return res


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--runtime", action="store_true")
    ap.add_argument("--chat-model")
    ap.add_argument("--embed-model")
    ap.add_argument("--models-home")
    ap.add_argument("--sim", default="2026-10-29T09:00")
    ap.add_argument("--tz", default="America/New_York")
    ap.add_argument("--only", help="comma list: exec,binding,ttl,nomodel,tz")
    a = ap.parse_args()
    out = Path(a.out).resolve()
    out.mkdir(parents=True, exist_ok=True)
    only = set(a.only.split(",")) if a.only else {"exec", "binding", "ttl", "nomodel", "tz"}
    rep = Report()
    fake = None
    if not a.runtime:
        fake = FakeLLM(log_path=str(out / "fake_llm.jsonl")).start()
    t0 = time.time()
    try:
        if "exec" in only:
            print("== exec", flush=True)
            phase_inventory_and_execute(a, out, rep, a.tz, a.sim, "exec", fake)
        if "binding" in only:
            print("== binding", flush=True)
            phase_binding(a, out, rep, a.tz, a.sim, fake)
        if "ttl" in only:
            print("== ttl", flush=True)
            phase_ttl_restart_due(a, out, rep, a.tz, a.sim, fake)
        if "nomodel" in only and not a.runtime:
            print("== nomodel", flush=True)
            phase_no_model(a, out, rep, a.tz, a.sim)
        if "tz" in only:
            print("== tz", flush=True)
            phase_tz(a, out, rep, fake)
    finally:
        if fake:
            fake.stop()
        (out / "summary.json").write_text(json.dumps({"report": rep.r, "args": vars(a),
                                                       "secs": round(time.time() - t0, 1)}, indent=1, default=str))
    bad = [k for k, v in rep.r.items() if not v["ok"]]
    print(f"{len(rep.r) - len(bad)}/{len(rep.r)} checks pass; failing: {bad}")


if __name__ == "__main__":
    main()
