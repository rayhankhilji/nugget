#!/usr/bin/env python3
"""GG7 harness: drive a real `nuggetd` over its IPC socket with the multilingual corpus.

    gg7.py run --nuggetd PATH --home DIR --chat-model ID [--embed-model ID] --out DIR
               [--suites search,ask,draft,knowledge,proactive] [--langs de,ja,...]
               [--no-models]  (lexical search and deterministic extraction only)

The daemon is the shipped binary, configured only through its config.toml and IPC (no test
hooks): it installs the manifest's models, serves them with llama-server, indexes the corpus
through the files connector, and answers search, Ask, Double Tap, knowledge and proactive calls
exactly as it does for the desktop app. Every request and answer is written to <out>/*.jsonl and
scored by score.py.
"""

import argparse
import itertools
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
QA = HERE.parent
CORPUS = QA / "corpus"
GOLD = QA / "gold"
LANGS = ["en", "de", "es", "fr", "hi", "ja", "pt-BR", "zh-CN", "ar", "mixed"]
ME = ("Sam Okafor", "sam.okafor@example.net")


def log(*a):
    print(time.strftime("%H:%M:%S"), *a, file=sys.stderr, flush=True)


class Ipc:
    """JSON-RPC 2.0 over the daemon's Unix socket, one JSON object per line."""

    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(str(path))
        self.ids = itertools.count(1)
        self.lock = threading.Lock()
        self.cv = threading.Condition(self.lock)
        self.replies = {}
        self.notes = []
        threading.Thread(target=self._reader, daemon=True).start()

    def _reader(self):
        buf = b""
        while True:
            chunk = self.sock.recv(1 << 16)
            if not chunk:
                with self.cv:
                    self.replies["__closed__"] = True
                    self.cv.notify_all()
                return
            buf += chunk
            while b"\n" in buf:
                line, buf = buf.split(b"\n", 1)
                if not line.strip():
                    continue
                msg = json.loads(line)
                with self.cv:
                    if "id" in msg and msg.get("id") is not None:
                        self.replies[msg["id"]] = msg
                    else:
                        self.notes.append(msg)
                    self.cv.notify_all()

    def call(self, method, params=None, timeout=600):
        i = next(self.ids)
        req = {"jsonrpc": "2.0", "id": i, "method": method, "params": params if params is not None else {}}
        self.sock.sendall((json.dumps(req, ensure_ascii=False) + "\n").encode())
        end = time.time() + timeout
        with self.cv:
            while i not in self.replies:
                if "__closed__" in self.replies:
                    raise RuntimeError("daemon closed the connection")
                left = end - time.time()
                if left <= 0:
                    raise TimeoutError(f"{method} timed out")
                self.cv.wait(left)
            msg = self.replies.pop(i)
        if "error" in msg:
            raise RpcError(method, msg["error"])
        return msg.get("result")

    def wait_note(self, pred, timeout):
        """Wait for (and consume) the first notification matching pred."""
        end = time.time() + timeout
        with self.cv:
            while True:
                for k, n in enumerate(self.notes):
                    if pred(n):
                        return self.notes.pop(k)
                left = end - time.time()
                if left <= 0:
                    raise TimeoutError("notification wait timed out")
                self.cv.wait(left)

    def stream(self, stream_id, timeout=900):
        """Collect chat.delta text until chat.done for stream_id."""
        out, end = [], time.time() + timeout
        with self.cv:
            while True:
                keep = []
                done = None
                for n in self.notes:
                    p = n.get("params") or {}
                    if p.get("stream_id") != stream_id:
                        keep.append(n)
                    elif n["method"] == "chat.delta":
                        out.append(p.get("text", ""))
                    elif n["method"] == "chat.done":
                        done = p
                self.notes = keep
                if done is not None:
                    return "".join(out), done
                left = end - time.time()
                if left <= 0:
                    return "".join(out), {"error": "harness timeout"}
                self.cv.wait(left)


class RpcError(Exception):
    def __init__(self, method, err):
        super().__init__(f"{method}: {err.get('code')} {err.get('message')}")
        self.err = err


def config_toml(a):
    rt = ["[runtime]", "enabled = true", "idle_stop_minutes = 0"]
    if a.chat_model:
        rt.append(f'chat_model = "{a.chat_model}"')
    if a.embed_model:
        rt.append(f'embedding_model = "{a.embed_model}"')
    if a.no_models:
        rt = ["[runtime]", "enabled = false"]
    return "\n".join(
        rt
        + [
            "",
            "[knowledge]",
            "enabled = true",
            "debounce_ms = 500",
            "[knowledge.me]",
            f'name = "{ME[0]}"',
            f'emails = ["{ME[1]}"]',
            "",
            "[vault]",
            "enabled = true",
            "",
            "[mcp]",
            "enabled = false",
            "",
            "[proactive]",
            "enabled = true",
            "",
        ]
    )


def start_daemon(a):
    home = Path(a.home).resolve()
    (home / "config").mkdir(parents=True, exist_ok=True)
    (home / "config" / "config.toml").write_text(config_toml(a))
    # A fresh index for every run (the models directory is kept: it is the download cache).
    for p in ["nugget.db", "nugget.db-wal", "nugget.db-shm", "vault", "ipc"]:
        q = home / "data" / p
        if q.is_dir():
            shutil.rmtree(q)
        elif q.exists():
            q.unlink()
    env = dict(os.environ, NUGGET_KEYCHAIN="off", NUGGET_LOG=os.environ.get("NUGGET_LOG", "info"))
    logf = open(Path(a.out) / "nuggetd.log", "ab")
    proc = subprocess.Popen([a.nuggetd, "--home", str(home)], env=env, stdout=logf, stderr=logf)
    sock = home / "data" / "ipc" / "nuggetd.sock"
    for _ in range(600):
        if proc.poll() is not None:
            raise SystemExit(f"nuggetd exited with {proc.returncode}; see {logf.name}")
        if sock.exists():
            try:
                ipc = Ipc(sock)
                ipc.call("status", timeout=10)
                return proc, ipc
            except (OSError, TimeoutError, RpcError):
                pass
        time.sleep(0.5)
    raise SystemExit("nuggetd did not open its socket")


def install_models(ipc, ids):
    models = {m["id"]: m for m in ipc.call("models.list")}
    for mid in ids:
        if not mid:
            continue
        if mid not in models:
            raise SystemExit(f"{mid} is not in the manifest: {sorted(models)}")
        if models[mid].get("installed"):
            log("model installed", mid)
            continue
        log("installing", mid)
        ipc.call("models.install", {"id": mid})
        n = ipc.wait_note(
            lambda n: n.get("method") == "models.progress"
            and n["params"].get("id") == mid
            and n["params"].get("phase") in ("installed", "failed"),
            timeout=3600,
        )
        if n["params"]["phase"] != "installed":
            raise SystemExit(f"install {mid}: {n['params']}")
        log("installed", mid)


def wait_runtime(ipc, timeout=900):
    end = time.time() + timeout
    while time.time() < end:
        rs = ipc.call("runtime.status")
        if rs and all(r["state"] in ("ready", "stopped") for r in rs):
            return rs
        if any(r["state"] == "failed" for r in rs or []):
            return rs
        time.sleep(3)
    return ipc.call("runtime.status")


def ingest(ipc, a, out):
    src = ipc.call(
        "sources.add",
        {"kind": "files", "name": "GG7 corpus", "settings": {"roots": [str(Path(a.corpus).resolve())]}},
    )
    sid = src["id"]
    ipc.call("sources.sync", {"id": sid})
    t0 = time.time()
    last = None
    while time.time() - t0 < 3600:
        st = ipc.call("ingest.status")
        info = [s for s in ipc.call("sources.list") if s["id"] == sid][0]
        if (st.get("queue_depth") or 0) == 0 and info["state"] in ("idle", "watching") and info["items"]:
            # Embeddings and extraction continue after the items land; give them a moment.
            if last == info["chunks"]:
                break
            last = info["chunks"]
        time.sleep(5)
    info = [s for s in ipc.call("sources.list") if s["id"] == sid][0]
    status = ipc.call("status")
    log("ingested", info["items"], "items", info["chunks"], "chunks in", round(time.time() - t0), "s")
    dump(out, "ingest", [{"source": info, "status": status, "ingest": ipc.call("ingest.status")}])
    return sid


def dump(out, name, rows):
    with open(Path(out) / f"{name}.jsonl", "a", encoding="utf-8") as f:
        for r in rows:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")


def gold(langs):
    for lang in langs:
        p = GOLD / f"{lang}.json"
        if p.exists():
            yield lang, json.loads(p.read_text(encoding="utf-8"))


def suite_search(ipc, a, out):
    for lang, g in gold(a.langs):
        for q in g.get("search", []):
            row = {"lang": lang, **q}
            try:
                hits = ipc.call("search", {"query": q["query"], "limit": 10}, timeout=120)
                row["hits"] = [
                    {k: h.get(k) for k in ("item", "title", "url", "score", "snippet")} for h in hits
                ]
            except Exception as e:  # noqa: BLE001 - the error is the result
                row["error"] = str(e)
            dump(out, "search", [row])


def run_stream(ipc, method, params, timeout):
    t0 = time.time()
    started = ipc.call(method, params, timeout=timeout)
    text, done = ipc.stream(started["stream_id"], timeout=timeout)
    return started, text, done, round(time.time() - t0, 2)


def suite_ask(ipc, a, out):
    for lang, g in gold(a.langs):
        for q in g.get("ask", []):
            row = {"lang": lang, "model": a.chat_model, **q}
            try:
                started, text, done, secs = run_stream(
                    ipc, "retrieve.ask", {"query": q["question"], "budget_tokens": 3000}, a.timeout
                )
                row.update(started=started, answer=text, done=done, seconds=secs)
            except Exception as e:  # noqa: BLE001
                row["error"] = str(e)
            dump(out, "ask", [row])
            log("ask", q["id"], row.get("seconds"), (row.get("answer") or row.get("error", ""))[:80])


def eml_thread(path):
    """The visible thread a mail client would show for this .eml: its quoted parts split off."""
    import email
    import email.policy

    msg = email.message_from_bytes(path.read_bytes(), policy=email.policy.default)
    body = msg.get_body(preferencelist=("plain",))
    text = body.get_content() if body else ""
    author = str(msg.get("From", ""))
    return msg, [{"author": author, "text": text.strip(), "from_me": ME[1] in author, "timestamp": None}]


def suite_draft(ipc, a, out):
    for lang, g in gold(a.langs):
        for d in g.get("draft", []):
            row = {"lang": lang, "model": a.chat_model, **d}
            try:
                msg, thread = eml_thread(CORPUS / d["file"])
                ctx = {
                    "app": {"id": "com.apple.mail", "name": "Mail", "window_title": str(msg.get("Subject", ""))},
                    "field": {"role": "AXTextArea", "label": "Message body", "value": ""},
                    "thread": thread,
                    "recipients": [str(msg.get("From", ""))],
                    "subject": "Re: " + str(msg.get("Subject", "")),
                }
                params = {"context": ctx, "instruction": d.get("instruction") or None}
                started, text, done, secs = run_stream(ipc, "draft.begin", params, a.timeout)
                row.update(started=started, draft=text, done=done, seconds=secs)
            except Exception as e:  # noqa: BLE001
                row["error"] = str(e)
            dump(out, "draft", [row])
            log("draft", d["id"], row.get("seconds"), (row.get("draft") or row.get("error", ""))[:80])


def suite_knowledge(ipc, a, out):
    # Extraction runs on its own after ingest; wait until it has nothing pending.
    end = time.time() + a.knowledge_wait
    st = {}
    while time.time() < end:
        st = ipc.call("knowledge.status")
        if not st.get("pending_items") and st.get("last_run"):
            break
        time.sleep(10)
    ents, off = [], 0
    while True:
        page = ipc.call("knowledge.entities", {"limit": 200, "offset": off})
        ents += page
        if len(page) < 200:
            break
        off += 200
    details = []
    for e in ents:
        try:
            details.append(ipc.call("knowledge.entity", {"id": e["id"]}))
        except Exception as ex:  # noqa: BLE001
            details.append({"id": e["id"], "error": str(ex)})
    dump(out, "knowledge", [{"status": st, "entities": ents, "details": details}])
    log("knowledge", len(ents), "entities", st)


def suite_proactive(ipc, a, out):
    rows = {}
    for m in ["proactive.refresh", "cards.list", "brief.get", "reminders.list"]:
        try:
            rows[m] = ipc.call(m, {"include_closed": True} if m == "cards.list" else {}, timeout=a.timeout)
        except Exception as e:  # noqa: BLE001
            rows[m] = {"error": str(e)}
    dump(out, "proactive", [rows])
    cards = rows.get("cards.list") or {}
    log("proactive", len(cards.get("cards", [])) if isinstance(cards, dict) else cards)


SUITES = {
    "search": suite_search,
    "ask": suite_ask,
    "draft": suite_draft,
    "knowledge": suite_knowledge,
    "proactive": suite_proactive,
}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", choices=["run"])
    ap.add_argument("--nuggetd", required=True)
    ap.add_argument("--home", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--corpus", default=str(CORPUS))
    ap.add_argument("--chat-model", default="")
    ap.add_argument("--embed-model", default="")
    ap.add_argument("--no-models", action="store_true")
    ap.add_argument("--suites", default="search,ask,draft,knowledge,proactive")
    ap.add_argument("--langs", default=",".join(LANGS))
    ap.add_argument("--timeout", type=int, default=900)
    ap.add_argument("--knowledge-wait", type=int, default=1800)
    a = ap.parse_args()
    a.langs = a.langs.split(",")
    Path(a.out).mkdir(parents=True, exist_ok=True)
    proc, ipc = start_daemon(a)
    try:
        if not a.no_models:
            install_models(ipc, [a.chat_model, a.embed_model])
            # The runtime picks newly installed models up on its own; give it time to start them.
            dump(a.out, "runtime", [{"runtime": wait_runtime(ipc), "providers": ipc.call("providers.list")}])
        ingest(ipc, a, a.out)
        for s in a.suites.split(","):
            log("suite", s)
            t0 = time.time()
            SUITES[s](ipc, a, a.out)
            log("suite", s, "done in", round(time.time() - t0), "s")
        dump(a.out, "runtime", [{"runtime": ipc.call("runtime.status"), "egress": ipc.call("egress.recent", {"limit": 100})}])
    finally:
        proc.send_signal(signal.SIGTERM)
        try:
            proc.wait(30)
        except subprocess.TimeoutExpired:
            proc.kill()


if __name__ == "__main__":
    main()
