"""Synthetic messaging fixtures for the JJ1 messaging QA (WhatsApp, Telegram, iMessage).

Everything here is invented: names, numbers and texts are made up. Formats follow what the
export apps write as commonly documented by open-source parsers; formats marked UNVERIFIED in a
comment were not checked against a real device export.
Stdlib only.
"""
import datetime as dt
import json
import os
import sqlite3
import struct
import zipfile

LRM = "‎"
RLM = "‏"
NNBSP = " "
LRE, PDF = "‪", "‬"

# ---------------------------------------------------------------------------------------------
# WhatsApp


def _h12(t):
    h = t.hour % 12 or 12
    return h, ("AM" if t.hour < 12 else "PM")


def wa_stamp(style, t):
    """The header prefix of a message line (`[...] ` or `... - `) for an export style."""
    d, m, y, yy = t.day, t.month, t.year, t.year % 100
    H, M, S = t.hour, t.minute, t.second
    h, ap = _h12(t)
    if style == "ios_us":  # iPhone, en_US, 12 h, narrow no-break space before AM/PM
        return f"[{m}/{d}/{yy:02d}, {h}:{M:02d}:{S:02d}{NNBSP}{ap}] "
    if style == "ios_uk":  # iPhone, en_GB, 24 h
        return f"[{d:02d}/{m:02d}/{y}, {H:02d}:{M:02d}:{S:02d}] "
    if style == "ios_de":  # iPhone, de_DE
        return f"[{d:02d}.{m:02d}.{yy:02d}, {H:02d}:{M:02d}:{S:02d}] "
    if style == "ios_ru":  # iPhone, ru_RU
        return f"[{d:02d}.{m:02d}.{y}, {H:02d}:{M:02d}:{S:02d}] "
    if style == "and_us":  # Android, en_US (Android 13+ writes U+202F before AM/PM)
        return f"{m}/{d}/{yy:02d}, {h}:{M:02d}{NNBSP}{ap} - "
    if style == "and_uk":  # Android, en_GB
        return f"{d:02d}/{m:02d}/{y}, {H:02d}:{M:02d} - "
    if style == "and_in":  # Android, en_IN: day first AND 12 h clock, lower-case am/pm
        return f"{d:02d}/{m:02d}/{yy:02d}, {h}:{M:02d}{NNBSP}{ap.lower()} - "
    if style == "and_de":
        return f"{d:02d}.{m:02d}.{yy:02d}, {H:02d}:{M:02d} - "
    if style == "and_es":
        return f"{d}/{m}/{yy:02d}, {H}:{M:02d} - "
    if style == "and_fr":  # UNVERIFIED: French Android writes "à" between date and time
        return f"{d:02d}/{m:02d}/{y} à {H:02d}:{M:02d} - "
    if style == "and_ru":
        return f"{d:02d}.{m:02d}.{y}, {H:02d}:{M:02d} - "
    if style == "and_ar":  # UNVERIFIED: Arabic locale, Western digits, Arabic comma, ص/م
        return f"{d:02d}/{m:02d}/{y}، {h}:{M:02d} {'ص' if t.hour < 12 else 'م'} - "
    if style == "and_ar_eastern":  # UNVERIFIED: Arabic locale with Eastern Arabic digits
        tr = str.maketrans("0123456789", "٠١٢٣٤٥٦٧٨٩")
        s = f"{d}{RLM}/{m}{RLM}/{y}، {h}:{M:02d} {'ص' if t.hour < 12 else 'م'} - "
        return s.translate(tr)
    raise ValueError(style)


def wa_text(style, msgs):
    """msgs: (datetime, author or None, text). Multi-line texts stay multi-line (continuation
    lines). Author None = system line (Android) / chat-name author with LRM (iOS)."""
    out = []
    for t, author, text in msgs:
        head = wa_stamp(style, t)
        if author is None:
            out.append(f"{head}{text}")
        else:
            out.append(f"{head}{author}: {text}")
    return "\n".join(out) + "\n"


def write_wa_txt(path, text, encoding="utf-8"):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        f.write(text.encode(encoding) if isinstance(text, str) else text)


def write_wa_zip(path, inner_name, text, media=()):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr(inner_name, text.encode("utf-8") if isinstance(text, str) else text)
        for name, data in media:
            z.writestr(name, data)


T = dt.datetime

# Per-locale strings: (encryption notice, media placeholder, deleted notice)
LOCALE = {
    "en_and": ("Messages and calls are end-to-end encrypted. No one outside of this chat, not even WhatsApp, can read or listen to them. Tap to learn more.",
               "<Media omitted>", "This message was deleted"),
    "de_and": ("Nachrichten und Anrufe sind Ende-zu-Ende-verschlüsselt. Niemand außerhalb dieses Chats kann sie lesen oder anhören, nicht einmal WhatsApp. Tippe, um mehr zu erfahren.",
               "<Medien ausgeschlossen>", "Diese Nachricht wurde gelöscht"),
    "es_and": ("Los mensajes y las llamadas están cifrados de extremo a extremo. Nadie fuera de este chat, ni siquiera WhatsApp, puede leerlos ni escucharlos. Toca para obtener más información.",
               "<Multimedia omitido>", "Se eliminó este mensaje."),
    "fr_and": ("Les messages et les appels sont chiffrés de bout en bout. Aucun tiers, pas même WhatsApp, ne peut les lire ou les écouter. Appuyez pour en savoir plus.",
               "<Médias omis>", "Ce message a été supprimé"),
    "ru_and": ("Сообщения и звонки защищены сквозным шифрованием. Никто вне этого чата, даже WhatsApp, не может их прочитать или прослушать.",
               "<Без медиафайлов>", "Данное сообщение удалено"),
    "ar_and": ("الرسائل والمكالمات مشفرة تمامًا بين الطرفين. لا أحد خارج هذه الدردشة، حتى واتساب، يمكنه قراءتها أو الاستماع إليها.",
               "<تم استبعاد الوسائط>", "تم حذف هذه الرسالة"),
}


def wa_locale_chat(style, loc, a, b, tok):
    """A small 1:1 chat in a locale: 6 messages over two months incl. media + deleted + multi-line.
    Unique search tokens: f"{tok}a" .. f"{tok}e"."""
    enc, media, deleted = LOCALE[loc]
    msgs = [
        (T(2026, 3, 14, 9, 5, 0), None, enc),
        (T(2026, 3, 14, 9, 6, 11), a, f"{tok}a Hallo / hola / salut 1"),
        (T(2026, 3, 14, 9, 7, 2), b, f"{tok}b first line\nsecond line {tok}c"),
        (T(2026, 3, 20, 18, 30, 0), a, media),
        (T(2026, 3, 21, 7, 0, 0), b, deleted),
        (T(2026, 4, 2, 22, 45, 30), a, f"{tok}d april message"),
        (T(2026, 4, 25, 13, 1, 0), b, f"{tok}e last"),
    ]
    return msgs


def wa_ios_chat(style, chat, a, me, tok):
    """An iOS chat with the iOS specifics: LRM-led system line authored by the chat name,
    `image omitted`, `<attached: ...>`, a document line, multi-line text."""
    return [
        (T(2026, 5, 1, 8, 0, 0), chat, LRM + "Messages and calls are end-to-end encrypted. No one outside of this chat, not even WhatsApp, can read or listen to them."),
        (T(2026, 5, 1, 8, 1, 5), a, f"{tok}a Morning! Flight lands at 18:40 on Friday"),
        (T(2026, 5, 1, 8, 2, 0), me, f"{tok}b ok\nI'll pick you up\n  at terminal 2"),
        (T(2026, 5, 1, 8, 3, 0), a, LRM + "image omitted"),
        (T(2026, 5, 1, 8, 3, 30), a, LRM + "<attached: 00000007-PHOTO-2026-05-01-08-03-30.jpg>"),
        (T(2026, 5, 1, 8, 4, 0), me, LRM + "Itinerary.pdf • 2 pages " + LRM + "document omitted"),
        (T(2026, 5, 31, 23, 59, 0), a, f"{tok}c just before midnight end of May"),
        (T(2026, 6, 1, 0, 0, 30), a, f"{tok}d just after midnight June"),
    ]


# ---------------------------------------------------------------------------------------------
# Telegram

BERLIN = dt.timezone(dt.timedelta(hours=2))


def tg_msg(mid, local, frm, from_id, text, **extra):
    """Telegram Desktop message: `date` is local wall time, `date_unixtime` the instant."""
    m = {"id": mid, "type": "message", "date": local.strftime("%Y-%m-%dT%H:%M:%S"),
         "date_unixtime": str(int(local.replace(tzinfo=BERLIN).timestamp())),
         "from": frm, "from_id": from_id, "text": text}
    if isinstance(text, str):
        m["text_entities"] = [{"type": "plain", "text": text}] if text else []
    else:
        m["text_entities"] = [p if isinstance(p, dict) else {"type": "plain", "text": p} for p in text]
    m.update(extra)
    return m


def tg_service(mid, local, actor, actor_id, action, **extra):
    m = {"id": mid, "type": "service", "date": local.strftime("%Y-%m-%dT%H:%M:%S"),
         "date_unixtime": str(int(local.replace(tzinfo=BERLIN).timestamp())),
         "actor": actor, "actor_id": actor_id, "action": action, "text": "", "text_entities": []}
    m.update(extra)
    return m


ME_TG = ("Tanuj", "user700000001")
SAM_TG = ("Sam Lee", "user700000002")
ROS_TG = ("Rosa Diaz", "user700000003")


def tg_personal_chat(tok, n_extra=0):
    sam, sid = SAM_TG
    me, mid_ = ME_TG
    msgs = [
        tg_msg(1, T(2026, 5, 3, 9, 12, 1), sam, sid, f"{tok}a The flight lands at 18:40, gate B"),
        tg_msg(2, T(2026, 5, 3, 9, 13, 0), me, mid_, ["Booked ", {"type": "bold", "text": f"{tok}b table"},
                                                      " at ", {"type": "link", "text": "https://example.com/r"}],
               edited="2026-05-03T09:20:00", edited_unixtime=str(int(T(2026, 5, 3, 9, 20, tzinfo=BERLIN).timestamp())),
               reply_to_message_id=1),
        tg_msg(3, T(2026, 5, 3, 9, 14, 0), sam, sid, "", photo="photos/photo_3@03-05-2026_09-14-00.jpg",
               photo_file_size=183245, width=1280, height=960),
        tg_msg(4, T(2026, 5, 3, 9, 15, 0), sam, sid, f"{tok}c here is the plan", file="files/Plan Q2.pdf",
               file_name="Plan Q2.pdf", file_size=44321, mime_type="application/pdf"),
        tg_service(5, T(2026, 5, 3, 10, 0, 0), sam, sid, "phone_call", duration_seconds=125, discard_reason="hangup"),
        # 00:30 local on 1 June = 22:30 UTC on 31 May
        tg_msg(6, T(2026, 6, 1, 0, 30, 0), sam, sid, f"{tok}d half past midnight on the first of June"),
    ]
    for i in range(n_extra):
        msgs.append(tg_msg(100 + i, T(2026, 6, 10, 12, i % 60, 0), me if i % 2 else sam, mid_ if i % 2 else sid,
                           f"{tok}x{i} extra message {i}"))
    return {"name": sam, "type": "personal_chat", "id": 700000002, "messages": msgs}


def tg_group_chat(tok):
    sam, sid = SAM_TG
    me, mid_ = ME_TG
    ros, rid = ROS_TG
    return {"name": "Hiking Crew", "type": "private_group", "id": 4400000001, "messages": [
        tg_service(1, T(2026, 4, 1, 10, 0, 0), ros, rid, "create_group", title="Hiking Crew", members=[sam, me]),
        tg_msg(2, T(2026, 4, 1, 10, 1, 0), ros, rid, f"{tok}g trailhead at 7am Saturday"),
        tg_msg(3, T(2026, 4, 1, 10, 2, 0), me, mid_, f"{tok}h I'll bring the map"),
        tg_msg(4, T(2026, 4, 1, 10, 3, 0), sam, sid, "", file="(File not included. Change data exporting settings to download.)",
               media_type="voice_message", mime_type="audio/ogg", duration_seconds=7),
        tg_msg(5, T(2026, 4, 1, 10, 4, 0), None, "user700000099", f"{tok}i message from a deleted account"),
    ]}


def tg_single_export(root, name, chat):
    d = os.path.join(root, name)
    os.makedirs(d, exist_ok=True)
    with open(os.path.join(d, "result.json"), "w", encoding="utf-8") as f:
        json.dump(chat, f, ensure_ascii=False, indent=1)
    return d


def tg_full_export(root, name, chats, left=()):
    d = os.path.join(root, name)
    os.makedirs(d, exist_ok=True)
    doc = {
        "about": "Here is the data you requested.",
        "personal_information": {"user_id": 700000001, "first_name": "Tanuj", "last_name": "",
                                 "phone_number": "+44 7700 900001", "username": "@tanuj_test", "bio": ""},
        "contacts": {"about": "contacts", "list": [
            {"user_id": 700000002, "first_name": "Sam", "last_name": "Lee", "phone_number": "+44 7700 900002",
             "date": "2025-01-01T10:00:00", "date_unixtime": "1735725600"}]},
        "chats": {"about": "chats", "list": list(chats)},
        "left_chats": {"about": "left", "list": list(left)},
    }
    with open(os.path.join(d, "result.json"), "w", encoding="utf-8") as f:
        json.dump(doc, f, ensure_ascii=False, indent=1)
    return d


# ---------------------------------------------------------------------------------------------
# iMessage: a chat.db with the macOS Messages schema (the tables and columns that matter, plus
# the many others Messages has, so that column discovery is exercised).

IMSG_SCHEMA = """
CREATE TABLE _SqliteDatabaseProperties (key TEXT, value TEXT, UNIQUE(key));
CREATE TABLE handle (ROWID INTEGER PRIMARY KEY AUTOINCREMENT UNIQUE, id TEXT NOT NULL, country TEXT,
  service TEXT NOT NULL, uncanonicalized_id TEXT, person_centric_id TEXT, UNIQUE (id, service));
CREATE TABLE chat (ROWID INTEGER PRIMARY KEY AUTOINCREMENT, guid TEXT UNIQUE NOT NULL, style INTEGER,
  state INTEGER, account_id TEXT, properties BLOB, chat_identifier TEXT, service_name TEXT, room_name TEXT,
  account_login TEXT, is_archived INTEGER DEFAULT 0, last_addressed_handle TEXT, display_name TEXT,
  group_id TEXT, is_filtered INTEGER DEFAULT 0, successful_query INTEGER, engram_id TEXT,
  server_change_token TEXT, ck_sync_state INTEGER DEFAULT 0, original_group_id TEXT,
  last_read_message_timestamp INTEGER DEFAULT 0, cloudkit_record_id TEXT, last_addressed_sim_id TEXT,
  is_blackholed INTEGER DEFAULT 0, syndication_date INTEGER DEFAULT 0, syndication_type INTEGER DEFAULT 0,
  is_recovered INTEGER DEFAULT 0, is_deleting_incoming_messages INTEGER DEFAULT 0);
CREATE TABLE message (ROWID INTEGER PRIMARY KEY AUTOINCREMENT, guid TEXT UNIQUE NOT NULL, text TEXT,
  replace INTEGER DEFAULT 0, service_center TEXT, handle_id INTEGER DEFAULT 0, subject TEXT, country TEXT,
  attributedBody BLOB, version INTEGER DEFAULT 0, type INTEGER DEFAULT 0, service TEXT, account TEXT,
  account_guid TEXT, error INTEGER DEFAULT 0, date INTEGER, date_read INTEGER, date_delivered INTEGER,
  is_delivered INTEGER DEFAULT 0, is_finished INTEGER DEFAULT 0, is_emote INTEGER DEFAULT 0,
  is_from_me INTEGER DEFAULT 0, is_empty INTEGER DEFAULT 0, is_delayed INTEGER DEFAULT 0,
  is_auto_reply INTEGER DEFAULT 0, is_prepared INTEGER DEFAULT 0, is_read INTEGER DEFAULT 0,
  is_system_message INTEGER DEFAULT 0, is_sent INTEGER DEFAULT 0, has_dd_results INTEGER DEFAULT 0,
  is_service_message INTEGER DEFAULT 0, is_forward INTEGER DEFAULT 0, was_downgraded INTEGER DEFAULT 0,
  is_archive INTEGER DEFAULT 0, cache_has_attachments INTEGER DEFAULT 0, cache_roomnames TEXT,
  was_data_detected INTEGER DEFAULT 0, was_deduplicated INTEGER DEFAULT 0, is_audio_message INTEGER DEFAULT 0,
  is_played INTEGER DEFAULT 0, date_played INTEGER, item_type INTEGER DEFAULT 0, other_handle INTEGER DEFAULT 0,
  group_title TEXT, group_action_type INTEGER DEFAULT 0, share_status INTEGER DEFAULT 0,
  share_direction INTEGER DEFAULT 0, is_expirable INTEGER DEFAULT 0, expire_state INTEGER DEFAULT 0,
  message_action_type INTEGER DEFAULT 0, message_source INTEGER DEFAULT 0, associated_message_guid TEXT,
  associated_message_type INTEGER DEFAULT 0, balloon_bundle_id TEXT, payload_data BLOB,
  expressive_send_style_id TEXT, associated_message_range_location INTEGER DEFAULT 0,
  associated_message_range_length INTEGER DEFAULT 0, time_expressive_send_played INTEGER,
  message_summary_info BLOB, ck_sync_state INTEGER DEFAULT 0, ck_record_id TEXT,
  ck_record_change_tag TEXT, destination_caller_id TEXT, is_corrupt INTEGER DEFAULT 0,
  reply_to_guid TEXT, sort_id INTEGER, is_spam INTEGER DEFAULT 0, has_unseen_mention INTEGER DEFAULT 0,
  thread_originator_guid TEXT, thread_originator_part TEXT, syndication_ranges TEXT,
  synced_syndication_ranges TEXT, was_delivered_quietly INTEGER DEFAULT 0,
  did_notify_recipient INTEGER DEFAULT 0, date_retracted INTEGER DEFAULT 0, date_edited INTEGER DEFAULT 0,
  was_detonated INTEGER DEFAULT 0, part_count INTEGER, is_stewie INTEGER DEFAULT 0,
  is_kt_verified INTEGER DEFAULT 0, is_sos INTEGER DEFAULT 0, is_critical INTEGER DEFAULT 0,
  bia_reference_id TEXT, fallback_hash TEXT);
CREATE TABLE attachment (ROWID INTEGER PRIMARY KEY AUTOINCREMENT, guid TEXT UNIQUE NOT NULL,
  created_date INTEGER DEFAULT 0, start_date INTEGER DEFAULT 0, filename TEXT, uti TEXT, mime_type TEXT,
  transfer_state INTEGER DEFAULT 0, is_outgoing INTEGER DEFAULT 0, user_info BLOB, transfer_name TEXT,
  total_bytes INTEGER DEFAULT 0, is_sticker INTEGER DEFAULT 0, sticker_user_info BLOB,
  attribution_info BLOB, hide_attachment INTEGER DEFAULT 0, ck_sync_state INTEGER DEFAULT 0,
  ck_server_change_token_blob BLOB, ck_record_id TEXT, original_guid TEXT UNIQUE NOT NULL,
  is_commsafety_sensitive INTEGER DEFAULT 0);
CREATE TABLE chat_message_join (chat_id INTEGER REFERENCES chat (ROWID) ON DELETE CASCADE,
  message_id INTEGER REFERENCES message (ROWID) ON DELETE CASCADE, message_date INTEGER DEFAULT 0,
  PRIMARY KEY (chat_id, message_id));
CREATE TABLE chat_handle_join (chat_id INTEGER REFERENCES chat (ROWID) ON DELETE CASCADE,
  handle_id INTEGER REFERENCES handle (ROWID) ON DELETE CASCADE, UNIQUE(chat_id, handle_id));
CREATE TABLE message_attachment_join (message_id INTEGER REFERENCES message (ROWID) ON DELETE CASCADE,
  attachment_id INTEGER REFERENCES attachment (ROWID) ON DELETE CASCADE, UNIQUE(message_id, attachment_id));
CREATE TABLE deleted_messages (ROWID INTEGER PRIMARY KEY AUTOINCREMENT UNIQUE, guid TEXT NOT NULL);
CREATE TABLE sync_deleted_messages (ROWID INTEGER PRIMARY KEY AUTOINCREMENT UNIQUE, guid TEXT NOT NULL, recordID TEXT);
"""

APPLE_EPOCH = 978307200


def apple_ns(t):
    """datetime (aware) -> Messages `date` (nanoseconds since 2001-01-01Z)."""
    return int((t.timestamp() - APPLE_EPOCH) * 1_000_000_000)


def typedstream(text, cls=b"NSString"):
    """An archived NSAttributedString as Messages stores it in `attributedBody` (written from
    the published typedstream layout: header, class chain, then `+` and the UTF-8 string)."""
    b = bytearray(b"\x04\x0bstreamtyped\x81\xe8\x03\x84\x01@\x84\x84\x84")
    outer = b"NSMutableAttributedString" if cls == b"NSMutableString" else b"NSAttributedString"
    b += bytes([len(outer)]) + outer + b"\x00\x84\x84\x08NSObject\x00\x85\x92\x84\x84\x84"
    b += bytes([len(cls)]) + cls + b"\x01\x94\x84\x01+"
    data = text.encode("utf-8")
    if len(data) < 0x80:
        b += bytes([len(data)])
    elif len(data) <= 0xFFFF:
        b += b"\x81" + struct.pack("<H", len(data))
    else:
        b += b"\x82" + struct.pack("<I", len(data))
    b += data
    b += b"\x86\x84\x02iI\x01" + bytes([len(text)]) + b"\x92\x84\x84\x84\x0cNSDictionary\x00\x94\x84\x01i\x01\x92\x84\x96\x96\x1d__kIMMessagePartAttributeName\x86\x84\x84\x84\x08NSNumber\x00\x84\x84\x07NSValue\x00\x94\x84\x01*\x84\x99\x99\x00\x86\x86\x86"
    return bytes(b)


UTC = dt.timezone.utc


def build_chat_db(path, tok, wal=False, bad_utf8=False, extra=()):
    """A chat.db with: a 1:1 iMessage chat (phone handle), the same person's SMS chat, a named
    group chat (style 43) with an e-mail handle, attributedBody-only messages, a tapback, an
    attachment, a group event, an unsent (retracted) message. Returns the open connection when
    `wal` (kept open so the WAL is not checkpointed), else None."""
    if os.path.exists(path):
        os.remove(path)
    for s in ("-wal", "-shm"):
        if os.path.exists(path + s):
            os.remove(path + s)
    c = sqlite3.connect(path)
    if wal:
        c.execute("PRAGMA journal_mode=WAL")
        c.execute("PRAGMA wal_autocheckpoint=0")
    c.executescript(IMSG_SCHEMA)
    H = [(1, "+15550100001", "US", "iMessage"), (2, "+15550100001", "US", "SMS"),
         (3, "rosa.diaz@example.com", None, "iMessage"), (4, "+447700900002", "GB", "iMessage")]
    c.executemany("INSERT INTO handle (ROWID,id,country,service,uncanonicalized_id,person_centric_id) VALUES (?,?,?,?,NULL,NULL)", H)
    C = [(1, "iMessage;-;+15550100001", 45, "+15550100001", "iMessage", None),
         (2, "SMS;-;+15550100001", 45, "+15550100001", "SMS", None),
         (3, "iMessage;+;chat123456789012345", 43, "chat123456789012345", "iMessage", "Book Club"),
         ]
    c.executemany("INSERT INTO chat (ROWID,guid,style,chat_identifier,service_name,display_name) VALUES (?,?,?,?,?,?)", C)
    c.executemany("INSERT INTO chat_handle_join VALUES (?,?)", [(1, 1), (2, 2), (3, 3), (3, 4)])
    rows = []
    t = lambda *a: T(*a, tzinfo=UTC)

    def msg(rid, chat, when, text=None, body=None, me=0, handle=0, **kw):
        rows.append((rid, chat, when, text, body, me, handle, kw))
    msg(1, 1, t(2026, 2, 10, 17, 0, 5), text=f"{tok}a Are we still on for dinner Thursday?", handle=1)
    msg(2, 1, t(2026, 2, 10, 17, 2, 0), text=f"{tok}b yes 7pm at Lucia's", me=1)
    msg(3, 1, t(2026, 2, 10, 17, 3, 0), body=typedstream(f"{tok}c attributed only message 👍 café"), handle=1)
    msg(4, 1, t(2026, 2, 10, 17, 4, 0), text="Loved “yes 7pm at Lucia's”", handle=1,
        associated_message_type=2000, associated_message_guid="p:0/M2")
    msg(5, 1, t(2026, 2, 10, 17, 5, 0), text="￼", handle=1)  # attachment only
    msg(6, 1, t(2026, 3, 1, 8, 0, 0), body=typedstream(f"{tok}d " + "long " * 40, b"NSMutableString"), me=1)
    msg(7, 1, t(2026, 3, 1, 8, 1, 0), text=None, body=None, handle=1, date_retracted=apple_ns(t(2026, 3, 1, 8, 2, 0)))
    msg(8, 2, t(2026, 1, 5, 12, 0, 0), text=f"{tok}e sms from the same person", handle=2)
    msg(9, 3, t(2026, 4, 1, 19, 0, 0), item_type=1, group_action_type=0, me=1, other_handle=4)
    msg(10, 3, t(2026, 4, 1, 19, 1, 0), text=f"{tok}f next book is Dune", handle=3)
    msg(11, 3, t(2026, 4, 1, 19, 2, 0), text=f"{tok}g I'll host in May", handle=4)
    msg(12, 3, t(2026, 4, 1, 19, 3, 0), item_type=2, group_title="Book Club 2026", handle=3)
    for r in extra:
        msg(*r[:3], **r[3])
    for rid, chat, when, text, body, me, handle, kw in rows:
        cols = {"ROWID": rid, "guid": f"MSG-{rid:04d}", "text": text, "attributedBody": body,
                "date": apple_ns(when), "is_from_me": me, "handle_id": handle, "service": "iMessage"}
        cols.update(kw)
        names = ",".join(cols)
        c.execute(f"INSERT INTO message ({names}) VALUES ({','.join('?' * len(cols))})", list(cols.values()))
        c.execute("INSERT INTO chat_message_join VALUES (?,?,?)", (chat, rid, apple_ns(when)))
    c.execute("INSERT INTO attachment (ROWID,guid,original_guid,filename,mime_type,transfer_name,total_bytes) VALUES (1,'AT1','AT1','~/Library/Messages/Attachments/ab/11/AT1/IMG_4021.HEIC','image/heic','IMG_4021.HEIC',2345678)")
    c.execute("INSERT INTO message_attachment_join VALUES (5,1)")
    if bad_utf8:
        # A text value with invalid UTF-8 bytes (seen in old SMS imports).
        c.execute("INSERT INTO message (ROWID,guid,text,date,handle_id) VALUES (50,'MSG-0050',CAST(X'48692080FF20746865726521' AS TEXT),?,1)",
                  (apple_ns(t(2026, 2, 11, 9, 0, 0)),))
        c.execute("INSERT INTO chat_message_join VALUES (1,50,0)")
    c.commit()
    if wal:
        return c
    c.close()
    return None


def build_address_book(dirpath):
    """AddressBook-v22.abcddb with the Core Data tables the reader looks at."""
    os.makedirs(dirpath, exist_ok=True)
    p = os.path.join(dirpath, "AddressBook-v22.abcddb")
    if os.path.exists(p):
        os.remove(p)
    c = sqlite3.connect(p)
    c.executescript("""
    CREATE TABLE ZABCDRECORD (Z_PK INTEGER PRIMARY KEY, Z_ENT INTEGER, ZFIRSTNAME VARCHAR, ZLASTNAME VARCHAR, ZORGANIZATION VARCHAR, ZNICKNAME VARCHAR);
    CREATE TABLE ZABCDPHONENUMBER (Z_PK INTEGER PRIMARY KEY, ZOWNER INTEGER, ZFULLNUMBER VARCHAR, ZLABEL VARCHAR);
    CREATE TABLE ZABCDEMAILADDRESS (Z_PK INTEGER PRIMARY KEY, ZOWNER INTEGER, ZADDRESS VARCHAR, ZADDRESSNORMALIZED VARCHAR);
    INSERT INTO ZABCDRECORD VALUES (1, 22, 'Alex', 'Moreno', NULL, NULL), (2, 22, 'Rosa', 'Diaz', NULL, NULL), (3, 22, NULL, NULL, 'Kestrel Books', NULL);
    INSERT INTO ZABCDPHONENUMBER VALUES (1, 1, '(555) 010-0001', '_$!<Mobile>!$_'), (2, 3, '+44 7700 900002', NULL);
    INSERT INTO ZABCDEMAILADDRESS VALUES (1, 2, 'Rosa.Diaz@example.com', 'rosa.diaz@example.com');
    """)
    c.commit()
    c.close()
    return p
