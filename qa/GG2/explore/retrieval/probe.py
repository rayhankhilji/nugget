#!/usr/bin/env python3
"""Start nuggetd on an already-ingested home and dump retrieve.context / retrieve.search for queries.
usage: [CORPUS=dir] probe.py HOME OUTDIR QUERYFILE [budget]   (QUERYFILE: one query per line)"""
import asyncio, json, os, sys
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "harness"))
import ask_qa as h

async def main(home, out, qfile, budget):
    h.NUGGETD = "/home/user/nugget/target/release/nuggetd"
    os.makedirs(out, exist_ok=True)
    proc = h.start_daemon(h.NUGGETD, home, open(os.path.join(out, "nuggetd.log"), "ab"))
    open(os.path.join(out, "pid"), "w").write(str(proc.pid))
    try:
        rpc = await h.wait_sock(home, proc)
        corpus = os.environ.get("CORPUS")
        if corpus and not (await rpc.call("sources.list", timeout=30))[0]:
            await rpc.call("sources.add", {"kind": "files", "name": "c", "settings": {"roots": [corpus]}}, timeout=120)
            await h.wait_ingest(rpc, sum(len(fs) for _, _, fs in os.walk(corpus)))
        res = []
        for q in [l.rstrip("\n") for l in open(qfile) if l.strip()]:
            ctx, _, _ = await rpc.call("retrieve.context", {"query": q, "budget_tokens": budget}, timeout=120)
            res.append({"query": q, "citations": ctx["citations"], "text": ctx["text"],
                        **{k: v for k, v in ctx.items() if k not in ("citations", "text")}})
        json.dump(res, open(os.path.join(out, "contexts.json"), "w"), indent=1)
    finally:
        h.stop_daemon(proc)

asyncio.run(main(sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4]) if len(sys.argv) > 4 else 2000))
