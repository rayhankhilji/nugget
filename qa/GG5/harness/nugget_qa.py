#!/usr/bin/env python3
"""GG5 QA harness: drive a real `nuggetd` the way a user does, and capture what it shows.

The daemon runs with its own `--home`, indexes the persona corpus through a `files` source,
extracts with the local runtime's model (when installed) and renders the vault. Every step's
observable state goes to `<out>/<label>/`:

- `status.json`        knowledge.status, ingest.status, runtime.status, providers.list
- `entities.json`      knowledge.entities (all) and knowledge.entity for each
- `graph.json`         knowledge.graph overview, plus the neighbourhood of every person
- `vault/`             a copy of the rendered vault (Markdown pages, README, .nugget/)
- `log.jsonl`          (top level) every step and edit the harness made, with timestamps

Only the standard library. Talks JSON-RPC 2.0 over the daemon's Unix socket
(docs/book/src/reference/ipc.md).
"""

from __future__ import annotations

import argparse
import itertools
import json
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
CORPUS = HERE.parent / "corpus"


def now() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


class RpcError(Exception):
    pass


class Client:
    """One connection; requests are sequential, notifications are kept in `self.notes`."""

    def __init__(self, path: str, timeout: float = 600.0):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(timeout)
        self.sock.connect(path)
        self.buf = b""
        self.ids = itertools.count(1)
        self.notes: list[dict] = []

    def _line(self) -> dict:
        while b"\n" not in self.buf:
            chunk = self.sock.recv(1 << 20)
            if not chunk:
                raise RpcError("daemon closed the connection")
            self.buf += chunk
        line, self.buf = self.buf.split(b"\n", 1)
        return json.loads(line)

    def call(self, method: str, params=None):
        rid = next(self.ids)
        msg = {"jsonrpc": "2.0", "id": rid, "method": method, "params": params if params is not None else {}}
        self.sock.sendall(json.dumps(msg).encode() + b"\n")
        while True:
            m = self._line()
            if "id" not in m or m.get("id") is None:
                self.notes.append(m)
                continue
            if m["id"] != rid:
                continue
            if "error" in m and m["error"]:
                raise RpcError(f"{method}: {m['error']}")
            return m.get("result")

    def close(self):
        try:
            self.sock.close()
        except OSError:
            pass


class Daemon:
    def __init__(self, binary: str, home: Path, out: Path):
        self.binary = binary
        self.home = home
        self.out = out
        self.proc: subprocess.Popen | None = None
        self.client: Client | None = None
        self.log = (out / "log.jsonl").open("a")
        self.runs = 0

    def note(self, step: str, **kw):
        rec = {"t": now(), "step": step, **kw}
        self.log.write(json.dumps(rec, ensure_ascii=False) + "\n")
        self.log.flush()
        print(f"[{rec['t']}] {step} {json.dumps(kw, ensure_ascii=False)[:300]}", flush=True)

    @property
    def vault(self) -> Path:
        return self.home / "data" / "vault"

    def write_config(self, chat_model: str | None, embed_model: str | None, extra: str = ""):
        cfg = self.home / "config"
        cfg.mkdir(parents=True, exist_ok=True)
        lines = ["[runtime]", "idle_stop_minutes = 0"]
        if chat_model:
            lines.append(f'chat_model = "{chat_model}"')
        if embed_model:
            lines.append(f'embedding_model = "{embed_model}"')
        lines += [
            "",
            "[knowledge]",
            # The persona's webmail domain (stands in for gmail.com, which is built in).
            'extra_freemail_domains = ["mailbox.example"]',
            "",
            "[knowledge.me]",
            'name = "Mara Okafor"',
            'emails = ["mara.okafor@brightwell.example", "mara.okafor@mailbox.example"]',
            "",
            "[knowledge.llm]",
            # The default local timeout (120 s) is kept; the CPU runner is slow but this is what
            # users get.
            "",
        ]
        (cfg / "config.toml").write_text("\n".join(lines) + extra)
        self.note("config", text=(cfg / "config.toml").read_text())

    def socket_path(self) -> str:
        p = self.home / "data" / "ipc" / "nuggetd.sock"
        if p.exists():
            return str(p)
        try:
            out = subprocess.run([self.binary, "--home", str(self.home), "status"], capture_output=True, text=True, timeout=20).stdout
            m = re.search(r"socket:\s+(\S+)", out)
            if m and Path(m.group(1)).exists():
                return m.group(1)
        except (subprocess.TimeoutExpired, OSError):
            pass
        return ""

    def start(self):
        self.runs += 1
        logf = (self.out / f"nuggetd-{self.runs}.log").open("w")
        env = dict(os.environ)
        env.setdefault("RUST_LOG", "info,nugget_knowledge=debug,nugget_vault=debug")
        self.proc = subprocess.Popen([self.binary, "--home", str(self.home)], stdout=logf, stderr=subprocess.STDOUT, env=env)
        deadline = time.time() + 120
        while time.time() < deadline:
            if self.proc.poll() is not None:
                raise RuntimeError(f"nuggetd exited with {self.proc.returncode}")
            path = self.socket_path()
            if path:
                try:
                    self.client = Client(path)
                    self.client.call("status")
                    self.note("daemon.started", pid=self.proc.pid, socket=path)
                    return
                except (OSError, RpcError):
                    self.client = None
            time.sleep(0.5)
        raise RuntimeError("daemon did not come up")

    def stop(self):
        if self.client:
            self.client.close()
            self.client = None
        if self.proc and self.proc.poll() is None:
            self.proc.send_signal(signal.SIGTERM)
            try:
                self.proc.wait(60)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()
        self.note("daemon.stopped")

    def call(self, method, params=None):
        assert self.client
        return self.client.call(method, params)

    # ----- models -----

    def install_models(self, ids: list[str], timeout_s: int = 3600):
        listed = {m["id"]: m for m in self.call("models.list")}
        for mid in ids:
            if mid not in listed:
                raise RuntimeError(f"model {mid} is not in the manifest: {sorted(listed)}")
            if listed[mid].get("installed"):
                self.note("models.already_installed", id=mid)
                continue
            self.note("models.install", id=mid)
            self.call("models.install", {"id": mid})
            deadline = time.time() + timeout_s
            while time.time() < deadline:
                ms = {m["id"]: m for m in self.call("models.list")}
                if ms[mid].get("installed"):
                    break
                failed = [n for n in self.client.notes if n.get("method") == "models.progress" and n["params"].get("id") == mid and n["params"].get("phase") == "failed"]
                if failed:
                    raise RuntimeError(f"install of {mid} failed: {failed[-1]}")
                time.sleep(5)
            else:
                raise RuntimeError(f"install of {mid} timed out")
            self.note("models.installed", id=mid)

    def wait_runtime(self, timeout_s: int = 900):
        deadline = time.time() + timeout_s
        last = None
        while time.time() < deadline:
            rt = self.call("runtime.status")
            last = rt
            if rt and all(r.get("state") in ("ready", "running", "idle", "stopped") for r in rt):
                self.note("runtime.status", runtime=rt)
                return rt
            time.sleep(5)
        self.note("runtime.status.timeout", runtime=last)
        return last

    # ----- waiting -----

    def settle(self, label: str, timeout_s: int, quiet_s: int = 20):
        """Wait until ingest's queue is empty and knowledge has no pending items, stably."""
        t0 = time.time()
        deadline = t0 + timeout_s
        stable_since = None
        last_sig = None
        ks = ing = None
        while time.time() < deadline:
            ing = self.call("ingest.status")
            ks = self.call("knowledge.status")
            busy = any(s.get("state") == "syncing" for s in self.call("sources.list"))
            sig = (ing.get("queue_depth"), ks.get("pending_items"), ks.get("entities"), ks.get("claims"), ks.get("last_run"), ks["vault"].get("last_render"), busy)
            idle = (ing.get("queue_depth") or 0) == 0 and (ks.get("pending_items") or 0) == 0 and not busy and ks.get("last_run")
            if idle and sig == last_sig:
                stable_since = stable_since or time.time()
                if time.time() - stable_since >= quiet_s:
                    self.note("settled", label=label, seconds=round(time.time() - t0), knowledge=ks)
                    return True
            else:
                stable_since = None
            last_sig = sig
            time.sleep(3)
        self.note("settle.timeout", label=label, seconds=round(time.time() - t0), knowledge=ks, ingest=ing)
        return False

    # ----- snapshots -----

    def snapshot(self, label: str):
        d = self.out / label
        if d.exists():
            shutil.rmtree(d)
        d.mkdir(parents=True)
        status = {
            "knowledge": self.call("knowledge.status"),
            "ingest": self.call("ingest.status"),
            "runtime": self.call("runtime.status"),
            "providers": self.call("providers.list"),
            "sources": self.call("sources.list"),
        }
        (d / "status.json").write_text(json.dumps(status, indent=1, ensure_ascii=False))
        ents = []
        off = 0
        while True:
            page = self.call("knowledge.entities", {"limit": 200, "offset": off})
            ents += page
            if len(page) < 200:
                break
            off += 200
        details = {}
        for e in ents:
            try:
                details[e["id"]] = self.call("knowledge.entity", {"id": e["id"]})
            except RpcError as err:
                details[e["id"]] = {"error": str(err)}
        (d / "entities.json").write_text(json.dumps({"list": ents, "details": details}, indent=1, ensure_ascii=False))
        graph = {"overview": self.call("knowledge.graph", {"center": None, "depth": 1, "limit": 200, "kinds": []}), "centered": {}}
        for e in ents:
            if e["kind"] == "person":
                try:
                    graph["centered"][e["id"]] = self.call("knowledge.graph", {"center": e["id"], "depth": 1, "limit": 100, "kinds": []})
                except RpcError as err:
                    graph["centered"][e["id"]] = {"error": str(err)}
        (d / "graph.json").write_text(json.dumps(graph, indent=1, ensure_ascii=False))
        if self.vault.exists():
            shutil.copytree(self.vault, d / "vault")
        self.note("snapshot", label=label, entities=len(ents), knowledge=status["knowledge"])
        return {"list": ents, "details": details}


# ----- vault edits (what a user does in a text editor) -----

FACT_RE = re.compile(r"^- (?P<text>.*?)(?P<foot>(?: \[\^\d+\])*) <!-- c:(?P<id>\d+) -->\s*$")


def find_page(vault: Path, name: str) -> Path | None:
    """The page titled `name`, else the first page listing `name` among its aliases."""
    hits = sorted(vault.glob(f"*/{name} (*).md"))
    if hits:
        return hits[0]
    for p in sorted(vault.glob("*/*.md")):
        m = re.search(r"^aliases: \[(.*)\]$", p.read_text(errors="replace"), re.M)
        if m and f'"{name}"' in m.group(1):
            return p
    return None


DATE_RE = re.compile(r"(20\d\d-\d\d-\d\d)")


def set_item_dates(root: Path):
    """git does not keep modification times, and Nugget dates Markdown and calendar items by
    them; give each file the date its name (or `date:` frontmatter) says, at 09:00 UTC."""
    import calendar

    for f in root.rglob("*"):
        if not f.is_file():
            continue
        m = DATE_RE.search(f.name)
        if not m and f.suffix == ".md":
            fm = re.search(r"^date:\s*\"?(20\d\d-\d\d-\d\d)", f.read_text(errors="replace"), re.M)
            m = fm
        if m:
            t = calendar.timegm(time.strptime(m.group(1), "%Y-%m-%d")) + 9 * 3600
            os.utime(f, (t, t))


def fact_lines(text: str):
    for i, line in enumerate(text.split("\n")):
        m = FACT_RE.match(line)
        if m:
            yield i, m


def edit_page(d: Daemon, page: Path, fn, what: str, **kw):
    before = page.read_text()
    after = fn(before)
    if after == before:
        d.note("edit.skipped", page=str(page.relative_to(d.vault)), what=what, **kw)
        return False
    page.write_text(after)
    d.note("edit", page=str(page.relative_to(d.vault)), what=what, diff=unified(before, after), **kw)
    return True


def unified(a: str, b: str) -> str:
    import difflib

    return "".join(difflib.unified_diff(a.splitlines(True), b.splitlines(True), "before", "after", n=1))


def run_edits(d: Daemon):
    """The user's edits, chosen from what is on the pages. Logged in log.jsonl with diffs."""
    v = d.vault
    done = []

    # E1 fix a wrong fact: Dana's page; a fact line naming Brightwell becomes Corvid Labs.
    p = find_page(v, "Dana Whitfield")
    if p:
        def fix(t):
            lines = t.split("\n")
            for i, m in fact_lines(t):
                if "Brightwell" in m["text"]:
                    lines[i] = f"- Dana is Head of Product at Corvid Labs (fixed by me){m['foot']} <!-- c:{m['id']} -->"
                    return "\n".join(lines)
            return t
        if edit_page(d, p, fix, "E1 fix fact (Brightwell -> Corvid Labs)"):
            done.append("E1")

    # E2 delete a fact: the first fact line on Theo's page (stale relationship), else Robert's.
    for name in ("Theo Lindqvist", "Robert Chen"):
        p = find_page(v, name)
        if not p:
            continue
        def delete(t):
            lines = t.split("\n")
            for i, m in fact_lines(t):
                del lines[i]
                return "\n".join(lines)
            return t
        if edit_page(d, p, delete, f"E2 delete first fact line ({name})"):
            done.append("E2")
            break

    # E3 add facts: under an existing fact heading on Sam Okonkwo's page, and a free bullet
    # above the Notes region; plus a personal note under ## Notes.
    p = find_page(v, "Sam Okonkwo")
    if p:
        def add(t):
            lines = t.split("\n")
            for i, line in enumerate(lines):
                if line.startswith("### "):
                    lines.insert(i + 1, "- Sam Okonkwo lives in Halden Bay (added by me)")
                    break
            else:
                for i, line in enumerate(lines):
                    if line.startswith("## Notes"):
                        lines.insert(i, "- Sam Okonkwo lives in Halden Bay (added by me)\n")
                        break
            t2 = "\n".join(lines)
            return t2.rstrip("\n") + "\n\nClimbs V5, prefers the north wall. My own note, keep it.\n"
        if edit_page(d, p, add, "E3 add fact + note (Sam Okonkwo)"):
            done.append("E3")

    # E4 alias in frontmatter: "Bobby" on Robert Chen.
    p = find_page(v, "Robert Chen")
    if p:
        def alias(t):
            return re.sub(r"^aliases: \[(.*?)\]$", lambda m: f'aliases: [{m.group(1)}{", " if m.group(1).strip() else ""}"Bobby"]', t, count=1, flags=re.M)
        if edit_page(d, p, alias, "E4 add alias Bobby (Robert Chen)"):
            done.append("E4")

    # E5 fix the user's own page if it states a stale partner: delete any fact line naming Theo.
    p = find_page(v, "Mara Okafor")
    if p:
        def drop_theo(t):
            lines = t.split("\n")
            for i, m in fact_lines(t):
                if "Theo" in m["text"]:
                    del lines[i]
                    return "\n".join(lines)
            return t
        if edit_page(d, p, drop_theo, "E5 delete a fact line naming Theo (Mara Okafor)"):
            done.append("E5")
    d.note("edits.done", edits=done)
    return done


def touch_sources(d: Daemon, corpus_dir: Path, names: list[str]):
    """Re-extraction: append a harmless line to source files, as a user re-saving them would."""
    touched = []
    for f in sorted(corpus_dir.rglob("*")):
        if f.is_file() and any(n.lower() in f.read_text(errors="ignore").lower() for n in names):
            if f.suffix in (".md", ".txt"):
                f.write_text(f.read_text() + "\n\n(re-saved)\n")
                touched.append(str(f.relative_to(corpus_dir)))
            elif f.suffix in (".eml", ".ics"):
                os.utime(f)  # mtime change: a new item version
                touched.append(str(f.relative_to(corpus_dir)))
    d.note("sources.touched", files=touched)
    return touched


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--nuggetd", required=True)
    ap.add_argument("--home", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--chat-model", default="")
    ap.add_argument("--embed-model", default="")
    ap.add_argument("--no-models", action="store_true", help="deterministic extractors only")
    ap.add_argument("--settle-timeout", type=int, default=7200)
    ap.add_argument("--phases", default="phase1,phase2,edits,reextract,restart")
    args = ap.parse_args()

    home = Path(args.home).resolve()
    out = Path(args.out).resolve()
    out.mkdir(parents=True, exist_ok=True)
    home.mkdir(parents=True, exist_ok=True)
    work = home / "corpus"  # the folder the user points Nugget at
    d = Daemon(args.nuggetd, home, out)
    phases = args.phases.split(",")
    # Without models the runtime has nothing installed, so only the deterministic extractors run.
    d.write_config(None if args.no_models else args.chat_model or None, None if args.no_models else args.embed_model or None)
    d.start()
    try:
        if not args.no_models:
            ids = [m for m in (args.chat_model, args.embed_model) if m]
            d.install_models(ids)
            d.stop()
            d.start()  # the runtime picks installed models up at boot
            d.wait_runtime()
        if work.exists():
            shutil.rmtree(work)
        work.mkdir()
        shutil.copytree(CORPUS / "phase1", work, dirs_exist_ok=True)
        set_item_dates(work)
        src = d.call("sources.add", {"kind": "files", "name": "Mara's files", "settings": {"roots": [str(work)]}})
        d.note("sources.add", source=src)
        d.settle("phase1", args.settle_timeout)
        d.snapshot("phase1")
        if "phase2" in phases:
            shutil.copytree(CORPUS / "phase2", work, dirs_exist_ok=True)
            set_item_dates(work)
            d.note("corpus.phase2.copied")
            time.sleep(5)
            d.settle("phase2", args.settle_timeout)
            d.snapshot("phase2")
        if "edits" in phases:
            run_edits(d)
            time.sleep(10)
            d.settle("edits", 600)
            d.snapshot("edits")
        if "reextract" in phases:
            touch_sources(d, work, ["Dana", "Theo", "Robert", "Bob", "Okonkwo"])
            time.sleep(5)
            d.settle("reextract", args.settle_timeout)
            d.snapshot("reextract")
        if "restart" in phases:
            d.stop()
            d.start()
            time.sleep(10)
            d.settle("restart", args.settle_timeout)
            d.snapshot("restart")
            # Does what a restart changed come back once the model is up? Wait for the runtime,
            # then for the periodic run (10 minutes), and look again.
            d.wait_runtime()
            time.sleep(660)
            d.settle("restart-later", args.settle_timeout)
            d.snapshot("restart-later")
    finally:
        d.stop()


if __name__ == "__main__":
    sys.exit(main())
