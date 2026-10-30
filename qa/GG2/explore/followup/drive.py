#!/usr/bin/env python3
"""Drive the follow-up conversations through retrieve.ask like apps/desktop Chat.tsx.

usage: drive.py <home> <fake_port> <out.jsonl> [--ingest] [--convs F01,F02|all] [--extra]
                [--rewrite-mode map|echo|error|slow|empty] [--delay S] [--answer TEXT]

For each turn records: started_ms (retrieve.ask round trip = rewrite + retrieval + first provider
event), query_rewrite, the cited files (relative to the corpus) and whether any support item was
cited. Also runs retrieve.context on the ORIGINAL follow-up text so the effect of the rewrite is
visible. History is built like Chat.tsx (user text, assistant text with [n] markers stripped).
"""
import argparse
import asyncio
import json
import os
import sys
import time
import urllib.request

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "harness"))
from ask_qa import Rpc, RpcError, strip_markers, wait_ingest  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", "..", "..", ".."))
CORPUS = os.path.join(REPO, "qa", "GG2", "corpus")
QUESTIONS = os.path.join(REPO, "qa", "GG2", "questions.jsonl")

# Extra probes (area 2): not in questions.jsonl.
EXTRA = [
    {"id": "X01", "turns": [{"q": "How much was the hotel in Lisbon?", "support": ["mail/6-travel/lisbon-hotel.eml"]},
                            {"q": "What about Kyoto?", "support": ["mail/6-travel/kyoto-flight.eml", "mail/6-travel/kyoto-ryokan.eml"]},
                            {"q": "When do I fly there?", "support": ["mail/6-travel/kyoto-flight.eml"]}]},
    {"id": "X02", "turns": [{"q": "When is my dentist appointment?", "support": ["mail/4-health/dentist-rescheduled.eml", "calendar/dentist.ics"]},
                            {"q": "Thanks! Now, what's my electricity bill?", "support": ["mail/5-bills/citypower.eml"]}]},
    {"id": "X03", "turns": [{"q": "Who is Bob Lindqvist?", "support": ["documents/work/Bob Lindqvist 1-1 notes.md"]},
                            {"q": "What's his phone number?", "support": []},
                            {"q": "What's he working on?", "support": ["documents/work/Bob Lindqvist 1-1 notes.md"]}]},
]

EXTRA += [
    # Contractions: "what's" splits into "what" + "s", and "s" is not a stopword.
    {"id": "X04", "turns": [{"q": "When is my dentist appointment?", "support": ["mail/4-health/dentist-rescheduled.eml", "calendar/dentist.ics"]},
                            {"q": "What's the address?", "support": ["mail/4-health/dentist-rescheduled.eml", "calendar/dentist.ics"]}]},
    {"id": "X05", "turns": [{"q": "When is my dentist appointment?", "support": ["mail/4-health/dentist-rescheduled.eml", "calendar/dentist.ics"]},
                            {"q": "What is the address?", "support": ["mail/4-health/dentist-rescheduled.eml", "calendar/dentist.ics"]}]},
]

EXTRA += [
    # Area 4: a general (no-citation) reply, then a follow-up that should search the items.
    {"id": "X06", "turns": [{"q": "Explain quantum entanglement simply", "support": []},
                            {"q": "Thanks. What's my electricity bill?", "support": ["mail/5-bills/citypower.eml"]},
                            {"q": "When is it due?", "support": ["mail/5-bills/citypower.eml"]}]},
    {"id": "X07", "turns": [{"q": "What is a ryokan?", "support": []},
                            {"q": "Am I staying in one?", "support": ["mail/6-travel/kyoto-ryokan.eml"]}]},
]

# What a good rewriting model would answer (used with --rewrite-mode map).
GOOD = {
    "Who is it with?": "dentist appointment with whom",
    "And where is it?": "dentist appointment location address",
    "How many nights was that?": "Lisbon hotel number of nights",
    "And what about Kyoto?": "Kyoto trip cost",
    "What about Kyoto?": "Kyoto trip cost",
    "What about cloud spend?": "Sam budget cloud spend",
    "Does unspent money roll over?": "budget unspent money roll over",
    "When is he on leave?": "Bob Lindqvist leave dates",
    "What's the salary?": "Corvid offer salary",
    "Is there a non-compete?": "Corvid offer non-compete clause",
    "When is it due?": "electricity bill due date",
    "How does that compare to the previous one?": "previous electricity bill amount",
    "What should I do about it?": "blood test results advice",
    "When should it be rechecked?": "blood test recheck",
    "And the booking reference?": "Kyoto flight booking reference",
    "Who else is on it?": "Kyoto flight passengers",
    "And Harbor?": "who is taking over Harbor",
    "When are the handover sessions?": "handover sessions dates",
    "What's the new rent he's proposing?": "landlord Achebe new rent proposal",
    "What's his phone number?": "landlord Achebe phone number",
    "What is it for?": "physio appointment reason",
    "And the equity?": "Corvid contract equity",
    "What was it in the first version?": "Corvid offer v1 equity",
    "What time?": "farewell drinks time",
    "When is my call with her?": "call with Lena Corvid",
    "What should I bring?": "mum birthday lunch what to bring",
    "What gift did I plan for her?": "gift idea for mum",
    "When do I fly there?": "Kyoto flight date",
    "What's he working on?": "Bob Lindqvist projects",
    "What's the address?": "dentist appointment address",
    "What is the address?": "dentist appointment address",
    "Am I staying in one?": "ryokan booking stay",
}


def rel(c):
    u = c.get("url") or ""
    if u.startswith("file://"):
        u = u[7:]
    u = urllib.request.unquote(u)
    return os.path.relpath(u, CORPUS) if u.startswith(CORPUS) else (u or c.get("title"))


def admin(port, **kw):
    req = urllib.request.Request(f"http://127.0.0.1:{port}/_admin", data=json.dumps(kw).encode(),
                                 headers={"content-type": "application/json"})
    return json.load(urllib.request.urlopen(req))


async def run(a):
    sock = os.path.join(a.home, "data", "ipc", "nuggetd.sock")
    rpc = await Rpc.connect(sock)
    if a.ingest:
        srcs, _, _ = await rpc.call("sources.list", timeout=30)
        if not srcs:
            await rpc.call("sources.add", {"kind": "files", "name": "Dana's files", "settings": {"roots": [CORPUS]}}, timeout=120)
        st, secs = await wait_ingest(rpc, sum(len(f) for _, _, f in os.walk(CORPUS)))
        print("ingest", secs, json.dumps(st)[:400])
    admin(a.port, rewrite=a.rewrite_mode, delay=a.delay, answer=a.answer, map=GOOD)
    convs = [json.loads(l) for l in open(QUESTIONS)][1:]
    convs = [c for c in convs if c["category"] == "followup"]
    if a.extra:
        convs += EXTRA
    if a.convs != "all":
        ids = set(a.convs.split(","))
        convs = [c for c in convs if c["id"] in ids]
    out = open(a.out, "w")
    for c in convs:
        history = []
        for i, t in enumerate(c["turns"]):
            params = {"query": t["q"], "budget_tokens": 2000, "provider": None, "tier": "synthesis",
                      "history": history[-24:]}
            rec = {"conv": c["id"], "turn": i + 1, "q": t["q"], "support": t.get("support", [])}
            t0 = time.monotonic()
            try:
                st, _, at = await rpc.call("retrieve.ask", params, timeout=120)
                rec["started_ms"] = round((at - t0) * 1000)
                rec["query_rewrite"] = st.get("query_rewrite")
                rec["general"] = st.get("general")
                rec["provider"] = st.get("provider")
                rec["cited"] = [rel(x) for x in st["citations"]]
                q = rpc.attach(st["stream_id"])
                text = []
                while True:
                    _, m, p = await asyncio.wait_for(q.get(), 60)
                    if m == "chat.delta":
                        text.append(p.get("text", ""))
                    else:
                        rec["done_error"] = p.get("error")
                        break
                rec["total_ms"] = round((time.monotonic() - t0) * 1000)
                answer = "".join(text)
            except (RpcError, asyncio.TimeoutError) as e:
                rec["error"] = str(e)
                answer = ""
            sup = set(t.get("support", []))
            rec["support_cited"] = sorted(sup & set(rec.get("cited") or []))
            rec["hit"] = bool(rec["support_cited"]) if sup else None
            if i > 0:
                ctx, _, _ = await rpc.call("retrieve.context", {"query": t["q"], "budget_tokens": 2000}, timeout=60)
                orig = [rel(x) for x in ctx["citations"]]
                rec["orig_cited"] = orig
                rec["orig_hit"] = bool(sup & set(orig)) if sup else None
            print(json.dumps({k: rec.get(k) for k in ("conv", "turn", "q", "query_rewrite", "started_ms", "hit", "orig_hit", "general", "error")}))
            out.write(json.dumps(rec) + "\n")
            out.flush()
            await asyncio.sleep(a.gap)
            history += [{"role": "user", "text": t["q"]}, {"role": "assistant", "text": strip_markers(answer)}]


ap = argparse.ArgumentParser()
ap.add_argument("home")
ap.add_argument("port", type=int)
ap.add_argument("out")
ap.add_argument("--ingest", action="store_true")
ap.add_argument("--convs", default="all")
ap.add_argument("--extra", action="store_true")
ap.add_argument("--rewrite-mode", default="map")
ap.add_argument("--delay", type=float, default=0)
ap.add_argument("--gap", type=float, default=0, help="seconds to wait after each turn (reading time)")
ap.add_argument("--answer", default="Canned answer [1].")
asyncio.run(run(ap.parse_args()))
