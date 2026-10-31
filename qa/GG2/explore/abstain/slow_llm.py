#!/usr/bin/env python3
"""Fake OpenAI-compatible server for GG2 explore/abstain: logs every request body (one JSON line)
and streams `--chunks` deltas `--delay` seconds apart, so a stream can be cancelled mid-way.
Usage: slow_llm.py --port P --log FILE [--chunks 40 --delay 0.25]"""
import argparse, json, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ap = argparse.ArgumentParser()
ap.add_argument("--port", type=int, required=True); ap.add_argument("--log", required=True)
ap.add_argument("--chunks", type=int, default=40); ap.add_argument("--delay", type=float, default=0.25)
a = ap.parse_args()

class H(BaseHTTPRequestHandler):
    def log_message(self, *x): pass
    def do_GET(self):
        b = json.dumps({"object": "list", "data": [{"id": "capture", "object": "model"}]}).encode()
        self.send_response(200); self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(b))); self.end_headers(); self.wfile.write(b)
    def do_POST(self):
        raw = self.rfile.read(int(self.headers.get("Content-Length") or 0))
        try: body = json.loads(raw)
        except Exception: body = {"_raw": raw[:2000].decode("utf-8", "replace")}
        rec = {"t": time.time(), "path": self.path, "body": body, "len": len(raw)}
        with open(a.log, "a") as f: f.write(json.dumps(rec) + "\n")
        self.send_response(200); self.send_header("Content-Type", "text/event-stream"); self.end_headers()
        ok = True
        try:
            for i in range(a.chunks):
                c = {"choices": [{"index": 0, "delta": {"content": f"w{i} "}}]}
                self.wfile.write(b"data: " + json.dumps(c).encode() + b"\n\n"); self.wfile.flush()
                time.sleep(a.delay)
            c = {"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}], "usage": {"prompt_tokens": 1, "completion_tokens": a.chunks, "total_tokens": 1}}
            self.wfile.write(b"data: " + json.dumps(c).encode() + b"\n\ndata: [DONE]\n\n"); self.wfile.flush()
        except Exception as e:
            ok = False
        with open(a.log, "a") as f: f.write(json.dumps({"t": time.time(), "end": "complete" if ok else "client-closed"}) + "\n")

ThreadingHTTPServer(("127.0.0.1", a.port), H).serve_forever()
