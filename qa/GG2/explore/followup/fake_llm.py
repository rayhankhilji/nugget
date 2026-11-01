#!/usr/bin/env python3
"""Fake OpenAI-compatible chat server for GG2 follow-up exploration.

Every request body is appended to <log> as JSON lines. Behaviour is switched at runtime with
POST /_admin {"rewrite": "map"|"echo"|"error"|"slow"|"empty"|"think", "delay": seconds,
              "answer": "text", "map": {"followup": "rewrite"}}.

- A request whose system prompt starts "You turn a follow-up question" is a query-rewrite
  request: answered per `rewrite` mode (map: look up the follow-up text in `map`, else echo it).
- Anything else is an answer request: streams `answer` (SSE) after `answer_delay` seconds.
"""
import json
import re
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

STATE = {"rewrite": "map", "delay": 0.0, "answer": "Canned answer.", "answer_delay": 0.0, "map": {}}
LOG = sys.argv[2] if len(sys.argv) > 2 else "fake_llm.log.jsonl"
LOCK = threading.Lock()


def log(rec):
    with LOCK, open(LOG, "a") as f:
        f.write(json.dumps(rec) + "\n")


def followup_of(user_text):
    m = re.search(r"Follow-up:\n<<<\n(.*?)\n>>>", user_text, re.S)
    return m.group(1) if m else ""


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def _json(self, code, obj):
        b = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)

    def do_GET(self):
        if self.path.endswith("/models"):
            return self._json(200, {"object": "list", "data": [{"id": "fake", "object": "model"}]})
        if self.path == "/_state":
            return self._json(200, STATE)
        self._json(404, {})

    def do_POST(self):
        n = int(self.headers.get("content-length") or 0)
        body = json.loads(self.rfile.read(n) or b"{}")
        if self.path == "/_admin":
            STATE.update(body)
            return self._json(200, STATE)
        t = time.time()
        msgs = body.get("messages", [])
        system = next((m.get("content") for m in msgs if m.get("role") == "system"), "") or ""
        if isinstance(system, list):
            system = " ".join(p.get("text", "") for p in system)
        is_rw = system.startswith("You turn a follow-up question")
        log({"t": t, "path": self.path, "kind": "rewrite" if is_rw else "answer", "body": body})
        if is_rw:
            mode = STATE["rewrite"]
            if mode == "error":
                return self._json(500, {"error": {"message": "fake failure"}})
            if mode == "slow":
                time.sleep(float(STATE["delay"]))
            user = msgs[-1].get("content") if msgs else ""
            if isinstance(user, list):
                user = " ".join(p.get("text", "") for p in user)
            fu = followup_of(user)
            text = "" if mode == "empty" else STATE["map"].get(fu, fu)
            if mode == "think":
                # A reasoning model whose server leaks the reasoning into `content`.
                text = "<think>\nThe user is asking a follow-up; resolve the pronoun.\n</think>\n\n" + text
        else:
            time.sleep(float(STATE["answer_delay"]))
            text = STATE["answer"]
        if body.get("stream"):
            self.send_response(200)
            self.send_header("content-type", "text/event-stream")
            self.send_header("cache-control", "no-cache")
            self.send_header("connection", "close")
            self.end_headers()
            def ev(o):
                self.wfile.write(b"data: " + json.dumps(o).encode() + b"\n\n")
                self.wfile.flush()
            base = {"id": "x", "object": "chat.completion.chunk", "created": int(t), "model": "fake"}
            ev({**base, "choices": [{"index": 0, "delta": {"role": "assistant", "content": ""}}]})
            for w in re.findall(r"\S+\s*", text) or [""]:
                ev({**base, "choices": [{"index": 0, "delta": {"content": w}}]})
            ev({**base, "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]})
            ev({**base, "choices": [], "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}})
            self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()
            self.close_connection = True
        else:
            self._json(200, {"id": "x", "object": "chat.completion", "created": int(t), "model": "fake",
                             "choices": [{"index": 0, "message": {"role": "assistant", "content": text}, "finish_reason": "stop"}],
                             "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}})


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
