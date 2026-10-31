"""Shared helpers for the GG8 real-model scenarios (qa/GG8/real/scenarios/s6*.py).

These run on GitHub runners (see .github/workflows/qa-GG8.yml) through the lab
(qa/GG8/harness/lab.py): the daemon in its own network namespace, every packet leaving it in
ns.pcap, every DNS query in dns.jsonl. In `internet` mode TLS is not intercepted, so the hosts a
scenario reached come from DNS and from the SNI of each TLS ClientHello (tshark).
"""
import json
import os
import shutil
import subprocess
import time

QA = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CORPUS = os.path.join(QA, "fixtures", "corpus")
# The manifest's smallest chat and embedding models (what `nugget-bench --local-models` picks).
CHAT_MODEL = os.environ.get("GG8_CHAT_MODEL", "qwen3-1.7b-q8")
EMBED_MODEL = os.environ.get("GG8_EMBED_MODEL", "nomic-embed-text-v1.5-q8")
LOCAL_CONFIG = f"""
[models.selected]
triage = "{CHAT_MODEL}"
light = "{CHAT_MODEL}"
synthesis = "{CHAT_MODEL}"
embedding = "{EMBED_MODEL}"

[runtime]
chat_model = "{CHAT_MODEL}"
embedding_model = "{EMBED_MODEL}"
idle_stop_minutes = 0

[knowledge.llm.local]
max_requests_per_item = 1
"""
SENSITIVE_MARKERS = ["GG8CANARY-SENS", "4111 1111 1111 1111", "512-34-6789",
                     "AKIAIOSFODNN7GG8TEST", "GB82 WEST", "sk-ant-api03-GG8"]


def adopt_home(lab, prev_name):
    """Move the data and config of an earlier scenario's home into this lab's home."""
    prev = f"/tmp/gg8/{prev_name}"
    for sub in ("data", "config", "Documents"):
        src = os.path.join(prev, sub)
        if os.path.exists(src):
            dst = os.path.join(lab.home, sub)
            shutil.rmtree(dst, ignore_errors=True)
            os.rename(src, dst)
    # Socket paths and pid files are per run.
    for p in ("data/ipc", "data/runtime"):
        shutil.rmtree(os.path.join(lab.home, p), ignore_errors=True)
    lab._rewrite_cfg = True


def copy_corpus(lab):
    dst = os.path.join(lab.home, "Documents")
    if not os.path.exists(dst):
        shutil.copytree(CORPUS, dst)
    return dst


def install_model(lab, mid, timeout=1800):
    c = lab.client(timeout=timeout)
    try:
        t0 = time.time()
        c.call("models.install", {"id": mid}, timeout=120)
        last = None
        while time.time() - t0 < timeout:
            n = c.wait_note(lambda m: m.get("method") == "models.progress"
                            and m["params"].get("id") == mid, timeout=60)
            if n is None:
                continue
            last = n["params"]
            if last.get("phase") in ("installed", "failed"):
                break
        return last, time.time() - t0
    finally:
        c.close()


def wait_runtime(lab, timeout=900):
    end = time.time() + timeout
    st = None
    while time.time() < end:
        st = lab.call("runtime.status", timeout=60)
        if st and all(r.get("state") == "ready" for r in st):
            return st
        if st and any(r.get("state") == "failed" for r in st):
            return st
        time.sleep(3)
    return st


def stream(lab, method, params, timeout=900):
    c = lab.client(timeout=timeout)
    try:
        return c.stream(method, params, timeout=timeout)
    finally:
        c.close()


def tls_streams(out):
    """Per TCP stream that left the namespace: remote ip/port, SNI, bytes up/down (tshark)."""
    pcap = os.path.join(out, "ns.pcap")
    if not os.path.exists(pcap) or not shutil.which("tshark"):
        return []
    r = subprocess.run(["tshark", "-r", pcap, "-Y", "tcp && !(ip.addr==127.0.0.1)", "-T", "fields",
                        "-E", "separator=|", "-e", "frame.time_epoch", "-e", "tcp.stream",
                        "-e", "ip.src", "-e", "ip.dst", "-e", "tcp.srcport", "-e", "tcp.dstport",
                        "-e", "tcp.len", "-e", "tls.handshake.extensions_server_name"],
                       capture_output=True, text=True)
    streams = {}
    for ln in r.stdout.splitlines():
        f = ln.split("|")
        if len(f) < 8 or not f[1]:
            continue
        t, sid, src, dst, sport, dport, tlen, sni = f[:8]
        s = streams.setdefault(sid, {"stream": int(sid), "first": float(t), "remote": None,
                                     "port": None, "sni": None, "up": 0, "down": 0})
        local = src.startswith("203.0.113.") or src.startswith("192.168.")
        if s["remote"] is None:
            s["remote"], s["port"] = (dst, dport) if local else (src, sport)
        n = int(tlen or 0)
        if src == s["remote"]:
            s["down"] += n
        else:
            s["up"] += n
        if sni:
            s["sni"] = sni
    return sorted(streams.values(), key=lambda s: s["first"])


def compare_with_ledger(lab, since=0.0):
    """Hosts reached (SNI, or DNS name of the remote IP) vs the ledger's hosts and bytes."""
    streams = [s for s in tls_streams(lab.out) if s["first"] >= since]
    ip2name = {}
    for d in lab.dns():
        for a in d.get("answers") or []:
            ip2name.setdefault(a, d.get("qname"))
    by_host = {}
    for s in streams:
        h = s["sni"] or ip2name.get(s["remote"]) or s["remote"]
        b = by_host.setdefault(h, {"streams": 0, "bytes_up": 0, "bytes_down": 0, "ports": set()})
        b["streams"] += 1
        b["bytes_up"] += s["up"]
        b["bytes_down"] += s["down"]
        b["ports"].add(s["port"])
    rows = lab.ledger()
    led = {}
    for r in rows:
        x = led.setdefault(r["host"], {"rows": 0, "bytes_sent": 0, "bytes_received": 0,
                                       "features": set()})
        x["rows"] += 1
        x["bytes_sent"] += r.get("bytes_sent", 0)
        x["bytes_received"] += r.get("bytes_received", 0)
        x["features"].add(r.get("feature"))
    res = {"observed": {h: {**v, "ports": sorted(v["ports"])} for h, v in by_host.items()},
           "ledger": {h: {**v, "features": sorted(v["features"])} for h, v in led.items()},
           "unledgered": sorted(h for h in by_host if h not in led),
           "ledger_rows": rows,
           "dns": sorted({d.get("qname") for d in lab.dns() if d.get("qname")})}
    with open(os.path.join(lab.out, "observed-vs-ledger.json"), "w") as f:
        json.dump(res, f, indent=1, default=list)
    with open(os.path.join(lab.out, "tls-streams.json"), "w") as f:
        json.dump(streams, f, indent=1)
    return res


def offns_packets_since(lab, since):
    """Packets that left the namespace (not loopback) after `since`, from the pcap."""
    pcap = os.path.join(lab.out, "ns.pcap")
    r = subprocess.run(["tcpdump", "-nn", "-tt", "-r", pcap, "not host 127.0.0.1"],
                       capture_output=True, text=True)
    out = []
    for ln in r.stdout.splitlines():
        try:
            t = float(ln.split()[0])
        except (ValueError, IndexError):
            continue
        if t >= since and " lo " not in ln:
            out.append(ln)
    return out
