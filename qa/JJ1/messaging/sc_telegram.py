"""Telegram Desktop JSON export scenarios."""
import copy
import json
import os
import time

import fx
from common import Check, add, daemon, docs_for, fresh, hits, short, source
from sc_whatsapp import find_token


def s_tg_single():
    base = fresh("tg_single")
    root = os.path.join(base, "Telegram Desktop")
    exp = fx.tg_single_export(root, "ChatExport_2026-06-02", fx.tg_personal_chat("zqtg"))
    out = []
    with daemon(base) as d:
        sid = add(d, "telegram", {"path": exp})
        c = Check("tg.single_chat.content")
        t, text, h, p = find_token(d, sid, "zqtga")
        c.ok(t == "Sam Lee — May 2026", "title 'Sam Lee — May 2026'", t)
        c.ok(text and "Booked zqtgb table at https://example.com/r" in text, "entity-array text flattened", short(text or "", 300))
        c.ok(text and "[photo: photo_3@03-05-2026_09-14-00.jpg]" in text, "photo listed by name", None)
        c.ok(text and "[file: Plan Q2.pdf]" in text, "file listed by name", None)
        c.ok(text and "Sam Lee called (125 s)" in text, "phone_call service message", None)
        c.ok(text and "**Me** · " in text, "my messages shown as Me in a personal chat export", None)
        c.ok(p and p["_hit"]["sensitive"] is True, "private by default", p and p["_hit"]["sensitive"])
        out.append(c.result())
        c = Check("tg.single_chat.local_time")
        c.ok(h and "2026-05-03 09:12" in h, "time shown as the export's local wall time (date: 2026-05-03T09:12:01)", h)
        t6, _, h6, _ = find_token(d, sid, "zqtgd")
        c.ok(t6 == "Sam Lee — June 2026", "message sent 00:30 on 1 June (local) filed in June", [t6, h6])
        c.note("WhatsApp shows the phone's wall-clock time; Telegram shows UTC from date_unixtime")
        out.append(c.result())
        c = Check("tg.single_chat.file_path")
        sid2 = add(d, "telegram", {"path": os.path.join(exp, "result.json")}, name="file")
        c.ok(hits(d, "zqtga", sid2)["total"] == 1, "path may be result.json itself", hits(d, "zqtga", sid2)["total"])
        out.append(c.result())
    return out


def s_tg_full_and_parent():
    base = fresh("tg_full")
    root = os.path.join(base, "Telegram Desktop")
    personal = fx.tg_personal_chat("zqtf")
    saved = {"type": "saved_messages", "id": 700000001, "messages": [
        fx.tg_msg(1, fx.T(2026, 5, 9, 8, 0), "Tanuj", "user700000001", "zqtfsave note to self: passport renewal form")]}
    left = {"name": "Old Club", "type": "private_supergroup", "id": 5500000001, "messages": [
        fx.tg_msg(1, fx.T(2025, 12, 1, 8, 0), "Rosa Diaz", "user700000003", "zqtfleft left chat message")]}
    fx.tg_full_export(root, "DataExport_2026-06-20", [personal, fx.tg_group_chat("zqtf"), saved], left=[left])
    # an older single-chat export of the same chat sits next to it
    older = copy.deepcopy(personal)
    older["messages"] = older["messages"][:3]
    fx.tg_single_export(root, "ChatExport_2026-05-04", older)
    out = []
    with daemon(base) as d:
        sid = add(d, "telegram", {"path": root})
        c = Check("tg.parent_folder.merge")
        _, text, _, _ = find_token(d, sid, "zqtfa")
        c.ok(text and text.count("zqtfa") == 1 and "zqtfc" in text, "same chat from two exports merged, no duplicates",
             text and text.count("zqtfa"))
        c.ok(text and text.count("Booked") == 1, "edited message (same text in both) not duplicated", text and text.count("Booked"))
        out.append(c.result())
        c = Check("tg.full_export.chats")
        tg, gtext, gh, gp = find_token(d, sid, "zqtfg")
        c.ok(tg == "Hiking Crew — April 2026", "group chat imported", tg)
        c.ok(gtext and "created the group “Hiking Crew”" in gtext, "create_group service line", None)
        c.ok(gtext and "**Me** · " in gtext, "my message in a group of a full export shown as Me", None)
        c.ok(gtext and "> [audio]" in gtext and "File not included" not in gtext, "'(File not included…)' voice -> [audio]", None)
        c.ok(gtext and "zqtfi" in gtext, "message from a deleted account (from: null) kept", short(gtext[gtext.find("10:04") - 40:][:120] if gtext else "", 200))
        ts, _, _, _ = find_token(d, sid, "zqtfsave")
        c.ok(ts == "Saved Messages — May 2026", "saved messages", ts)
        tl, _, _, _ = find_token(d, sid, "zqtfleft")
        c.ok(tl == "Old Club — December 2025", "left_chats imported", tl)
        out.append(c.result())
        c = Check("tg.include_groups_false")
        s2 = add(d, "telegram", {"path": root, "include_groups": False}, name="nogroups")
        c.ok(hits(d, "zqtfg", s2)["total"] == 0 and hits(d, "zqtfa", s2)["total"] == 1, "group dropped, personal kept", None)
        out.append(c.result())
        c = Check("tg.people")
        r = hits(d, "zqtfa", sid)
        ppl = [x["name"] for x in r["hits"][0]["people"]] if r["hits"] else []
        c.ok("Sam Lee" in ppl, "Sam Lee is a person on the hit", ppl)
        ents = d.call("knowledge.entities", {"text": "Sam", "limit": 5})
        det = [d.call("knowledge.entity", {"id": e["id"]}) for e in ents[:2]]
        c.ok(any("447700900002" in short(x.get("identifiers"), 3000) for x in det),
             "Sam's phone from the export's contacts list reaches the person", [(x["name"], x.get("identifiers")) for x in det])
        out.append(c.result())
    return out


def s_tg_reexport_and_half():
    base = fresh("tg_reexport")
    root = os.path.join(base, "Telegram Desktop")
    v1 = fx.tg_personal_chat("zqtr")
    fx.tg_single_export(root, "ChatExport_2026-06-02", v1)
    out = []
    with daemon(base) as d:
        sid = add(d, "telegram", {"path": root})
        n1 = source(d, sid)["items"]
        v2 = fx.tg_personal_chat("zqtr", n_extra=5)
        v2["messages"][0]["text"] = "zqtra The flight lands at 19:10 now, gate B"  # edited after v1
        v2["messages"][0]["edited"] = "2026-05-03T09:30:00"
        fx.tg_single_export(root, "ChatExport_2026-06-21", v2)
        time.sleep(4)
        d.call("sources.sync", {"id": sid})
        time.sleep(2)
        d.wait_idle(source=sid)
        c = Check("tg.reexport.both_present")
        _, text, _, _ = find_token(d, sid, "zqtrx1")
        c.ok(text is not None, "new messages imported", None)
        _, t1, _, _ = find_token(d, sid, "zqtrc")
        c.ok(t1 and t1.count("zqtrc") == 1, "unchanged messages not duplicated", t1 and t1.count("zqtrc"))
        c.ok(t1 and t1.count("zqtra") == 1, "a message edited between exports appears once", t1 and [l for l in t1.split("\n") if "zqtra" in l])
        out.append(c.result())
        c = Check("tg.reexport.old_deleted")
        import shutil
        shutil.rmtree(os.path.join(root, "ChatExport_2026-06-02"))
        time.sleep(4)
        d.call("sources.sync", {"id": sid})
        time.sleep(2)
        d.wait_idle(source=sid)
        _, t1, _, _ = find_token(d, sid, "zqtrc")
        c.ok(t1 and t1.count("zqtra") == 1 and "19:10" in t1, "after deleting the old export only the edited text remains", t1 and [l for l in t1.split("\n") if "zqtra" in l])
        out.append(c.result())
        c = Check("tg.half_written")
        p = os.path.join(root, "ChatExport_2026-06-21", "result.json")
        data = open(p, "rb").read()
        with open(p, "wb") as f:
            f.write(data[: len(data) // 2])
        time.sleep(4)
        d.wait_idle(source=sid)
        st = source(d, sid)
        c.ok(hits(d, "zqtrc", sid)["total"] == 1, "docs kept while result.json is half written", st["items"])
        c.ok(st["state"] != "error", "source not in error", [st["state"], st.get("last_error")])
        with open(p, "wb") as f:
            f.write(data)
        out.append(c.result())
    return out


def s_tg_symlink():
    base = fresh("tg_symlink")
    root = os.path.join(base, "Telegram Desktop")
    os.makedirs(root)
    elsewhere = os.path.join(base, "someone-else")
    fx.tg_single_export(elsewhere, "ChatExport_x", {"name": "Other", "type": "personal_chat", "id": 1,
                        "messages": [fx.tg_msg(1, fx.T(2026, 5, 1, 9, 0), "Other", "user1", "zqtslink outside export")]})
    os.symlink(os.path.join(elsewhere, "ChatExport_x"), os.path.join(root, "ChatExport_link"))
    fx.tg_single_export(root, "ChatExport_real", {"name": "Real", "type": "personal_chat", "id": 2,
                        "messages": [fx.tg_msg(1, fx.T(2026, 5, 1, 9, 0), "Real", "user2", "zqtsreal inside")]})
    out = []
    with daemon(base) as d:
        sid = add(d, "telegram", {"path": root})
        c = Check("tg.symlink_outside")
        n = hits(d, "zqtslink", sid)["total"]
        c.ok(hits(d, "zqtsreal", sid)["total"] == 1, "real export imported", None)
        c.ok(n == 0, "symlinked export folder outside the chosen folder is not followed (as WhatsApp does)", n)
        out.append(c.result())
    return out


def s_tg_bad_inputs():
    base = fresh("tg_bad")
    root = os.path.join(base, "Telegram Desktop")
    fx.tg_single_export(root, "ChatExport_good", {"name": "Good", "type": "personal_chat", "id": 3,
                        "messages": [fx.tg_msg(1, fx.T(2026, 5, 1, 9, 0), "Good", "user3", "zqtbgood fine")]})
    d2 = os.path.join(root, "ChatExport_html")
    os.makedirs(d2)
    open(os.path.join(d2, "messages.html"), "w").write("<html>zqtbhtml</html>")
    d3 = os.path.join(root, "ChatExport_bad")
    os.makedirs(d3)
    open(os.path.join(d3, "result.json"), "wb").write(b'{"name": "Bad", "type": "personal_chat", "id": 4, "messages": [{"id": 1, "type": "message", "date_unixtime": "1777000000", "from": "B", "text": "zqtbbad \xff\xfe"}]}')
    out = []
    with daemon(base) as d:
        sid = add(d, "telegram", {"path": root})
        st = source(d, sid)
        c = Check("tg.bad_inputs")
        c.ok(hits(d, "zqtbgood", sid)["total"] == 1, "good export imported next to an HTML export and invalid UTF-8 JSON", None)
        c.note(f"invalid-UTF-8 result.json imported: {hits(d, 'zqtbbad', sid)['total']}; source={st['state']} {st.get('last_error')} {st.get('health')}")
        out.append(c.result())
    return out


ALL = [s_tg_single, s_tg_full_and_parent, s_tg_reexport_and_half, s_tg_symlink, s_tg_bad_inputs]
