#!/usr/bin/env python3
"""A tiny OpenAI-compatible chat server for GG4 QA runs without a local model.

    srv = FakeLLM(log_path="requests.jsonl").start()   # srv.base_url -> http://127.0.0.1:PORT/v1
    ...
    srv.stop()

`POST /v1/chat/completions` answers with a fixed, recognisable reply (streamed as SSE when the
request asks for `stream`), `GET /v1/models` lists one model, `POST /v1/embeddings` returns zero
vectors. Every request body is appended to `log_path` (JSON lines) so a scenario can check what
the daemon sent. The reply is chosen by `reply(messages)`; the default writes a short e-mail
signed by Maya that quotes nothing from the request (so a test can tell model text from
template text). Everything here is synthetic.
"""
from __future__ import annotations

import json
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

MARK = "FAKE-LLM-7731"


def default_reply(messages):
    return (f"Hi there,\n\nThanks for your patience. I am on it and will get back to you today. "
            f"[{MARK}]\n\nBest,\nMaya")


class FakeLLM:
    def __init__(self, log_path=None, reply=default_reply, model="fake-chat", port=0):
        self.log_path, self.reply, self.model = log_path, reply, model
        self.requests = []
        outer = self

        class H(BaseHTTPRequestHandler):
            def log_message(self, *a):
                pass

            def _json(self, code, obj):
                b = json.dumps(obj).encode()
                self.send_response(code)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(b)))
                self.end_headers()
                self.wfile.write(b)

            def do_GET(self):
                if self.path.rstrip("/").endswith("/models"):
                    return self._json(200, {"object": "list", "data": [{"id": outer.model, "object": "model"}]})
                self._json(404, {"error": {"message": "not found"}})

            def do_POST(self):
                n = int(self.headers.get("Content-Length") or 0)
                raw = self.rfile.read(n)
                try:
                    body = json.loads(raw or b"{}")
                except ValueError:
                    body = {"_raw": raw.decode("utf-8", "replace")}
                rec = {"t": time.time(), "path": self.path, "body": body}
                outer.requests.append(rec)
                if outer.log_path:
                    with open(outer.log_path, "a") as f:
                        f.write(json.dumps(rec) + "\n")
                if self.path.rstrip("/").endswith("/embeddings"):
                    inp = body.get("input")
                    k = len(inp) if isinstance(inp, list) else 1
                    return self._json(200, {"object": "list", "model": outer.model, "data": [
                        {"object": "embedding", "index": i, "embedding": [0.0] * 8} for i in range(k)]})
                if not self.path.rstrip("/").endswith("/chat/completions"):
                    return self._json(404, {"error": {"message": "not found"}})
                text = outer.reply(body.get("messages") or [])
                if body.get("stream"):
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Cache-Control", "no-cache")
                    self.end_headers()
                    cid = "chatcmpl-fake"
                    chunks = [text[i:i + 24] for i in range(0, len(text), 24)] or [""]
                    first = True
                    for c in chunks:
                        delta = {"content": c}
                        if first:
                            delta["role"] = "assistant"
                            first = False
                        ev = {"id": cid, "object": "chat.completion.chunk", "model": outer.model,
                              "choices": [{"index": 0, "delta": delta, "finish_reason": None}]}
                        self.wfile.write(b"data: " + json.dumps(ev).encode() + b"\n\n")
                    ev = {"id": cid, "object": "chat.completion.chunk", "model": outer.model,
                          "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}
                    self.wfile.write(b"data: " + json.dumps(ev).encode() + b"\n\n")
                    usage = {"id": cid, "object": "chat.completion.chunk", "model": outer.model, "choices": [],
                             "usage": {"prompt_tokens": 10, "completion_tokens": 10, "total_tokens": 20}}
                    self.wfile.write(b"data: " + json.dumps(usage).encode() + b"\n\n")
                    self.wfile.write(b"data: [DONE]\n\n")
                    self.wfile.flush()
                    return
                self._json(200, {"id": "chatcmpl-fake", "object": "chat.completion", "model": outer.model,
                                 "choices": [{"index": 0, "finish_reason": "stop",
                                              "message": {"role": "assistant", "content": text}}],
                                 "usage": {"prompt_tokens": 10, "completion_tokens": 10, "total_tokens": 20}})

        self.httpd = ThreadingHTTPServer(("127.0.0.1", port), H)
        self.port = self.httpd.server_address[1]
        self.base_url = f"http://127.0.0.1:{self.port}/v1"

    def start(self):
        threading.Thread(target=self.httpd.serve_forever, daemon=True).start()
        return self

    def stop(self):
        self.httpd.shutdown()
        self.httpd.server_close()

    def provider_toml(self, pid="fake", tiers=("triage", "light", "synthesis")):
        t = ", ".join(f'"{x}"' for x in tiers)
        return (f'[[providers]]\nid = "{pid}"\nkind = "openai_compatible"\nbase_url = "{self.base_url}"\n'
                f'model = "{self.model}"\ntiers = [{t}]\n')


if __name__ == "__main__":
    s = FakeLLM().start()
    print(s.base_url, flush=True)
    try:
        while True:
            time.sleep(3600)
    except KeyboardInterrupt:
        s.stop()
