#!/usr/bin/env python3
"""GG2 QA harness: drive a real nuggetd over its IPC socket the way the desktop Chat does.

Independent of the bench crate (no shared code). Steps:
  setup  write config.toml (real default config, with the chosen manifest models), start
         nuggetd, install the models with `models.install`, add the corpus as a `files` source
         (the real folder ingest path), wait for ingest to settle.
  ask    run every conversation through `retrieve.ask` exactly like apps/desktop Chat.tsx:
         budget 2000 tokens, tier synthesis, history = earlier turns with [n] markers stripped,
         last 24 messages. Records AskStarted, every delta's arrival time and chat.done.
Scoring is in score.py; this file only records.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import re
import signal
import subprocess
import sys
import time

CHAT_ASK_BUDGET = 2000      # apps/desktop/src/api.ts
CHAT_ASK_HISTORY = 24


class Rpc:
    def __init__(self, reader, writer):
        self.r, self.w = reader, writer
        self.next_id = 0
        self.pending: dict[int, asyncio.Future] = {}
        self.streams: dict[str, asyncio.Queue] = {}
        self.early: dict[str, list] = {}
        self.notes: asyncio.Queue = asyncio.Queue()
        self.task = asyncio.create_task(self._pump())

    @classmethod
    async def connect(cls, sock):
        r, w = await asyncio.open_unix_connection(sock, limit=32 * 1024 * 1024)
        return cls(r, w)

    async def _pump(self):
        while True:
            line = await self.r.readline()
            if not line:
                for f in self.pending.values():
                    if not f.done():
                        f.set_exception(ConnectionError("daemon closed the connection"))
                return
            now = time.monotonic()
            msg = json.loads(line)
            if "id" in msg and msg["id"] in self.pending:
                self.pending.pop(msg["id"]).set_result((now, msg))
                continue
            method, params = msg.get("method"), msg.get("params") or {}
            sid = params.get("stream_id") if isinstance(params, dict) else None
            if method in ("chat.delta", "chat.done") and sid:
                ev = (now, method, params)
                if sid in self.streams:
                    self.streams[sid].put_nowait(ev)
                else:
                    self.early.setdefault(sid, []).append(ev)
            else:
                self.notes.put_nowait((now, method, params))

    async def call(self, method, params=None, timeout=None):
        self.next_id += 1
        i = self.next_id
        fut = asyncio.get_running_loop().create_future()
        self.pending[i] = fut
        t0 = time.monotonic()
        self.w.write((json.dumps({"jsonrpc": "2.0", "id": i, "method": method, "params": params or {}}) + "\n").encode())
        await self.w.drain()
        at, msg = await asyncio.wait_for(fut, timeout)
        if "error" in msg:
            raise RpcError(msg["error"], at - t0)
        return msg["result"], t0, at

    def attach(self, sid):
        q = asyncio.Queue()
        for ev in self.early.pop(sid, []):
            q.put_nowait(ev)
        self.streams[sid] = q
        return q


class RpcError(Exception):
    def __init__(self, err, elapsed):
        super().__init__(f"{err.get('code')}: {err.get('message')}")
        self.err, self.elapsed = err, elapsed


def log(*a):
    print(time.strftime("%H:%M:%S"), *a, flush=True)


NUGGETD = "nuggetd"


def sock_path(home):
    p = os.path.join(home, "data", "ipc", "nuggetd.sock")
    if os.path.exists(p):
        return p
    # A long home path makes the daemon fall back to a short private directory; ask it.
    try:
        out = subprocess.run([NUGGETD, "--home", home, "status"], capture_output=True, text=True, timeout=20).stdout
    except Exception:
        return p
    m = re.search(r"^socket:\s+(\S+)", out, re.M)
    return m.group(1) if m else p


async def wait_sock(home, proc, timeout=180):
    t = time.time()
    while time.time() - t < timeout:
        if proc.poll() is not None:
            raise SystemExit(f"nuggetd exited with {proc.returncode}")
        p = sock_path(home)
        if os.path.exists(p):
            try:
                rpc = await Rpc.connect(p)
                await rpc.call("status", timeout=30)
                return rpc
            except Exception:
                pass
        await asyncio.sleep(1)
    raise SystemExit("nuggetd socket never came up")


def start_daemon(bin, home, logf):
    return subprocess.Popen([bin, "--home", home], stdout=logf, stderr=subprocess.STDOUT,
                            env=dict(os.environ, RUST_LOG=os.environ.get("RUST_LOG", "info")))


def stop_daemon(proc):
    if proc.poll() is None:
        proc.send_signal(signal.SIGINT)
        try:
            proc.wait(60)
        except subprocess.TimeoutExpired:
            proc.kill()


def edit_config(path, chat_model, embed_model, knowledge, drop_ollama):
    s = open(path, encoding="utf-8").read()
    s = s.replace('# chat_model = "qwen3-4b-q4km"', f'chat_model = "{chat_model}"', 1)
    s = s.replace('# embedding_model = "nomic-embed-text-v1.5-q8"', f'embedding_model = "{embed_model}"', 1)
    s = re.sub(r"idle_stop_minutes = \d+", "idle_stop_minutes = 0", s, count=1)
    if not knowledge:
        s = s.replace("[knowledge]\nenabled = true", "[knowledge]\nenabled = false", 1)
    if drop_ollama:
        s = re.sub(r'\[\[providers\]\]\nid = "local"\n(.*\n){4}', "", s, count=1)
    open(path, "w", encoding="utf-8").write(s)
    return s


async def install(rpc, model_id, timeout=5400):
    models, _, _ = await rpc.call("models.list", timeout=60)
    m = next((m for m in models if m["id"] == model_id), None)
    if m is None:
        raise SystemExit(f"model {model_id} is not in models.list")
    if m["installed"]:
        log(f"{model_id}: already installed")
        return
    await rpc.call("models.install", {"id": model_id}, timeout=60)
    t = time.time()
    last = 0
    while time.time() - t < timeout:
        _, method, p = await asyncio.wait_for(rpc.notes.get(), timeout)
        if method != "models.progress" or p.get("id") != model_id:
            continue
        if p["phase"] == "installed":
            log(f"{model_id}: installed in {time.time() - t:.0f}s")
            return
        if p["phase"] == "failed":
            raise SystemExit(f"{model_id}: install failed: {p.get('error')}")
        if time.time() - last > 30:
            last = time.time()
            log(f"{model_id}: {p['phase']} {p.get('downloaded')}/{p.get('total')}")
    raise SystemExit(f"{model_id}: install timed out")


async def wait_ingest(rpc, expected, timeout=3600, timeline=None):
    t = time.time()
    stable = 0
    last = None
    while time.time() - t < timeout:
        st, _, _ = await rpc.call("ingest.status", timeout=60)
        src = st["sources"][0] if st["sources"] else {}
        cur = (src.get("items"), src.get("chunks"), st["queue_depth"], src.get("state"))
        if cur != last:
            if timeline is not None:
                timeline.append({"s": round(time.time() - t), "items": cur[0], "chunks": cur[1], "queue": cur[2], "state": cur[3]})
            log(f"ingest: items={cur[0]} chunks={cur[1]} queue={cur[2]} state={cur[3]} "
                f"embed={st.get('embedding_provider')} triage={st.get('triage_provider')}")
        stable = stable + 1 if cur == last and cur[2] == 0 and cur[3] in ("watching", "idle") else 0
        last = cur
        if stable >= 6 and (cur[0] or 0) >= expected * 0.9:
            return st, time.time() - t
        await asyncio.sleep(5)
    st, _, _ = await rpc.call("ingest.status", timeout=60)
    return st, time.time() - t


def strip_markers(text):
    # Chat.tsx stripCitationMarkers: an earlier reply's [n] pointed at that reply's passages.
    return re.sub(r"\s?\[\d+\](\[\d+\])*", "", text)


async def ask(rpc, query, history, timeout):
    params = {"query": query, "budget_tokens": CHAT_ASK_BUDGET, "provider": None, "tier": "synthesis",
              "history": history[-CHAT_ASK_HISTORY:]}
    rec = {"query": query, "history_len": len(params["history"])}
    t0 = time.monotonic()
    try:
        started, _, at = await rpc.call("retrieve.ask", params, timeout=timeout)
    except (RpcError, asyncio.TimeoutError, ConnectionError) as e:
        rec.update(error=str(e) or type(e).__name__, started_ms=round((time.monotonic() - t0) * 1000))
        return rec
    rec["started_ms"] = round((at - t0) * 1000)
    rec.update({k: started.get(k) for k in ("provider", "locality", "citations", "excluded_sensitive", "query_rewrite", "general")})
    q = rpc.attach(started["stream_id"])
    text, first = [], None
    done = None
    deadline = t0 + timeout
    while True:
        try:
            now_left = max(1, deadline - time.monotonic())
            at, method, p = await asyncio.wait_for(q.get(), now_left)
        except asyncio.TimeoutError:
            await rpc.call("chat.cancel", {"stream_id": started["stream_id"]}, timeout=30)
            rec["error"] = f"timeout after {timeout}s"
            break
        if method == "chat.delta":
            if first is None and p.get("text"):
                first = at
            text.append(p.get("text", ""))
        else:
            done = (at, p)
            break
    rpc.streams.pop(started["stream_id"], None)
    rec["answer"] = "".join(text)
    rec["first_token_ms"] = round((first - t0) * 1000) if first else None
    if done:
        rec["total_ms"] = round((done[0] - t0) * 1000)
        rec["done_error"] = done[1].get("error")
        rec["input_tokens"] = done[1].get("input_tokens")
        rec["output_tokens"] = done[1].get("output_tokens")
    return rec


async def run(a):
    os.makedirs(a.out, exist_ok=True)
    home = os.path.abspath(a.home)
    logf = open(os.path.join(a.out, "nuggetd.log"), "ab")
    cfg = os.path.join(home, "config", "config.toml")
    global NUGGETD
    NUGGETD = a.nuggetd
    if not os.path.exists(cfg):
        proc = start_daemon(a.nuggetd, home, logf)
        try:
            await wait_sock(home, proc)
        finally:
            stop_daemon(proc)
    text = edit_config(cfg, a.chat_model, a.embed_model, a.knowledge, a.drop_ollama)
    open(os.path.join(a.out, "config.toml"), "w").write(text)
    proc = start_daemon(a.nuggetd, home, logf)
    try:
        rpc = await wait_sock(home, proc)
        setup = {}
        t = time.time()
        if not a.retrieval_only:
            await install(rpc, a.embed_model)
            await install(rpc, a.chat_model)
        setup["install_s"] = round(time.time() - t)
        srcs, _, _ = await rpc.call("sources.list", timeout=30)
        if not srcs:
            info, _, _ = await rpc.call("sources.add", {"kind": "files", "name": "Dana's files",
                                                        "settings": {"roots": [os.path.abspath(a.corpus)]}}, timeout=120)
            log("source added:", json.dumps(info))
        expected = sum(len(fs) for _, _, fs in os.walk(a.corpus))
        timeline = []
        st, secs = await wait_ingest(rpc, expected, a.ingest_timeout, timeline)
        setup.update(ingest_s=round(secs), expected_files=expected, ingest_status=st, ingest_timeline=timeline)
        if a.probe_search:
            probes = {}
            for q in [l.strip() for l in open(a.probe_search, encoding="utf-8") if l.strip() and not l.startswith("#")]:
                hits, _, _ = await rpc.call("search", {"query": q, "limit": 10}, timeout=120)
                probes[q] = [{"key": h["item"]["key"], "title": h.get("title"), "score": h.get("score")} for h in hits]
                log(f"search {q!r}: {[h['key'] for h in probes[q]][:5]}")
            json.dump(probes, open(os.path.join(a.out, "probe_search.json"), "w"), indent=1)
        status, _, _ = await rpc.call("status", timeout=30)
        rt, _, _ = await rpc.call("runtime.status", timeout=30)
        setup.update(status=status, runtime=rt)
        try:
            setup["knowledge"], _, _ = await rpc.call("knowledge.status", timeout=30)
        except Exception as e:
            setup["knowledge"] = str(e)
        json.dump(setup, open(os.path.join(a.out, "setup.json"), "w"), indent=1)
        log("setup:", json.dumps({k: setup[k] for k in ("install_s", "ingest_s", "expected_files")}), "status items", status["items"])

        convs = [json.loads(l) for l in open(a.questions, encoding="utf-8")]
        meta = convs.pop(0)["meta"]
        if a.only:
            ids = set(a.only.split(","))
            convs = [c for c in convs if c["id"] in ids or c["category"] in ids]
        shard, nshards = map(int, a.shard.split("/"))
        convs = [c for i, c in enumerate(convs) if i % nshards == shard]
        outp = os.path.join(a.out, "answers.jsonl")
        done_ids = set()
        if os.path.exists(outp):
            done_ids = {json.loads(l)["id"] for l in open(outp)}
        with open(outp, "a", encoding="utf-8") as f:
            for c in convs:
                if c["id"] in done_ids:
                    continue
                history, turns = [], []
                for t in c["turns"]:
                    if a.retrieval_only:
                        ctx, t0, at = await rpc.call("retrieve.context", {"query": t["q"], "budget_tokens": CHAT_ASK_BUDGET}, timeout=120)
                        rec = {"query": t["q"], "citations": ctx["citations"], "context": ctx["text"], "started_ms": round((at - t0) * 1000), "answer": ""}
                    else:
                        rec = await ask(rpc, t["q"], history, a.timeout)
                    turns.append(rec)
                    log(f"{c['id']} [{len(turns)}] {t['q']!r} -> ttft={rec.get('first_token_ms')} total={rec.get('total_ms')} "
                        f"cites={len(rec.get('citations') or [])} general={rec.get('general')} err={rec.get('error')}")
                    log("   ", (rec.get("answer") or "")[:300].replace("\n", " "))
                    if rec.get("error") and not rec.get("answer"):
                        break
                    history += [{"role": "user", "text": t["q"]},
                                {"role": "assistant", "text": strip_markers(rec.get("answer") or "")}]
                f.write(json.dumps({"id": c["id"], "category": c["category"], "model": a.chat_model, "today": meta["today"],
                                    "turns": turns}) + "\n")
                f.flush()
    finally:
        stop_daemon(proc)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--nuggetd", default="target/release/nuggetd")
    ap.add_argument("--home", required=True)
    ap.add_argument("--corpus", required=True)
    ap.add_argument("--questions", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--chat-model", default="qwen3-4b-q4km")
    ap.add_argument("--embed-model", default="nomic-embed-text-v1.5-q8")
    ap.add_argument("--knowledge", action="store_true", help="leave knowledge extraction on (default config)")
    ap.add_argument("--drop-ollama", action="store_true", help="remove the default Ollama provider entry")
    ap.add_argument("--retrieval-only", action="store_true", help="no models: record retrieve.context per question")
    ap.add_argument("--shard", default="0/1")
    ap.add_argument("--only", default="")
    ap.add_argument("--timeout", type=float, default=600)
    ap.add_argument("--ingest-timeout", type=float, default=3600, help="seconds to wait for ingest to settle")
    ap.add_argument("--probe-search", default="", help="file of queries to run through `search` after ingest")
    asyncio.run(run(ap.parse_args()))


if __name__ == "__main__":
    main()
