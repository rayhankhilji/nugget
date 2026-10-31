#!/usr/bin/env python3
"""A capture stub for an OpenAI-compatible /v1/chat/completions endpoint (no real model).

It records every request body nuggetd sends (system prompt, history, question) as one JSON line
in CAPTURE, and answers with a canned streamed reply taken from env STUB_REPLY (default "ok [1]").
Used to show exactly what a model would receive; it says nothing about how a model behaves.

Usage: capture_stub.py PORT CAPTURE.jsonl
"""
import json, os, sys, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT, CAPTURE = int(sys.argv[1]), sys.argv[2]


class H(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def do_GET(self):
        body = json.dumps({"object": "list", "data": [{"id": "qwen3:4b", "object": "model"}]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(n)
        try:
            req = json.loads(raw)
        except Exception:
            req = {"raw": raw.decode("utf-8", "replace")}
        with open(CAPTURE, "a", encoding="utf-8") as f:
            f.write(json.dumps({"t": time.time(), "path": self.path, "body": req}, ensure_ascii=False) + "\n")
        reply = os.environ.get("STUB_REPLY", "ok [1]")
        try:
            reply = open(os.environ["STUB_REPLY_FILE"], encoding="utf-8").read()
        except Exception:
            pass
        if not req.get("stream"):
            body = json.dumps({"id": "x", "object": "chat.completion", "model": "qwen3:4b",
                               "choices": [{"index": 0, "message": {"role": "assistant", "content": reply},
                                            "finish_reason": "stop"}],
                               "usage": {"prompt_tokens": 1, "completion_tokens": 1}}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        chunks = [{"id": "x", "object": "chat.completion.chunk", "model": "qwen3:4b",
                   "choices": [{"index": 0, "delta": {"role": "assistant", "content": reply}, "finish_reason": None}]},
                  {"id": "x", "object": "chat.completion.chunk", "model": "qwen3:4b",
                   "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                   "usage": {"prompt_tokens": 1, "completion_tokens": 1}}]
        for c in chunks:
            self.wfile.write(f"data: {json.dumps(c)}\n\n".encode())
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()


ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
