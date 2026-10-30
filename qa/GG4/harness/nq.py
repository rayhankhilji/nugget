"""GG4 QA harness: run the real `nuggetd` on a simulated clock and talk to it over its IPC.

The daemon has no clock hook (finding GG4-clock), so the process runs under libfaketime:
`LD_PRELOAD=libfaketime.so.1 FAKETIME=@<epoch>` starts its wall clock at the simulated instant
and lets it tick in real time; `FAKETIME_DONT_FAKE_MONOTONIC=1` keeps tokio's timers on the real
monotonic clock. `TZ` sets the machine's zone (the daemon asks `date +%z`, which inherits both).

    from nq import Home, Daemon
    home = Home("/path/work/h1", user="maya@larkspur.example")
    with Daemon(home, sim="2026-10-26T07:50", tz="America/New_York") as d:
        d.call("sources.add", {...})

Everything is JSON-RPC 2.0, one object per line, over the Unix socket (docs/book/src/reference/ipc.md).
"""
from __future__ import annotations

import datetime as dt
import glob
import json
import os
import secrets
import shutil
import socket
import subprocess
import tempfile
import time
from pathlib import Path
from zoneinfo import ZoneInfo

REPO = Path(__file__).resolve().parents[3]
FAKETIME_LIBS = [
    "/usr/lib/x86_64-linux-gnu/faketime/libfaketime.so.1",
    "/usr/lib/aarch64-linux-gnu/faketime/libfaketime.so.1",
    "/usr/lib/faketime/libfaketime.so.1",
]


def nuggetd_exe() -> str:
    for c in (os.environ.get("GG4_NUGGETD"), REPO / "target/release/nuggetd", REPO / "target/debug/nuggetd"):
        if c and Path(c).exists():
            return str(c)
    raise SystemExit("build nuggetd first (cargo build -p nuggetd) or set GG4_NUGGETD")


def faketime_lib() -> str:
    for p in FAKETIME_LIBS + glob.glob("/usr/lib/*/faketime/libfaketime.so.1"):
        if Path(p).exists():
            return p
    raise SystemExit("libfaketime not found: apt-get install faketime")


def fnv1a(b: bytes) -> int:
    h = 0xCBF29CE484222325
    for x in b:
        h ^= x
        h = (h * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return h


def local(ts: str, tz: str) -> dt.datetime:
    """`2026-10-26T07:50` in zone `tz` as an aware datetime."""
    return dt.datetime.fromisoformat(ts).replace(tzinfo=ZoneInfo(tz))


class Home:
    """A NUGGET_HOME: key file (NUGGET_KEYCHAIN=off), config.toml, nothing else."""

    def __init__(self, path, user="maya@larkspur.example", runtime=False, chat_model=None,
                 embed_model=None, judge="local", brief_time="08:00", utc_offset=None,
                 engine_extra="", draft_voice=True, models_home=None):
        self.path = Path(path).resolve()
        self.data = self.path / "data"
        self.config = self.path / "config"
        self.data.mkdir(parents=True, exist_ok=True)
        self.config.mkdir(parents=True, exist_ok=True)
        key = self.data / "store.key"
        if not key.exists():
            key.write_text(secrets.token_hex(32))
            os.chmod(key, 0o600)
        if models_home:
            # Share installed models between homes (they are big).
            m = self.data / "models"
            if not m.exists():
                Path(models_home).mkdir(parents=True, exist_ok=True)
                m.symlink_to(Path(models_home).resolve())
        self.user = user
        rt = "[runtime]\nenabled = false\n"
        if runtime:
            rt = "[runtime]\nenabled = true\nidle_stop_minutes = 0\n"
            if chat_model:
                rt += f'chat_model = "{chat_model}"\n'
            if embed_model:
                rt += f'embedding_model = "{embed_model}"\n'
        off = f'utc_offset = "{utc_offset}"\n' if utc_offset else ""
        voice = (f'[draft.voice]\nnames = ["Maya Okafor"]\nemails = ["{user}"]\n\n' if draft_voice else "")
        self.write_config(
            f"{rt}\n[mcp]\nenabled = false\n\n"
            f"[vault]\nenabled = true\npath = '{self.path / 'vault'}'\n\n"
            f'[knowledge]\nenabled = true\n[knowledge.me]\nname = "Maya Okafor"\nemails = ["{user}"]\n\n'
            f"{voice}"
            f'[proactive]\nenabled = true\nbrief_time = "{brief_time}"\njudge = "{judge}"\n'
            f"refresh_minutes = 30\ndebounce_secs = 2\n{off}\n"
            f'[proactive.engine]\nuser_addresses = ["{user}"]\n{engine_extra}\n'
        )

    def write_config(self, text: str):
        (self.config / "config.toml").write_text(text)

    def socket_path(self) -> Path:
        pref = self.data / "ipc" / "nuggetd.sock"
        if len(str(pref)) <= 100:
            return pref
        leaf = f"nugget-{fnv1a(str(self.data).encode()):016x}"
        rt = os.environ.get("XDG_RUNTIME_DIR")
        if rt and len(str(Path(rt) / leaf / "nuggetd.sock")) <= 100:
            p = Path(rt) / leaf / "nuggetd.sock"
            if p.parent.exists():
                return p
        return Path(tempfile.gettempdir()) / leaf / "nuggetd.sock"


class RpcError(Exception):
    def __init__(self, err):
        super().__init__(f"{err.get('code')}: {err.get('message')}")
        self.code = err.get("code")
        self.message = err.get("message")
        self.data = err.get("data")


class Daemon:
    """`nuggetd` on `home`, its wall clock starting at `sim` (local time in `tz`, or an aware
    datetime). `sim=None` runs on the real clock."""

    def __init__(self, home: Home, sim=None, tz="America/New_York", log_name="nuggetd", env=None,
                 start_timeout=120):
        self.home, self.tz, self.env_extra = home, tz, env or {}
        if isinstance(sim, str):
            sim = local(sim, tz)
        self.sim = sim
        self.log = home.path / f"{log_name}.log"
        self.proc = None
        self.sock = None
        self.buf = b""
        self.next_id = 1
        self.notes: list[dict] = []
        self.start_timeout = start_timeout

    # ---- process ----
    def env(self):
        e = dict(os.environ)
        for k in ("NUGGET_LOG", "RUST_LOG", "LD_PRELOAD", "FAKETIME"):
            e.pop(k, None)
        e.update(NUGGET_HOME=str(self.home.path), NUGGET_KEYCHAIN="off", TZ=self.tz,
                 NUGGET_LOG="info,nuggetd=debug,nugget_proactive=debug")
        if self.sim is not None:
            e.update(LD_PRELOAD=faketime_lib(), FAKETIME_FMT="%s",
                     FAKETIME=f"@{int(self.sim.timestamp())}",
                     FAKETIME_DONT_FAKE_MONOTONIC="1", FAKETIME_DONT_RESET="1")
        e.update(self.env_extra)
        return e

    def start(self):
        sp = self.home.socket_path()
        self.logf = open(self.log, "ab")
        self.logf.write(f"\n==== start sim={self.sim} tz={self.tz} real={dt.datetime.now(dt.timezone.utc)} ====\n".encode())
        self.logf.flush()
        self.proc = subprocess.Popen([nuggetd_exe()], env=self.env(), stdin=subprocess.DEVNULL,
                                     stdout=self.logf, stderr=self.logf)
        t0 = time.time()
        while time.time() - t0 < self.start_timeout:
            if self.proc.poll() is not None:
                raise RuntimeError(f"nuggetd exited {self.proc.returncode}; see {self.log}")
            if sp.exists():
                try:
                    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                    s.connect(str(sp))
                    self.sock = s
                    self.call("status", {})
                    return self
                except (ConnectionRefusedError, FileNotFoundError, OSError):
                    self.sock = None
            time.sleep(0.2)
        raise RuntimeError(f"nuggetd did not answer on {sp}; see {self.log}")

    def stop(self):
        if self.sock:
            try:
                self.sock.close()
            except OSError:
                pass
            self.sock = None
        if self.proc and self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(30)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()
        if getattr(self, "logf", None):
            self.logf.close()

    def __enter__(self):
        return self.start()

    def __exit__(self, *a):
        self.stop()

    # ---- rpc ----
    def _read_line(self, timeout):
        self.sock.settimeout(timeout)
        while b"\n" not in self.buf:
            chunk = self.sock.recv(1 << 20)
            if not chunk:
                raise ConnectionError("daemon closed the connection")
            self.buf += chunk
        line, self.buf = self.buf.split(b"\n", 1)
        return json.loads(line)

    def call(self, method, params=None, timeout=600):
        i = self.next_id
        self.next_id += 1
        msg = {"jsonrpc": "2.0", "id": i, "method": method}
        if params is not None:
            msg["params"] = params
        self.sock.sendall(json.dumps(msg).encode() + b"\n")
        deadline = time.time() + timeout
        while True:
            m = self._read_line(max(0.1, deadline - time.time()))
            if m.get("id") == i:
                if "error" in m:
                    raise RpcError(m["error"])
                return m.get("result")
            if "method" in m and "id" not in m:
                self.notes.append(m)

    def try_call(self, method, params=None, timeout=600):
        """(result, None) or (None, RpcError)."""
        try:
            return self.call(method, params, timeout), None
        except RpcError as e:
            return None, e

    def wait_note(self, method, timeout=120, pred=lambda n: True):
        """Wait for a notification (already received ones count)."""
        deadline = time.time() + timeout
        while True:
            for n in self.notes:
                if n.get("method") == method and pred(n):
                    self.notes.remove(n)
                    return n
            left = deadline - time.time()
            if left <= 0:
                return None
            try:
                m = self._read_line(left)
            except (TimeoutError, socket.timeout):
                return None
            if "method" in m and "id" not in m:
                self.notes.append(m)

    def drain(self, secs=0.5):
        try:
            while True:
                m = self._read_line(secs)
                if "method" in m:
                    self.notes.append(m)
        except (TimeoutError, socket.timeout):
            pass

    # ---- helpers ----
    def add_files_source(self, root, name="Maya's files"):
        return self.call("sources.add", {"kind": "files", "name": name,
                                         "settings": {"roots": [str(Path(root).resolve())]}})

    def sync(self, source_id=None, timeout=300):
        """`sources.sync` and wait until ingest reports the source done."""
        self.notes = [n for n in self.notes if n.get("method") != "ingest.progress"]
        self.call("sources.sync", {"id": source_id} if source_id else {})
        deadline = time.time() + timeout
        while time.time() < deadline:
            n = self.wait_note("ingest.progress", timeout=max(1, deadline - time.time()))
            if n is None:
                break
            p = n.get("params") or {}
            if source_id and p.get("source") != source_id:
                continue
            if p.get("phase") in ("idle", "error"):
                return p
        return None

    def install_models(self, ids, timeout=3600):
        """`models.install` each id (on the real clock) and wait for `installed`."""
        for mid in ids:
            info = {m["id"]: m for m in self.call("models.list", {})}
            if info.get(mid, {}).get("installed"):
                continue
            self.call("models.install", {"id": mid})
            n = self.wait_note("models.progress", timeout=timeout,
                               pred=lambda n: n["params"]["id"] == mid and n["params"]["phase"] in ("installed", "failed"))
            if not n or n["params"]["phase"] != "installed":
                raise RuntimeError(f"model {mid} did not install: {n}")

    def wait_runtime_ready(self, timeout=900):
        deadline = time.time() + timeout
        st = None
        while time.time() < deadline:
            st = self.call("runtime.status", {})
            if st and all(r["state"] in ("ready", "stopped") for r in st):
                return st
            if st and any(r["state"] == "failed" for r in st):
                raise RuntimeError(f"runtime failed: {st}")
            time.sleep(3)
        raise RuntimeError(f"runtime not ready: {st}")

    def refresh(self):
        return self.call("proactive.refresh", {})

    def cards(self, include_closed=False):
        return self.call("cards.list", {"include_closed": include_closed})

    def brief(self):
        return self.call("brief.get", {})

    def feedback(self, card_id, fb, reason=None, reason_text=None, until=None):
        p = {"card_id": card_id, "feedback": fb}
        if reason:
            p["reason"] = reason
        if reason_text:
            p["reason_text"] = reason_text
        if until:
            p["until"] = until
        return self.call("cards.feedback", p)


def copy_tree(src, dst):
    """Copy files from src into dst (merge)."""
    src, dst = Path(src), Path(dst)
    for f in src.rglob("*"):
        if f.is_file():
            t = dst / f.relative_to(src)
            t.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(f, t)
