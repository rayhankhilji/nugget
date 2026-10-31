"""Area B: providers.probe ledger feature `probe`; an openai_responses provider (headers, store
flag); a loopback server marked locality = "cloud" (treated and recorded as cloud?)."""
import json
import os
import sys
import time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import b_common as B  # noqa: E402

NAME = "s26_probe_locality"
CONFIG = """
[[providers]]
id = "openai"
kind = "openai_responses"
model = "gpt-mock"
api_key = "env:OPENAI_API_KEY"
tiers = ["synthesis"]

[[providers]]
id = "gateway"
kind = "openai_compatible"
base_url = "http://127.0.0.1:18080/v1"
model = "mock-model"
locality = "cloud"
tiers = ["light"]
"""
ENV = {"OPENAI_API_KEY": "sk-GG8CANARY-APIKEY-openai"}


def run(lab):
    import subprocess
    # a loopback OpenAI-compatible server inside the namespace (a "local gateway to the cloud")
    srv = os.path.join(lab.out, "loop_srv.py")
    open(srv, "w").write(r'''
import json, sys, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
sys.path.insert(0, %r)
import mocks
class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *a): pass
    def do_GET(self): self.go()
    def do_POST(self): self.go()
    def go(self):
        n = int(self.headers.get("Content-Length") or 0); b = self.rfile.read(n)
        open(%r, "a").write(json.dumps({"t": time.time(), "path": self.path, "body": b.decode(errors="replace")}) + "\n")
        st, h, p = mocks.openai_compat(self.command, self.path, {}, json.loads(b) if b else {})
        self.send_response(st)
        for k, v in h.items(): self.send_header(k, v)
        if isinstance(p, bytes):
            self.send_header("Content-Length", str(len(p))); self.end_headers(); self.wfile.write(p)
        else:
            self.send_header("Transfer-Encoding", "chunked"); self.end_headers()
            for c in p:
                c = c.encode(); self.wfile.write(b"%%x\r\n%%s\r\n" %% (len(c), c))
            self.wfile.write(b"0\r\n\r\n")
ThreadingHTTPServer(("127.0.0.1", 18080), H).serve_forever()
''' % (os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "harness"),
       os.path.join(lab.out, "loopback.jsonl")))
    p = subprocess.Popen(["ip", "netns", "exec", "gg8-1", "python3", srv])
    time.sleep(1)
    try:
        st = lab.call("status")
        lab.note("providers: " + json.dumps(st["providers"]))
        gw = [x for x in st["providers"] if x["id"] == "gateway"]
        lab.check("gateway.locality_cloud", gw and gw[0]["locality"] == "cloud", gw)
        t = time.time()
        for pid in ("openai", "gateway"):
            try:
                lab.note(f"probe {pid}: {lab.call('providers.probe', {'provider': pid}, timeout=120)}")
            except Exception as e:  # noqa: BLE001
                lab.note(f"probe {pid}: {e}")
        time.sleep(2)
        rows = [r for r in lab.ledger() if r["at"] >= time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(t - 1))]
        lab.check("probe.ledger_feature", rows and all(r["feature"] == "probe" for r in rows if r["host"] == "api.openai.com")
                  and any(r["host"] == "api.openai.com" for r in rows),
                  [(r["host"], r["feature"], r["bytes_sent"]) for r in rows])
        reqs = B.cloud_reqs(lab, t)
        lab.check("probe.requests_ledgered", len(reqs) <= len([r for r in rows if r["host"] == "api.openai.com"]),
                  f"{len(reqs)} requests to api.openai.com, {len([r for r in rows if r['host']=='api.openai.com'])} rows")
        B.save_requests(lab, "probe", reqs)
        loop = [json.loads(x) for x in open(os.path.join(lab.out, "loopback.jsonl"))] if os.path.exists(os.path.join(lab.out, "loopback.jsonl")) else []
        lab.check("gateway.probe_ledgered", (not loop) or any(r["host"].startswith("127.0.0.1") for r in rows),
                  f"{len(loop)} requests to the cloud-marked loopback gateway; ledger rows: {[(r['host'], r['feature']) for r in rows]}",
                  severity="Medium")
        # Ask through the cloud-marked gateway: sensitive items must be withheld
        _, corpus = B.add_corpus(lab)
        c = lab.client()
        n0 = len(loop)
        t = time.time()
        st2, _, done = c.stream("retrieve.ask", {"query": "Acme renewal payment", "budget_tokens": 3000,
                                                 "provider": "gateway"}, timeout=120)
        time.sleep(2)
        loop = [json.loads(x) for x in open(os.path.join(lab.out, "loopback.jsonl"))]
        b = "\n".join(x["body"] for x in loop[n0:])
        lab.check("gateway.ask_withholds_sensitive", not any(s in b for s in B.SECRETS),
                  f"locality={st2.get('locality')} excluded={st2.get('excluded_sensitive')}", severity="High")
        rows = [r for r in lab.ledger() if r["at"] >= time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(t - 1))]
        lab.check("gateway.ask_ledgered", any(r["feature"] == "ask" for r in rows),
                  f"rows={[(r['host'], r['feature']) for r in rows]}", severity="Medium")
        # Ask through openai_responses: store flag and headers
        t = time.time()
        c.stream("retrieve.ask", {"query": "Acme renewal payment", "budget_tokens": 3000, "provider": "openai"}, timeout=120)
        reqs = B.cloud_reqs(lab, t)
        b = json.loads(reqs[0]["body"]) if reqs else {}
        lab.check("openai.store_false", b.get("store") is False, f"store={b.get('store')}")
        B.no_secrets(lab, "openai.ask", reqs)
        B.ledger_covers(lab, "openai.ask", t, "ask", corpus)
        B.save_requests(lab, "openai-ask", reqs)
        B.keys_only_to_own_host(lab, "keys", {"GG8CANARY-APIKEY-openai": "api.openai.com"})
        c.close()
    finally:
        p.kill()
