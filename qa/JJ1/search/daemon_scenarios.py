"""JJ1 search QA, daemon level: search.faceted / search.timeline / search.preview against a
synthetic corpus with known ground truth (gen_corpus.py). Each scenario returns a list of
(check, ok, detail); run_all.py runs them and writes results.json."""
import json
import os
import time
from datetime import datetime, timezone, timedelta

from common import Daemon, RpcError, NUGGETD, corpus, fresh, index, key_to_name, faceted, all_hits

Q = "quokka"


def iso(s):
    if len(s) == 10:
        s += "T00:00:00Z"
    return datetime.fromisoformat(s.replace("Z", "+00:00"))


class Ctx:
    def __init__(self, d, truth, srcs):
        self.d, self.srcs = d, srcs
        # (source, key) -> truth
        self.t = {}
        for name, v in truth["items"].items():
            self.t[(srcs["files"], "0:" + name)] = dict(v, name=name)
        for month, at in truth["chat"]["months"].items():
            self.t[(srcs["wa"], f"wa:alice-varga/{month}")] = {"kind": "chat", "date": at, "people": ["alice", "erin"],
                                                               "sensitive": False, "name": f"chat {month}"}
        r = faceted(d, Q, limit=100)
        self.ids = {}
        for p in r["facets"]["people"]:
            for k, v in list(truth["people"].items()) + [("erin", {"name": "Erin Moss"})]:
                if p["name"] == v["name"]:
                    self.ids[k] = p["value"]
        self.people_names = {k: v["name"] for k, v in truth["people"].items()}

    def expect(self, kinds=None, sources=None, people=None, start=None, end=None):
        out = set()
        for ref, v in self.t.items():
            if kinds and v["kind"] not in kinds:
                continue
            if sources and ref[0] not in sources:
                continue
            if people and not (set(v["people"]) - set(v.get("text_people", []))) & set(people):
                continue
            at = iso(v["date"])
            if start and at < start:
                continue
            if end and at >= end:
                continue
            out.add(ref)
        return out


def refs(hits):
    return [(h["item"]["source"], h["item"]["key"]) for h in hits]


def short(s):
    return sorted(key_to_name(k) for _, k in s)


def chk(res, name, ok, detail=""):
    res.append((name, bool(ok), detail if not ok else ""))


# ------------------------------------------------------------------ scenarios

def s_facet_counts(c):
    res = []
    r = faceted(c.d, Q, limit=100)
    chk(res, "total equals true count (19)", r["total"] == len(c.t), f"total={r['total']} expected={len(c.t)}")
    chk(res, "every hit is unique", len(refs(r["hits"])) == len(set(refs(r["hits"]))), refs(r["hits"]))
    want = {}
    for v in c.t.values():
        want[v["kind"]] = want.get(v["kind"], 0) + 1
    got = {f["value"]: f["count"] for f in r["facets"]["kinds"]}
    chk(res, "kind counts", got == want, f"got={got} want={want}")
    got = {f["value"]: f["count"] for f in r["facets"]["sources"]}
    want = {c.srcs["files"]: 17, c.srcs["wa"]: 2}
    chk(res, "source counts", got == want, f"got={got} want={want}")
    for p in ["alice", "bob", "chen", "dana"]:
        want_n = len([v for v in c.t.values() if p in v["people"] and p not in v.get("text_people", [])])
        got_n = next((f["count"] for f in r["facets"]["people"] if f["value"] == c.ids.get(p)), None)
        chk(res, f"people count {p}", got_n == want_n, f"got={got_n} want={want_n}")
    months = {}
    for v in c.t.values():
        m = v["date"][:7]
        months[m] = months.get(m, 0) + 1
    got = {f["value"]: f["count"] for f in r["facets"]["months"]}
    chk(res, "month counts (UTC)", got == months, f"got={got} want={months}")
    return res


def s_own_dates(c):
    res = []
    r = faceted(c.d, Q, limit=100)
    for h in r["hits"]:
        v = c.t.get((h["item"]["source"], h["item"]["key"]))
        if not v:
            chk(res, f"unexpected hit {h['item']}", False, h["item"])
            continue
        ok = iso(h["date"]) == iso(v["date"])
        chk(res, f"date of {v['name']} is its own date", ok,
            f"got {h['date']} ({h['date_source']}), expected {v['date']}")
        want_src = "extracted" if v["kind"] in ("mail", "event") else "modified"
        chk(res, f"date_source of {v['name']}", h["date_source"] == want_src, f"got {h['date_source']} want {want_src}")
        chk(res, f"kind of {v['name']}", h["kind"] == v["kind"], f"got {h['kind']} want {v['kind']}")
        chk(res, f"sensitive flag of {v['name']}", h["sensitive"] == v["sensitive"], f"got {h['sensitive']} want {v['sensitive']}")
    return res


def s_filters(c):
    res = []
    F, WA = c.srcs["files"], c.srcs["wa"]
    cases = []
    for k in ["mail", "event", "chat", "file"]:
        cases.append(({"kinds": [k]}, dict(kinds=[k])))
    cases.append(({"kinds": ["mail", "event"]}, dict(kinds=["mail", "event"])))
    cases.append(({"sources": [WA]}, dict(sources=[WA])))
    cases.append(({"sources": [F, WA]}, dict(sources=[F, WA])))
    for p in ["alice", "bob", "dana"]:
        cases.append(({"people": [c.ids[p]]}, dict(people=[p])))
    cases.append(({"people": [c.ids["bob"], c.ids["dana"]]}, dict(people=["bob", "dana"])))
    cases.append(({"kinds": ["mail"], "people": [c.ids["alice"]]}, dict(kinds=["mail"], people=["alice"])))
    cases.append(({"kinds": ["mail", "chat"], "sources": [F, WA], "people": [c.ids["alice"]], "from": "2026-02-01", "to": "2026-06-30"},
                  dict(kinds=["mail", "chat"], sources=[F, WA], people=["alice"], start=iso("2026-02-01"), end=iso("2026-07-01"))))
    cases.append(({"from": "2026-03-01", "to": "2026-03-31"}, dict(start=iso("2026-03-01"), end=iso("2026-04-01"))))
    cases.append(({"from": "2026-06-01"}, dict(start=iso("2026-06-01"))))
    cases.append(({"to": "2026-01-31"}, dict(end=iso("2026-02-01"))))
    for f, e in cases:
        hits, totals = all_hits(c.d, Q, f, page=5, utc_offset_minutes=0)
        want = c.expect(**e)
        got = set(refs(hits))
        chk(res, f"filter {json.dumps(f)} hits", got == want, f"extra={short(got - want)} missing={short(want - got)}")
        chk(res, f"filter {json.dumps(f)} total", totals == {len(want)}, f"totals={totals} want={len(want)}")
    # Unknown person: matches nothing, still listed with count 0 so it can be cleared.
    r = faceted(c.d, Q, filters={"people": ["ent_doesnotexist"]})
    entry = [p for p in r["facets"]["people"] if p["value"] == "ent_doesnotexist"]
    chk(res, "unknown person filter: 0 hits and a 0-count entry", r["total"] == 0 and entry and entry[0]["count"] == 0, r["facets"]["people"][-1:])
    r = faceted(c.d, Q, filters={"sources": ["nope-9"]})
    entry = [p for p in r["facets"]["sources"] if p["value"] == "nope-9"]
    chk(res, "unknown source filter: 0 hits and a 0-count entry", r["total"] == 0 and entry and entry[0]["count"] == 0, r["facets"]["sources"])
    return res


def s_disjunctive(c):
    res = []
    F, WA = c.srcs["files"], c.srcs["wa"]
    r = faceted(c.d, Q, filters={"kinds": ["mail"]}, limit=100)
    kinds = {f["value"]: f["count"] for f in r["facets"]["kinds"]}
    chk(res, "kind=mail: kind facet still shows other kinds' full counts", kinds == {"mail": 10, "file": 4, "event": 3, "chat": 2}, kinds)
    src = {f["value"]: f["count"] for f in r["facets"]["sources"]}
    chk(res, "kind=mail: source counts are mail-only", src == {F: 10}, src)
    ppl = {f["value"]: f["count"] for f in r["facets"]["people"]}
    chk(res, "kind=mail: Alice count is mail-only (6)", ppl.get(c.ids["alice"]) == 6, ppl.get(c.ids["alice"]))
    chk(res, "kind=mail: Erin (chat only) is absent", c.ids.get("erin") not in ppl, ppl)
    r = faceted(c.d, Q, filters={"kinds": ["mail"], "people": [c.ids["alice"]]}, limit=100)
    kinds = {f["value"]: f["count"] for f in r["facets"]["kinds"]}
    chk(res, "kind=mail+Alice: kind facet counts Alice's items per kind", kinds == {"mail": 6, "event": 2, "chat": 2}, kinds)
    ppl = {f["value"]: f["count"] for f in r["facets"]["people"]}
    chk(res, "kind=mail+Alice: people facet = mail counts", ppl.get(c.ids["bob"]) == 5 and ppl.get(c.ids["alice"]) == 6, ppl)
    r = faceted(c.d, Q, filters={"from": "2026-06-01", "to": "2026-06-30"}, limit=100, utc_offset_minutes=0)
    months = {f["value"]: f["count"] for f in r["facets"]["months"]}
    chk(res, "date filter: months facet ignores the date filter", sum(months.values()) == 19, months)
    return res


def s_date_bounds(c):
    res = []
    F = c.srcs["files"]

    def keys(f, off):
        hits, _ = all_hits(c.d, Q, f, page=50, utc_offset_minutes=off)
        return sorted(key_to_name(k) for _, k in refs(hits))

    cases = [
        ({"from": "2026-01-31", "to": "2026-01-31"}, 0, ["mail/m03.eml"]),
        ({"from": "2026-01-31", "to": "2026-01-31"}, -300, ["mail/m02.eml", "mail/m03.eml"]),
        ({"from": "2026-01-31", "to": "2026-01-31"}, 60, []),
        ({"from": "2026-02-01", "to": "2026-02-01"}, 60, ["mail/m02.eml", "mail/m03.eml"]),
        ({"from": "2026-06-30", "to": "2026-06-30"}, 0, ["mail/m09.eml"]),
        ({"from": "2026-07-01", "to": "2026-07-01"}, 0, ["mail/m10.eml"]),
        ({"from": "2026-06-30", "to": "2026-06-30"}, -300, ["mail/m09.eml", "mail/m10.eml"]),
        # An all-day event (1 March) is on 1 March at any offset.
        ({"kinds": ["event"], "from": "2026-03-01", "to": "2026-03-01"}, -300, ["cal/e02.ics"]),
        ({"kinds": ["event"], "from": "2026-03-01", "to": "2026-03-01"}, 600, ["cal/e02.ics"]),
        # A 20:00 New York event on 15 April is 16 April for a Berlin (+120) user and 15 April in New York (-240).
        ({"kinds": ["event"], "from": "2026-04-16", "to": "2026-04-16"}, 120, ["cal/e03.ics"]),
        ({"kinds": ["event"], "from": "2026-04-15", "to": "2026-04-15"}, -240, ["cal/e03.ics"]),
    ]
    for f, off, want in cases:
        got = keys(f, off)
        chk(res, f"{json.dumps(f)} at offset {off}", got == want, f"got={got} want={want}")
    # The desktop sends today's offset. In October a Berlin client sends +120 (CEST), but a mail
    # sent 23:30 CET on 15 Jan (UTC+1 then) is on 15 Jan for that user.
    got = keys({"kinds": ["mail"], "from": "2026-01-15", "to": "2026-01-15"}, 120)
    chk(res, "DST: Berlin user (today +120) filtering 15 Jan sees the 23:30 CET mail", "mail/m05.eml" in got, f"got={got}")
    r = faceted(c.d, Q, limit=1, utc_offset_minutes=-300)
    months = {f["value"]: f["count"] for f in r["facets"]["months"]}
    chk(res, "months at -300: 1 Jul 00:00Z counts in June", months.get("2026-07", 0) == 0 and months.get("2026-06") == 4, months)
    # Invalid input is refused with a clear error.
    for f, off in [({"from": "2026-03-02", "to": "2026-03-01"}, 0), ({"from": "2026-02-30"}, 0), ({"to": "yesterday"}, 0),
                   ({}, 18 * 60 + 1), ({"from": "2026-1-01"}, 0)]:
        try:
            faceted(c.d, Q, filters=f, utc_offset_minutes=off)
            chk(res, f"invalid {json.dumps(f)} offset {off} refused", False, "accepted")
        except RpcError as e:
            chk(res, f"invalid {json.dumps(f)} offset {off} refused", e.code == -32602, f"code={e.code} {e}")
    return res


def s_pagination(c):
    res = []
    full = faceted(c.d, Q, limit=100)["hits"]
    for page in (1, 3, 7):
        hits, totals = all_hits(c.d, Q, {}, page=page)
        chk(res, f"quokka paged by {page}: same order, no dup/gap", refs(hits) == refs(full), f"{len(hits)} vs {len(full)}")
    # Pool limit: 350 wombat notes.
    r = faceted(c.d, "wombat", limit=100)
    chk(res, "wombat: pool 300, pool_limited", r["pool"] == 300 and r["pool_limited"] is True, {k: r[k] for k in ("pool", "pool_limited", "total")})
    hits, totals = all_hits(c.d, "wombat", {}, page=100)
    rr = refs(hits)
    chk(res, "wombat paged: 300 unique hits, total 300", len(rr) == 300 and len(set(rr)) == 300 and totals == {300}, f"n={len(rr)} unique={len(set(rr))} totals={totals}")
    hits25, _ = all_hits(c.d, "wombat", {}, page=25)
    chk(res, "wombat paged by 25 equals paged by 100", refs(hits25) == rr, f"{len(hits25)}")
    r = faceted(c.d, Q, limit=10, offset=1000)
    chk(res, "offset past the end: no hits, total kept", r["hits"] == [] and r["total"] == 19, {k: r[k] for k in ("total",)})
    r = faceted(c.d, Q, limit=100000)
    chk(res, "limit clamps to 100", len(r["hits"]) == 19, len(r["hits"]))
    r = faceted(c.d, "wombat", limit=100000)
    chk(res, "limit clamps to 100 (wombat)", len(r["hits"]) == 100, len(r["hits"]))
    for bad in [-1, "x", 2 ** 40]:
        try:
            faceted(c.d, Q, limit=bad)
            chk(res, f"limit={bad!r} refused", False, "accepted")
        except RpcError as e:
            chk(res, f"limit={bad!r} refused", e.code == -32602, f"{e.code} {e}")
    return res


def s_empty_and_hostile(c):
    res = []
    for q in ["", "   ", "\n\t"]:
        r = faceted(c.d, q)
        chk(res, f"empty query {q!r}: no hits, no facets", r["total"] == 0 and r["hits"] == [], r)
    hostile = ['"', '""', '"quokka', 'quokka"', "*", "quokka*", "*quokka", "NEAR", "NEAR(quokka survey)", "quokka NEAR survey",
               "-", "-quokka", "quokka -survey", "quokka OR", "OR", "AND", "NOT", "quokka AND NOT", "(", ")", "quokka)", "((quokka",
               "'", "\\", "title:quokka", "text:quokka", "^quokka", "quokka:", "{quokka}", "[quokka]", "quokka;DROP TABLE items;--",
               "%", "_", "kängurú 袋鼠 🦘 quokka", "ｑｕｏｋｋａ", "QUOKKA", "quo\u0000kka", "‮quokka", "quokka​",
               "a" * 4096, ("quokka " * 600)[:4096]]
    must_find = {"quokka*", '"quokka', 'quokka"', "QUOKKA", "quokka)", "((quokka", "title:quokka", "^quokka", "{quokka}",
                 "[quokka]", "quokka:", "kängurú 袋鼠 🦘 quokka", "quokka -survey", "quokka OR", "quokka AND NOT", "quokka NEAR survey"}
    for q in hostile:
        label = q if len(q) < 40 else f"{q[:12]}... ({len(q)} bytes)"
        try:
            r = faceted(c.d, q, limit=5)
            ok = isinstance(r.get("hits"), list)
            if q in must_find:
                ok = ok and r["total"] > 0
            chk(res, f"hostile {label!r}", ok, f"total={r['total']}")
        except RpcError as e:
            chk(res, f"hostile {label!r}", False, f"error {e.code}: {e}")
    for q in ["a" * 4097, "é" * 3000]:
        try:
            faceted(c.d, q)
            chk(res, f"query of {len(q.encode())} bytes refused", False, "accepted")
        except RpcError as e:
            chk(res, f"query of {len(q.encode())} bytes refused", e.code == -32602, f"{e.code} {e}")
    chk(res, "daemon alive after hostile queries", c.d.call("status").get("items") is not None)
    # Bad filter values.
    for f in [{"kinds": ["Mail"]}, {"kinds": ["email"]}, {"people": ["x"] * 51}, {"sources": ["s" * 1025]}]:
        try:
            faceted(c.d, Q, filters=f)
            chk(res, f"bad filter {json.dumps(f)[:60]} refused", False, "accepted")
        except RpcError as e:
            chk(res, f"bad filter {json.dumps(f)[:60]} refused", e.code == -32602, f"{e.code} {e}")
    return res


def s_preview(c):
    res = []
    F, WA = c.srcs["files"], c.srcs["wa"]
    hits = faceted(c.d, Q, limit=100)["hits"]
    chunk_of = {(h["item"]["source"], h["item"]["key"]): h["chunk_id"] for h in hits}
    tokens = {ref: f"zq{key_to_name(ref[1]).split('/')[-1].split('.')[0]}" for ref in c.t if ref[0] == F}
    for ref, v in c.t.items():
        p = c.d.call("search.preview", {"item": {"source": ref[0], "key": ref[1]}, "chunk_id": chunk_of.get(ref)})
        chk(res, f"preview {v['name']}: sensitive label", p["sensitive"] == v["sensitive"], p["sensitive"])
        chk(res, f"preview {v['name']}: ≤ 20,000 chars", len(p["text"]) <= 20000, len(p["text"]))
        chk(res, f"preview {v['name']}: own date", iso(p["date"]) == iso(v["date"]), f"{p['date']} vs {v['date']}")
        others = [t for r2, t in tokens.items() if r2 != ref and t in p["text"]]
        chk(res, f"preview {v['name']}: no other item's text", not others, others)
        if p["passage"] is not None:
            # A chunk may start with the item's title as context; the rest must be this item's text.
            body = p["passage"].split("\n", 1)[-1] if p["title"] and p["passage"].startswith(p["title"]) else p["passage"]
            full = c.d.call("search.preview", {"item": p["item"]})["text"]
            chk(res, f"preview {v['name']}: passage is from this item", body.strip()[:200] in full or len(full) >= 20000, p["passage"][:80])
    # Long item: cut, flagged, marker past 20,000 chars absent.
    p = c.d.call("search.preview", {"item": {"source": F, "key": "0:notes/long.txt"}})
    chk(res, "long item: truncated=true, 20,000 chars, marker beyond the cut absent",
        p["truncated"] is True and len(p["text"]) == 20000 and "PAST_PREVIEW_LIMIT_MARKER" not in p["text"], (p["truncated"], len(p["text"])))
    # Chat month documents contain only their month.
    p = c.d.call("search.preview", {"item": {"source": WA, "key": "wa:alice-varga/2026-06"}})
    chk(res, "chat preview (June) holds only June messages", "zqwa2" in p["text"] and "zqwa1" not in p["text"], p["text"][:200])
    # chunk_id of another item: no passage from it.
    other_chunk = chunk_of[(F, "0:mail/m04.eml")]
    p = c.d.call("search.preview", {"item": {"source": F, "key": "0:mail/m01.eml"}, "chunk_id": other_chunk})
    chk(res, "chunk_id of another (sensitive) item gives no passage", p["passage"] is None and "DE89" not in json.dumps(p), p["passage"])
    p = c.d.call("search.preview", {"item": {"source": F, "key": "0:mail/m01.eml"}, "chunk_id": 999999})
    chk(res, "unknown chunk_id gives no passage", p["passage"] is None, p["passage"])
    # Sensitive item: labelled and its text shown (local only).
    p = c.d.call("search.preview", {"item": {"source": F, "key": "0:mail/m04.eml"}})
    chk(res, "IBAN mail labelled sensitive", p["sensitive"] is True, p["sensitive"])
    p = c.d.call("search.preview", {"item": {"source": F, "key": "0:notes/f03.txt"}})
    chk(res, "card-number note labelled sensitive", p["sensitive"] is True, p["sensitive"])
    # Unknown / mismatched / hostile item refs.
    for item in [{"source": F, "key": "0:nope.txt"}, {"source": WA, "key": "0:mail/m01.eml"}, {"source": "files-9", "key": "0:mail/m01.eml"},
                 {"source": F, "key": "0:../../etc/passwd"}, {"source": F, "key": "/etc/passwd"}, {"source": "", "key": "x"}, {"source": F, "key": "k" * 2000}]:
        try:
            p = c.d.call("search.preview", {"item": item})
            chk(res, f"preview of {json.dumps(item)[:60]} refused", False, p.get("text", "")[:80])
        except RpcError as e:
            chk(res, f"preview of {json.dumps(item)[:60]} refused", e.code in (-32602, -32004, -32001, -32000, 404) or "not found" in str(e).lower(), f"{e.code} {e}")
    return res


def s_timeline(c):
    res = []
    F, WA = c.srcs["files"], c.srcs["wa"]
    alice = c.ids["alice"]
    t = c.d.call("search.timeline", {"entity": alice, "limit": 1000, "utc_offset_minutes": 0})
    items = [e for e in t["events"] if e["type"] == "item"]
    facts = [e for e in t["events"] if e["type"] == "fact"]
    got = {(e["item"]["source"], e["item"]["key"]) for e in items}
    want_struct = {ref for ref, v in c.t.items() if "alice" in v["people"] and "alice" not in v.get("text_people", [])}
    want_all = {ref for ref, v in c.t.items() if "alice" in v["people"]}
    chk(res, "Alice: every item linked by headers/attendees/chat is in the timeline", want_struct <= got, f"missing={short(want_struct - got)}")
    chk(res, "Alice: no item that does not involve her", got <= want_all, f"extra={short(got - want_all)}")
    chk(res, "Alice: the note naming her in its text is in the timeline (guide: 'every item that mentions them')",
        want_all <= got, f"missing={short(want_all - got)}")
    ats = [iso(e["at"]) for e in t["events"]]
    chk(res, "Alice: events in time order", ats == sorted(ats), [e["at"] for e in t["events"]])
    for e in items:
        v = c.t.get((e["item"]["source"], e["item"]["key"]))
        if v:
            chk(res, f"Alice timeline: {v['name']} at its own date", iso(e["at"]) == iso(v["date"]), f"{e['at']} vs {v['date']}")
            chk(res, f"Alice timeline: {v['name']} sensitive flag", e["sensitive"] == v["sensitive"], e["sensitive"])
    chk(res, "Alice: total equals events, not truncated", t["total"] == len(t["events"]) and t["truncated"] is False, (t["total"], len(t["events"]), t["truncated"]))
    chk(res, "Alice: month counts add up to total", sum(m["count"] for m in t["months"]) == t["total"], t["months"])
    # Truncation keeps the newest.
    t3 = c.d.call("search.timeline", {"entity": alice, "limit": 3, "utc_offset_minutes": 0})
    chk(res, "limit=3 keeps the newest 3, ascending, truncated, total unchanged",
        [e["at"] for e in t3["events"]] == [e["at"] for e in t["events"]][-3:] and t3["truncated"] is True and t3["total"] == t["total"],
        ([e["at"] for e in t3["events"]], t3["truncated"], t3["total"]))
    chk(res, "limit=3: month counts still cover everything", t3["months"] == t["months"], t3["months"])
    # Bounds.
    tb = c.d.call("search.timeline", {"entity": alice, "from": "2026-02-01", "to": "2026-03-31", "utc_offset_minutes": 0, "limit": 1000})
    lo, hi = iso("2026-02-01"), iso("2026-04-01")
    want_n = len([e for e in t["events"] if lo <= iso(e["at"]) < hi])
    chk(res, "from/to bounds: only events in range, all of them",
        all(lo <= iso(e["at"]) < hi for e in tb["events"]) and len(tb["events"]) == want_n, ([e["at"] for e in tb["events"]], want_n))
    tb = c.d.call("search.timeline", {"entity": alice, "from": "2026-01-31", "to": "2026-01-31", "utc_offset_minutes": -300})
    keys = sorted(key_to_name(e["item"]["key"]) for e in tb["events"] if e["type"] == "item")
    chk(res, "timeline 31 Jan at -300: m02 and m03", keys == ["mail/m02.eml", "mail/m03.eml"], keys)
    # Erin (only in chats).
    if c.ids.get("erin"):
        te = c.d.call("search.timeline", {"entity": c.ids["erin"]})
        ek = {(e["item"]["source"], e["item"]["key"]) for e in te["events"] if e["type"] == "item"}
        chk(res, "Erin (chat only): both chat months", ek == {(WA, "wa:alice-varga/2026-02"), (WA, "wa:alice-varga/2026-06")}, ek)
    # Errors.
    for p in [{"entity": "ent_nope"}, {"entity": ""}, {"entity": alice, "from": "2026-05-01", "to": "2026-04-01"}, {"entity": "x" * 2000}]:
        try:
            c.d.call("search.timeline", p)
            chk(res, f"timeline {json.dumps(p)[:60]} refused", False, "accepted")
        except RpcError as e:
            chk(res, f"timeline {json.dumps(p)[:60]} refused", True)
    res.append(("info: facts on Alice's timeline", True, len(facts)))
    return res


def s_removed_source(c):
    """Last: removes the WhatsApp source."""
    res = []
    WA = c.srcs["wa"]
    alice = c.ids["alice"]
    c.d.call("sources.remove", {"id": WA})
    time.sleep(3)
    c.d.wait_idle(settle=2)
    try:
        p = c.d.call("search.preview", {"item": {"source": WA, "key": "wa:alice-varga/2026-06"}})
        chk(res, "preview of an item from a removed source is refused", False, p["text"][:100])
    except RpcError as e:
        chk(res, "preview of an item from a removed source is refused", True)
    r = faceted(c.d, Q, limit=100)
    chk(res, "removed source gone from hits and source facet",
        all(h["item"]["source"] != WA for h in r["hits"]) and all(f["value"] != WA for f in r["facets"]["sources"]) and r["total"] == 17,
        {"total": r["total"], "sources": r["facets"]["sources"]})
    try:
        t = c.d.call("search.timeline", {"entity": alice, "limit": 1000})
        chk(res, "Alice's timeline drops the removed chats", all(e["item"]["source"] != WA for e in t["events"] if e["type"] == "item"),
            [e["item"] for e in t["events"] if e["type"] == "item" and e["item"]["source"] == WA])
    except RpcError as e:
        chk(res, "Alice's timeline still answers", False, str(e))
    if c.ids.get("erin"):
        try:
            t = c.d.call("search.timeline", {"entity": c.ids["erin"]})
            chk(res, "Erin (only in removed chats): empty timeline or not found", not [e for e in t["events"] if e["type"] == "item"], t["events"][:2])
        except RpcError:
            chk(res, "Erin (only in removed chats): empty timeline or not found", True)
    r = faceted(c.d, Q, limit=100)
    chk(res, "Erin no longer in the people facet", all(p["value"] != c.ids.get("erin") for p in r["facets"]["people"]),
        [p for p in r["facets"]["people"] if p["value"] == c.ids.get("erin")])
    return res


def s_restart(c):
    res = []
    before = faceted(c.d, Q, limit=100, filters={"kinds": ["mail"]})
    c.d.restart()
    after = faceted(c.d, Q, limit=100, filters={"kinds": ["mail"]})
    chk(res, "same hits and facets after a daemon restart",
        refs(before["hits"]) == refs(after["hits"]) and before["facets"] == after["facets"], (before["total"], after["total"]))
    return res


def s_date_phrases(c):
    res = []
    r = faceted(c.d, "quokka in march 2026", limit=100)
    chk(res, "'quokka in march 2026' keeps March items only", r["total"] == 3 and all(h["date"].startswith("2026-03") for h in r["hits"]),
        [h["date"][:10] for h in r["hits"]])
    # Today is after the corpus: "last month" names a month with no quokka items.
    r = faceted(c.d, "quokka last month", limit=100)
    chk(res, "'quokka last month' (no items that month): no hits, or the result says the date was not applied",
        r["total"] == 0 or any(k in r for k in ("date", "date_filter")),
        f"total={r['total']}, keys={sorted(r)}; months={[m['value'] for m in r['facets']['months']]}")
    return res


SCENARIOS = [
    ("D1 facet counts", s_facet_counts),
    ("D2 own dates, kinds, sensitive", s_own_dates),
    ("D3 filters and combinations", s_filters),
    ("D4 disjunctive counts", s_disjunctive),
    ("D5 date boundaries and offsets", s_date_bounds),
    ("D6 pagination and pool", s_pagination),
    ("D7 empty and hostile queries", s_empty_and_hostile),
    ("D8 preview", s_preview),
    ("D9 timeline", s_timeline),
    ("D10 restart", s_restart),
    ("D12 date phrases in the query", s_date_phrases),
    ("D11 removed source", s_removed_source),
]


def run(only=None):
    c_dir, truth = corpus()
    home = fresh("daemon")
    out = []
    with Daemon(home, nuggetd=NUGGETD) as d:
        srcs = index(d, c_dir)
        c = Ctx(d, truth, srcs)
        for name, fn in SCENARIOS:
            if only and not any(o in name for o in only):
                continue
            try:
                checks = fn(c)
            except Exception as e:  # a crash fails the scenario
                checks = [("scenario ran", False, f"{type(e).__name__}: {e}")]
            out.append({"scenario": name, "checks": [{"check": n, "ok": ok, "detail": det if not ok else None} for n, ok, det in checks],
                        "pass": all(ok for _, ok, _ in checks)})
            print(f"{'PASS' if out[-1]['pass'] else 'FAIL'}  {name}  ({sum(ok for _, ok, _ in checks)}/{len(checks)})")
            for n, ok, det in checks:
                if not ok:
                    print(f"      x {n}: {str(det)[:300]}")
    return out


if __name__ == "__main__":
    import sys
    run(sys.argv[1:] or None)
