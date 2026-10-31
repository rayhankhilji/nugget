#!/usr/bin/env python3
"""Synthetic persona for GG4: two weeks (Mon 2026-10-26 .. Sun 2026-11-08) of Maya Okafor's mail,
calendar and documents, plus three weeks of history before it. Everything here is invented.

Maya is a product designer at Larkspur Studio (larkspur.example) in New York
(America/New_York). US daylight saving ends Sun 2026-11-01 02:00. She flies to London on the
evening of Tue 2026-11-03 and is there (Europe/London, already on GMT) Wed 4 .. Sat 7 Nov.

`python3 gen.py OUT` writes:
- OUT/files/<path>: every .eml / .ics / .md file
- OUT/timeline.json: [{id, at (RFC 3339, when it arrives in Maya's files), path, kind, scenario}]
- OUT/truth.json: scenarios with what a good assistant should (and must not) surface, per day
- OUT/days.json: the simulated mornings ({date, tz, morning (local), brief_time})
"""
import json
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path
from zoneinfo import ZoneInfo

NY = ZoneInfo("America/New_York")
LDN = ZoneInfo("Europe/London")
ME = ("Maya Okafor", "maya@larkspur.example")
THEO = ("Theo Brandt", "theo@larkspur.example")
PRIYA = ("Priya Raman", "priya@larkspur.example")
DANA = ("Dana Whitfield", "dana@northwind-outfitters.example")
MARCUS = ("Marcus Lee", "marcus@northwind-outfitters.example")
SOFIA = ("Sofia Alvarez", "sofia.alvarez@gmail.com")
MOM = ("Ngozi Okafor", "ngozi.okafor@gmail.com")
JONAH = ("Jonah Pike", "jonah@pikeprint.example")
KELSEY = ("Kelsey Vance", "kelsey@talentbridge.example")
GUS = ("Gus Moreno", "gus.moreno@outlook.com")
HARRIET = ("Harriet Cole", "harriet@foundry-ldn.example")
PAULA = ("Paula Grant, RN", "paula.grant@riverside-clinic.example")
REBECCA = ("Rebecca Moss", "rebecca@mossplace-therapy.example")

ITEMS = []
TRUTH = []


def ny(s):
    return datetime.fromisoformat(s).replace(tzinfo=NY)


def ldn(s):
    return datetime.fromisoformat(s).replace(tzinfo=LDN)


def addr(p):
    return f"{p[0]} <{p[1]}>"


def rfc2822(t):
    return t.strftime("%a, %d %b %Y %H:%M:%S %z")


def add(path, text, at, kind, scenario, iid=None):
    iid = iid or Path(path).stem
    ITEMS.append({"id": iid, "at": at.isoformat(), "path": path, "kind": kind, "scenario": scenario,
                  "text": text})


def mail(iid, at, frm, to, subject, body, scenario, cc=(), reply_to_id=None, refs=(), headers=None,
         folder="Inbox"):
    h = [
        f"From: {addr(frm)}",
        "To: " + ", ".join(addr(p) for p in to),
    ]
    if cc:
        h.append("Cc: " + ", ".join(addr(p) for p in cc))
    h += [f"Subject: {subject}", f"Date: {rfc2822(at)}", f"Message-ID: <{iid}@mail.gg4.example>"]
    if reply_to_id:
        h.append(f"In-Reply-To: <{reply_to_id}@mail.gg4.example>")
        allrefs = list(refs) + [reply_to_id]
        h.append("References: " + " ".join(f"<{r}@mail.gg4.example>" for r in allrefs))
    for k, v in (headers or {}).items():
        h.append(f"{k}: {v}")
    h += ["MIME-Version: 1.0", "Content-Type: text/plain; charset=utf-8"]
    text = "\r\n".join(h) + "\r\n\r\n" + body.strip().replace("\n", "\r\n") + "\r\n"
    sent = frm == ME
    path = f"Mail/{'Sent' if sent else folder}/{at.strftime('%Y%m%d-%H%M')}-{iid}.eml"
    add(path, text, at, "mail-sent" if sent else "mail", scenario, iid)


def ics_dt(name, t, all_day=False):
    if all_day:
        return f"{name};VALUE=DATE:{t.strftime('%Y%m%d')}"
    if t.tzinfo is None:  # floating
        return f"{name}:{t.strftime('%Y%m%dT%H%M%S')}"
    if t.tzinfo == timezone.utc:
        return f"{name}:{t.strftime('%Y%m%dT%H%M%SZ')}"
    return f"{name};TZID={t.tzinfo.key}:{t.strftime('%Y%m%dT%H%M%S')}"


def event(iid, at, uid, summary, start, end, scenario, attendees=(), organizer=ME, location=None,
          seq=0, status=None, rrule=None, all_day=False, method="PUBLISH", description=None,
          partstat_me=None):
    lines = ["BEGIN:VCALENDAR", "VERSION:2.0", "PRODID:-//GG4 QA//Persona//EN", f"METHOD:{method}",
             "BEGIN:VEVENT", f"UID:{uid}", f"DTSTAMP:{at.astimezone(timezone.utc).strftime('%Y%m%dT%H%M%SZ')}",
             f"SEQUENCE:{seq}", f"SUMMARY:{summary}", ics_dt("DTSTART", start, all_day),
             ics_dt("DTEND", end, all_day), f"ORGANIZER;CN={organizer[0]}:mailto:{organizer[1]}"]
    for p in attendees:
        ps = ""
        if p == ME and partstat_me:
            ps = f";PARTSTAT={partstat_me}"
        lines.append(f"ATTENDEE;CN={p[0]}{ps}:mailto:{p[1]}")
    if location:
        lines.append(f"LOCATION:{location}")
    if description:
        lines.append(f"DESCRIPTION:{description}")
    if status:
        lines.append(f"STATUS:{status}")
    if rrule:
        lines.append(f"RRULE:{rrule}")
    lines += ["END:VEVENT", "END:VCALENDAR"]
    add(f"Calendar/{iid}.ics", "\r\n".join(lines) + "\r\n", at, "event", scenario, iid)


def doc(iid, at, path, text, scenario):
    add(path, text.strip() + "\n", at, "doc", scenario, iid)


def truth(sid, title, *, expect=(), forbid=(), note=""):
    """expect: [{kind, match (substring of title/detail/people, lowercase), first, last}] where
    first/last are the dates (inclusive) of the mornings a card for it should be open.
    forbid: [{match, why, from, to}] cards that must not appear (dates inclusive, optional)."""
    TRUTH.append({"id": sid, "title": title, "expect": list(expect), "forbid": list(forbid),
                  "note": note})


def build():
    # ---------------- history before the two weeks (arrives before day 1) ----------------
    for i, d in enumerate(["2026-10-02", "2026-10-07", "2026-10-13", "2026-10-20"]):
        mail(f"h-theo-{i}", ny(f"{d}T10:05"), ME, [THEO], f"Weekly design notes ({d})",
             "Hi Theo,\n\nNotes from this week: checkout flow, onboarding copy, icon audit.\n\nMaya", "history")
        mail(f"h-dana-{i}", ny(f"{d}T15:30"), ME, [DANA], f"Northwind checkout: status {d}",
             "Hi Dana,\n\nQuick status: wireframes done, visual pass next.\n\nBest,\nMaya", "history")
        mail(f"h-priya-{i}", ny(f"{d}T11:00"), ME, [PRIYA], "Handoff specs",
             "Priya, specs for the cart screen are in the shared folder.\nMaya", "history")
    mail("h-mom-0", ny("2026-10-11T19:00"), ME, [MOM], "Sunday call",
         "Hi Mom, can we call Sunday at 6? Love, Maya", "history")
    mail("h-sofia-0", ny("2026-10-17T12:00"), ME, [SOFIA], "brunch?",
         "Brunch next weekend? x", "history")
    mail("h-pixel-receipt", ny("2026-10-03T06:12"), ("Pixelboard", "receipts@pixelboard.example"), [ME],
         "Your Pixelboard receipt", "Payment received: Pixelboard Pro (monthly), $12.00, charged to Visa ending 4417.\nThanks for being a Pixelboard Pro customer.",
         "S5-renewal")
    mail("h-harriet-0", ny("2026-10-21T09:00"), HARRIET, [ME], "Foundry x Larkspur workshop in London",
         "Hi Maya,\n\nDelighted you can join the service design workshop at Foundry on 5 November.\nI'll send the details closer to the time.\n\nHarriet", "S12-harriet")
    mail("h-harriet-0r", ny("2026-10-21T13:00"), ME, [HARRIET], "Re: Foundry x Larkspur workshop in London",
         "Thanks Harriet, looking forward to it!\nMaya", "S12-harriet", reply_to_id="h-harriet-0")
    doc("doc-northwind-notes", ny("2026-10-22T17:00"), "Documents/Northwind/checkout-notes.md", """
# Northwind checkout redesign: notes

- Dana wants a one-page checkout; Marcus worries about address validation latency.
- Open question: guest checkout or forced account creation?
- Next review: Wednesday 28 Oct with Dana and Marcus.
""", "S4-meetings")
    doc("doc-todo", ny("2026-10-25T21:00"), "Documents/todo.md", """
# To do

- [ ] Send Jonah the final brochure files by Nov 4
- [ ] Book a London hotel for the Foundry workshop
- [x] Renew passport
""", "S10-notes")

    # Recurring internal standup and an all-day birthday.
    event("ev-standup", ny("2026-10-01T08:00"), "standup-gg4@larkspur.example", "Design standup",
          ny("2026-10-05T09:30"), ny("2026-10-05T09:45"), "S4-meetings", attendees=[ME, THEO, PRIYA],
          organizer=THEO, rrule="FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR", location="Zoom")
    event("ev-mom-bday", ny("2026-10-01T08:05"), "mom-bday-gg4@larkspur.example", "Mom's birthday",
          datetime(2026, 10, 31), datetime(2026, 11, 1), "S4-meetings", all_day=True)

    # ---------------- S1: client asks for mockups; Maya promises and delivers ----------------
    mail("s1-dana-ask", ny("2026-10-26T09:14"), DANA, [ME], "Revised checkout mockups",
         "Hi Maya,\n\nCould you send the revised checkout mockups by Thursday (Oct 29)? Marcus wants to review them before our sprint planning.\n\nThanks,\nDana",
         "S1-dana", cc=[MARCUS])
    # The same message saved twice (Inbox and a filtered folder): one card, not two.
    mail("s1-dana-ask", ny("2026-10-26T09:14"), DANA, [ME], "Revised checkout mockups",
         "Hi Maya,\n\nCould you send the revised checkout mockups by Thursday (Oct 29)? Marcus wants to review them before our sprint planning.\n\nThanks,\nDana",
         "S9-dup", cc=[MARCUS], folder="Clients")
    ITEMS[-1]["id"] = "s1-dana-ask-copy"
    mail("s1-maya-promise", ny("2026-10-27T16:02"), ME, [DANA], "Re: Revised checkout mockups",
         "Hi Dana,\n\nYes, I'll send the revised mockups by Thursday.\n\nMaya", "S1-dana",
         cc=[MARCUS], reply_to_id="s1-dana-ask")
    mail("s1-maya-deliver", ny("2026-10-29T11:20"), ME, [DANA], "Re: Revised checkout mockups",
         "Hi Dana,\n\nAttached are the revised checkout mockups (v3). Happy to walk through them.\n\nMaya",
         "S1-dana", cc=[MARCUS], reply_to_id="s1-maya-promise", refs=["s1-dana-ask"])
    truth("S1", "Dana asks for mockups by Thu; Maya answers Tue and delivers Thu",
          expect=[{"kind": "unanswered_thread", "match": "checkout mockups", "first": "2026-10-27", "last": "2026-10-27"}],
          forbid=[{"match": "checkout mockups", "from": "2026-10-28", "kind": "unanswered_thread",
                   "why": "Maya replied Tue 16:02"},
                  {"match": "checkout mockups", "from": "2026-10-30", "why": "delivered Thu 11:20"}],
          note="A follow-up card for 'I'll send ... by Thursday' on Wed 28/Thu 29 is fine.")

    # ---------------- S2: Maya promises Theo a budget by Friday and slips ----------------
    mail("s2-theo-ask", ny("2026-10-27T10:40"), THEO, [ME], "Q4 design budget",
         "Maya, can you send me the Q4 design budget estimate? Finance needs it next week.\nT.", "S2-budget")
    mail("s2-maya-promise", ny("2026-10-27T11:05"), ME, [THEO], "Re: Q4 design budget",
         "Sure, I'll send the budget estimate by Friday.\nMaya", "S2-budget", reply_to_id="s2-theo-ask")
    doc("doc-budget", ny("2026-10-30T15:00"), "Documents/Q4-budget-draft.md", """
# Q4 design budget (draft)

| Item | Cost |
|---|---|
| Usability testing (2 rounds) | $6,000 |
| Pixelboard Pro seats (4) | $576 |
| Illustration contractor | $4,500 |
""", "S2-budget")
    mail("s2-maya-deliver", ny("2026-11-02T09:10"), ME, [THEO], "Re: Q4 design budget",
         "Theo, here's the Q4 budget estimate: $11,076 total, details in the doc. Sorry it slipped past Friday.\nMaya",
         "S2-budget", reply_to_id="s2-maya-promise", refs=["s2-theo-ask"])
    truth("S2", "Maya promised Theo the budget by Fri Oct 30; sends it Mon Nov 2",
          expect=[{"kind": "follow_up", "match": "budget", "first": "2026-10-29", "last": "2026-11-02"}],
          forbid=[{"match": "budget", "from": "2026-11-03", "why": "sent Mon 09:10"}],
          note="Card should appear by the lead time (2 days before Fri) and stay (overdue) until she sends it.")

    # ---------------- S3: Maya asks a vendor; reply comes Fri ----------------
    mail("s3-maya-ask", ny("2026-10-26T13:30"), ME, [JONAH], "Quote for 500 brochures",
         "Hi Jonah,\n\nCould you quote 500 A5 brochures, 4 pages, matte? We'd need them by Nov 6. What's your turnaround?\n\nThanks,\nMaya",
         "S3-vendor")
    mail("s3-jonah-reply", ny("2026-10-30T14:45"), JONAH, [ME], "Re: Quote for 500 brochures",
         "Hi Maya,\n\n500 brochures come to $640, turnaround 4 working days from proof approval.\n\nJonah",
         "S3-vendor", reply_to_id="s3-maya-ask")
    truth("S3", "Maya waits on Jonah's quote (3+ days), he answers Fri afternoon",
          expect=[{"kind": "follow_up", "match": "brochure", "first": "2026-10-29", "last": "2026-10-30"},
                  {"kind": "unanswered_thread", "match": "brochure", "first": "2026-10-31", "last": "2026-11-02",
                   "optional": True}],
          forbid=[],
          note="'No answer yet' card from Thu 29 (3 days); after Jonah's reply an unanswered card for his quote is reasonable.")

    # ---------------- S4: meetings, DST, trip, moved and cancelled events ----------------
    event("ev-nw-review", ny("2026-10-23T12:00"), "nw-review-gg4@northwind-outfitters.example",
          "Northwind checkout review", ny("2026-10-28T14:00"), ny("2026-10-28T15:00"), "S4-meetings",
          attendees=[ME, DANA, MARCUS], organizer=DANA, location="Northwind office, 5th floor")
    event("ev-nw-qa-v0", ny("2026-10-26T09:00"), "nw-qa-gg4@northwind-outfitters.example",
          "Northwind design QA", ny("2026-10-30T13:00"), ny("2026-10-30T14:00"), "S4-meetings",
          attendees=[ME, MARCUS], organizer=MARCUS, method="REQUEST")
    event("ev-nw-qa-v1", ny("2026-10-29T08:00"), "nw-qa-gg4@northwind-outfitters.example",
          "Northwind design QA", ny("2026-10-30T15:00"), ny("2026-10-30T16:00"), "S4-meetings",
          attendees=[ME, MARCUS], organizer=MARCUS, seq=1, method="REQUEST")
    event("ev-maint", ny("2026-10-28T10:00"), "maint-gg4@larkspur.example", "Studio server maintenance",
          datetime(2026, 11, 1, 1, 30, tzinfo=NY), datetime(2026, 11, 1, 1, 30, fold=1, tzinfo=NY) + timedelta(minutes=30),
          "S4-dst", attendees=[ME, PRIYA], organizer=PRIYA)
    event("ev-nw-sprint", ny("2026-10-27T09:00"), "nw-sprint-gg4@northwind-outfitters.example",
          "Northwind sprint planning", ny("2026-11-02T10:00"), ny("2026-11-02T11:00"), "S4-dst",
          attendees=[ME, DANA, MARCUS], organizer=DANA, location="Zoom")
    event("ev-runclub", ny("2026-10-27T20:00"), "runclub-gg4@personal.example", "Run club",
          datetime(2026, 11, 2, 7, 0), datetime(2026, 11, 2, 8, 0), "S4-dst")
    event("ev-flight", ny("2026-10-20T18:00"), "flight-gg4@personal.example", "Flight BA178 JFK to LHR",
          ny("2026-11-03T19:00"), ldn("2026-11-04T07:00"), "S4-trip", location="JFK Terminal 8")
    event("ev-foundry", ldn("2026-10-29T09:00"), "foundry-ws-gg4@foundry-ldn.example",
          "Foundry service design workshop", ldn("2026-11-05T10:00"), ldn("2026-11-05T16:00"), "S4-trip",
          attendees=[ME, HARRIET], organizer=HARRIET, location="Foundry, 12 Clerkenwell Rd, London")
    event("ev-kelsey-coffee", ny("2026-10-27T15:00"), "kelsey-coffee-gg4@talentbridge.example",
          "Coffee with Kelsey (TalentBridge)", ny("2026-11-06T09:00"), ny("2026-11-06T09:30"), "S4-meetings",
          attendees=[ME, KELSEY], organizer=KELSEY, method="REQUEST")
    event("ev-kelsey-cancel", ny("2026-10-30T10:00"), "kelsey-coffee-gg4@talentbridge.example",
          "Coffee with Kelsey (TalentBridge)", ny("2026-11-06T09:00"), ny("2026-11-06T09:30"), "S4-meetings",
          attendees=[ME, KELSEY], organizer=KELSEY, seq=1, status="CANCELLED", method="CANCEL")
    event("ev-retro", ny("2026-10-30T09:00"), "retro-gg4@larkspur.example", "Studio retro",
          ny("2026-11-06T16:00"), ny("2026-11-06T17:00"), "S4-meetings", attendees=[ME, THEO, PRIYA],
          organizer=THEO)
    truth("S4", "Meetings: external prep cards, internal none, moved and cancelled events, DST, trip",
          expect=[{"kind": "meeting_prep", "match": "northwind checkout review", "first": "2026-10-27", "last": "2026-10-28"},
                  {"kind": "meeting_prep", "match": "northwind design qa", "first": "2026-10-29", "last": "2026-10-30"},
                  {"kind": "meeting_prep", "match": "northwind sprint planning", "first": "2026-11-01", "last": "2026-11-02"},
                  {"kind": "meeting_prep", "match": "foundry", "first": "2026-11-04", "last": "2026-11-05"}],
          forbid=[{"match": "design standup", "kind": "meeting_prep", "why": "internal recurring"},
                  {"match": "studio retro", "kind": "meeting_prep", "why": "internal"},
                  {"match": "kelsey", "kind": "meeting_prep", "from": "2026-10-30", "why": "cancelled Fri 30"}],
          note="Brief: QA at 15:00 (moved), sprint at 10:00 EST on Nov 2, workshop 10:00 GMT on Nov 5, birthday on Sat 31 only.")
    TRUTH[-1]["brief"] = [
        {"date": "2026-10-28", "meeting": "Northwind checkout review", "local": "14:00", "tz": "America/New_York"},
        {"date": "2026-10-30", "meeting": "Northwind design QA", "local": "15:00", "tz": "America/New_York",
         "not_local": "13:00"},
        {"date": "2026-10-31", "meeting": "Mom's birthday", "all_day": True},
        {"date": "2026-10-30", "absent": "Mom's birthday"},
        {"date": "2026-11-01", "absent": "Mom's birthday"},
        {"date": "2026-11-01", "meeting": "Studio server maintenance", "local": "01:30", "tz": "America/New_York"},
        {"date": "2026-11-02", "meeting": "Northwind sprint planning", "local": "10:00", "tz": "America/New_York"},
        {"date": "2026-11-02", "meeting": "Run club", "local": "07:00", "tz": "America/New_York"},
        {"date": "2026-11-03", "meeting": "Flight BA178", "local": "19:00", "tz": "America/New_York"},
        {"date": "2026-11-05", "meeting": "Foundry service design workshop", "local": "10:00", "tz": "Europe/London"},
        {"date": "2026-11-06", "absent": "Kelsey"},
        {"date": "2026-11-06", "meeting": "Studio retro", "local": "21:00", "tz": "Europe/London"},
        {"date": "2026-10-26", "meeting": "Design standup", "local": "09:30", "tz": "America/New_York"},
        {"date": "2026-11-02", "meeting": "Design standup", "local": "09:30", "tz": "America/New_York"},
        {"date": "2026-10-31", "absent": "Design standup"},
    ]

    # ---------------- S5: renewals ----------------
    mail("s5-pixel-notice", ny("2026-10-27T05:30"), ("Pixelboard", "billing@pixelboard.example"), [ME],
         "Your Pixelboard Pro plan renews soon",
         "Hi Maya,\n\nYour Pixelboard Pro plan switches to annual billing and renews on November 3, 2026 for $144.00.\nTo change your plan, visit your account settings.\n\nPixelboard",
         "S5-renewal")
    mail("s5-cloudvault-trial", ny("2026-10-29T07:00"), ("Cloudvault", "hello@cloudvault.example"), [ME],
         "Your Cloudvault trial ends November 5",
         "Your free trial of Cloudvault Plus ends on November 5, 2026. Upgrade for $9.99/month to keep your files.\n\nUnsubscribe: https://cloudvault.example/u",
         "S5-renewal", headers={"List-Unsubscribe": "<https://cloudvault.example/u>"})
    truth("S5", "Pixelboard renews Nov 3 for $144 (she pays them); Cloudvault trial she never paid",
          expect=[{"kind": "renewal", "match": "pixelboard", "first": "2026-10-28", "last": "2026-11-03"}],
          forbid=[{"match": "cloudvault", "why": "trial of a service she never paid; marketing"}])

    # ---------------- S6: sensitive ----------------
    mail("s6-lab", ny("2026-10-28T06:40"), ("Riverside Clinic", "noreply@riverside-clinic.example"), [ME],
         "Your test results are available",
         "Your recent lab results (lipid panel, HbA1c) are available in the patient portal.", "S6-sensitive")
    mail("s6-nurse", ny("2026-10-29T10:15"), PAULA, [ME], "Your results - please call",
         "Hi Maya,\n\nDr. Hale reviewed your HbA1c result and would like to discuss starting metformin. Could you call us at 555-0142 to book a follow-up this week?\n\nPaula Grant, RN\nRiverside Clinic",
         "S6-sensitive")
    mail("s6-bank", ny("2026-10-30T02:11"), ("First Harbor Bank", "alerts@firstharbor-bank.example"), [ME],
         "Fraud alert: unusual charge on card ending 4417",
         "We noticed a charge of $1,284.00 at ELECTRO-MART on 10/29. Reply YES if this was you, NO if not.",
         "S6-sensitive")
    mail("s6-hr", ny("2026-10-30T16:00"), ("Larkspur People Team", "hr@larkspur.example"), [ME],
         "Confidential: your 2027 compensation",
         "Maya, your salary for 2027 will be $118,500 (+6%). Please don't share this letter.", "S6-sensitive")
    mail("s6-therapy", ny("2026-11-02T08:20"), REBECCA, [ME], "Thursday session",
         "Hi Maya, can we move Thursday's session to 5pm? Let me know. - Rebecca", "S6-sensitive")
    mail("s6-pw", ny("2026-11-04T07:05"), ("Pixelboard", "security@pixelboard.example"), [ME],
         "Your Pixelboard verification code", "Your verification code is 482-913. It expires in 10 minutes.",
         "S6-sensitive")
    truth("S6", "Health, money, salary, therapy, codes",
          expect=[{"kind": "unanswered_thread", "match": "results", "first": "2026-10-30", "last": "2026-11-03",
                   "sensitive": True, "optional": True}],
          forbid=[{"match": "verification code", "why": "one-time code"},
                  {"match": "compensation", "why": "HR letter, no action asked"},
                  {"match": "test results are available", "why": "automated portal notice"}],
          note="If clinic/therapy cards appear they must be marked sensitive; the brief should not quote diagnoses.")

    # ---------------- S7: personal ----------------
    mail("s7-mom", ny("2026-10-28T19:30"), MOM, [ME], "Thanksgiving",
         "Hi sweetheart, are you coming home for Thanksgiving? Let me know by next week so I can tell your aunt. Love, Mom",
         "S7-personal")
    mail("s7-sofia", ny("2026-10-29T12:10"), SOFIA, [ME], "drinks friday?",
         "Drinks Friday after work? The usual place?", "S7-personal")
    mail("s7-sofia-r", ny("2026-10-29T12:40"), ME, [SOFIA], "Re: drinks friday?",
         "Yes! 7pm?", "S7-personal", reply_to_id="s7-sofia")
    mail("s7-mom-r", ny("2026-11-05T21:00"), ME, [MOM], "Re: Thanksgiving",
         "Yes Mom, I'll be home Wednesday night. Love you", "S7-personal", reply_to_id="s7-mom")
    truth("S7", "Mom asks about Thanksgiving (answered Nov 5); Sofia answered same day",
          expect=[{"kind": "unanswered_thread", "match": "thanksgiving", "first": "2026-10-30", "last": "2026-11-05"}],
          forbid=[{"match": "drinks", "why": "answered in 30 minutes"},
                  {"match": "thanksgiving", "from": "2026-11-06", "why": "answered Nov 5"}])

    # ---------------- S8: noise ----------------
    noise = [
        ("n-news1", "2026-10-26T06:00", ("UX Weekly", "digest@uxweekly.example"), "UX Weekly #212: dark patterns", {"List-Unsubscribe": "<mailto:u@uxweekly.example>"}),
        ("n-promo", "2026-10-27T07:00", ("Sole Supply", "deals@solesupply.example"), "48 hours only: 30% off boots - ends Thursday", {"List-Unsubscribe": "<https://solesupply.example/u>"}),
        ("n-gh", "2026-10-28T03:00", ("Builds", "notifications@builds.example"), "[larkspur/site] Build failed on main", {"Auto-Submitted": "auto-generated"}),
        ("n-ooo", "2026-10-29T09:00", DANA, "Automatic reply: Revised checkout mockups", {"Auto-Submitted": "auto-replied"}),
        ("n-accept", "2026-10-26T09:40", MARCUS, "Accepted: Northwind design QA", {}),
        ("n-news2", "2026-11-02T06:00", ("UX Weekly", "digest@uxweekly.example"), "UX Weekly #213: can you answer this survey?", {"List-Unsubscribe": "<mailto:u@uxweekly.example>"}),
        ("n-airline", "2026-11-02T11:00", ("BA", "noreply@ba-flights.example"), "Check in now for BA178 to London", {}),
    ]
    for iid, at, frm, subj, hdr in noise:
        mail(iid, ny(at), frm, [ME], subj, "This is an automated message. Do you want to manage your preferences?", "S8-noise", headers=hdr)
    mail("n-kelsey", ny("2026-10-27T14:00"), KELSEY, [ME], "Senior designer role at a fintech?",
         "Hi Maya, I'm hiring for a senior product designer at a fintech startup. Would you be open to a quick chat?",
         "S8-noise")
    truth("S8", "Newsletters, promos, CI, auto-replies, acceptances, airline notices: no cards",
          forbid=[{"match": "ux weekly", "why": "newsletter"}, {"match": "boots", "why": "promo"},
                  {"match": "build failed", "why": "CI"}, {"match": "automatic reply", "why": "auto-reply"},
                  {"match": "accepted:", "why": "calendar response"}, {"match": "check in now", "why": "airline bulk"}],
          note="Recruiter cold mail (Kelsey) is acceptable only ranked low.")

    # ---------------- S11 / S12 ----------------
    mail("s11-gus", ny("2026-10-30T18:30"), GUS, [ME], "Radiator repair",
         "Hi Maya, can I come in Thursday Nov 5 at 10am to fix the radiator? Please confirm so I can book the plumber. Gus",
         "S11-landlord")
    mail("s11-maya", ny("2026-11-02T20:00"), ME, [GUS], "Re: Radiator repair",
         "Hi Gus, Thursday works - I'm traveling, the spare key is with the doorman. Maya", "S11-landlord", reply_to_id="s11-gus")
    truth("S11", "Landlord asks to confirm access; Maya answers Mon evening",
          expect=[{"kind": "unanswered_thread", "match": "radiator", "first": "2026-10-31", "last": "2026-11-02"}],
          forbid=[{"match": "radiator", "from": "2026-11-03", "why": "answered Mon 20:00"}])
    mail("s12-harriet-ask", ldn("2026-10-29T09:15"), HARRIET, [ME], "Workshop agenda",
         "Hi Maya,\n\nCould you send me your agenda for the 5 November workshop by Monday 2 November? We start at 10:00 London time.\n\nHarriet",
         "S12-harriet", reply_to_id="h-harriet-0r", refs=["h-harriet-0"])
    mail("s12-maya-agenda", ny("2026-11-02T12:00"), ME, [HARRIET], "Re: Workshop agenda",
         "Hi Harriet, agenda attached: 10:00 intro, 11:00 journey mapping, 14:00 prototyping. See you Thursday!\nMaya",
         "S12-harriet", reply_to_id="s12-harriet-ask", refs=["h-harriet-0", "h-harriet-0r"])
    truth("S12", "Harriet needs the agenda by Mon Nov 2 (sent Mon noon)",
          expect=[{"kind": "unanswered_thread", "match": "agenda", "first": "2026-10-30", "last": "2026-11-02"}],
          forbid=[{"match": "agenda", "kind": "unanswered_thread", "from": "2026-11-03", "why": "sent Mon 12:00"}])
    truth("S10", "Note to-do: send Jonah the brochure files by Nov 4",
          expect=[{"kind": "any", "match": "brochure files", "first": "2026-11-02", "last": "2026-11-04", "optional": True}])


DAYS = []


def days():
    d0 = datetime(2026, 10, 26)
    for i in range(14):
        d = d0 + timedelta(days=i)
        tz = "Europe/London" if datetime(2026, 11, 4) <= d <= datetime(2026, 11, 7) else "America/New_York"
        DAYS.append({"day": i + 1, "date": d.strftime("%Y-%m-%d"), "weekday": d.strftime("%a"), "tz": tz,
                     "morning": d.strftime("%Y-%m-%d") + "T07:50", "brief_time": "08:00"})


def main():
    out = Path(sys.argv[1] if len(sys.argv) > 1 else Path(__file__).parent / "world")
    build()
    days()
    for it in ITEMS:
        p = out / "files" / it["path"]
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(it["text"], newline="")
    tl = [{k: v for k, v in it.items() if k != "text"} for it in sorted(ITEMS, key=lambda x: x["at"])]
    (out / "timeline.json").write_text(json.dumps(tl, indent=1))
    (out / "truth.json").write_text(json.dumps(TRUTH, indent=1))
    (out / "days.json").write_text(json.dumps(DAYS, indent=1))
    print(f"{len(ITEMS)} items, {len(TRUTH)} scenarios, {len(DAYS)} days -> {out}")


if __name__ == "__main__":
    main()
