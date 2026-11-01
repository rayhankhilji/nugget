"""JJ1 search QA: synthetic corpus with known ground truth.

    python3 qa/JJ1/search/gen_corpus.py /tmp/jj1/search/corpus

Writes:
  files/   mail (.eml with Date headers in several zones), calendar (.ics: UTC, all-day, TZID),
           notes (.txt/.md whose mtime differs from any content date), sensitive items (IBAN,
           card number), one long note (> 20,000 chars), every item containing "quokka".
  bulk/    350 small notes containing "wombat" (pool limit / pagination).
  whatsapp/ a WhatsApp export (two months) containing "quokka".
  truth.json  the ground truth the scenarios check against.
Every file's mtime is pinned (MTIME) so a date taken from mtime is detectable.
"""
import json
import os
import sys
import calendar as _cal
from datetime import datetime, timezone, timedelta

MTIME = datetime(2025, 11, 20, 12, 0, tzinfo=timezone.utc).timestamp()

PEOPLE = {
    "alice": ("Alice Varga", "alice.varga@example.test"),
    "bob": ("Bob Okafor", "bob.okafor@example.test"),
    "chen": ("Chen Li", "chen.li@example.test"),
    "dana": ("Dana Ruiz", "dana.ruiz@example.test"),
}

# key, Date header, UTC instant, from, to, extra body, sensitive?
MAILS = [
    ("m01", "Thu, 15 Jan 2026 10:00:00 +0000", "2026-01-15T10:00:00Z", "alice", ["bob"], "", False),
    # 23:30 at -05:00 on 31 Jan is 04:30Z on 1 Feb: a New York reader's January, a UTC reader's February.
    ("m02", "Sat, 31 Jan 2026 23:30:00 -0500", "2026-02-01T04:30:00Z", "bob", ["alice"], "", False),
    # 00:30 at +01:00 on 1 Feb is 23:30Z on 31 Jan.
    ("m03", "Sun, 01 Feb 2026 00:30:00 +0100", "2026-01-31T23:30:00Z", "chen", ["alice"], "", False),
    ("m04", "Tue, 10 Mar 2026 09:00:00 +0000", "2026-03-10T09:00:00Z", "dana", ["chen"],
     "Please pay the invoice to IBAN DE89 3704 0044 0532 0130 00 by Friday.", True),
    # Berlin winter time: 23:30 CET on 15 Jan (22:30Z). A client sending today's (October, CEST)
    # offset +120 sees it on 16 Jan.
    ("m05", "Thu, 15 Jan 2026 23:30:00 +0100", "2026-01-15T22:30:00Z", "dana", ["alice"], "", False),
    ("m06", "Mon, 06 Apr 2026 14:00:00 +0200", "2026-04-06T12:00:00Z", "alice", ["dana"], "", False),
    ("m07", "Fri, 15 May 2026 08:00:00 +0000", "2026-05-15T08:00:00Z", "bob", ["chen"], "", False),
    ("m08", "Wed, 10 Jun 2026 16:45:00 +0000", "2026-06-10T16:45:00Z", "chen", ["bob", "dana"], "", False),
    ("m09", "Tue, 30 Jun 2026 23:59:59 +0000", "2026-06-30T23:59:59Z", "alice", ["chen"], "", False),
    ("m10", "Wed, 01 Jul 2026 00:00:00 +0000", "2026-07-01T00:00:00Z", "dana", ["bob"], "", False),
]

# key, ics body lines, UTC instant (as Nugget should date it), attendees
EVENTS = [
    ("e01", ["DTSTART:20260210T090000Z", "DTEND:20260210T100000Z"], "2026-02-10T09:00:00Z", ["alice", "bob"]),
    ("e02", ["DTSTART;VALUE=DATE:20260301", "DTEND;VALUE=DATE:20260302"], "2026-03-01", ["chen"]),
    ("e03", ["DTSTART;TZID=America/New_York:20260415T200000", "DTEND;TZID=America/New_York:20260415T210000"],
     "2026-04-16T00:00:00Z", ["dana", "alice"]),
]


def eml(key, date, frm, to, extra):
    fn, fa = PEOPLE[frm]
    tos = ", ".join(f'"{PEOPLE[t][0]}" <{PEOPLE[t][1]}>' for t in to)
    body = f"Hello,\n\nNotes on the quokka survey, item {key} (token zq{key}).\n{extra}\n\nRegards\n"
    return (f'From: "{fn}" <{fa}>\nTo: {tos}\nSubject: Quokka survey {key}\nDate: {date}\n'
            f"Message-ID: <{key}@example.test>\nMIME-Version: 1.0\nContent-Type: text/plain; charset=utf-8\n\n{body}")


def ics(key, lines, att):
    a = "\n".join(f'ATTENDEE;CN="{PEOPLE[p][0]}";PARTSTAT=ACCEPTED:mailto:{PEOPLE[p][1]}' for p in att)
    org = PEOPLE[att[0]]
    return ("BEGIN:VCALENDAR\nVERSION:2.0\nPRODID:-//JJ1//QA//EN\nBEGIN:VEVENT\n"
            f"UID:{key}@example.test\nDTSTAMP:20251101T000000Z\n" + "\n".join(lines) +
            f"\nSUMMARY:Quokka planning {key}\nDESCRIPTION:Quokka review, token zq{key}.\n"
            f'ORGANIZER;CN="{org[0]}":mailto:{org[1]}\n{a}\nEND:VEVENT\nEND:VCALENDAR\n')


def write(path, text, mtime=MTIME):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        f.write(text)
    os.utime(path, (mtime, mtime))


def main(out):
    truth = {"items": {}, "people": {k: {"name": v[0], "email": v[1]} for k, v in PEOPLE.items()}}
    fdir = os.path.join(out, "files")
    for key, date, utc, frm, to, extra, sens in MAILS:
        name = f"mail/{key}.eml"
        write(os.path.join(fdir, name), eml(key, date, frm, to, extra))
        truth["items"][name] = {"kind": "mail", "date": utc, "people": sorted({frm, *to}), "sensitive": sens}
    for key, lines, utc, att in EVENTS:
        name = f"cal/{key}.ics"
        write(os.path.join(fdir, name), ics(key, lines, att))
        truth["items"][name] = {"kind": "event", "date": utc, "people": sorted(att), "sensitive": False}
    # Notes: mtime is the date the guide promises for files.
    notes = [
        ("notes/f01.txt", "Field notes: quokka sightings near the ridge, token zqf01.\n", datetime(2026, 2, 20, 12, tzinfo=timezone.utc), False, []),
        # A note that names Alice in its text (no model: the guide says timelines show every item that mentions a person).
        ("notes/f02.md", "# Ridge walk\n\nWalked the ridge with Alice Varga; three quokka families, token zqf02.\n", datetime(2026, 3, 18, 12, tzinfo=timezone.utc), False, ["alice"]),
        ("notes/f03.txt", "Card on file: 4111 1111 1111 1111 exp 12/29 for the quokka sanctuary donation, token zqf03.\n", datetime(2026, 5, 2, 12, tzinfo=timezone.utc), True, []),
    ]
    for name, text, mt, sens, ppl in notes:
        write(os.path.join(fdir, name), text, mt.timestamp())
        truth["items"][name] = {"kind": "file", "date": mt.strftime("%Y-%m-%dT%H:%M:%SZ"), "people": ppl, "sensitive": sens,
                                "text_people": ppl}
    # Long note: > 20,000 chars, "quokka" at start and end, a marker past 20,000 chars.
    para = "The quokka colony census continues along the western trail with careful counts. "
    long = "LONGSTART quokka zqlong\n" + para * 300 + "\nPAST_PREVIEW_LIMIT_MARKER quokka\n"
    mt = datetime(2026, 4, 25, 12, tzinfo=timezone.utc)
    write(os.path.join(fdir, "notes/long.txt"), long, mt.timestamp())
    truth["items"]["notes/long.txt"] = {"kind": "file", "date": "2026-04-25T12:00:00Z", "people": [], "sensitive": False,
                                        "chars": len(long)}
    # Bulk: 350 notes with "wombat", dated one per day from 2025-01-01.
    for i in range(350):
        d = datetime(2025, 1, 1, 12, tzinfo=timezone.utc) + timedelta(days=i)
        write(os.path.join(out, "bulk", f"w{i:03d}.txt"), f"Wombat burrow log {i:03d}: one wombat seen.\n", d.timestamp())
    truth["bulk"] = 350
    # WhatsApp export: two months -> two documents (one per conversation per month).
    chat = [
        "14/02/2026, 09:15 - Alice Varga: Saw a quokka by the ferry, token zqwa1",
        "14/02/2026, 09:16 - Erin Moss: Lucky you!",
        "21/06/2026, 18:02 - Alice Varga: Another quokka at the jetty, token zqwa2",
        "",
    ]
    write(os.path.join(out, "whatsapp", "WhatsApp Chat with Alice Varga.txt"), "\n".join(chat))
    truth["chat"] = {"months": {"2026-02": "2026-02-14T09:16:00Z", "2026-06": "2026-06-21T18:02:00Z"}}
    with open(os.path.join(out, "truth.json"), "w") as f:
        json.dump(truth, f, indent=1)
    return truth


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "/tmp/jj1/search/corpus")
