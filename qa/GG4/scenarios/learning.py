#!/usr/bin/env python3
"""GG4 A2: does Nugget learn the right thing from Accept/Dismiss (BB2), and does reset undo it?

Simulated users act on mail cards over simulated days (the daemon restarted under libfaketime at a
later instant = time passing). Every scenario runs on twin homes fed the *same* mail: `U` (the user
gives feedback) and `C` (control: no feedback). A card's rank in U minus its rank in C at the same
simulated instant is what learning did to it, so bleed onto cards the user never touched is
measurable directly, and after `reset_learning all` U must look like C again.

Synthetic mail only (`*.example` addresses plus a few `gmail.com` "friends"); nothing is copied.

    python3 learning.py --out DIR [--runtime --chat-model ID --embed-model ID --models-home DIR]

Writes DIR/summary.json: {"report": {check: {"ok": bool, "evidence": ...}}, ...}. Needs
`target/debug/nuggetd` and (for the audit checks) `target/debug/gg4-audit`
(cargo build --manifest-path qa/GG4/scenarios/a2audit/Cargo.toml --target-dir target).
"""
from __future__ import annotations

import argparse
import json
import math
import subprocess
import sys
import time
from datetime import datetime, timedelta
from email.utils import format_datetime
from pathlib import Path
from zoneinfo import ZoneInfo

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "harness"))
from nq import REPO, Daemon, Home  # noqa: E402

TZ = "America/New_York"
Z = ZoneInfo(TZ)
USER = "maya@larkspur.example"
ME = f"Maya Okafor <{USER}>"
D0 = datetime(2026, 11, 9, 9, 0, tzinfo=Z)  # a Monday, after the DST change
SENSITIVE_MARKERS = ["123-45-6789", "GB82WEST", "SSN", "Payroll correction"]
HALF_LIFE_SPEC = 30.0  # what the QA brief says BB2 promises
HALF_LIFE_CODE = 60.0  # feedback.rs HALF_LIFE_DAYS
MIN_SCORE = 0.45  # config.rs min_score default


def logit(p):
    p = min(max(p, 1e-12), 1 - 1e-12)
    return math.log(p / (1 - p))


class World:
    """A mail folder both twin homes index."""

    def __init__(self, root: Path):
        self.root = root
        root.mkdir(parents=True, exist_ok=True)
        self.n = 0

    def mail(self, fid, frm, subject, at, body, to=ME, irt=None, cc=None):
        """Write `fid`.eml (Message-ID <fid@a2.example>); `irt` is the fid it replies to."""
        h = [f"From: {frm}", f"To: {to}"]
        if cc:
            h.append(f"Cc: {cc}")
        h += [f"Subject: {subject}", f"Date: {format_datetime(at)}", f"Message-ID: <{fid}@a2.example>"]
        if irt:
            h += [f"In-Reply-To: <{irt}@a2.example>", f"References: <{irt}@a2.example>"]
        h += ["MIME-Version: 1.0", "Content-Type: text/plain; charset=utf-8", "", body, ""]
        (self.root / f"{fid}.eml").write_text("\n".join(h))
        self.n += 1
        return fid


class Twin:
    """Two homes over one world. `session(at)` yields (d_user, d_control) daemons."""

    def __init__(self, out: Path, name: str, world: World, a):
        self.name, self.world = name, world
        kw = dict(judge="off", runtime=a.runtime, chat_model=a.chat_model,
                  embed_model=a.embed_model, models_home=a.models_home)
        self.homes = {"U": Home(out / name / "U", **kw), "C": Home(out / name / "C", **kw)}
        self.src = {}

    def open(self, who, at):
        d = Daemon(self.homes[who], sim=at, tz=TZ).start()
        if who not in self.src:
            self.src[who] = d.add_files_source(self.world.root)["id"]
        d.sync(self.src[who])
        d.drain(0.3)
        d.call("proactive.refresh", {})
        return d


def by_file(cards, fid):
    """The card whose primary evidence is `fid`.eml."""
    for c in cards:
        ev = c.get("evidence") or []
        if ev and ev[0]["item"]["key"].endswith(f"{fid}.eml"):
            return c
    return None


def snap(d):
    """Every card (open, hidden and closed) plus which are offered in the inbox."""
    allc = d.cards(include_closed=True)["cards"]
    inbox = {c["id"] for c in d.cards()["cards"]}
    for c in allc:
        # Eligible to be offered: open, rank >= min_score (0.45) and not hidden by feedback. The
        # inbox itself is capped at 12 cards, so membership is reported separately (`inbox`).
        c["_inbox"] = c["id"] in inbox
        c["_shown"] = (c["state"] in ("new", "seen") and c["rank"] >= MIN_SCORE
                       and not any(e["direction"] == "hidden" for e in c["learned"]))
    return allc


def brief_card(c):
    if not c:
        return None
    return {"id": c["id"], "title": c["title"], "state": c["state"], "rank": round(c["rank"], 4),
            "shown": c.get("_shown"), "inbox": c.get("_inbox"),
            "factors": [(f["name"], round(f["weight"], 3), round(f["contribution"], 3)) for f in c.get("factors", [])],
            "learned": [(e["scope"], e["key"], e["direction"], e["accepted"], e["dismissed"], e["reason"],
                         round(e["contribution"], 3)) for e in c["learned"]]}


def learned_sum(c, scopes=None):
    return sum(e["contribution"] for e in c["learned"] if scopes is None or e["scope"] in scopes)


def entry(learning, scope, key):
    for e in learning["entries"]:
        if e["scope"] == scope and e["key"] == key:
            return e
    return None


def audit(home: Home):
    exe = REPO / "target/debug/gg4-audit"
    if not exe.exists():
        return {"error": f"{exe} missing"}
    r = subprocess.run([str(exe), str(home.path), "500"], capture_output=True, text=True)
    if r.returncode:
        return {"error": r.stderr[-2000:]}
    return json.loads(r.stdout)


class Report:
    def __init__(self):
        self.r = {}

    def check(self, name, ok, evidence):
        prev = self.r.get(name)
        if prev:  # repeated check: N grows, ok is all-of
            prev["n"] += 1
            prev["ok"] = prev["ok"] and bool(ok)
            prev.setdefault("more", []).append(evidence)
        else:
            self.r[name] = {"ok": bool(ok), "n": 1, "evidence": evidence}
        print(f"  [{'PASS' if ok else 'FAIL'}] {name}", flush=True)


# ---------------------------------------------------------------------------------------------
# Scenario 1: a week of a realistic inbox, then 35 days later, then resets.
# ---------------------------------------------------------------------------------------------

JO = "Jo Brandt <jo@talentpeak.example>"            # recruiter: always dismissed not_relevant
SAM = "Sam Ortiz <sam@talentpeak.example>"          # same recruiting firm, never touched
LEE = "Lee Park <lee@hirewave.example>"             # another recruiter, never touched
PRIYA = "Priya Raman <priya@larkspur.example>"      # manager: always accepted
TOM = "Tom Becker <tom@larkspur.example>"           # coworker, never touched
KEVIN = "Kevin Ames <kevin@larkspur.example>"       # noisy coworker: dismissed not_important (days 3-4)
DEALS = "Deal Guy <deals.guy@gmail.com>"            # gmail stranger: dismissed not_important
SOFIA = "Sofia Alvarez <sofia.alvarez@gmail.com>"   # gmail friend, never touched
BEN = "Ben Kowalski <ben.k@gmail.com>"              # gmail friend, never touched
DANA = "Dana Whitfield <dana@northwind-outfitters.example>"  # client: one thread already handled
BILL = "Rita Gomez <rita@cloudnest.example>"                  # account manager: dismissed wrong_info once
SUPPORT = "Omar Haddad <omar@cloudnest.example>"              # same vendor, never touched
HR = "Benefits Desk <hr@benefitsco.example>"                 # thread that turns sensitive


def mail_day(w: World, k: int, at: datetime):
    """Mail that arrived the day before morning `k` (dated 26 h before `at`)."""
    t = at - timedelta(hours=26)
    f = {}
    f["jo"] = w.mail(f"jo-{k}", JO, f"Staff designer role at Studio{k}?", t,
                     f"Hi Maya,\n\nWould you be open to a quick call about a staff designer role at Studio{k}?\n\nJo")
    f["priya"] = w.mail(f"priya-{k}", PRIYA, f"Can you review the onboarding flow v{k}?", t,
                        f"Maya, can you review the onboarding flow v{k} before Thursday?\nPriya")
    f["deals"] = w.mail(f"deals-{k}", DEALS, f"Want a discount on monitors #{k}?", t,
                        "Hey Maya, want 40% off a new monitor this week? Reply yes.")
    f["sofia"] = w.mail(f"sofia-{k}", SOFIA, f"climbing on day {k}?", t, "Are you coming climbing tonight?\nS")
    f["ben"] = w.mail(f"ben-{k}", BEN, f"book club pick {k}?", t, "Which book should we read next month?\nBen")
    f["tom"] = w.mail(f"tom-{k}", TOM, f"Icon set question {k}", t, "Maya, should the icons be outlined or filled?\nTom")
    f["lee"] = w.mail(f"lee-{k}", LEE, f"Design lead opening {k}?", t,
                      "Hi Maya, are you interested in a design lead opening at a fintech?\nLee")
    if k in (0, 3, 4):
        f["sam"] = w.mail(f"sam-{k}", SAM, f"Contract UX gig {k}?", t,
                          "Hi Maya, would you consider a 3-month contract UX gig?\nSam")
    if k in (3, 4):
        f["kevin"] = w.mail(f"kevin-{k}", KEVIN, f"Lunch and learn signup {k}?", t,
                            "Can you sign up for Friday's lunch and learn?\nKevin")
    return f


def scenario_week(out: Path, a, rep: Report):
    print("== scenario 1: a week of feedback, decay, persistence, resets", flush=True)
    w = World(out / "s1" / "mail")
    tw = Twin(out, "s1", w, a)
    ev = {"days": []}
    # History: Maya writes to her manager often (importance), and once to Dana.
    for i in range(10):
        w.mail(f"sent-priya-{i}", ME, f"Notes {i}", D0 - timedelta(days=20 - i), "Notes attached.", to=PRIYA)
    actions = []  # (sim time, who, fid, feedback, reason)

    def act(d, cards, fid, fb, reason=None, who="U"):
        c = by_file(cards, fid)
        if c is None:
            actions.append({"fid": fid, "error": "no card"})
            return None, "no card"
        p = {"card_id": c["id"], "feedback": fb}
        if reason:
            p["reason"] = reason
        r, err = d.try_call("cards.feedback", p)
        actions.append({"fid": fid, "card": c["id"], "fb": fb, "reason": reason, "error": str(err) if err else None})
        return r, err

    days = [0, 1, 2, 3, 4]
    learning_end = {}
    for k in days:
        at = D0 + timedelta(days=k)
        f = mail_day(w, k, at)
        t = at - timedelta(hours=26)
        if k == 0:
            w.mail("dana-t1", DANA, "Checkout copy: which version?", t,
                   "Hi Maya, which version of the checkout copy should we ship, A or B?\nDana")
            w.mail("dana-inv", DANA, "October invoice: PO number?", t,
                   "Could you send me the PO number for the October invoice?\nDana")
            w.mail("bill-0", BILL, "Billing address still 12 Elm St?", t,
                   "Is your billing address still 12 Elm St, Springfield? Please confirm.")
            w.mail("hr-0", HR, "Open enrollment: dental plan choice?", t,
                   "Hi Maya, which dental plan do you want for next year?")
            w.mail("hrs-0", HR, "Payroll correction for Maya, SSN 123-45-6789?", t,
                   "Hi Maya, payroll has SSN 123-45-6789 and IBAN GB82WEST12345698765432 on file. "
                   "Can you confirm these are right?")
        if k == 1:
            # The open-enrollment thread turns sensitive: a reply with an SSN.
            w.mail("hr-1", HR, "Re: Open enrollment: dental plan choice?", t,
                   "Also, can you confirm your SSN 123-45-6789 for the plan?", irt="hr-0")
            w.mail("bill-1", BILL, "Card ending 4242 expiring: update?", t,
                   "Your card on file expires soon. Can you update it?")
            w.mail("sup-1", SUPPORT, "Ticket 5521: still seeing the error?", t,
                   "Are you still seeing the sync error from last week?")
        if k == 2:
            # Dana writes again in the handled thread: something new, should surface.
            w.mail("dana-t1b", DANA, "Re: Checkout copy: which version?", t,
                   "One more thing: which CTA colour should we use with version B?", irt="dana-t1")
            # Jo follows up in his day-0 thread.
            w.mail("jo-0b", JO, "Re: Staff designer role at Studio0?", t,
                   "Just following up, any interest in a call this week?", irt="jo-0")
        if k == 3:
            w.mail("bill-3", BILL, "Invoice 3391 attached: approve?", t, "Can you approve invoice 3391?")
            w.mail("sup-3", SUPPORT, "Feature request vote?", t, "Would you vote on our roadmap item?")
        day = {"k": k, "at": at.isoformat()}
        sessions = {}
        for who in ("C", "U"):
            d = tw.open(who, at)
            try:
                cards = snap(d)
                sessions[who] = cards
                if who == "U":
                    day["learning_before"] = d.call("proactive.learning", {})
                    day["metrics_before"] = d.call("proactive.metrics", {})
                    # The user acts.
                    act(d, cards, f["jo"], "dismiss", "not_relevant")
                    act(d, cards, f["priya"], "accept")
                    act(d, cards, f["deals"], "dismiss", "not_important")
                    if "kevin" in f:
                        act(d, cards, f["kevin"], "dismiss", "not_important")
                    if k == 0:
                        act(d, cards, "dana-t1", "dismiss", "already_handled")
                        act(d, cards, "bill-0", "dismiss", "wrong_info")
                        act(d, cards, "hr-0", "dismiss", "not_relevant")
                        act(d, cards, "hrs-0", "dismiss", "not_relevant")
                        # Repeated accepts are no-ops.
                        c = by_file(cards, f["priya"])
                        l1 = d.call("proactive.learning", {})
                        m1 = d.call("proactive.metrics", {})
                        r2, e2 = d.try_call("cards.feedback", {"card_id": c["id"], "feedback": "accept"})
                        r3, e3 = d.try_call("cards.feedback", {"card_id": c["id"], "feedback": "done"})
                        l2 = d.call("proactive.learning", {})
                        m2 = d.call("proactive.metrics", {})
                        e1 = entry(l1, "sender", "priya@larkspur.example")
                        e2_ = entry(l2, "sender", "priya@larkspur.example")
                        ok = (e1 and e2_ and abs(e1["accepted"] - e2_["accepted"]) < 1e-6
                              and m1["model_updates"] == m2["model_updates"] and e2 is None)
                        rep.check("repeat_accept_and_done_are_noops", ok, {
                            "sender_accepted_before": e1 and e1["accepted"],
                            "after_second_accept_and_done": e2_ and e2_["accepted"],
                            "model_updates": [m1["model_updates"], m2["model_updates"]],
                            "second_accept_error": str(e2) if e2 else None,
                            "done_error": str(e3) if e3 else None,
                            "state_after_done": r3 and r3["state"]})
                    if k == 1:
                        act(d, cards, "hr-1", "dismiss", "not_relevant")
                    after = snap(d)
                    day["after"] = {fid: brief_card(by_file(after, fid)) for fid in
                                    [f["jo"], f["priya"], f["deals"]]}
                    day["learning_after"] = d.call("proactive.learning", {})
                    learning_end[k] = day["learning_after"]
                    if k == 0:
                        # Persistence: stop and start again at (about) the same instant.
                        lb = day["learning_after"]
                        d.stop()
                        d = Daemon(tw.homes["U"], sim=at + timedelta(minutes=1), tz=TZ).start()
                        la = d.call("proactive.learning", {})
                        keys_b = sorted((e["scope"], e["key"], round(e["accepted"], 3), round(e["dismissed"], 3)) for e in lb["entries"])
                        keys_a = sorted((e["scope"], e["key"], round(e["accepted"], 3), round(e["dismissed"], 3)) for e in la["entries"])
                        c2 = by_file(snap(d), f["jo"])
                        rep.check("learning_persists_across_restart", keys_a == keys_b and c2 and c2["state"] == "dismissed",
                                  {"before_stop": keys_b, "after_start": keys_a})
            finally:
                d.stop()
        day["U"] = {}
        day["C"] = {}
        for fid in list(f.values()) + ["dana-t1", "dana-inv", "dana-t1b", "jo-0b", "bill-0", "bill-1", "sup-1",
                                       "bill-3", "sup-3", "hr-0", "hr-1", "hrs-0"]:
            cu, cc = by_file(sessions["U"], fid), by_file(sessions["C"], fid)
            if cu or cc:
                day["U"][fid] = brief_card(cu)
                day["C"][fid] = brief_card(cc)
        ev["days"].append(day)

    ev["actions"] = actions
    D = {d["k"]: d for d in ev["days"]}

    # ---- direction & scope ----
    # Before any feedback on day k (k>=1), compare each new card in U vs C.
    def delta(k, fid):
        u, c = D[k]["U"].get(fid), D[k]["C"].get(fid)
        if not u or not c:
            return None
        return {"U": u["rank"], "C": c["rank"], "d_logodds": round(logit(u["rank"]) - logit(c["rank"]), 3),
                "U_shown": u["shown"], "C_shown": c["shown"], "learned": u["learned"]}

    table = {k: {n: delta(k, fid) for n, fid in [
        ("jo", f"jo-{k}"), ("sam", f"sam-{k}"), ("lee", f"lee-{k}"), ("priya", f"priya-{k}"),
        ("tom", f"tom-{k}"), ("kevin", f"kevin-{k}"), ("deals", f"deals-{k}"), ("sofia", f"sofia-{k}"),
        ("ben", f"ben-{k}")]} for k in days}
    ev["delta_table"] = table
    t1 = table[1]
    rep.check("recruiter_not_relevant_demotes_and_hides_sender", t1["jo"]["d_logodds"] < -1 and not t1["jo"]["U_shown"]
              and t1["jo"]["C_shown"], {"day1_jo": t1["jo"]})
    rep.check("manager_accepts_raise_learned_sender_effect", t1["priya"]["learned"] and t1["priya"]["learned"][0][0] == "sender"
              and t1["priya"]["learned"][0][6] > 1, {"day1_priya": t1["priya"]})
    pri = {k: table[k]["priya"]["d_logodds"] for k in (1, 2, 3, 4)}
    rep.check("manager_ranks_higher_than_without_feedback", all(v > 0 for v in pri.values()),
              {"priya_d_logodds_vs_control_by_day": pri, "day4_priya": table[4]["priya"]})
    # Gmail: dismissing deals.guy as not_important must not make a gmail.com domain entry and must not
    # move Sofia/Ben by any sender/domain effect.
    gm_entries = [e for e in learning_end[4]["entries"] if e["key"] in ("gmail.com",)]
    friend_scopes = sorted({e[0] for k in (1, 2, 3, 4) for n in ("sofia", "ben") for e in (table[k][n] or {}).get("learned", [])})
    rep.check("no_gmail_domain_bleed_in_learned_layer", not gm_entries and set(friend_scopes) <= {"kind"},
              {"gmail_entries": gm_entries, "friend_learned_scopes_days1to4": friend_scopes,
               "day4_sofia": table[4]["sofia"], "day4_ben": table[4]["ben"], "day4_deals": table[4]["deals"]})
    # Total movement of untouched friends (shared model + kind): how much do they lose?
    friend_drop = {k: {n: table[k][n]["d_logodds"] for n in ("sofia", "ben", "lee", "tom")} for k in (1, 2, 3, 4)}
    worst = min(v for k in friend_drop for v in friend_drop[k].values())
    hidden_friends = [(k, n) for k in (1, 2, 3, 4) for n in ("sofia", "ben", "lee")
                      if table[k][n]["C_shown"] and not table[k][n]["U_shown"]]
    rep.check("untouched_friends_not_hidden_by_others_feedback", not hidden_friends,
              {"hidden (day, who)": hidden_friends, "d_logodds_vs_control": friend_drop})
    rep.check("untouched_friends_rank_moves_less_than_0.5_logodds", worst > -0.5,
              {"worst_d_logodds": worst, "d_logodds_vs_control": friend_drop,
               "note": "everything beyond the learned 'kind' effect comes from the shared scoring model (score.rs update_weighted), which is not listed in card.learned"})
    # Same recruiting firm: domain effect expected (not a public domain) but weaker than sender.
    lj = learned_sum({"learned": [{"scope": e[0], "contribution": e[6]} for e in table[3]["jo"]["learned"]]}, {"sender", "domain"})
    ls = learned_sum({"learned": [{"scope": e[0], "contribution": e[6]} for e in table[3]["sam"]["learned"]]}, {"sender", "domain"})
    ll = [e for e in table[3]["lee"]["learned"] if e[0] in ("sender", "domain")]
    rep.check("same_company_domain_effect_weaker_than_sender", lj < ls < 0 and not ll,
              {"day3_jo": table[3]["jo"], "day3_sam": table[3]["sam"], "day3_lee_other_firm": table[3]["lee"]})
    # Own company domain: does dismissing Kevin as not_important demote the manager?
    p4 = table[4]["priya"]
    dom = [e for e in p4["learned"] if e[0] == "domain"]
    dl = entry(learning_end[4], "domain", "larkspur.example")
    rep.check("noisy_coworker_not_important_does_not_demote_manager_via_domain", (not dom or dom[0][6] >= 0)
              and (dl is None or dl["adjustment"] >= 0),
              {"day4_priya_learned": p4["learned"], "day4_tom": table[4]["tom"],
               "learning_domain_larkspur": entry(learning_end[4], "domain", "larkspur.example")})

    # ---- counts ----
    # Expected decayed counts for jo (sender) at the end of day 4: one not_relevant per day 0..4.
    def expected(fids_times, at, hl):
        return sum(0.5 ** ((at - t).total_seconds() / 86400 / hl) for t in fids_times)
    at4 = D0 + timedelta(days=4)
    times = [D0 + timedelta(days=k) for k in days]
    ej = entry(learning_end[4], "sender", "jo@talentpeak.example")
    rep.check("learning_counts_match_actions", ej and abs(ej["dismissed"] - expected(times, at4, HALF_LIFE_CODE)) < 0.05
              and abs(ej["accepted"]) < 1e-9,
              {"jo_entry_day4": ej, "expected_dismissed_hl60": expected(times, at4, 60),
               "expected_dismissed_hl30": expected(times, at4, 30)})
    # Card explanation on day 4 for jo's new card (4 prior dismissals).
    jl = {e[0]: e for e in table[4]["jo"]["learned"]}
    rep.check("card_learned_explains_counts", jl.get("sender") and jl["sender"][4] == 4 and jl["sender"][5] == "not_relevant",
              {"day4_jo_card_learned": table[4]["jo"]["learned"]})
    # Manager: 4 accepts before day 4 (the done on day 0 must not count twice).
    pl = {e[0]: e for e in table[4]["priya"]["learned"]}
    rep.check("card_learned_counts_accepts", pl.get("sender") and pl["sender"][3] == 4, {"day4_priya_card_learned": table[4]["priya"]["learned"]})

    # ---- already_handled thread ----
    le0 = learning_end[0]
    th = [e for e in le0["entries"] if e["suppresses"]]
    dana_sender = entry(learning_end[4], "sender", "dana@northwind-outfitters.example")
    d2 = D[2]["U"].get("dana-t1b")
    inv1 = delta(1, "dana-inv")
    rep.check("already_handled_suppresses_thread_without_penalising_sender",
              len(th) == 1 and dana_sender is None and inv1 and all(e[0] == "kind" for e in inv1["learned"]),
              {"suppressing_entries_day0": th, "dana_sender_entry_day4": dana_sender, "day1_other_dana_thread": inv1})
    rep.check("already_handled_thread_not_hidden_after_new_message", d2 and d2["state"] in ("new", "seen")
              and not any(e[2] == "hidden" for e in d2["learned"]),
              {"day2_dana_new_message_card": d2, "control": D[2]["C"].get("dana-t1b")})
    rep.check("already_handled_thread_new_message_is_offered", d2 and d2["shown"],
              {"day2_dana_new_message_card": d2, "control": D[2]["C"].get("dana-t1b")})

    # ---- wrong_info ----
    b1, s1 = delta(1, "bill-1"), delta(1, "sup-1")
    b3, s3 = delta(3, "bill-3"), delta(3, "sup-3")
    rep.check("wrong_info_only_mild_sender_effect_no_domain", b1 and s1 and
              -1.0 < learned_sum({"learned": [{"scope": e[0], "contribution": e[6]} for e in b1["learned"]]}, {"sender"}) <= 0 and
              not any(e[0] == "domain" for e in b1["learned"]) and not any(e[0] in ("sender", "domain") for e in s1["learned"]),
              {"day1_billing_other_card": b1, "day1_support_same_vendor": s1, "day3_billing": b3, "day3_support": s3,
               "learning_billing": entry(learning_end[1], "sender", "rita@cloudnest.example"),
               "learning_cloudnest_domain": entry(learning_end[1], "domain", "cloudnest.example")})

    # ---- sensitive labels ----
    leak = []
    for k in days:
        for e in learning_end[k]["entries"]:
            lab = (e.get("label") or "")
            if any(m.lower() in lab.lower() for m in SENSITIVE_MARKERS) or "dental" in lab.lower() and k >= 1:
                leak.append({"day": k, "entry": e})
    hr_label0 = [e for e in learning_end[0]["entries"] if (e.get("label") or "").lower().startswith("reply to benefits")]
    rep.check("sensitive_titles_not_kept_as_thread_labels", not leak,
              {"leaks": leak, "day0_benefits_labels": hr_label0,
               "sensitive_cards": {fid: D[k]["U"].get(fid) for k, fid in [(0, "hrs-0"), (1, "hr-1")]}})

    # ---- learning never changes actions ----
    def actions_of(cards, fid):
        c = by_file(cards, fid)
        return c and [(x["kind"], x["risk"], x["supported"], x["label"]) for x in c["actions"]]

    # ---- day 35: decay ----
    at35 = D0 + timedelta(days=35)
    t = at35 - timedelta(hours=26)
    w.mail("jo-35", JO, "Principal designer role at Studio35?", t, "Would you be open to a call about a principal role?\nJo")
    w.mail("sam-35", SAM, "Contract UX gig 35?", t, "Would you consider a contract gig?\nSam")
    w.mail("priya-35", PRIYA, "Can you review the Q1 plan?", t, "Can you review the Q1 plan before Friday?\nPriya")
    w.mail("deals-35", DEALS, "Want a discount on chairs?", t, "Want 30% off a chair? Reply yes.")
    w.mail("sofia-35", SOFIA, "climbing again?", t, "Are you coming climbing on Thursday?\nS")
    w.mail("tom-35", TOM, "Icon set question 35", t, "Should the icons be outlined or filled?\nTom")
    w.mail("lee-35", LEE, "Design lead opening 35?", t, "Are you interested in a design lead opening?\nLee")
    w.mail("dana-35", DANA, "Re: Checkout copy: which version?", t, "Shall we A/B test the CTA too?", irt="dana-t1b")
    w.mail("jo-0c", JO, "Re: Staff designer role at Studio0?", t, "Last try: any interest at all?", irt="jo-0b")
    sess = {}
    acts_cmp = {}
    previews = {}
    for who in ("C", "U"):
        d = tw.open(who, at35)
        try:
            sess[who] = snap(d)
            if who == "U":
                sess["learning35"] = d.call("proactive.learning", {})
                sess["metrics35"] = d.call("proactive.metrics", {})
            for fid in ("jo-35", "priya-35", "deals-35"):
                c = by_file(sess[who], fid)
                acts_cmp.setdefault(fid, {})[who] = c and [(x["kind"], x["risk"], x["supported"], x["label"]) for x in c["actions"]]
                if c:
                    r, e = d.try_call("cards.preview", {"card_id": c["id"], "action": 1})
                    previews.setdefault(fid, {})[who] = (r and {k: r[k] for k in ("kind", "risk", "target", "diff", "summary")}) or str(e)
        finally:
            d.stop()
    l35 = sess["learning35"]
    ej35 = entry(l35, "sender", "jo@talentpeak.example")
    ratio = ej35["dismissed"] / ej["dismissed"] if ej35 and ej else None
    hl = 31 * math.log(2) / -math.log(ratio) if ratio and 0 < ratio < 1 else None
    rep.check("decay_half_life_matches_code_constant_60d", hl is not None and abs(hl - HALF_LIFE_CODE) < 1,
              {"jo_dismissed_day4": ej and ej["dismissed"], "jo_dismissed_day35": ej35 and ej35["dismissed"],
               "measured_half_life_days": hl, "spec_days": HALF_LIFE_SPEC, "code_const_days": HALF_LIFE_CODE})
    dec = {}
    for n, fid in [("jo", "jo-35"), ("sam", "sam-35"), ("priya", "priya-35"), ("deals", "deals-35"),
                   ("sofia", "sofia-35"), ("tom", "tom-35"), ("lee", "lee-35"), ("dana_thread", "dana-35"), ("jo_thread", "jo-0c")]:
        u, c = by_file(sess["U"], fid), by_file(sess["C"], fid)
        dec[n] = u and c and {"U": round(u["rank"], 4), "C": round(c["rank"], 4),
                              "d_logodds": round(logit(u["rank"]) - logit(c["rank"]), 3),
                              "learned_sum": round(learned_sum(u), 3),
                              "model_part": round(logit(u["rank"]) - logit(c["rank"]) - learned_sum(u), 3),
                              "U_shown": u["_shown"], "C_shown": c["_shown"], "learned": brief_card(u)["learned"]}
    ev["day35"] = dec
    rep.check("decay_effects_smaller_after_35_days", dec["jo"] and table[4]["jo"] and
              dec["jo"]["learned_sum"] > learned_sum({"learned": [{"scope": e[0], "contribution": e[6]} for e in table[4]["jo"]["learned"]]}),
              {"day4_jo": table[4]["jo"], "day35_jo": dec["jo"]})
    rep.check("shared_model_part_decays_too", all(abs(v["model_part"]) < 0.1 for v in dec.values() if v),
              {"model_part_by_sender_day35": {k: v and v["model_part"] for k, v in dec.items()},
               "note": "model_part = rank change vs control not explained by card.learned (the shared ScoringModel, no decay)"})
    rep.check("dana_thread_handled_then_new_messages_not_hidden_day35", dec["dana_thread"] and dec["dana_thread"]["U_shown"],
              {"day35_dana": dec["dana_thread"]})
    rep.check("learning_never_changes_actions", all(v.get("U") == v.get("C") for v in acts_cmp.values())
              and all(v.get("U") == v.get("C") for v in previews.values()),
              {"actions": acts_cmp, "archive_previews": previews})

    # ---- day 36: resets (single session, then compare with control) ----
    at36 = D0 + timedelta(days=35, hours=2)
    reset_ev = {}
    d = tw.open("U", at36)
    try:
        def state():
            cs = snap(d)
            return {fid: by_file(cs, fid) for fid in ("jo-35", "sam-35", "priya-35", "deals-35", "sofia-35",
                                                       "tom-35", "lee-35", "dana-35", "jo-0c")}

        def rank_map(s):
            return {k: (round(v["rank"], 6), v["_shown"], sorted({e["scope"] for e in v["learned"]})) for k, v in s.items() if v}

        def do_reset(params, expect_changed):
            before = state()
            lb = d.call("proactive.learning", {})
            d.notes.clear()
            r, e = d.try_call("proactive.reset_learning", params)
            note = d.wait_note("cards.changed", timeout=3, pred=lambda n: n["params"]["reason"] == "reset_learning")
            after = state()
            la = d.call("proactive.learning", {})
            rb, ra = rank_map(before), rank_map(after)
            moved = sorted(k for k in rb if k in ra and abs(rb[k][0] - ra[k][0]) > 1e-4)
            unexpected = sorted(set(moved) - set(expect_changed))
            missing = sorted(set(expect_changed) - set(moved))
            gone = sorted({(x["scope"], x["key"]) for x in lb["entries"]} - {(x["scope"], x["key"]) for x in la["entries"]})
            return {"params": params, "result": r, "error": e and {"code": e.code, "message": e.message},
                    "notification": note and note["params"], "moved": moved, "unexpected_moves": unexpected,
                    "expected_but_unmoved": missing, "entries_removed": gone,
                    "before": rb, "after": ra}

        tk = None
        for x in d.call("proactive.learning", {})["entries"]:
            if x["scope"] == "thread" and (x.get("label") or "").startswith("Reply to Jo Brandt: Re: Staff designer role at Studio0"):
                tk = x["key"]
        if tk is None:  # fall back: the thread of jo-0c's card effects
            c = by_file(snap(d), "jo-0c")
            tk = next((e["key"] for e in c["learned"] if e["scope"] == "thread"), None)
        reset_ev["thread_key"] = tk
        reset_ev["sender"] = do_reset({"scope": "sender", "key": "  JO@TalentPeak.example "}, ["jo-35", "jo-0c"])
        reset_ev["domain"] = do_reset({"scope": "domain", "key": "talentpeak.example"}, ["jo-35", "jo-0c", "sam-35"])
        reset_ev["thread"] = do_reset({"scope": "thread", "key": tk}, ["jo-0c"])
        reset_ev["kind"] = do_reset({"scope": "kind", "key": "unanswered_thread"},
                                    ["jo-35", "sam-35", "priya-35", "deals-35", "sofia-35", "tom-35", "lee-35", "dana-35", "jo-0c"])
        # bad input: nothing reset, no audit, no notification, clear error
        bad = {}
        for name, p in [("unknown_scope", {"scope": "everything"}), ("missing_key", {"scope": "sender"}),
                        ("empty_key", {"scope": "domain", "key": "  "}), ("unknown_kind", {"scope": "kind", "key": "spam"}),
                        ("no_params", None), ("scope_case", {"scope": "Sender", "key": "deals.guy@gmail.com"}),
                        ("nonexistent_sender", {"scope": "sender", "key": "nobody@nowhere.example"}),
                        ("nonexistent_thread", {"scope": "thread", "key": "ffffffffffffffff"}),
                        ("public_domain", {"scope": "domain", "key": "gmail.com"})]:
            d.notes.clear()
            lb = d.call("proactive.learning", {})
            r, e = d.try_call("proactive.reset_learning", p)
            note = d.wait_note("cards.changed", timeout=1.5, pred=lambda n: n["params"]["reason"] == "reset_learning")
            la = d.call("proactive.learning", {})
            bad[name] = {"params": p, "result": r, "error": e and {"code": e.code, "message": e.message},
                         "notified": bool(note), "learning_unchanged": len(lb["entries"]) == len(la["entries"])}
        reset_ev["bad"] = bad
        reset_ev["all"] = do_reset({"scope": "all"}, ["jo-35", "sam-35", "priya-35", "deals-35", "sofia-35",
                                                       "tom-35", "lee-35", "dana-35", "jo-0c"])
        reset_ev["learning_after_all"] = d.call("proactive.learning", {})
        reset_ev["metrics_after_all"] = d.call("proactive.metrics", {})
        reset_ev["final_U"] = rank_map(state())
    finally:
        d.stop()
    d = tw.open("C", at36)
    try:
        cs = snap(d)
        reset_ev["final_C"] = {fid: (round(by_file(cs, fid)["rank"], 6), by_file(cs, fid)["_shown"])
                               for fid in reset_ev["final_U"] if by_file(cs, fid)}
    finally:
        d.stop()
    ev["resets"] = reset_ev
    for sc in ("sender", "domain", "thread", "kind", "all"):
        x = reset_ev[sc]
        rep.check(f"reset_{sc}_moves_only_its_cards", x["result"] and not x["unexpected_moves"] and not x["expected_but_unmoved"],
                  {k: x[k] for k in ("params", "result", "error", "moved", "unexpected_moves", "expected_but_unmoved", "entries_removed")})
        rep.check(f"reset_{sc}_broadcasts_cards_changed", x["notification"] is not None, {"notification": x["notification"]})
    rep.check("reset_sender_removes_only_sender_entry",
              reset_ev["sender"]["entries_removed"] == [["sender", "jo@talentpeak.example"]] or
              reset_ev["sender"]["entries_removed"] == [("sender", "jo@talentpeak.example")],
              {"removed": reset_ev["sender"]["entries_removed"]})
    fu, fc = reset_ev["final_U"], reset_ev["final_C"]
    # After forgetting Jo (sender), his firm (domain), his thread and the kind, his cards should be
    # back where they would be without any feedback.
    jo_after = {"jo-35": reset_ev["kind"]["after"].get("jo-35"), "jo-0c": reset_ev["kind"]["after"].get("jo-0c")}
    gap = {k: v and fc.get(k) and round(logit(v[0]) - logit(fc[k][0]), 3) for k, v in jo_after.items()}
    rep.check("scoped_resets_restore_jo_cards_to_no_feedback_rank", all(g is not None and abs(g) < 0.3 for g in gap.values()),
              {"jo_cards_after_sender_domain_thread_kind_resets": jo_after, "control_same_instant": {k: fc.get(k) for k in jo_after},
               "gap_logodds": gap, "remaining_learned_scopes": {k: v and v[2] for k, v in jo_after.items()}})
    mism = {k: (fu[k][0], fc[k][0]) for k in fu if k in fc and abs(fu[k][0] - fc[k][0]) > 2e-3}
    rep.check("reset_all_restores_control_ranks", not mism and not reset_ev["learning_after_all"]["entries"]
              and reset_ev["metrics_after_all"]["model_updates"] == 0,
              {"mismatch U vs C": mism, "final_U": fu, "final_C": fc,
               "model_updates_after": reset_ev["metrics_after_all"]["model_updates"]})
    b = reset_ev["bad"]
    for name in ("unknown_scope", "missing_key", "empty_key", "unknown_kind", "no_params"):
        rep.check(f"bad_reset_{name}_clear_error", b[name]["error"] and b[name]["error"]["code"] == -32602
                  and not b[name]["notified"] and b[name]["learning_unchanged"], b[name])
    rep.check("bad_reset_nonexistent_key_gives_clear_answer", any(b[n]["error"] for n in ("nonexistent_sender", "nonexistent_thread")),
              {n: b[n] for n in ("nonexistent_sender", "nonexistent_thread", "public_domain", "scope_case")})

    # ---- reset while [proactive] enabled = false ----
    # Learn something again (U only), then disable, reset, re-enable.
    at37 = D0 + timedelta(days=36)
    t = at37 - timedelta(hours=26)
    w.mail("lee-36", LEE, "Design lead opening, still?", t, "Are you still interested in the design lead opening?\nLee")
    w.mail("lee-36b", LEE, "Another opening for you?", t, "Could we talk about a second opening?\nLee")
    dis = {}
    d = tw.open("U", at37)
    try:
        cs = snap(d)
        c = by_file(cs, "lee-36")
        dis["dismiss"] = d.try_call("cards.feedback", {"card_id": c["id"], "feedback": "dismiss", "reason": "not_important"})[1]
        dis["lee_other_before"] = brief_card(by_file(snap(d), "lee-36b"))
    finally:
        d.stop()
    home = tw.homes["U"]
    cfg = (home.config / "config.toml").read_text()
    home.write_config(cfg.replace("[proactive]\nenabled = true", "[proactive]\nenabled = false"))
    d = Daemon(home, sim=at37 + timedelta(minutes=5), tz=TZ).start()
    try:
        dis["cards_list_disabled"] = d.cards()
        dis["learning_disabled"] = d.try_call("proactive.learning", {})
        d.notes.clear()
        r, e = d.try_call("proactive.reset_learning", {"scope": "sender", "key": "lee@hirewave.example"})
        dis["reset_disabled"] = {"result": r, "error": e and str(e)}
        n = d.wait_note("cards.changed", timeout=3, pred=lambda n: n["params"]["reason"] == "reset_learning")
        dis["notification"] = n and n["params"]
        dis["learning_after_reset"] = d.try_call("proactive.learning", {})
    finally:
        d.stop()
    home.write_config(cfg)
    d = Daemon(home, sim=at37 + timedelta(minutes=10), tz=TZ).start()
    try:
        d.call("proactive.refresh", {})
        dis["lee_other_after"] = brief_card(by_file(snap(d), "lee-36b"))
        dis["learning_reenabled"] = d.call("proactive.learning", {})
    finally:
        d.stop()
    ev["disabled"] = dis
    lr = dis["learning_disabled"][0]
    rep.check("reset_works_when_proactive_disabled",
              lr and entry(lr, "sender", "lee@hirewave.example") and dis["reset_disabled"]["result"] == {"cleared": 1}
              and dis["notification"] and not entry(dis["learning_reenabled"], "sender", "lee@hirewave.example")
              and not any(e[0] == "sender" for e in dis["lee_other_after"]["learned"]),
              {k: dis[k] for k in ("reset_disabled", "notification", "lee_other_before", "lee_other_after")})

    # ---- audit ----
    au = audit(home)
    recs = [r for r in au.get("records", []) if r["action"] == "proactive.reset_learning"]
    recs.sort(key=lambda r: r["seq"])
    got = [(r["actor"], r["target"], r["detail"].get("cleared")) for r in recs]
    want_targets = ["sender:jo@talentpeak.example", "domain:talentpeak.example", f"thread:{tk}",
                    "kind:unanswered_thread"]
    ev["audit"] = {"chain_ok": au.get("chain_ok"), "reset_records": recs, "error": au.get("error")}
    tg = [g[1] for g in got]
    rep.check("reset_writes_audit_entries", au.get("chain_ok") and tg[:4] == want_targets and "all" in tg
              and tg[-1] == "sender:lee@hirewave.example" and all(g[0] == "user" for g in got),
              {"records(actor,target,cleared)": got, "chain_ok": au.get("chain_ok")})
    rep.check("bad_resets_not_audited", not any(t.startswith(("everything", "kind:spam")) for t in tg), {"targets": tg})
    return ev


# ---------------------------------------------------------------------------------------------
# Scenario 2: bounded. The user dismisses their manager as not_important five days running; then
# a genuinely pressing message arrives. Does it ever come back?
# ---------------------------------------------------------------------------------------------

def scenario_bounded(out: Path, a, rep: Report):
    print("== scenario 2: bounded (manager dismissed as not_important x5)", flush=True)
    w = World(out / "s2" / "mail")
    tw = Twin(out, "s2", w, a)
    for i in range(10):
        w.mail(f"sent-priya-{i}", ME, f"Notes {i}", D0 - timedelta(days=20 - i), "Notes attached.", to=PRIYA)
    ev = {"days": []}
    for k in range(5):
        at = D0 + timedelta(days=k)
        w.mail(f"p-{k}", PRIYA, f"Weekly status {k}?", at - timedelta(hours=26), "Can you send me your weekly status?\nPriya")
        d = tw.open("U", at)
        try:
            c = by_file(snap(d), f"p-{k}")
            r, e = d.try_call("cards.feedback", {"card_id": c["id"], "feedback": "dismiss", "reason": "not_important"})
            ev["days"].append({"k": k, "card": brief_card(c), "error": e and str(e)})
        finally:
            d.stop()
    probes = {}
    for off in (5, 35, 95, 185):
        at = D0 + timedelta(days=off)
        # An urgent ask, 12 days old (high urgency), CC-free, direct, a question.
        fid = f"urgent-{off}"
        w.mail(fid, PRIYA, "URGENT: legal needs your sign-off on the release today?", at - timedelta(days=12),
               "Maya, legal needs your sign-off on the release today. Can you confirm by noon?\nPriya")
        res = {}
        for who in ("C", "U"):
            d = tw.open(who, at)
            try:
                c = by_file(snap(d), fid)
                res[who] = brief_card(c)
                if who == "U":
                    res["learning"] = [e for e in d.call("proactive.learning", {})["entries"] if e["scope"] in ("sender", "domain")]
            finally:
                d.stop()
        res["d_logodds"] = res["U"] and res["C"] and round(logit(res["U"]["rank"]) - logit(res["C"]["rank"]), 3)
        probes[off] = res
    ev["probes"] = probes
    shown = {off: (p["U"] or {}).get("shown") for off, p in probes.items()}
    first_back = next((off for off in sorted(probes) if shown[off]), None)
    rep.check("bounded_learned_adjustment_within_cap", all(abs(learned_sum({"learned": [{"scope": e[0], "contribution": e[6]} for e in p["U"]["learned"]]})) <= 4.0 + 1e-6
                                                       for p in probes.values() if p["U"]),
              {off: p["U"] and p["U"]["learned"] for off, p in probes.items()})
    rep.check("bounded_urgent_manager_mail_resurfaces_within_95_days", first_back is not None and first_back <= 95,
              {"shown_by_day_offset": shown, "first_shown": first_back,
               "ranks": {off: (p["U"] and p["U"]["rank"], p["C"] and p["C"]["rank"], p["d_logodds"]) for off, p in probes.items()}})
    return ev


# ---------------------------------------------------------------------------------------------
# Scenario 3: minimal repros. One fresh home per case, one session: rank of an untouched friend's
# card before and after a single dismissal of somebody else's card.
# ---------------------------------------------------------------------------------------------

def scenario_minimal(out: Path, a, rep: Report):
    print("== scenario 3: one dismissal, untouched friend", flush=True)
    ev = {}
    at = D0
    t = at - timedelta(hours=26)
    for case, reason in [("not_relevant", "not_relevant"), ("not_important", "not_important"),
                         ("already_handled", "already_handled"), ("wrong_info", "wrong_info"), ("snooze", None)]:
        w = World(out / "s3" / case / "mail")
        w.mail("jo-a", JO, "Staff designer role at StudioA?", t, "Would you be open to a call about a staff role?\nJo")
        for i in range(5):
            w.mail(f"jo-x{i}", JO, f"Another role {i}?", t, f"Would role {i} interest you?\nJo")
        w.mail("sofia-a", SOFIA, "dinner saturday?", t, "Are you free for dinner Saturday?\nS")
        w.mail("priya-a", PRIYA, "Can you review the deck?", t, "Can you review the deck today?\nPriya")
        home = Home(out / "s3" / case / "U", judge="off", runtime=a.runtime, chat_model=a.chat_model,
                    embed_model=a.embed_model, models_home=a.models_home)
        d = Daemon(home, sim=at, tz=TZ).start()
        try:
            d.sync(d.add_files_source(w.root)["id"])
            d.call("proactive.refresh", {})
            s0 = snap(d)
            r0 = {fid: by_file(s0, fid)["rank"] for fid in ("sofia-a", "priya-a")}
            c = by_file(s0, "jo-a")
            if case == "snooze":
                d.call("cards.feedback", {"card_id": c["id"], "feedback": "snooze"})
            else:
                d.call("cards.feedback", {"card_id": c["id"], "feedback": "dismiss", "reason": reason})
            s1 = snap(d)
            row = {"one": {fid: {"before": round(r0[fid], 4), "after": round(by_file(s1, fid)["rank"], 4),
                                 "d_logodds": round(logit(by_file(s1, fid)["rank"]) - logit(r0[fid]), 3),
                                 "learned": brief_card(by_file(s1, fid))["learned"],
                                 "factors_after": brief_card(by_file(s1, fid))["factors"]} for fid in r0}}
            if case == "not_relevant":
                for i in range(5):
                    c = by_file(snap(d), f"jo-x{i}")
                    d.call("cards.feedback", {"card_id": c["id"], "feedback": "dismiss", "reason": reason})
                s2 = snap(d)
                row["six"] = {fid: {"after": round(by_file(s2, fid)["rank"], 4), "eligible": by_file(s2, fid)["_shown"],
                                    "d_logodds": round(logit(by_file(s2, fid)["rank"]) - logit(r0[fid]), 3),
                                    "learned": brief_card(by_file(s2, fid))["learned"]} for fid in r0}
                d.call("proactive.reset_learning", {"scope": "sender", "key": "jo@talentpeak.example"})
                d.call("proactive.reset_learning", {"scope": "domain", "key": "talentpeak.example"})
                d.call("proactive.reset_learning", {"scope": "kind", "key": "unanswered_thread"})
                s3 = snap(d)
                row["after_sender_domain_kind_reset"] = {fid: {"after": round(by_file(s3, fid)["rank"], 4),
                                                               "d_logodds": round(logit(by_file(s3, fid)["rank"]) - logit(r0[fid]), 3),
                                                               "learned": brief_card(by_file(s3, fid))["learned"]} for fid in r0}
                row["metrics"] = d.call("proactive.metrics", {})
        finally:
            d.stop()
        ev[case] = row
    one = {k: v["one"]["sofia-a"]["d_logodds"] for k, v in ev.items()}
    rep.check("one_dismissal_of_X_moves_untouched_friend_less_than_0.25_logodds",
              all(abs(x) < 0.25 for x in one.values()),
              {"sofia_d_logodds_after_one_jo_feedback_by_reason": one,
               "sofia_learned_after": {k: v["one"]["sofia-a"]["learned"] for k, v in ev.items()},
               "sofia_factors_after_not_relevant": ev["not_relevant"]["one"]["sofia-a"]["factors_after"]})
    six = ev["not_relevant"]["six"]
    rep.check("six_recruiter_dismissals_keep_friend_and_manager_offered", six["sofia-a"]["eligible"] and six["priya-a"]["eligible"],
              {"six": six})
    rr = ev["not_relevant"]["after_sender_domain_kind_reset"]
    rep.check("scoped_resets_undo_bleed_on_friend", abs(rr["sofia-a"]["d_logodds"]) < 0.1,
              {"after_resets": rr, "model_updates": ev["not_relevant"]["metrics"]["model_updates"]})
    return ev


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--runtime", action="store_true")
    ap.add_argument("--chat-model")
    ap.add_argument("--embed-model")
    ap.add_argument("--models-home")
    ap.add_argument("--only", choices=["week", "bounded", "minimal"])
    a = ap.parse_args()
    out = Path(a.out).resolve()
    out.mkdir(parents=True, exist_ok=True)
    rep = Report()
    t0 = time.time()
    evidence = {}
    if a.only in (None, "week"):
        evidence["week"] = scenario_week(out, a, rep)
    if a.only in (None, "bounded"):
        evidence["bounded"] = scenario_bounded(out, a, rep)
    if a.only in (None, "minimal"):
        evidence["minimal"] = scenario_minimal(out, a, rep)
    (out / "evidence.json").write_text(json.dumps(evidence, indent=1, default=str))
    summary = {"report": rep.r, "secs": round(time.time() - t0, 1),
               "passed": sum(v["ok"] for v in rep.r.values()), "total": len(rep.r)}
    (out / "summary.json").write_text(json.dumps(summary, indent=1, default=str))
    print(f"{summary['passed']}/{summary['total']} checks passed in {summary['secs']}s -> {out}/summary.json")


if __name__ == "__main__":
    main()
