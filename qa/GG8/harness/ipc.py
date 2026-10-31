"""Minimal nuggetd IPC client (JSON-RPC 2.0, one object per line, Unix socket). Standard library.

    c = Client("/tmp/gg8/home/data/ipc/nuggetd.sock")
    c.call("status")
    text, done = c.stream("retrieve.ask", {"query": "...", "budget_tokens": 2000})
"""
import json
import socket
import threading
import time


class RpcError(Exception):
    def __init__(self, err):
        super().__init__(f"{err.get('code')}: {err.get('message')}")
        self.code = err.get("code")
        self.message = err.get("message")
        self.data = err.get("data")


class Client:
    def __init__(self, path, timeout=600):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(path)
        self.sock.settimeout(timeout)
        self.f = self.sock.makefile("rb")
        self.next_id = 0
        self.notes = []
        self.lock = threading.Lock()

    def close(self):
        try:
            self.sock.close()
        except OSError:
            pass

    def _send(self, obj):
        self.sock.sendall(json.dumps(obj).encode() + b"\n")

    def _read(self):
        line = self.f.readline()
        if not line:
            raise ConnectionError("daemon closed the connection")
        return json.loads(line)

    def call(self, method, params=None, timeout=None):
        """Call and wait for the response; notifications seen meanwhile go to self.notes."""
        self.next_id += 1
        rid = self.next_id
        self._send({"jsonrpc": "2.0", "id": rid, "method": method,
                    "params": {} if params is None else params})
        if timeout:
            self.sock.settimeout(timeout)
        while True:
            msg = self._read()
            if msg.get("id") == rid:
                if "error" in msg:
                    raise RpcError(msg["error"])
                return msg.get("result")
            if "method" in msg:
                self.notes.append(msg)

    def wait_note(self, pred, timeout=120):
        """Wait for a notification matching pred (checks already-buffered ones first)."""
        for n in list(self.notes):
            if pred(n):
                self.notes.remove(n)
                return n
        end = time.time() + timeout
        while time.time() < end:
            self.sock.settimeout(max(0.1, end - time.time()))
            try:
                msg = self._read()
            except socket.timeout:
                break
            if "method" in msg:
                if pred(msg):
                    return msg
                self.notes.append(msg)
        return None

    def stream(self, method, params, timeout=600):
        """Start a streaming call (chat, retrieve.ask, draft.begin); return (started, text, done)."""
        started = self.call(method, params, timeout=timeout)
        sid = started.get("stream_id") if isinstance(started, dict) else None
        text = []
        done = None
        end = time.time() + timeout
        pending = [n for n in self.notes if n.get("params", {}).get("stream_id") == sid]
        for n in pending:
            self.notes.remove(n)
        queue = list(pending)
        while done is None and time.time() < end:
            if queue:
                msg = queue.pop(0)
            else:
                self.sock.settimeout(max(0.1, end - time.time()))
                try:
                    msg = self._read()
                except socket.timeout:
                    break
            p = msg.get("params") or {}
            if msg.get("method") == "chat.delta" and p.get("stream_id") == sid:
                text.append(p.get("text", ""))
            elif msg.get("method") == "chat.done" and p.get("stream_id") == sid:
                done = p
            elif "method" in msg:
                self.notes.append(msg)
        return started, "".join(text), done


def wait_socket(path, timeout=120):
    end = time.time() + timeout
    last = None
    while time.time() < end:
        try:
            c = Client(path, timeout=10)
            c.call("status")
            c.close()
            return True
        except (OSError, ConnectionError, RpcError, ValueError) as e:
            last = e
            time.sleep(0.5)
    raise TimeoutError(f"daemon socket {path} not ready: {last}")
