#!/usr/bin/env python3
"""Call retrieve.ask (no models available) with a short history to see query_rewrite + citations.
usage: probe_ask.py HOME OUTDIR"""
import asyncio, json, os, sys
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "harness"))
import ask_qa as h
CASES = [
    ("When is my dentist appointment?", "What time?"),
    ("When is my dentist appointment?", "Who is it with?"),
    ("When is my Kyoto flight?", "What time does it leave?"),
    ("How much is my Lumen Fiber bill?", "How does that compare to the previous one?"),
]
async def main(home, out):
    h.NUGGETD = "/home/user/nugget/target/release/nuggetd"
    os.makedirs(out, exist_ok=True)
    proc = h.start_daemon(h.NUGGETD, home, open(os.path.join(out, "nuggetd.log"), "ab"))
    try:
        rpc = await h.wait_sock(home, proc)
        res = []
        for q1, q2 in CASES:
            hist = [{"role": "user", "text": q1}, {"role": "assistant", "text": "(answer)"}]
            rec = await h.ask(rpc, q2, hist, 60)
            res.append({"q1": q1, **rec})
            print(q1, "->", q2, json.dumps({k: rec.get(k) for k in ("query_rewrite", "general", "error", "done_error")}),
                  [c["title"] for c in rec.get("citations") or []], flush=True)
        json.dump(res, open(os.path.join(out, "asks.json"), "w"), indent=1)
    finally:
        h.stop_daemon(proc)
asyncio.run(main(sys.argv[1], sys.argv[2]))
