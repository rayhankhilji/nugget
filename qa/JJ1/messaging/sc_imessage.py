"""iMessage scenarios against a synthetic chat.db (db_path, so they run on Linux)."""
import os
import time

import fx
from common import Check, add, daemon, fresh, hits, short, source
from sc_whatsapp import find_token


def s_im_basic():
    base = fresh("im_basic")
    db = os.path.join(base, "Messages", "chat.db")
    os.makedirs(os.path.dirname(db))
    fx.build_chat_db(db, "zqim")
    out = []
    with daemon(base) as d:
        sid = add(d, "imessage", {"db_path": db, "contacts": False})
        st = source(d, sid)
        c = Check("im.basic")
        t, text, h, p = find_token(d, sid, "zqima")
        c.ok(t == "+15550100001 — February 2026", "1:1 chat titled by the handle, February 2026", t)
        c.ok(h and "2026-02-10 17:00" in h, "Apple-epoch nanoseconds -> 2026-02-10 17:00", h)
        c.ok(text and "zqimc attributed only message 👍 café" in text, "attributedBody text (NULL text) decoded", None)
        c.ok(text and "Loved" not in text, "tapback skipped", None)
        c.ok(text and "[photo: IMG_4021.HEIC]" in text, "attachment listed by transfer_name", None)
        c.ok(text and "**Me** · 2026-02-10 17:02" in text, "is_from_me -> Me", None)
        c.ok(p and p["_hit"]["sensitive"] is True, "private by default", None)
        t2, text2, _, _ = find_token(d, sid, "zqimd")
        c.ok(t2 == "+15550100001 — March 2026" and "long long" in (text2 or ""), "NSMutableString blob, 200-byte text (0x81 length)", t2)
        out.append(c.result())
        c = Check("im.group")
        tg, gt, gh, gp = find_token(d, sid, "zqimf")
        c.ok(tg == "Book Club — April 2026", "group chat named by display_name", tg)
        c.ok(gt and "You added +447700900002" in gt and "named the conversation “Book Club 2026”" in gt, "group events rendered", short(gt or "", 500))
        ppl = sorted(x["name"] for x in gp["_hit"]["people"]) if gp else []
        c.ok("rosa.diaz@example.com" in " ".join(ppl) or len(ppl) >= 2, "group participants are people on the hit", ppl)
        out.append(c.result())
        c = Check("im.sms_and_imessage_same_person")
        ts, _, _, _ = find_token(d, sid, "zqime")
        c.ok(ts == "+15550100001 — January 2026", "SMS chat imported", ts)
        r = hits(d, "15550100001", sid)
        c.note(f"titles for the same person: {sorted({h['title'] for h in r['hits']})}")
        out.append(c.result())
        c = Check("im.people_phone")
        ents = d.call("knowledge.entities", {"text": "555", "limit": 10})
        det = [d.call("knowledge.entity", {"id": e["id"]}) for e in ents[:3]]
        c.ok(any("15550100001" in short(x.get("identifiers"), 2000) for x in det),
             "handle-only participant (no Contacts) becomes a person with the phone", [(x["name"], x.get("identifiers")) for x in det])
        out.append(c.result())
        # with Contacts
        ab = os.path.join(base, "AddressBook")
        fx.build_address_book(ab)
        s2 = add(d, "imessage", {"db_path": db, "contacts_dir": ab}, name="with-contacts")
        c = Check("im.contacts")
        t, text, h, p = find_token(d, s2, "zqima")
        c.ok(t == "Alex Moreno — February 2026", "phone resolved to Alex Moreno via (555) 010-0001", t)
        tg, gt, _, _ = find_token(d, s2, "zqimf")
        c.ok(gt and "**Rosa Diaz**" in gt, "e-mail handle resolved case-insensitively", short(gt or "", 300))
        ents = d.call("knowledge.entities", {"text": "Alex", "limit": 5})
        det = [d.call("knowledge.entity", {"id": e["id"]}) for e in ents[:2]]
        c.ok(any("15550100001" in short(x.get("identifiers"), 2000) for x in det), "Alex Moreno person carries the phone",
             [(x["name"], x.get("identifiers")) for x in det])
        out.append(c.result())
    return out


def s_im_wal_and_bad():
    base = fresh("im_wal")
    db = os.path.join(base, "chat.db")
    conn = fx.build_chat_db(db, "zqiw", wal=True)
    # a message written after the schema, still only in the WAL
    conn.execute("INSERT INTO message (ROWID,guid,text,date,handle_id) VALUES (60,'MSG-0060','zqiwwal only in the wal',?,1)",
                 (fx.apple_ns(fx.T(2026, 2, 12, 9, 0, tzinfo=fx.UTC)),))
    conn.execute("INSERT INTO chat_message_join VALUES (1,60,0)")
    conn.commit()
    out = []
    with daemon(base) as d:
        c = Check("im.wal_copy")
        walsize = os.path.getsize(db + "-wal") if os.path.exists(db + "-wal") else 0
        sid = add(d, "imessage", {"db_path": db, "contacts": False})
        c.ok(hits(d, "zqiwwal", sid)["total"] == 1, "a message only in chat.db-wal is read", {"wal_bytes": walsize})
        before = os.path.getmtime(db)
        c.ok(os.path.getmtime(db) == before and os.path.getsize(db + "-wal") == walsize, "live database untouched", None)
        scratch = os.path.join(base, "home")
        left = [p for r, ds, fs in os.walk(scratch) for p in ds if p.startswith("imessage-")]
        c.ok(not left, "temp copy removed after the pass", left)
        out.append(c.result())
        conn.close()
        c = Check("im.invalid_utf8_text")
        db2 = os.path.join(base, "bad.db")
        fx.build_chat_db(db2, "zqib", bad_utf8=True)
        s2 = add(d, "imessage", {"db_path": db2, "contacts": False}, name="bad")
        st = source(d, s2)
        n = hits(d, "zqiba", s2)["total"]
        c.ok(n == 1, "one message with invalid UTF-8 text does not block the whole database", {"hits": n, "state": st["state"], "error": st.get("last_error")})
        out.append(c.result())
        c = Check("im.missing_db")
        try:
            d.call("sources.add", {"kind": "imessage", "name": "x", "settings": {"db_path": os.path.join(base, "nope.db")}})
            msg = "accepted"
        except Exception as e:  # noqa: BLE001
            msg = str(e)
        c.ok("not found" in msg, "missing db_path refused with 'Messages database not found'", msg)
        try:
            d.call("sources.add", {"kind": "imessage", "name": "y", "settings": {}})
            msg = "accepted"
        except Exception as e:  # noqa: BLE001
            msg = str(e)
        c.ok("only available on macOS" in msg, "no db_path on Linux -> 'iMessage is only available on macOS'", msg)
        out.append(c.result())
    return out


ALL = [s_im_basic, s_im_wal_and_bad]
