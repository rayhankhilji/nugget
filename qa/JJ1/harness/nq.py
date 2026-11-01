"""JJ1 QA harness core: a small synchronous JSON-RPC client for nuggetd, plus daemon start/stop.

Stdlib only. Usage from a scenario script:

    import sys; sys.path.insert(0, "qa/JJ1/harness")
    from nq import Daemon
    with Daemon("/tmp/jj1/home", nuggetd="target/debug/nuggetd") as d:
        print(d.call("status"))
        src = d.call("sources.add", {"kind": "whatsapp", "name": "WA", "settings": {"folder": "/tmp/wa"}})
        d.wait_idle()
        print(d.call("search", {"query": "flight", "limit": 10}))

Environment: NUGGETD (binary path), JJ1_KEEP=1 keeps the daemon running at exit (debugging).
"""
import json
import os
import queue
import re
import signal
import socket
import subprocess
import threading
import time


class RpcError(Exception):
    def __init__(self, err):
        super().__init__(f"{err.get('code')}: {err.get('message')}")
        self.code = err.get("code")
        self.err = err


class Client:
    """One connection. Calls block; notifications are queued (self.notes) and streams are
    collected per stream_id."""

    def __init__(self, path):
        self.s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.s.connect(path)
        self.f = self.s.makefile("rb")
        self.lock = threading.Lock()
        self.next_id = 0
        self.pending = {}
        self.notes = queue.Queue()
        self.streams = {}
        self.cv = threading.Condition()
        self.closed = False
        threading.Thread(target=self._pump, daemon=True).start()

    def _pump(self):
        for line in self.f:
            msg = json.loads(line)
            with self.cv:
                if "id" in msg and msg["id"] in self.pending:
                    self.pending[msg["id"]] = msg
                else:
                    m, p = msg.get("method"), msg.get("params") or {}
                    sid = p.get("stream_id") if isinstance(p, dict) else None
                    if m in ("chat.delta", "chat.done") and sid:
                        self.streams.setdefault(sid, []).append((time.monotonic(), m, p))
                    else:
                        self.notes.put((time.monotonic(), m, p))
                self.cv.notify_all()
        with self.cv:
            self.closed = True
            self.cv.notify_all()

    def call(self, method, params=None, timeout=120):
        with self.cv:
            self.next_id += 1
            i = self.next_id
            self.pending[i] = None
        data = json.dumps({"jsonrpc": "2.0", "id": i, "method": method, "params": params if params is not None else {}})
        with self.lock:
            self.s.sendall(data.encode() + b"\n")
        end = time.monotonic() + timeout
        with self.cv:
            while self.pending[i] is None:
                left = end - time.monotonic()
                if left <= 0 or self.closed:
                    self.pending.pop(i, None)
                    raise TimeoutError(f"{method}: no reply in {timeout}s" if not self.closed else f"{method}: connection closed")
                self.cv.wait(left)
            msg = self.pending.pop(i)
        if "error" in msg:
            raise RpcError(msg["error"])
        return msg["result"]

    def stream(self, sid, timeout=600):
        """Wait for chat.done on a stream; return (text, done_params, first_token_monotonic)."""
        end = time.monotonic() + timeout
        with self.cv:
            while True:
                evs = self.streams.get(sid, [])
                if any(m == "chat.done" for _, m, _ in evs):
                    break
                left = end - time.monotonic()
                if left <= 0 or self.closed:
                    raise TimeoutError(f"stream {sid}: no chat.done in {timeout}s")
                self.cv.wait(left)
            evs = self.streams.pop(sid)
        text = "".join(p.get("text", "") for _, m, p in evs if m == "chat.delta")
        first = next((t for t, m, p in evs if m == "chat.delta" and p.get("text")), None)
        done = next(p for _, m, p in evs if m == "chat.done")
        return text, done, first

    def close(self):
        try:
            self.s.close()
        except OSError:
            pass


def sock_path(home, nuggetd):
    p = os.path.join(home, "data", "ipc", "nuggetd.sock")
    if os.path.exists(p):
        return p
    try:
        out = subprocess.run([nuggetd, "--home", home, "status"], capture_output=True, text=True, timeout=20).stdout
        m = re.search(r"^socket:\s+(\S+)", out, re.M)
        if m:
            return m.group(1)
    except Exception:
        pass
    return p


class Daemon:
    def __init__(self, home, nuggetd=None, log=None, env=None, args=()):
        self.home = os.path.abspath(home)
        self.bin = nuggetd or os.environ.get("NUGGETD") or "target/debug/nuggetd"
        self.log = log or os.path.join(self.home, "nuggetd.log")
        self.env = env or {}
        self.args = list(args)
        self.proc = None
        self.c = None

    def start(self, timeout=120):
        os.makedirs(self.home, exist_ok=True)
        lf = open(self.log, "ab")
        env = dict(os.environ, RUST_LOG=os.environ.get("RUST_LOG", "info"), **self.env)
        self.proc = subprocess.Popen([self.bin, "--home", self.home, *self.args], stdout=lf, stderr=subprocess.STDOUT, env=env)
        t = time.time()
        while time.time() - t < timeout:
            if self.proc.poll() is not None:
                raise SystemExit(f"nuggetd exited with {self.proc.returncode}; see {self.log}")
            p = sock_path(self.home, self.bin)
            if os.path.exists(p):
                try:
                    self.c = Client(p)
                    self.c.call("status", timeout=30)
                    return self
                except Exception:
                    self.c = None
            time.sleep(0.5)
        raise SystemExit("nuggetd socket never came up")

    def stop(self):
        if self.c:
            self.c.close()
            self.c = None
        if self.proc and self.proc.poll() is None:
            self.proc.send_signal(signal.SIGINT)
            try:
                self.proc.wait(60)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()

    def restart(self):
        self.stop()
        return self.start()

    def __enter__(self):
        return self.start()

    def __exit__(self, *a):
        if not os.environ.get("JJ1_KEEP"):
            self.stop()

    def call(self, method, params=None, timeout=120):
        return self.c.call(method, params, timeout)

    def config_path(self):
        return os.path.join(self.home, "config", "config.toml")

    def wait_idle(self, source=None, timeout=300, settle=3, min_items=0):
        """Poll ingest.status until the queue is empty and the source(s) are idle/watching for
        `settle` consecutive polls. Returns the last IngestStatus."""
        t = time.time()
        stable, last = 0, None
        while time.time() - t < timeout:
            st = self.call("ingest.status")
            srcs = [s for s in st["sources"] if source is None or s.get("id") == source]
            cur = (st["queue_depth"], tuple((s.get("id"), s.get("items"), s.get("chunks"), s.get("state")) for s in srcs))
            quiet = st["queue_depth"] == 0 and all(s.get("state") in ("watching", "idle", "error", "disabled") for s in srcs)
            enough = sum(s.get("items") or 0 for s in srcs) >= min_items
            stable = stable + 1 if cur == last and quiet and enough else 0
            last = cur
            if stable >= settle:
                return st
            time.sleep(1)
        return self.call("ingest.status")

    def ask(self, query, budget=2000, provider=None, tier="synthesis", history=None, timeout=900):
        t0 = time.monotonic()
        started = self.call("retrieve.ask", {"query": query, "budget_tokens": budget, "provider": provider,
                                             "tier": tier, "history": history or []}, timeout=timeout)
        text, done, first = self.c.stream(started["stream_id"], timeout)
        return {"query": query, "answer": text, "provider": started.get("provider"), "locality": started.get("locality"),
                "citations": started.get("citations"), "excluded_sensitive": started.get("excluded_sensitive"),
                "first_token_ms": round((first - t0) * 1000) if first else None,
                "total_ms": round((time.monotonic() - t0) * 1000), "done": done}


def dump(obj, path):
    os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        json.dump(obj, f, indent=1, ensure_ascii=False)
