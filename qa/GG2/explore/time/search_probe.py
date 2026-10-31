#!/usr/bin/env python3
"""Start nuggetd on an already-ingested home and run `search` / `retrieve.context` for given
queries (one per argument), printing hit counts and cited (date, key). Used to show which
date-phrase queries fell back to the unfiltered search: the remaining words find nothing.

Usage: search_probe.py --home H QUERY...
"""
import argparse, asyncio, json, os, sys
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "harness"))
import ask_qa
from ask_qa import start_daemon, stop_daemon, wait_sock


async def run(a):
    ask_qa.NUGGETD = a.nuggetd
    logf = open(os.devnull, "wb")
    proc = start_daemon(a.nuggetd, os.path.abspath(a.home), logf)
    try:
        rpc = await wait_sock(os.path.abspath(a.home), proc)
        for q in a.queries:
            hits, _, _ = await rpc.call("search", {"query": q, "limit": 50}, timeout=60)
            ctx, _, _ = await rpc.call("retrieve.context", {"query": q, "budget_tokens": 2000}, timeout=60)
            print(json.dumps({"query": q, "search_hits": len(hits),
                              "context_citations": [(c.get("date"), c["item"]["key"]) for c in ctx["citations"]]}))
    finally:
        stop_daemon(proc)


ap = argparse.ArgumentParser()
ap.add_argument("--nuggetd", default="target/release/nuggetd")
ap.add_argument("--home", required=True)
ap.add_argument("queries", nargs="+")
asyncio.run(run(ap.parse_args()))
