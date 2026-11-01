"""WhatsApp scenarios."""
import os
import re
import resource
import shutil
import socket
import subprocess
import time
import zipfile

import fx
from common import Check, add, daemon, docs_for, fresh, hits, preview, short, source, wait_for
from fx import LOCALE, T, wa_text, write_wa_txt, write_wa_zip


def header_of(text, token):
    """The `**Name** · stamp` header line of the message whose text contains `token`."""
    lines = text.split("\n")
    for i, l in enumerate(lines):
        if token in l and l.startswith(">"):
            for j in range(i, -1, -1):
                if lines[j].startswith("**"):
                    return lines[j]
    return None


def find_token(d, sid, token):
    """(title, preview text, header line) of the first doc containing `token`."""
    for k, p in docs_for(d, token, sid).items():
        if token in p["text"]:
            return p["title"], p["text"], header_of(p["text"], token), p
    return None, None, None, None


LOCALE_CHATS = [
    # style, locale, file name, a, b, token, expected chat name
    ("and_us", "en_and", "WhatsApp Chat with Sam Lee.txt", "Sam Lee", "Robin Park", "zqus", "Sam Lee"),
    ("and_uk", "en_and", "WhatsApp Chat with Olivia Hart.txt", "Olivia Hart", "Robin Park", "zquk", "Olivia Hart"),
    ("and_de", "de_and", "WhatsApp Chat mit Anna Schmidt.txt", "Anna Schmidt", "Jonas Weber", "zqde", "Anna Schmidt"),
    ("and_es", "es_and", "Chat de WhatsApp con Lucía.txt", "Lucía", "Mateo", "zqes", "Lucía"),
    ("and_fr", "fr_and", "Discussion WhatsApp avec Chloé.txt", "Chloé", "Hugo", "zqfr", "Chloé"),
    ("and_ru", "ru_and", "Чат WhatsApp с Саша.txt", "Саша", "Дима", "zqru", "Саша"),
    ("and_ar", "ar_and", "دردشة واتساب مع ليلى.txt", "ليلى", "عمر", "zqar", "ليلى"),
    ("and_ar_eastern", "ar_and", "WhatsApp Chat with Omar.txt", "Omar", "Layla", "zqae", "Omar"),
]


def s_locales():
    base = fresh("wa_locales")
    folder = os.path.join(base, "exports")
    for style, loc, fname, a, b, tok, _ in LOCALE_CHATS:
        write_wa_txt(os.path.join(folder, fname), wa_text(style, fx.wa_locale_chat(style, loc, a, b, tok)))
    # iOS exports: en_US zip with _chat.txt + one media file, German and Russian zips.
    write_wa_zip(os.path.join(folder, "WhatsApp Chat - Mia Chen.zip"), "_chat.txt",
                 wa_text("ios_us", fx.wa_ios_chat("ios_us", "Mia Chen", "Mia Chen", "Robin Park", "zqios")),
                 media=[("00000007-PHOTO-2026-05-01-08-03-30.jpg", b"\xff\xd8" + b"0" * 5000)])
    de = [
        (T(2026, 3, 14, 9, 5, 0), "Familie", fx.LRM + "Nachrichten und Anrufe sind Ende-zu-Ende-verschlüsselt. Niemand außerhalb dieses Chats kann sie lesen oder anhören, nicht einmal WhatsApp."),
        (T(2026, 3, 14, 9, 6, 0), "Anna Schmidt", "zqiosdea Wer bringt den Kuchen?"),
        (T(2026, 3, 14, 9, 7, 0), "Jonas Weber", fx.LRM + "Bild weggelassen"),
        (T(2026, 3, 14, 9, 8, 0), "Oma Ilse", "zqiosdeb Ich!"),
        (T(2026, 3, 14, 9, 9, 0), "Anna Schmidt", fx.LRM + "Anna Schmidt hat Oma Ilse hinzugefügt"),
    ]
    write_wa_zip(os.path.join(folder, "WhatsApp Chat - Familie.zip"), "_chat.txt", wa_text("ios_de", de))
    ru = [
        (T(2026, 3, 13, 21, 15, 42), "Саша", "zqiosrua Привет, как дела?"),
        (T(2026, 3, 13, 21, 16, 0), "Robin Park", fx.LRM + "изображение отсутствует"),
        (T(2026, 3, 13, 21, 17, 0), "Саша", "zqiosrub отлично"),
    ]
    write_wa_zip(os.path.join(folder, "WhatsApp Chat - Саша.zip"), "_chat.txt", wa_text("ios_ru", ru))
    results = []
    with daemon(base) as d:
        sid = add(d, "whatsapp", {"folder": folder, "me": "Robin Park"})
        src = source(d, sid)
        for style, loc, fname, a, b, tok, chat in LOCALE_CHATS:
            c = Check(f"wa.locale.{style}")
            title, text, hdr, p = find_token(d, sid, tok + "a")
            c.ok(title == f"{chat} — March 2026", f"{tok}a is in '{chat} — March 2026'", title)
            c.ok(hdr is not None and "2026-03-14 09:06" in hdr, "first message dated 2026-03-14 09:06", hdr)
            title4, text4, hdr4, _ = find_token(d, sid, tok + "d")
            c.ok(title4 == f"{chat} — April 2026" and hdr4 and "2026-04-02 22:45" in hdr4,
                 "April message in the April doc at 2026-04-02 22:45", [title4, hdr4])
            _, t2, _, _ = find_token(d, sid, tok + "c")
            c.ok(t2 is not None and re.search(rf"> {tok}b first line\n> second line {tok}c", t2 or ""),
                 "multi-line message kept as one message", short(t2 or "", 200))
            if text:
                ph = LOCALE[loc][1].strip("<>")
                c.ok(ph not in text and "> [file]" in text, f"media placeholder '{LOCALE[loc][1]}' becomes an attachment",
                     short(text[text.find("2026-03-20"):][:120] if "2026-03-20" in text else text[-300:], 200))
                c.ok("_(message deleted)_" in text, "deleted notice recognised", None)
                c.ok("_2026-03-14 09:05 ·" in text, "encryption notice rendered as a system line", None)
                c.ok("**Me**" in text or b != "Robin Park", "`me` (Robin Park) shown as Me", None)
            results.append(c.result())
        # iOS en_US
        c = Check("wa.ios_us.zip")
        title, text, hdr, p = find_token(d, sid, "zqiosa")
        c.ok(title == "Mia Chen — May 2026", "chat name from 'WhatsApp Chat - Mia Chen.zip'", title)
        c.ok(hdr and "2026-05-01 08:01" in hdr, "12 h iOS time (8:01:05 AM with U+202F) -> 08:01", hdr)
        c.ok(text and "> [photo]" in text, "'image omitted' -> [photo]", None)
        c.ok(text and "[photo: 00000007-PHOTO-2026-05-01-08-03-30.jpg]" in text, "<attached: ...jpg> -> named photo", None)
        c.ok(text and "[file: Itinerary.pdf]" in text, "'Itinerary.pdf • 2 pages document omitted' -> named file", None)
        c.ok(text and "**Me** · 2026-05-01 08:02" in text, "me=Robin Park -> Me", None)
        c.ok(text and "end-to-end encrypted" in text and "**Mia Chen** · 2026-05-01 08:00" not in text,
             "LRM-led encryption line is a system line, not a message from Mia", None)
        t5, _, h5, _ = find_token(d, sid, "zqiosc")
        t6, _, h6, _ = find_token(d, sid, "zqiosd")
        c.ok(t5 == "Mia Chen — May 2026" and t6 == "Mia Chen — June 2026", "23:59 31 May in May, 00:00:30 1 June in June", [t5, t6])
        if p:
            md = p["_hit"]
            c.ok(md.get("date", "").startswith("2026-05-31T23:59"), "hit date = last message of the month", md.get("date"))
            c.ok(md.get("sensitive") is True, "private by default: hit sensitive", md.get("sensitive"))
        results.append(c.result())
        c = Check("wa.ios_de_group.zip")
        title, text, hdr, p = find_token(d, sid, "zqiosdea")
        c.ok(title == "Familie — March 2026", "German iOS group chat title", title)
        c.ok(hdr and "2026-03-14 09:06" in hdr, "dd.mm.yy date", hdr)
        c.ok(text and "> [photo]" in text and "weggelassen" not in text, "'Bild weggelassen' -> [photo]", None)
        c.ok(text and "hinzugefügt" in text and "_2026-03-14 09:09" in text, "German LRM system line is a system line", None)
        results.append(c.result())
        c = Check("wa.ios_ru.zip")
        title, text, hdr, p = find_token(d, sid, "zqiosrua")
        c.ok(title == "Саша — March 2026", "Cyrillic chat name", title)
        c.ok(hdr and "2026-03-13 21:15" in hdr, "dd.mm.yyyy date", hdr)
        c.ok(text and "> [photo]" in text and "отсутствует" not in text, "'изображение отсутствует' -> [photo]",
             short(text[text.find("21:16"):][:80] if text and "21:16" in text else "", 120))
        results.append(c.result())
        for r in results:
            r.setdefault("notes", []).append(f"source state={src and src['state']}")
    return results


def s_ambiguous_dates():
    """Day-first locales with a 12 h clock (en_IN), all days <= 12."""
    base = fresh("wa_ambig")
    folder = os.path.join(base, "exports")
    m = [(T(2026, 5, 3, 21, 0), "Priya", "zqina dinner on the third of May"),
         (T(2026, 6, 11, 9, 30), "Robin Park", "zqinb reply on the eleventh of June")]
    write_wa_txt(os.path.join(folder, "WhatsApp Chat with Priya.txt"), wa_text("and_in", m))
    m2 = [(T(2026, 5, 4, 21, 0), "Ana", "zquka 24h day-first chat on 4 May"),
          (T(2026, 7, 9, 9, 30), "Ana", "zqukb 9 July")]
    write_wa_txt(os.path.join(folder, "WhatsApp Chat with Ana.txt"), wa_text("and_uk", m2))
    out = []
    with daemon(base) as d:
        sid = add(d, "whatsapp", {"folder": folder})
        c = Check("wa.dates.ambiguous_en_IN")
        t, _, h, _ = find_token(d, sid, "zqina")
        c.ok(t == "Priya — May 2026" and h and "2026-05-03 21:00" in h,
             "en_IN '03/05/26, 9:00 pm' read as 3 May 2026", [t, h])
        t, _, h, _ = find_token(d, sid, "zqinb")
        c.ok(t == "Priya — June 2026", "'11/06/26' read as 11 June 2026", [t, h])
        out.append(c.result())
        c = Check("wa.dates.ambiguous_en_GB_24h")
        t, _, h, _ = find_token(d, sid, "zquka")
        c.ok(t == "Ana — May 2026" and h and "2026-05-04 21:00" in h, "en_GB 24 h, days<=12 read day-first", [t, h])
        out.append(c.result())
        # Re-export of the en_IN chat, now with an unambiguous day (25/06): the old export (still
        # in the folder) and the new one disagree about the order.
        m3 = m + [(T(2026, 6, 25, 10, 0), "Priya", "zqinc the twenty-fifth")]
        write_wa_txt(os.path.join(folder, "WhatsApp Chat with Priya (1).txt"), wa_text("and_in", m3))
        d.call("sources.sync", {"id": sid})
        time.sleep(2)
        d.wait_idle(source=sid)
        c = Check("wa.dates.reexport_disambiguates")
        docs = docs_for(d, "zqina", sid)
        n = sum(p["text"].count("zqina") for p in docs.values())
        c.ok(n == 1, "re-export with an unambiguous day does not duplicate the ambiguous messages",
             {"occurrences": n, "titles": sorted(p["title"] for p in docs.values())})
        out.append(c.result())
    return out


def s_reexport():
    base = fresh("wa_reexport")
    folder = os.path.join(base, "exports")
    msgs = [(T(2026, 4, 1 + i, 10, i), "Olivia Hart" if i % 2 else "Robin Park", f"zqre{chr(97 + i)} message {i}") for i in range(10)]
    msgs.insert(3, (T(2026, 4, 3, 10, 2), "Olivia Hart", "ok"))
    msgs.insert(4, (T(2026, 4, 3, 10, 2), "Olivia Hart", "ok"))
    write_wa_txt(os.path.join(folder, "WhatsApp Chat with Olivia Hart.txt"), wa_text("and_uk", msgs))
    out = []
    with daemon(base) as d:
        sid = add(d, "whatsapp", {"folder": folder, "me": "Robin Park"})
        items1 = source(d, sid)["items"]
        c = Check("wa.reexport.both_present")
        _, text, _, _ = find_token(d, sid, "zqrea")
        c.ok(text and text.count("> ok\n") == 2, "two identical 'ok' in the same minute both kept", text and text.count("> ok\n"))
        more = msgs + [(T(2026, 4, 20, 9, 0), "Olivia Hart", "zqrenew newer message"),
                       (T(2026, 5, 2, 9, 0), "Robin Park", "zqremay may message")]
        # the phone re-exports into a zip named like the first
        write_wa_zip(os.path.join(folder, "WhatsApp Chat with Olivia Hart (1).zip"), "WhatsApp Chat with Olivia Hart.txt",
                     wa_text("and_uk", more))
        d.call("sources.sync", {"id": sid})
        time.sleep(2)
        d.wait_idle(source=sid)
        _, text, _, _ = find_token(d, sid, "zqrea")
        c.ok(text and text.count("zqrea") == 1 and text.count("> ok\n") == 2 and "zqrenew" in text,
             "old + new export: no duplicates, new message added", text and {"zqrea": text.count("zqrea"), "ok": text.count("> ok\n")})
        st = source(d, sid)
        c.ok(st["items"] == items1 + 1, "one more month doc (May), no duplicate docs", [items1, st["items"]])
        out.append(c.result())
        c = Check("wa.reexport.old_deleted")
        os.remove(os.path.join(folder, "WhatsApp Chat with Olivia Hart.txt"))
        d.call("sources.sync", {"id": sid})
        time.sleep(2)
        d.wait_idle(source=sid)
        _, text, _, _ = find_token(d, sid, "zqrej")
        c.ok(text and text.count("zqrea") == 1 and "zqrenew" in text, "after deleting old export the chat is intact", None)
        c.ok(source(d, sid)["items"] == items1 + 1, "same item count", source(d, sid)["items"])
        out.append(c.result())
        c = Check("wa.reexport.message_deleted_on_phone")
        # Next export: message 'zqreb' was deleted for everyone on the phone, old exports removed.
        newer = [m for m in more if "zqreb" not in m[2]]
        os.remove(os.path.join(folder, "WhatsApp Chat with Olivia Hart (1).zip"))
        write_wa_txt(os.path.join(folder, "WhatsApp Chat with Olivia Hart (2).txt"), wa_text("and_uk", newer))
        d.call("sources.sync", {"id": sid})
        time.sleep(2)
        d.wait_idle(source=sid)
        r = hits(d, "zqreb", sid)
        c.ok(r["total"] == 0, "a message missing from the only remaining export is gone", r["total"])
        out.append(c.result())
    return out


def s_half_written():
    base = fresh("wa_half")
    folder = os.path.join(base, "exports")
    full = wa_text("ios_uk", [(T(2026, 2, 1, 9, 0, i), "Leo", f"zqhw{chr(97 + i)} line {i}" + " pad" * 50) for i in range(20)])
    zpath = os.path.join(folder, "WhatsApp Chat - Leo.zip")
    write_wa_zip(zpath, "_chat.txt", full)
    out = []
    with daemon(base) as d:
        sid = add(d, "whatsapp", {"folder": folder})
        c = Check("wa.half_written.zip_being_replaced")
        c.ok(hits(d, "zqhwa", sid)["total"] >= 1, "initial import", None)
        data = open(zpath, "rb").read()
        with open(zpath, "wb") as f:  # the new copy is half there
            f.write(data[: len(data) // 2])
        d.call("sources.sync", {"id": sid})
        time.sleep(3)
        d.wait_idle(source=sid)
        st = source(d, sid)
        c.ok(hits(d, "zqhwa", sid)["total"] >= 1, "documents kept while the export is unreadable", st["items"])
        c.ok(st["state"] != "error", "source not in error state for a half-copied export", [st["state"], st.get("last_error")])
        out.append(c.result())
        c = Check("wa.half_written.txt_cut_mid_line")
        tpath = os.path.join(folder, "WhatsApp Chat with Kai.txt")
        msgs = [(T(2026, 2, 2, 9, i), "Kai", f"zqcut{chr(97 + i)} message {i} the flight lands at noon") for i in range(5)]
        txt = wa_text("and_uk", msgs)
        cut = txt[: txt.find("zqcute") + 14]  # mid last message
        with open(tpath, "w") as f:
            f.write(cut)
        time.sleep(3)
        d.wait_idle(source=sid)
        with open(tpath, "w") as f:
            f.write(txt)
        time.sleep(3)
        d.call("sources.sync", {"id": sid})
        time.sleep(2)
        d.wait_idle(source=sid)
        _, text, _, _ = find_token(d, sid, "zqcuta")
        c.ok(text and text.count("zqcute") == 1 and "message 4 the flight lands at noon" in text,
             "completed file replaces the partial last message (no duplicate partial)", short(text[-300:] if text else None))
        out.append(c.result())
        c = Check("wa.half_written.new_zip_copy_in_progress")
        npath = os.path.join(folder, "WhatsApp Chat - Nora.zip")
        tmpz = os.path.join(base, "nora.zip")
        write_wa_zip(tmpz, "_chat.txt", wa_text("ios_uk", [(T(2026, 2, 3, 9, 0, i), "Nora", f"zqnor{chr(97 + i)} hi" + " x" * 3000) for i in range(10)]))
        nd = open(tmpz, "rb").read()
        with open(npath, "wb") as f:
            f.write(nd[:4096])
            f.flush()
            time.sleep(2)
            f.write(nd[4096:])
        ok = wait_for(lambda: hits(d, "zqnora", sid)["total"] >= 1, timeout=20)
        st = source(d, sid)
        c.ok(ok, "watcher imports the zip once the copy completes", [st["state"], st.get("last_error")])
        out.append(c.result())
    return out


def s_groups_and_filters():
    base = fresh("wa_groups")
    folder = os.path.join(base, "exports")
    g = [(T(2026, 3, 1, 10, 0), None, "Robin Park created group \"Climbing 🧗\""),
         (T(2026, 3, 1, 10, 1), None, "Robin Park added Ken, Mei"),
         (T(2026, 3, 1, 10, 2), "Ken", "zqgra Saturday at the wall?"),
         (T(2026, 3, 1, 10, 3), "Mei", "zqgrb yes"),
         (T(2026, 3, 1, 10, 4), "Robin Park", "zqgrc booked"),
         (T(2024, 1, 5, 10, 4), "Ken", "zqgrold an old message from 2024")]
    write_wa_txt(os.path.join(folder, "WhatsApp Chat with Climbing 🧗.txt"), wa_text("and_uk", g))
    one = [(T(2026, 3, 2, 10, 0), "Ken", "zqgrone one to one")]
    write_wa_txt(os.path.join(folder, "WhatsApp Chat with Ken.txt"), wa_text("and_uk", one))
    out = []
    with daemon(base) as d:
        s1 = add(d, "whatsapp", {"folder": folder, "me": "Robin Park"}, name="all")
        s2 = add(d, "whatsapp", {"folder": folder, "include_groups": False}, name="nogroups")
        s3 = add(d, "whatsapp", {"folder": folder, "since_days": 365}, name="recent")
        c = Check("wa.group")
        t, text, h, p = find_token(d, s1, "zqgra")
        c.ok(t == "Climbing 🧗 — March 2026", "emoji chat name kept", t)
        people = sorted(x["name"] for x in (p["_hit"]["people"] if p else []))
        c.ok(people and {"Ken", "Mei"} <= set(people), "group authors are people on the hit", people)
        out.append(c.result())
        c = Check("wa.include_groups_false")
        c.ok(hits(d, "zqgra", s2)["total"] == 0 and hits(d, "zqgrone", s2)["total"] == 1, "group dropped, 1:1 kept",
             [hits(d, "zqgra", s2)["total"], hits(d, "zqgrone", s2)["total"]])
        out.append(c.result())
        c = Check("wa.since_days")
        c.ok(hits(d, "zqgrold", s3)["total"] == 0 and hits(d, "zqgra", s3)["total"] == 1, "2024 message dropped with since_days=365",
             [hits(d, "zqgrold", s3)["total"], hits(d, "zqgra", s3)["total"]])
        out.append(c.result())
    return out


def s_privacy():
    base = fresh("wa_privacy")
    folder = os.path.join(base, "exports")
    plain = [(T(2026, 3, 1, 10, 0), "Ken", "zqpva let's meet at the bakery on Elm street")]
    card = [(T(2026, 3, 1, 10, 0), "Ken", "zqpvb my card is 4111 1111 1111 1111 exp 12/29")]
    iban = [(T(2026, 3, 1, 10, 0), "Ken", "zqpvc send it to DE89 3704 0044 0532 0130 00 please")]
    key = [(T(2026, 3, 1, 10, 0), "Ken", "zqpvd the token is ghp_R8bK2mQ7xV4nT1cY9pL3sD6fH0jW5zA2eU8i")]
    for n, m in [("Plain", plain), ("Card", card), ("Iban", iban), ("Key", key)]:
        write_wa_txt(os.path.join(folder, f"WhatsApp Chat with {n}.txt"), wa_text("and_uk", m))
    out = []
    with daemon(base, safe=True) as d:
        s_on = add(d, "whatsapp", {"folder": folder}, name="private")
        s_off = add(d, "whatsapp", {"folder": folder, "private": False}, name="public")

        def sens(tok, sid):
            h = hits(d, tok, sid)["hits"]
            return h[0]["sensitive"] if h else None
        c = Check("wa.private_default")
        c.ok(sens("zqpva", s_on) is True, "private on (default): sensitive", sens("zqpva", s_on))
        out.append(c.result())
        c = Check("wa.private_off")
        c.ok(sens("zqpva", s_off) is False, "private off + nothing found: not sensitive (assume_unflagged_safe)", sens("zqpva", s_off))
        c.ok(sens("zqpvb", s_off) is True, "card number marks sensitive", sens("zqpvb", s_off))
        c.ok(sens("zqpvc", s_off) is True, "IBAN marks sensitive", sens("zqpvc", s_off))
        c.ok(sens("zqpvd", s_off) is True, "API key marks sensitive", sens("zqpvd", s_off))
        out.append(c.result())
        c = Check("wa.private_toggle")
        d.call("sources.update", {"id": s_off, "settings": {"folder": folder, "private": True}})
        time.sleep(2)
        d.wait_idle(source=s_off)
        v = wait_for(lambda: sens("zqpva", s_off) is True, timeout=20)
        c.ok(v, "turning private on re-marks existing docs sensitive", sens("zqpva", s_off))
        d.call("sources.update", {"id": s_off, "settings": {"folder": folder, "private": False}})
        time.sleep(2)
        d.wait_idle(source=s_off)
        v = wait_for(lambda: sens("zqpva", s_off) is False, timeout=20)
        c.ok(v, "turning private off again clears it", sens("zqpva", s_off))
        out.append(c.result())
        c = Check("wa.retrieve_context_sensitive")
        r = d.call("retrieve.context", {"query": "zqpva bakery", "budget_tokens": 1000})
        cits = r.get("citations") or []
        c.ok(any(x.get("sensitive") for x in cits), "retrieve.context citations carry sensitive=true for private chats",
             [(x["title"], x.get("sensitive")) for x in cits][:4])
        out.append(c.result())
    return out


def s_people():
    base = fresh("wa_people")
    folder = os.path.join(base, "exports")
    m = [(T(2026, 3, 1, 10, 0), "+49 151 23456789", "zqppa hi from an unsaved number"),
         (T(2026, 3, 1, 10, 1), "Robin Park", "zqppb who is this?")]
    write_wa_txt(os.path.join(folder, "WhatsApp Chat with +49 151 23456789.txt"), wa_text("and_de", m))
    # iOS wraps unsaved numbers in LRE/PDF; iOS number formatting can use U+2011 (UNVERIFIED)
    num = f"{fx.LRE}+1 (555) 010‑0199{fx.PDF}"
    m2 = [(T(2026, 3, 1, 10, 0), num, "zqppc ios unsaved number"),
          (T(2026, 3, 1, 10, 1), f"{fx.LRE}+1 (555) 010-0177{fx.PDF}", "zqppd plain hyphen")]
    write_wa_zip(os.path.join(folder, "WhatsApp Chat - Neighbours.zip"), "_chat.txt", wa_text("ios_uk", m2))
    out = []
    with daemon(base) as d:
        sid = add(d, "whatsapp", {"folder": folder, "me": "Robin Park"})
        time.sleep(3)
        c = Check("wa.people.phone")
        r = hits(d, "zqppa", sid)
        people = r["hits"][0]["people"] if r["hits"] else []
        c.ok(any("49" in (p["name"] or "") for p in people), "phone-number sender appears in hit people", people)
        ents = d.call("knowledge.entities", {"text": "+49", "limit": 10}) + d.call("knowledge.entities", {"text": "151", "limit": 10})
        idents = []
        for e in ents[:3]:
            det = d.call("knowledge.entity", {"id": e["id"]})
            idents.append({"name": det["name"], "identifiers": det.get("identifiers")})
        c.ok(any("4915123456789" in short(x["identifiers"], 2000) for x in idents),
             "a person entity carries the phone identifier +4915123456789", idents)
        out.append(c.result())
        c = Check("wa.people.ios_wrapped_numbers")
        r = hits(d, "zqppd", sid)
        ppl = r["hits"][0]["people"] if r["hits"] else []
        names = [p["name"] for p in ppl]
        c.ok(any("010-0177" in n or "0100177" in n for n in names), "LRE/PDF-wrapped number recognised", names)
        e1 = d.call("knowledge.entities", {"text": "0199", "limit": 5})
        det = [d.call("knowledge.entity", {"id": e["id"]}) for e in e1[:2]]
        c.ok(any("15550100199" in short(x.get("identifiers"), 2000) for x in det),
             "number with U+2011 non-breaking hyphen gets a phone identifier (UNVERIFIED format)",
             [(x["name"], x.get("identifiers")) for x in det] or names)
        out.append(c.result())
    return out


def _rss_kb(pid):
    try:
        for l in open(f"/proc/{pid}/status"):
            if l.startswith("VmHWM"):
                return int(l.split()[1])
    except OSError:
        return None


def s_hostile():
    base = fresh("wa_hostile")
    folder = os.path.join(base, "exports")
    os.makedirs(folder)
    outside = os.path.join(base, "outside")
    write_wa_txt(os.path.join(outside, "WhatsApp Chat with Secret.txt"),
                 wa_text("and_uk", [(T(2026, 3, 1, 10, 0), "Secret", "zqhosecret outside the folder")]))
    os.symlink(os.path.join(outside, "WhatsApp Chat with Secret.txt"), os.path.join(folder, "WhatsApp Chat with Link.txt"))
    os.symlink(outside, os.path.join(folder, "linkdir"))
    # traversal names inside a zip
    with zipfile.ZipFile(os.path.join(folder, "WhatsApp Chat - Trav.zip"), "w") as z:
        z.writestr("../../../../tmp/jj1_evil/_chat.txt", wa_text("and_uk", [(T(2026, 3, 1, 10, 0), "Eve", "zqhotrav traversal")]))
        z.writestr("_chat.txt", wa_text("and_uk", [(T(2026, 3, 1, 10, 0), "Eve", "zqhotravok real chat text")]))
        z.writestr("../../evil.jpg", b"x")
    # zip bomb-ish: 300 MB of text that compresses to < 1 MB
    zb = os.path.join(folder, "WhatsApp Chat - Bomb.zip")
    with zipfile.ZipFile(zb, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
        with z.open("_chat.txt", "w", force_zip64=True) as w:
            w.write(wa_text("and_uk", [(T(2026, 3, 1, 10, 0), "Bob", "zqhobomb start")]).encode())
            blk = b"a" * (1 << 20)
            for _ in range(300):
                w.write(blk)
    # invalid UTF-8 and UTF-16
    write_wa_txt(os.path.join(folder, "WhatsApp Chat with Bytes.txt"),
                 wa_text("and_uk", [(T(2026, 3, 1, 10, 0), "Bytes", "zqhoutf ok")]).encode() + b"01/03/2026, 10:01 - Bytes: zqhobad \xff\xfe\x80 broken\n")
    write_wa_txt(os.path.join(folder, "WhatsApp Chat with Wide.txt"),
                 "﻿" + wa_text("and_uk", [(T(2026, 3, 1, 10, 0), "Wide", "zqhowide utf16 export")]), encoding="utf-16-le")
    # FIFO
    os.mkfifo(os.path.join(folder, "WhatsApp Chat with Fifo.txt"))
    # a big but legit chat: ~40 MB in one month
    big = os.path.join(folder, "WhatsApp Chat with Big.txt")
    with open(big, "w") as f:
        line = " lorem ipsum dolor sit amet" * 6
        for i in range(250_000):
            f.write(f"{(i % 28) + 1:02d}/03/2026, {(i // 60) % 24:02d}:{i % 60:02d} - Big: zqhobig{i % 7} msg{i}{line}\n")
    out = []
    t0 = time.time()
    with daemon(base) as d:
        sid = add(d, "whatsapp", {"folder": folder})
        st = d.wait_idle(source=sid, timeout=600)
        dt_ = time.time() - t0
        src = source(d, sid)
        hwm = _rss_kb(d.proc.pid)
        c = Check("wa.hostile.symlink_outside")
        c.ok(hits(d, "zqhosecret", sid)["total"] == 0, "symlinked file/dir outside the folder not imported", hits(d, "zqhosecret", sid)["total"])
        out.append(c.result())
        c = Check("wa.hostile.zip_traversal")
        c.ok(not os.path.exists("/tmp/jj1_evil") and not os.path.exists(os.path.join(base, "evil.jpg")), "nothing written outside", None)
        c.ok(hits(d, "zqhotravok", sid)["total"] == 1 and hits(d, "zqhotrav", sid)["total"] <= 1, "real _chat.txt used, traversal entry ignored",
             [hits(d, "zqhotravok", sid)["total"]])
        out.append(c.result())
        c = Check("wa.hostile.zip_bomb")
        c.ok(src["state"] in ("watching", "idle"), "daemon survives a 300 MB entry", [src["state"], src.get("last_error")])
        c.ok(hwm is None or hwm < 1_500_000, "peak RSS under 1.5 GB", hwm)
        c.note(f"bomb import: zqhobomb hits={hits(d, 'zqhobomb', sid)['total']}")
        out.append(c.result())
        c = Check("wa.hostile.encodings")
        c.ok(hits(d, "zqhobad", sid)["total"] == 1, "invalid UTF-8 line imported (lossy)", None)
        c.ok(hits(d, "zqhowide", sid)["total"] == 1, "UTF-16LE export with BOM imported", None)
        out.append(c.result())
        c = Check("wa.hostile.fifo_and_big")
        c.ok(dt_ < 600 and st["queue_depth"] == 0, "FIFO does not hang the pass", round(dt_, 1))
        r = hits(d, "zqhobig3", sid)
        titles = sorted({h["title"] for h in r["hits"]})
        c.ok(any("part" in t for t in titles), "40 MB single-month chat split into parts", {"titles": titles[:5], "items": src["items"], "secs": round(dt_, 1)})
        c.note(f"total import time {dt_:.1f}s, items={src['items']}, chunks={src['chunks']}, peak RSS kB={hwm}")
        out.append(c.result())
    return out


def s_watch_and_remove():
    base = fresh("wa_watch")
    folder = os.path.join(base, "exports")
    write_wa_txt(os.path.join(folder, "WhatsApp Chat with A.txt"), wa_text("and_uk", [(T(2026, 3, 1, 10, 0), "A", "zqwaa first")]))
    out = []
    with daemon(base) as d:
        sid = add(d, "whatsapp", {"folder": folder})
        c = Check("wa.watch.new_export")
        t0 = time.time()
        write_wa_zip(os.path.join(folder, "WhatsApp Chat - B.zip"), "_chat.txt",
                     wa_text("ios_uk", [(T(2026, 3, 2, 10, 0), "B", "zqwab new export dropped in")]))
        ok = wait_for(lambda: hits(d, "zqwab", sid)["total"] >= 1, timeout=30, step=0.5)
        c.ok(ok, "new export picked up by the watcher without a sync", round(time.time() - t0, 1))
        sub = os.path.join(folder, "phone")
        os.makedirs(sub)
        write_wa_txt(os.path.join(sub, "WhatsApp Chat with C.txt"), wa_text("and_uk", [(T(2026, 3, 3, 10, 0), "C", "zqwac in a subfolder")]))
        ok = wait_for(lambda: hits(d, "zqwac", sid)["total"] >= 1, timeout=30, step=0.5)
        c.ok(ok, "export in a new subfolder picked up", None)
        os.remove(os.path.join(folder, "WhatsApp Chat - B.zip"))
        ok = wait_for(lambda: hits(d, "zqwab", sid)["total"] == 0, timeout=30, step=0.5)
        c.ok(ok, "deleting the export removes the chat", None)
        out.append(c.result())
        c = Check("wa.remove_purge")
        d.call("sources.remove", {"id": sid})
        time.sleep(2)
        r = hits(d, "zqwaa")
        c.ok(r["total"] == 0, "sources.remove purges the chat docs", r["total"])
        c.ok(all(s["id"] != sid for s in d.call("sources.list")), "source gone", None)
        out.append(c.result())
    return out


def s_settings_validation():
    base = fresh("wa_settings")
    out = []
    with daemon(base) as d:
        c = Check("wa.settings")
        errs = {}
        for name, spec in {"missing": {"folder": os.path.join(base, "nope")}, "path_key": {"path": base},
                           "file": {"folder": __file__}}.items():
            try:
                d.call("sources.add", {"kind": "whatsapp", "name": "x", "settings": spec})
                errs[name] = "accepted"
            except Exception as e:  # noqa: BLE001
                errs[name] = str(e)
        c.ok(all(v != "accepted" for v in errs.values()), "bad folder / wrong key / file refused with a message", errs)
        try:
            os.makedirs(os.path.join(base, "home", "tilde"), exist_ok=True)
            d.call("sources.add", {"kind": "whatsapp", "name": "t", "settings": {"folder": "~/does-not-exist-jj1"}})
            errs["tilde"] = "accepted"
        except Exception as e:  # noqa: BLE001
            errs["tilde"] = str(e)
        c.note(short(errs, 800))
        out.append(c.result())
    return out


ALL = [s_locales, s_ambiguous_dates, s_reexport, s_half_written, s_groups_and_filters, s_privacy, s_people,
       s_hostile, s_watch_and_remove, s_settings_validation]
