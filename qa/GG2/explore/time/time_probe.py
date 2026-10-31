#!/usr/bin/env python3
"""GG2 explore/time: what does nuggetd do with time-sensitive questions?

For each query in --queries it records:
  - `retrieve.context` (budget 2000, as Chat): every citation's title, key and `date` (the item
    date retrieval filters on), whether that date lies in the range we expect the phrase to name,
    and the packed passage text;
  - with --ask: `retrieve.ask` against a fake OpenAI-compatible server (capture_llm.py) that
    stands in for the local model, so the exact request the answering model would get (system
    prompt + messages) is captured.

The daemon is real (target/release/nuggetd); the corpus is ingested through a `files` source.

Usage:
  time_probe.py --home H --corpus DIR --queries queries.json --out OUT [--ask --capture-port P]
The home must already hold a config.toml (start nuggetd once); this script edits it.
"""

import argparse
import asyncio
import datetime as dt
import json
import os
import re
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "harness"))
import ask_qa  # noqa: E402
from ask_qa import Rpc, start_daemon, stop_daemon, wait_ingest, wait_sock  # noqa: E402,F401


def edit_config(path, capture_port):
    s = open(path, encoding="utf-8").read()
    s = s.replace("[knowledge]\nenabled = true", "[knowledge]\nenabled = false", 1)
    s = s.replace("[vault]\nenabled = true", "[vault]\nenabled = false", 1)
    s = re.sub(r"(\[mcp\]\nenabled = \w+\n)http = true", r"\1http = false", s, count=1)
    s = re.sub(r"idle_stop_minutes = \d+", "idle_stop_minutes = 0", s, count=1)
    if capture_port:
        s = s.replace('base_url = "http://127.0.0.1:11434/v1"\nmodel = "qwen3:4b"',
                      f'base_url = "http://127.0.0.1:{capture_port}/v1"\nmodel = "capture"', 1)
    open(path, "w", encoding="utf-8").write(s)
    return s


def in_range(date, rng):
    if not rng or not date:
        return None
    return rng[0] <= date < rng[1]


async def run(a):
    os.makedirs(a.out, exist_ok=True)
    home = os.path.abspath(a.home)
    ask_qa.NUGGETD = a.nuggetd
    cfg = os.path.join(home, "config", "config.toml")
    text = edit_config(cfg, a.capture_port if a.ask else None)
    open(os.path.join(a.out, "config.toml"), "w").write(text)
    logf = open(os.path.join(a.out, "nuggetd.log"), "ab")
    proc = start_daemon(a.nuggetd, home, logf)
    open(os.path.join(a.out, "nuggetd.pid"), "w").write(str(proc.pid))
    cap_log = os.path.abspath(a.capture_log) if a.capture_log else None
    try:
        rpc = await wait_sock(home, proc)
        srcs, _, _ = await rpc.call("sources.list", timeout=30)
        if not srcs:
            await rpc.call("sources.add", {"kind": "files", "name": "Dana's files",
                                           "settings": {"roots": [os.path.abspath(a.corpus)]}}, timeout=120)
        expected = sum(len(fs) for _, _, fs in os.walk(a.corpus))
        st, secs = await wait_ingest(rpc, expected)
        status, _, _ = await rpc.call("status", timeout=30)
        json.dump({"ingest_s": round(secs), "expected": expected, "ingest_status": st, "status": status,
                   "wall_clock_utc": dt.datetime.now(dt.timezone.utc).isoformat()},
                  open(os.path.join(a.out, "setup.json"), "w"), indent=1)
        queries = json.load(open(a.queries))
        out = open(os.path.join(a.out, "results.jsonl"), "w")
        md = [f"# time probe: {a.label}\n", f"wall clock (UTC): {dt.datetime.now(dt.timezone.utc).isoformat()}  ",
              f"corpus: `{a.corpus}`, items indexed: {status.get('items')}\n"]
        for qd in queries:
            q, rng = qd["q"], qd.get("range")
            ctx, _, _ = await rpc.call("retrieve.context", {"query": q, "budget_tokens": 2000}, timeout=120)
            cites = [{"n": c["n"], "key": c["item"]["key"] if isinstance(c["item"], dict) else c["item"],
                      "title": c.get("title"), "date": c.get("date"), "in_range": in_range(c.get("date"), rng)}
                     for c in ctx["citations"]]
            rec = {"id": qd["id"], "q": q, "expected_range": rng, "note": qd.get("note"),
                   "citations": cites, "context": ctx["text"]}
            n_in = sum(1 for c in cites if c["in_range"])
            rec["all_in_range"] = bool(cites) and rng is not None and n_in == len(cites)
            if a.ask:
                before = os.path.getsize(cap_log) if cap_log and os.path.exists(cap_log) else 0
                try:
                    started, _, _ = await rpc.call("retrieve.ask", {"query": q, "budget_tokens": 2000, "provider": None,
                                                                    "tier": "synthesis", "history": qd.get("history", [])},
                                                   timeout=120)
                    sq = rpc.attach(started["stream_id"])
                    while True:
                        _, method, p = await asyncio.wait_for(sq.get(), 60)
                        if method == "chat.done":
                            break
                    rec["ask_provider"] = started.get("provider")
                    rec["ask_general"] = started.get("general")
                except Exception as e:  # noqa: BLE001
                    rec["ask_error"] = str(e)
                await asyncio.sleep(0.3)
                if cap_log and os.path.exists(cap_log):
                    with open(cap_log, encoding="utf-8") as f:
                        f.seek(before)
                        reqs = [json.loads(l) for l in f if l.strip()]
                    rec["captured_requests"] = [r["body"] for r in reqs]
            out.write(json.dumps(rec) + "\n")
            out.flush()
            md.append(f"## {qd['id']}: {q!r}\n")
            md.append(f"expected range: {rng}  " if rng else "expected range: (none parsed / not a supported phrase)  ")
            if qd.get("note"):
                md.append(f"note: {qd['note']}  ")
            md.append(f"citations in expected range: {n_in}/{len(cites)}\n")
            for c in cites:
                md.append(f"- [{c['n']}] {c['date']} {'IN ' if c['in_range'] else 'out' if c['in_range'] is False else '   '} "
                          f"`{c['key']}` {c['title']!r}")
            md.append("")
        out.close()
        open(os.path.join(a.out, "summary.md"), "w").write("\n".join(md) + "\n")
    finally:
        stop_daemon(proc)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--nuggetd", default="target/release/nuggetd")
    ap.add_argument("--home", required=True)
    ap.add_argument("--corpus", required=True)
    ap.add_argument("--queries", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--label", default="")
    ap.add_argument("--ask", action="store_true")
    ap.add_argument("--capture-port", type=int, default=0)
    ap.add_argument("--capture-log", default="")
    asyncio.run(run(ap.parse_args()))


if __name__ == "__main__":
    main()
