#!/usr/bin/env python3
"""GG8 scenario runner: run nuggetd inside the lab namespace and watch everything it sends.

    sudo python3 qa/GG8/harness/lab.py run qa/GG8/scenarios/s01_idle_local.py --out OUT
    sudo python3 qa/GG8/harness/lab.py run-all --out OUT [--only s01,s02]

A scenario is a Python file with:
    NAME = "short id"
    MODE = "sinkhole" | "internet"       (default sinkhole)
    CONFIG = '''toml'''                  ({fixtures}, {home}, {data} are substituted)
    ENV = {"VAR": "value"}               (extra daemon environment, e.g. API keys)
    EXTRA_MOCKS = ["mock_slack"]         (modules in harness/ serving more hosts)
    def run(lab): ...                    (drive the daemon; record checks with lab.check)

Each run writes to OUT/<NAME>/: dns.jsonl, requests.jsonl (every request the sink answered, in
full), ns.pcap and flows.txt (every packet that left the namespace), ledger.json (egress.recent),
daemon logs, checks.json and summary.json (observed hosts vs. ledger rows, canary hits).
"""
import argparse
import glob
import importlib.util
import json
import os
import shutil
import signal
import subprocess
import sys
import time
import traceback

HERE = os.path.dirname(os.path.abspath(__file__))
QA = os.path.dirname(HERE)
REPO = os.path.dirname(os.path.dirname(QA))
sys.path.insert(0, HERE)
import ipc  # noqa: E402

NS = os.environ.get("GG8_NS", "gg8")
SINK_IP = "203.0.113.10"
CA_DIR = os.environ.get("GG8_CA_DIR", "/tmp/gg8-ca")
NUGGETD = os.environ.get("GG8_NUGGETD") or next(
    (p for p in [os.path.join(REPO, "target/release/nuggetd"),
                 os.path.join(REPO, "target/debug/nuggetd")] if os.path.exists(p)),
    "nuggetd")
FIXTURES = os.path.join(QA, "fixtures")
# Canary strings planted in fixtures. Each must appear only where the scenario allows it.
CANARY_PREFIX = "GG8CANARY"


def sh(cmd, **kw):
    return subprocess.run(cmd, check=True, **kw)


def ensure_ca():
    os.makedirs(CA_DIR, exist_ok=True)
    key, crt = os.path.join(CA_DIR, "ca.key"), os.path.join(CA_DIR, "ca.crt")
    if not os.path.exists(crt):
        sh(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", key,
            "-out", crt, "-days", "30", "-subj", "/CN=GG8 QA test CA (synthetic)",
            "-addext", "basicConstraints=critical,CA:TRUE",
            "-addext", "keyUsage=critical,keyCertSign,cRLSign"], capture_output=True)
    dst = "/usr/local/share/ca-certificates/gg8-qa.crt"
    if not os.path.exists(dst) or open(dst).read() != open(crt).read():
        shutil.copy(crt, dst)
        sh(["update-ca-certificates"], capture_output=True)
    return crt


class Lab:
    def __init__(self, scen, out):
        self.scen = scen
        self.name = scen.NAME
        self.mode = getattr(scen, "MODE", "sinkhole")
        self.out = os.path.abspath(out)
        self.home = f"/tmp/gg8/{self.name}"
        self.data = os.path.join(self.home, "data")
        self.checks = []
        self.marks = []
        self.procs = {}
        self.daemon = None
        self.daemon_log = None

    # -- infrastructure -----------------------------------------------------------------------
    def up(self):
        shutil.rmtree(self.out, ignore_errors=True)
        os.makedirs(self.out)
        shutil.rmtree(self.home, ignore_errors=True)
        os.makedirs(self.home)
        ensure_ca()
        sh([os.path.join(HERE, "netlab.sh"), "up", self.mode])
        env = dict(os.environ)
        env["GG8_EXTRA_MOCKS"] = ",".join(getattr(self.scen, "EXTRA_MOCKS", []))
        env.update(getattr(self.scen, "SINK_ENV", {}))
        upstream = os.environ.get("GG8_UPSTREAM_DNS", "8.8.8.8")
        self.procs["sink"] = subprocess.Popen(
            [sys.executable, os.path.join(HERE, "sink.py"), "--out", self.out, "--ca-dir", CA_DIR,
             "--mode", self.mode, "--upstream-dns", upstream,
             "--extra-ports", getattr(self.scen, "RAW_PORTS", "993,143,8443,25,465,587")],
            env=env, stdout=open(os.path.join(self.out, "sink.out"), "w"),
            stderr=subprocess.STDOUT)
        for _ in range(100):
            if os.path.exists(os.path.join(self.out, "sink.ready")):
                break
            if self.procs["sink"].poll() is not None:
                raise RuntimeError("sink died: " + open(os.path.join(self.out, "sink.out")).read())
            time.sleep(0.1)
        self.procs["tcpdump"] = subprocess.Popen(
            ["ip", "netns", "exec", NS, "tcpdump", "-i", "any", "-nn", "-U", "-s", "0",
             "-w", os.path.join(self.out, "ns.pcap")],
            stdout=subprocess.DEVNULL, stderr=open(os.path.join(self.out, "tcpdump.err"), "w"))
        time.sleep(1.0)

    def down(self):
        self.stop_daemon()
        for name in ("tcpdump", "sink"):
            p = self.procs.get(name)
            if p and p.poll() is None:
                p.send_signal(signal.SIGINT if name == "tcpdump" else signal.SIGTERM)
                try:
                    p.wait(10)
                except subprocess.TimeoutExpired:
                    p.kill()
        subprocess.run([os.path.join(HERE, "netlab.sh"), "down"])

    # -- the daemon ---------------------------------------------------------------------------
    def fmt(self, s):
        return (s.replace("{fixtures}", FIXTURES).replace("{home}", self.home)
                 .replace("{data}", self.data).replace("{out}", self.out))

    def start_daemon(self, extra_env=None, log_level=None):
        cfg_dir = os.path.join(self.home, "config")
        os.makedirs(cfg_dir, exist_ok=True)
        cfg = self.fmt(getattr(self.scen, "CONFIG", ""))
        cfg_path = os.path.join(cfg_dir, "config.toml")
        if not os.path.exists(cfg_path) or getattr(self, "_rewrite_cfg", True):
            with open(cfg_path, "w") as f:
                f.write(cfg)
            os.chmod(cfg_path, 0o600)
            self._rewrite_cfg = False
        env = {"PATH": "/usr/local/bin:/usr/bin:/bin", "HOME": self.home, "LANG": "C.UTF-8",
               "NUGGET_KEYCHAIN": "0", "NUGGET_LOG": log_level or getattr(self.scen, "LOG", "info"),
               "NUGGET_TRUST_DEV_MANIFEST_KEY": os.environ.get("NUGGET_TRUST_DEV_MANIFEST_KEY", "")}
        env.update({k: self.fmt(v) for k, v in getattr(self.scen, "ENV", {}).items()})
        env.update(extra_env or {})
        env = {k: v for k, v in env.items() if v != ""}
        self.daemon_log = open(os.path.join(self.out, "daemon.stdout"), "a")
        self.mark("daemon.start")
        self.daemon = subprocess.Popen(
            ["ip", "netns", "exec", NS, "env", "-i", *[f"{k}={v}" for k, v in env.items()],
             NUGGETD, "--home", self.home],
            stdout=self.daemon_log, stderr=subprocess.STDOUT)
        ipc.wait_socket(self.sock, timeout=180)
        self.mark("daemon.ready")

    @property
    def sock(self):
        return os.path.join(self.data, "ipc", "nuggetd.sock")

    def client(self, timeout=600):
        return ipc.Client(self.sock, timeout=timeout)

    def call(self, method, params=None, timeout=600):
        c = self.client(timeout)
        try:
            return c.call(method, params, timeout=timeout)
        finally:
            c.close()

    def stop_daemon(self):
        if self.daemon and self.daemon.poll() is None:
            try:
                self.ledger_snapshot("final")
            except Exception as e:  # noqa: BLE001
                self.note(f"ledger snapshot failed: {e!r}")
            self.mark("daemon.stop")
            # `ip netns exec` execs env, which execs nuggetd: the pid is the daemon's.
            self.daemon.send_signal(signal.SIGTERM)
            try:
                self.daemon.wait(60)
            except subprocess.TimeoutExpired:
                self.daemon.kill()
                self.note("daemon did not stop within 60 s of SIGTERM")
        self.daemon = None

    def restart_daemon(self, **kw):
        self.stop_daemon()
        self.start_daemon(**kw)

    def ns_run(self, argv, **kw):
        """Run a command inside the lab namespace (for clients that need its loopback)."""
        return subprocess.run(["ip", "netns", "exec", NS, *argv], **kw)

    # -- observations -------------------------------------------------------------------------
    def mark(self, label):
        self.marks.append({"t": time.time(), "label": label})

    def note(self, text):
        self.marks.append({"t": time.time(), "note": text})

    def check(self, cid, ok, detail="", severity=None):
        self.checks.append({"id": cid, "ok": bool(ok), "detail": detail, "severity": severity,
                            "t": time.time()})
        print(f"  [{'PASS' if ok else 'FAIL'}] {cid}: {str(detail)[:300]}", flush=True)
        return ok

    def requests(self, since=None, host=None):
        out = []
        p = os.path.join(self.out, "requests.jsonl")
        if os.path.exists(p):
            for line in open(p):
                r = json.loads(line)
                if since and r["t"] < since:
                    continue
                if host and r.get("host") != host:
                    continue
                out.append(r)
        return out

    def dns(self):
        p = os.path.join(self.out, "dns.jsonl")
        return [json.loads(x) for x in open(p)] if os.path.exists(p) else []

    def ledger(self, limit=1000):
        return self.call("egress.recent", {"limit": limit})

    def ledger_snapshot(self, tag):
        rows = self.ledger()
        with open(os.path.join(self.out, f"ledger-{tag}.json"), "w") as f:
            json.dump(rows, f, indent=1)
        return rows

    def wait_idle(self, seconds=5, timeout=300, source=None):
        """Wait until ingest is idle (queue empty) and no request reached the sink for `seconds`."""
        end = time.time() + timeout
        while time.time() < end:
            try:
                st = self.call("ingest.status", timeout=30)
            except Exception:  # noqa: BLE001
                st = None
            busy = st and st.get("queue_depth", 0) > 0
            srcs = (st or {}).get("sources") or []
            if source:
                srcs = [s for s in srcs if s.get("id") == source]
            if any(s.get("state") == "syncing" for s in srcs):
                busy = True
            last = max([r["t"] for r in self.requests()] or [0])
            if not busy and time.time() - last > seconds:
                return True
            time.sleep(1)
        return False

    # -- analysis -----------------------------------------------------------------------------
    def flows(self):
        pcap = os.path.join(self.out, "ns.pcap")
        if not os.path.exists(pcap):
            return []
        r = subprocess.run(["tcpdump", "-nn", "-r", pcap, "-q", "-tt"], capture_output=True,
                           text=True)
        lines = r.stdout.splitlines()
        with open(os.path.join(self.out, "flows.txt"), "w") as f:
            f.write(r.stdout)
        flows = {}
        for ln in lines:
            parts = ln.split()
            # "<ts> <if> <dir> IP a.b.c.d.p > e.f.g.h.q: tcp 0"
            try:
                i = parts.index("IP") if "IP" in parts else parts.index("IP6")
            except ValueError:
                continue
            iface = parts[1] if len(parts) > 2 else "?"
            src, dst = parts[i + 1], parts[i + 3].rstrip(":")
            proto = parts[i + 4] if len(parts) > i + 4 else "?"
            if iface == "lo":
                continue
            key = (src.rsplit(".", 1)[0], dst.rsplit(".", 1)[0], dst.rsplit(".", 1)[-1],
                   proto.rstrip(","))
            if key[0].startswith("203.0.113.10") or key[0].startswith("192.168.88.1"):
                continue  # replies
            flows[key] = flows.get(key, 0) + 1
        return [{"src": k[0], "dst": k[1], "dport": k[2], "proto": k[3], "packets": v}
                for k, v in sorted(flows.items())]

    def canary_hits(self):
        """Every canary string found in sink requests, DNS, daemon logs and plaintext files."""
        hits = []

        def scan(label, text):
            i = 0
            while True:
                i = text.find(CANARY_PREFIX, i)
                if i < 0:
                    break
                end = i
                while end < len(text) and (text[end].isalnum() or text[end] in "-_"):
                    end += 1
                hits.append({"where": label, "canary": text[i:end]})
                i = end
        for r in self.requests():
            scan(f"request {r.get('method')} {r.get('host')}{r.get('path', '')}",
                 json.dumps(r.get("headers")) + (r.get("body") or ""))
        for path in [os.path.join(self.out, "daemon.stdout")] + glob.glob(
                os.path.join(self.data, "logs", "*")):
            if os.path.isfile(path):
                scan("log " + os.path.basename(path), open(path, errors="replace").read())
        for root, _, files in os.walk(self.home):
            for fn in files:
                p = os.path.join(root, fn)
                if "/logs/" in p or fn.startswith("nugget.db") or "/models/" in p:
                    continue
                try:
                    with open(p, "rb") as fh:
                        blob = fh.read(4_000_000)
                except OSError:
                    continue
                scan("file " + os.path.relpath(p, self.home), blob.decode("utf-8", "replace"))
        dedup = {}
        for h in hits:
            dedup.setdefault((h["where"], h["canary"]), 0)
            dedup[(h["where"], h["canary"])] += 1
        return [{"where": w, "canary": c, "count": n} for (w, c), n in sorted(dedup.items())]

    def summarize(self):
        final = os.path.join(self.out, "ledger-final.json")
        ledger = json.load(open(final)) if os.path.exists(final) else []
        reqs = [r for r in self.requests() if "method" in r]
        observed = {}
        for r in reqs:
            if r["listen"].startswith("203.0.113.10") or r["listen"].startswith("192.168.88.1"):
                h = r.get("host") or r.get("sni") or r["listen"]
                o = observed.setdefault(h, {"requests": 0, "bytes_in": 0, "paths": set()})
                o["requests"] += 1
                o["bytes_in"] += r.get("bytes_in", 0)
                o["paths"].add(r["method"] + " " + r["path"].split("?")[0])
        ledger_hosts = {}
        for row in ledger:
            lh = ledger_hosts.setdefault(row["host"], {"rows": 0, "bytes_sent": 0, "features": set()})
            lh["rows"] += 1
            lh["bytes_sent"] += row.get("bytes_sent", 0)
            lh["features"].add(row.get("feature"))
        raw = [r for r in self.requests() if r.get("raw_tcp") or r.get("tls_handshake_error")]
        flows = self.flows()
        dns = self.dns()
        summary = {
            "scenario": self.name, "mode": self.mode,
            "dns_names": sorted({d.get("qname") for d in dns if d.get("qname")}),
            "flows_off_namespace": flows,
            "observed_hosts": {h: {**v, "paths": sorted(v["paths"])} for h, v in observed.items()},
            "ledger_hosts": {h: {**v, "features": sorted(f for f in v["features"] if f)}
                             for h, v in ledger_hosts.items()},
            "unledgered_hosts": sorted(h for h in observed if h not in ledger_hosts),
            "ledger_without_traffic": sorted(h for h in ledger_hosts if h not in observed
                                             and self.mode == "sinkhole"),
            "raw_or_failed_tls": raw,
            "canaries": self.canary_hits(),
            "checks": self.checks,
            "marks": self.marks,
        }
        with open(os.path.join(self.out, "summary.json"), "w") as f:
            json.dump(summary, f, indent=1, default=list)
        logs = os.path.join(self.data, "logs")
        if os.path.isdir(logs):
            shutil.copytree(logs, os.path.join(self.out, "logs"), dirs_exist_ok=True)
        return summary


def load(path):
    spec = importlib.util.spec_from_file_location(os.path.basename(path)[:-3], path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def run_one(path, out_root):
    scen = load(path)
    lab = Lab(scen, os.path.join(out_root, scen.NAME))
    print(f"== {scen.NAME} ({lab.mode})", flush=True)
    err = None
    try:
        lab.up()
        if getattr(scen, "AUTOSTART", True):
            lab.start_daemon()
        scen.run(lab)
    except Exception:  # noqa: BLE001
        err = traceback.format_exc()
        print(err, flush=True)
        lab.check("scenario.completed", False, err.splitlines()[-1])
    finally:
        try:
            lab.stop_daemon()
        except Exception:  # noqa: BLE001
            pass
        lab.down()
    s = lab.summarize()
    if err:
        with open(os.path.join(lab.out, "error.txt"), "w") as f:
            f.write(err)
    print(f"   hosts observed: {sorted(s['observed_hosts'])}")
    print(f"   ledger hosts:   {sorted(s['ledger_hosts'])}")
    print(f"   unledgered:     {s['unledgered_hosts']}")
    print(f"   flows:          {[(f['dst'], f['dport']) for f in s['flows_off_namespace']]}")
    print(f"   canaries:       {len(s['canaries'])} hits")
    return s


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run")
    r.add_argument("scenario")
    r.add_argument("--out", required=True)
    a = sub.add_parser("run-all")
    a.add_argument("--out", required=True)
    a.add_argument("--only", default="")
    a.add_argument("--dir", default=os.path.join(QA, "scenarios"))
    args = ap.parse_args()
    if args.cmd == "run":
        run_one(args.scenario, args.out)
    else:
        only = [x for x in args.only.split(",") if x]
        results = []
        for p in sorted(glob.glob(os.path.join(args.dir, "s*.py"))):
            base = os.path.basename(p)
            if only and not any(base.startswith(o) for o in only):
                continue
            s = run_one(p, args.out)
            results.append({"scenario": s["scenario"],
                            "passed": sum(c["ok"] for c in s["checks"]),
                            "failed": sum(not c["ok"] for c in s["checks"]),
                            "unledgered": s["unledgered_hosts"]})
        with open(os.path.join(args.out, "index.json"), "w") as f:
            json.dump(results, f, indent=1)
        print(json.dumps(results, indent=1))


if __name__ == "__main__":
    main()
