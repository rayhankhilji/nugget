#!/usr/bin/env python3
"""GG8 on macOS: onboarding (local models only) and a short local session, with every packet
captured on the pktap pseudo-interface, which tags each packet with the process that sent it.
There is no network namespace on macOS, so packets are attributed by process (nuggetd and
llama-server) rather than isolated.

    sudo python3 qa/GG8/real/macos_run.py --nuggetd target/release/nuggetd --out OUT
"""
import argparse
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(os.path.dirname(HERE), "harness"))
sys.path.insert(0, HERE)
import common  # noqa: E402
import ipc  # noqa: E402


class MacLab:
    def __init__(self, out, nuggetd):
        self.out, self.nuggetd = out, nuggetd
        self.home = "/tmp/gg8mac"
        self.data = os.path.join(self.home, "data")
        self.checks = []
        self.marks = []

    def check(self, cid, ok, detail="", severity=None):
        self.checks.append({"id": cid, "ok": bool(ok), "detail": detail, "severity": severity})
        print(f"  [{'PASS' if ok else 'FAIL'}] {cid}: {str(detail)[:300]}", flush=True)

    def mark(self, label):
        self.marks.append({"t": time.time(), "label": label})

    note = mark

    @property
    def sock(self):
        return os.path.join(self.data, "ipc", "nuggetd.sock")

    def client(self, timeout=600):
        return ipc.Client(self.sock, timeout=timeout)

    def call(self, m, p=None, timeout=600):
        c = self.client(timeout)
        try:
            return c.call(m, p, timeout=timeout)
        finally:
            c.close()

    def ledger(self):
        return self.call("egress.recent", {"limit": 1000})


def attribute(pcap):
    """Remote endpoints per process (pktap metadata), loopback excluded."""
    r = subprocess.run(["tcpdump", "-nn", "-tt", "-k", "NP", "-r", pcap, "not host 127.0.0.1 and not host ::1"],
                       capture_output=True, text=True)
    per = {}
    for ln in r.stdout.splitlines():
        m = re.search(r"proc ([^:,)]+):(\d+)", ln)
        proc = m.group(1) if m else "?"
        ip = re.search(r" IP6? ([0-9a-f.:]+)\.(\d+) > ([0-9a-f.:]+)\.(\d+):", ln)
        if not ip:
            continue
        src, sport, dst, dport = ip.groups()
        d = per.setdefault(proc, {})
        key = f"{dst}:{dport}" if not dst.startswith(("10.", "192.168.", "172.")) else f"{src}:{sport}"
        d[key] = d.get(key, 0) + 1
    return per


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--nuggetd", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--idle", type=int, default=120)
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    lab = MacLab(a.out, os.path.abspath(a.nuggetd))
    shutil.rmtree(lab.home, ignore_errors=True)
    os.makedirs(os.path.join(lab.home, "config"))
    with open(os.path.join(lab.home, "config", "config.toml"), "w") as f:
        f.write(common.LOCAL_CONFIG)
    pcap = os.path.join(a.out, "pktap.pcapng")
    td = subprocess.Popen(["tcpdump", "-i", "pktap,all", "-k", "-U", "-w", pcap],
                          stderr=open(os.path.join(a.out, "tcpdump.err"), "w"))
    time.sleep(2)
    env = {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "HOME": lab.home, "NUGGET_KEYCHAIN": "0",
           "NUGGET_TRUST_DEV_MANIFEST_KEY": "1", "NUGGET_LOG": "info"}
    d = subprocess.Popen([lab.nuggetd, "--home", lab.home], env=env,
                         stdout=open(os.path.join(a.out, "daemon.stdout"), "w"), stderr=subprocess.STDOUT)
    try:
        ipc.wait_socket(lab.sock)
        t0 = time.time()
        for mid in (common.EMBED_MODEL, common.CHAT_MODEL):
            last, secs = common.install_model(lab, mid)
            lab.check(f"install.{mid}", last and last.get("phase") == "installed", f"{last} {secs:.0f}s")
        s, text, done = common.stream(lab, "chat", {"provider": None, "tier": "synthesis", "items": [],
                                                    "messages": [{"role": "user", "text": "Hi"}]})
        lab.check("first.chat", done and not done.get("error"), text[:100])
        rows_after_onboarding = lab.ledger()
        t1 = time.time()
        lab.mark("session")
        corpus = os.path.join(lab.home, "Documents")
        shutil.copytree(common.CORPUS, corpus)
        lab.call("sources.add", {"kind": "files", "name": "Documents", "settings": {"roots": [corpus]}})
        time.sleep(60)
        lab.check("search", len(lab.call("search", {"query": "Acme renewal", "limit": 5})) > 0)
        s, text, done = common.stream(lab, "retrieve.ask", {"query": "When is the Acme renewal due?",
                                                            "budget_tokens": 1500})
        lab.check("ask.local", s.get("locality") == "on_device", f"{s.get('provider')} {text[:120]!r}")
        time.sleep(a.idle)
        rows = lab.ledger()
        lab.check("session.no_new_ledger_rows", len(rows) == len(rows_after_onboarding),
                  rows[: len(rows) - len(rows_after_onboarding)])
        with open(os.path.join(a.out, "ledger.json"), "w") as f:
            json.dump(rows, f, indent=1)
        lab.t0, lab.t1 = t0, t1
    finally:
        d.send_signal(signal.SIGTERM)
        try:
            d.wait(60)
        except subprocess.TimeoutExpired:
            d.kill()
        time.sleep(2)
        td.send_signal(signal.SIGINT)
        td.wait(20)
    per = attribute(pcap)
    ours = {p: v for p, v in per.items() if p.startswith("nuggetd") or p.startswith("llama")}
    lab.check("llama_server.no_offhost_traffic", not any(p.startswith("llama") for p in ours), ours)
    hosts = sorted({r["host"] for r in rows})
    lab.check("nuggetd.endpoints", True, {"endpoints": {p: sorted(v) for p, v in ours.items()},
                                          "ledger_hosts": hosts})
    # Session-phase packets from nuggetd/llama-server.
    r = subprocess.run(["tcpdump", "-nn", "-tt", "-k", "NP", "-r", pcap, "not host 127.0.0.1 and not host ::1"],
                       capture_output=True, text=True)
    late = [ln for ln in r.stdout.splitlines()
            if re.search(r"proc (nuggetd|llama-server)", ln) and float(ln.split()[0]) >= getattr(lab, "t1", 1e18)]
    lab.check("session.no_packets_from_nugget", not late, late[:10], severity="Blocker")
    json.dump({"checks": lab.checks, "per_process": per, "marks": lab.marks},
              open(os.path.join(a.out, "summary.json"), "w"), indent=1, default=list)


if __name__ == "__main__":
    main()
