#!/usr/bin/env python3
"""A fake OpenAI-compatible chat server that records every request nuggetd sends it.

Used to see exactly what the answering model receives for `retrieve.ask` (system prompt,
messages) without running a real model. Every POST body is appended to --log as one JSON line.
Streaming requests get a short SSE reply; non-streaming ones a minimal chat.completion.

Usage: capture_llm.py --port 18731 --log requests.jsonl
"""

import argparse
import json
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

REPLY = "CAPTURED"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--log", required=True)
    a = ap.parse_args()

    class H(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def _json(self, code, obj):
            b = json.dumps(obj).encode()
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(b)))
            self.end_headers()
            self.wfile.write(b)

        def do_GET(self):
            # /v1/models for probes
            self._json(200, {"object": "list", "data": [{"id": "capture", "object": "model"}]})

        def do_POST(self):
            n = int(self.headers.get("Content-Length") or 0)
            raw = self.rfile.read(n)
            try:
                body = json.loads(raw)
            except Exception:
                body = {"_raw": raw.decode("utf-8", "replace")}
            with open(a.log, "a", encoding="utf-8") as f:
                f.write(json.dumps({"t": time.time(), "path": self.path, "body": body}) + "\n")
            if isinstance(body, dict) and body.get("stream"):
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                for chunk in [
                    {"choices": [{"index": 0, "delta": {"role": "assistant", "content": REPLY}}]},
                    {"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                     "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}},
                ]:
                    self.wfile.write(b"data: " + json.dumps(chunk).encode() + b"\n\n")
                    self.wfile.flush()
                self.wfile.write(b"data: [DONE]\n\n")
                self.wfile.flush()
            else:
                self._json(200, {"id": "x", "object": "chat.completion", "model": "capture",
                                 "choices": [{"index": 0, "message": {"role": "assistant", "content": REPLY},
                                              "finish_reason": "stop"}],
                                 "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}})

    ThreadingHTTPServer(("127.0.0.1", a.port), H).serve_forever()


if __name__ == "__main__":
    main()
