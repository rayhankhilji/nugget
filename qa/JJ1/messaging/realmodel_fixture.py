#!/usr/bin/env python3
"""Small, realistic (synthetic) messaging corpus for Ask with a real local model.

    python3 qa/JJ1/messaging/realmodel_fixture.py --out DIR [--check]

Writes under DIR:
  whatsapp/   an iOS export zip (1:1) and an Android .txt export (group), me = "Robin"
  telegram/   a Telegram Desktop ChatExport_*/result.json (times in UTC so the gold is unambiguous)
  imessage/   chat.db (+ AddressBook/ for names)
  sources.json    list of sources.add params (absolute paths; `private` left at its default)
  questions.jsonl {"id", "q", "gold": [regex...] (all must match, case-insensitive),
                   "support": [substring of the supporting citation's title or url]}
--check starts target/debug/nuggetd on DIR/home, adds the sources and verifies with
retrieve.context that each question's supporting chat month is retrieved.
All people, numbers and places are invented.
"""
import argparse
import datetime as dt
import json
import os
import shutil
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import fx  # noqa: E402
from fx import T, wa_text, write_wa_txt, write_wa_zip  # noqa: E402

UTC = dt.timezone.utc

SAM = [
    (T(2026, 3, 12, 19, 2, 11), "Sam Rivera", "Hey! Did you get my postcard from Lisbon?"),
    (T(2026, 3, 12, 19, 5, 40), "Robin", "Yes, it's on the fridge 😄"),
    (T(2026, 3, 13, 8, 30, 0), "Sam Rivera", "Quick one: I'm finally coming back next week"),
    (T(2026, 3, 13, 8, 31, 12), "Sam Rivera", "My flight from Lisbon lands at 18:40 on Friday, terminal 2.\nTAP flight TP1350"),
    (T(2026, 3, 13, 8, 40, 3), "Robin", "Perfect, I'll pick you up. Text me when you're through passport control"),
    (T(2026, 3, 13, 8, 41, 0), "Sam Rivera", fx.LRM + "image omitted"),
    (T(2026, 3, 20, 21, 15, 0), "Robin", "Good to have you back! Still on for the camping trip?"),
    (T(2026, 3, 20, 21, 17, 30), "Sam Rivera", "Absolutely. I'll book the cabin at Loch Venn for the long weekend"),
    (T(2026, 4, 2, 10, 4, 0), "Sam Rivera", "Booked! The cabin booking reference is KX7Q2P, check-in from 3pm"),
    (T(2026, 4, 2, 10, 6, 0), "Robin", "Amazing. I'll bring the blue tent in case the cabin is cramped"),
    (T(2026, 4, 2, 10, 7, 0), "Sam Rivera", "And I'll sort the food. No mushrooms for Jo, remember"),
    (T(2026, 4, 28, 18, 0, 0), "Sam Rivera", "Back home. That was the best trip in years"),
]

FLAT = [
    (T(2026, 5, 1, 9, 0), None, "Priya Shah created group \"Flat 4B\""),
    (T(2026, 5, 1, 9, 1), None, "Priya Shah added Robin, Tom Becker"),
    (T(2026, 5, 1, 9, 5), "Priya Shah", "Landlord emailed: rent goes up to £1,450 a month from June"),
    (T(2026, 5, 1, 9, 7), "Tom Becker", "Ugh. Fine, but then he fixes the shower"),
    (T(2026, 5, 2, 18, 30), "Tom Becker", "Council letter: bin day moved to Wednesday starting next week"),
    (T(2026, 5, 2, 18, 31), "Robin", "<Media omitted>"),
    (T(2026, 5, 6, 7, 45), "Priya Shah", "The plumber, Marek, is coming on 12 May between 8 and 10am for the shower"),
    (T(2026, 5, 6, 7, 50), "Robin", "I'm working from home that day, I'll let him in"),
    (T(2026, 5, 20, 22, 10), "Tom Becker", "Who finished the oat milk"),
    (T(2026, 5, 20, 22, 12), "Priya Shah", "Guilty. Buying more tomorrow"),
]


def tg_msg(mid, when, frm, fid, text, **kw):
    m = {"id": mid, "type": "message", "date": when.strftime("%Y-%m-%dT%H:%M:%S"),
         "date_unixtime": str(int(when.replace(tzinfo=UTC).timestamp())), "from": frm, "from_id": fid, "text": text,
         "text_entities": [{"type": "plain", "text": text}] if isinstance(text, str) else []}
    m.update(kw)
    return m


def telegram_chat():
    dana, did = "Dana Kim", "user810000002"
    me, mid = "Robin", "user810000001"
    return {"name": dana, "type": "personal_chat", "id": 810000002, "messages": [
        tg_msg(1, T(2026, 4, 10, 17, 0), dana, did, "Hiking on Sunday? Weather looks dry"),
        tg_msg(2, T(2026, 4, 10, 17, 3), me, mid, "Yes! Where do we meet?"),
        tg_msg(3, T(2026, 4, 10, 17, 4), dana, did, ["Let's meet at the ", {"type": "bold", "text": "Pinecrest trailhead"},
                                                     " car park, it's the one after the old mill"]),
        tg_msg(4, T(2026, 4, 10, 17, 6), dana, did, "", photo="photos/photo_4@10-04-2026_17-06-00.jpg", photo_file_size=220133),
        tg_msg(5, T(2026, 4, 22, 12, 30), dana, did, "Big news: my sister Mira is getting married on 6 September in Porto!"),
        tg_msg(6, T(2026, 4, 22, 12, 35), me, mid, "That's wonderful, congratulations to her"),
        tg_msg(7, T(2026, 5, 3, 9, 15), dana, did, "I returned your drill, it's in the shed behind the bikes",
               edited="2026-05-03T09:16:00"),
        tg_msg(8, T(2026, 5, 3, 9, 20), me, mid, "Thanks Dana!", reply_to_message_id=7),
    ]}


def build(out):
    out = os.path.abspath(out)
    for sub in ("whatsapp", "telegram", "imessage"):
        shutil.rmtree(os.path.join(out, sub), ignore_errors=True)
    wa = os.path.join(out, "whatsapp")
    write_wa_zip(os.path.join(wa, "WhatsApp Chat - Sam Rivera.zip"), "_chat.txt",
                 wa_text("ios_uk", [(T(2026, 3, 12, 19, 0), "Sam Rivera", fx.LRM + "Messages and calls are end-to-end encrypted. No one outside of this chat, not even WhatsApp, can read or listen to them.")] + SAM))
    write_wa_txt(os.path.join(wa, "WhatsApp Chat with Flat 4B.txt"), wa_text("and_uk", FLAT))
    tg = os.path.join(out, "telegram")
    fx.tg_single_export(tg, "ChatExport_2026-05-10", telegram_chat())
    im = os.path.join(out, "imessage")
    os.makedirs(im, exist_ok=True)
    db = os.path.join(im, "chat.db")
    t = lambda *a: T(*a, tzinfo=UTC)
    extra = [
        (20, 1, t(2026, 2, 17, 12, 0), {"text": "Dinner moved to Lucia's on Thursday at 7:30pm, I booked for four", "handle_id": 1}),
        (21, 1, t(2026, 2, 17, 12, 3), {"text": "Great, see you there", "is_from_me": 1}),
        (22, 1, t(2026, 2, 18, 9, 0), {"attributedBody": fx.typedstream("The gate code for the garage is 4512 if you get there first"), "handle_id": 1}),
        (23, 3, t(2026, 4, 3, 20, 0), {"text": "Next book is The Left Hand of Darkness, we meet on the 30th", "handle_id": 3}),
    ]
    fx.build_chat_db(db, "rm", extra=extra)
    fx.build_address_book(os.path.join(im, "AddressBook"))
    sources = [
        {"kind": "whatsapp", "name": "WhatsApp", "settings": {"folder": wa, "me": "Robin"}},
        {"kind": "telegram", "name": "Telegram", "settings": {"path": tg}},
        {"kind": "imessage", "name": "iMessage", "settings": {"db_path": db, "contacts_dir": os.path.join(im, "AddressBook")}},
    ]
    qs = [
        ("m01", "When does Sam's flight from Lisbon land?", [r"18[:.]40|6[:.]40 ?pm"], ["Sam Rivera — March 2026"]),
        ("m02", "Which terminal is Sam's flight from Lisbon arriving at?", [r"terminal 2|\b2\b"], ["Sam Rivera — March 2026"]),
        ("m03", "What is the cabin booking reference Sam sent?", [r"KX7Q2P"], ["Sam Rivera — April 2026"]),
        ("m04", "Which tent did Robin say he would bring on the camping trip?", [r"blue"], ["Sam Rivera — April 2026"]),
        ("m05", "How much will the rent be from June according to the Flat 4B chat?", [r"1,?450"], ["Flat 4B — May 2026"]),
        ("m06", "What day is bin day now?", [r"wednesday"], ["Flat 4B — May 2026"]),
        ("m07", "When is the plumber Marek coming?", [r"12(th)? may|may 12", r"\b8\b|8 ?am|08"], ["Flat 4B — May 2026"]),
        ("m08", "Where did Dana suggest we meet for the hike?", [r"pinecrest"], ["Dana Kim — April 2026"]),
        ("m09", "Where and when is Mira's wedding?", [r"porto", r"6(th)? sept|sept\w* 6"], ["Dana Kim — April 2026"]),
        ("m10", "Where did Dana leave my drill?", [r"shed"], ["Dana Kim — May 2026"]),
        ("m11", "Where is dinner with Alex on Thursday?", [r"lucia"], ["Alex Moreno — February 2026"]),
        ("m12", "What is the garage gate code Alex sent me?", [r"4512"], ["Alex Moreno — February 2026"]),
        ("m13", "What is the next book for the book club?", [r"left hand of darkness"], ["Book Club"]),
    ]
    with open(os.path.join(out, "questions.jsonl"), "w", encoding="utf-8") as f:
        for i, q, gold, sup in qs:
            f.write(json.dumps({"id": i, "q": q, "gold": gold, "support": sup}, ensure_ascii=False) + "\n")
    with open(os.path.join(out, "sources.json"), "w") as f:
        json.dump(sources, f, indent=1, ensure_ascii=False)
    return out, sources, qs


def check(out, sources, qs):
    from common import NUGGETD
    from nq import Daemon
    import re
    home = os.path.join(out, "home")
    shutil.rmtree(home, ignore_errors=True)
    bad = 0
    with Daemon(home, nuggetd=NUGGETD) as d:
        for s in sources:
            sid = d.call("sources.add", s)["id"]
            d.wait_idle(source=sid)
        for i, q, gold, sup in qs:
            r = d.call("retrieve.context", {"query": q, "budget_tokens": 2000})
            cits = r.get("citations") or []
            found = [c for c in cits if any(x in (c.get("title") or "") + (c.get("url") or "") for x in sup)]
            golds = all(re.search(g, r["text"], re.I) for g in gold)
            ok = bool(found) and golds
            bad += not ok
            print(f"{'ok ' if ok else 'BAD'} {i} support={'yes' if found else 'no'} gold_in_context={golds} "
                  f"rank={cits.index(found[0]) + 1 if found else '-'} top={[c['title'] for c in cits[:3]]}")
    return bad


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--check", action="store_true")
    a = ap.parse_args()
    out, sources, qs = build(a.out)
    print(f"wrote corpus, sources.json and questions.jsonl ({len(qs)} questions) under {out}")
    if a.check:
        sys.exit(1 if check(out, sources, qs) else 0)


if __name__ == "__main__":
    main()
