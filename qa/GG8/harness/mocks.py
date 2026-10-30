"""Mock backends for the GG8 sink, chosen by the request's Host header.

`respond(host, method, path, headers, body, server)` returns `(status, headers, payload)`, where
`payload` is bytes or an iterator of chunks (server-sent events). The mocks are deliberately dumb:
GG8 checks what Nugget *sends*, so the answers only need to be well-formed enough for the daemon
to finish the exchange.
"""
import hashlib
import importlib
import json
import math
import os

REPLY = os.environ.get("GG8_MOCK_REPLY", "GG8-MOCK-REPLY: noted [1].")
EMBED_DIM = int(os.environ.get("GG8_EMBED_DIM", "768"))

# Optional extra mocks (connectors), each a module with `HOSTS` and `respond(...)`.
EXTRA = []
for name in os.environ.get("GG8_EXTRA_MOCKS", "").split(","):
    if name:
        EXTRA.append(importlib.import_module(name))


def j(status, obj, extra=None):
    h = {"Content-Type": "application/json"}
    h.update(extra or {})
    return status, h, json.dumps(obj).encode()


def sse(events):
    def gen():
        for ev in events:
            if isinstance(ev, tuple):
                name, data = ev
                yield f"event: {name}\ndata: {json.dumps(data)}\n\n"
            elif ev == "[DONE]":
                yield "data: [DONE]\n\n"
            else:
                yield f"data: {json.dumps(ev)}\n\n"
    return 200, {"Content-Type": "text/event-stream", "Cache-Control": "no-cache"}, gen()


def wants_json(req):
    if req.get("response_format") or req.get("output_format"):
        return True
    text = req.get("text")
    if isinstance(text, dict) and text.get("format"):
        return True
    return False


def reply_text(req):
    return "{}" if wants_json(req) else REPLY


def vec(text):
    h = hashlib.sha256(text.encode()).digest()
    v = [((h[i % 32] / 255.0) - 0.5) + math.sin(i) * 0.01 for i in range(EMBED_DIM)]
    n = math.sqrt(sum(x * x for x in v)) or 1.0
    return [x / n for x in v]


def anthropic(method, path, headers, req):
    if path.startswith("/v1/models"):
        return j(200, {"data": [{"id": "mock-claude", "type": "model"}], "has_more": False})
    if not path.startswith("/v1/messages"):
        return j(404, {"type": "error", "error": {"type": "not_found_error", "message": path}})
    if path.startswith("/v1/messages/count_tokens"):
        return j(200, {"input_tokens": 100})
    text = reply_text(req)
    model = req.get("model", "mock")
    if not req.get("stream"):
        return j(200, {"id": "msg_gg8", "type": "message", "role": "assistant", "model": model,
                       "content": [{"type": "text", "text": text}], "stop_reason": "end_turn",
                       "stop_sequence": None, "usage": {"input_tokens": 100, "output_tokens": 8}})
    words = text.split(" ")
    evs = [("message_start", {"type": "message_start", "message": {
        "id": "msg_gg8", "type": "message", "role": "assistant", "model": model, "content": [],
        "stop_reason": None, "stop_sequence": None,
        "usage": {"input_tokens": 100, "output_tokens": 1}}}),
        ("content_block_start", {"type": "content_block_start", "index": 0,
                                 "content_block": {"type": "text", "text": ""}})]
    for i, w in enumerate(words):
        evs.append(("content_block_delta", {"type": "content_block_delta", "index": 0, "delta": {
            "type": "text_delta", "text": (w if i == 0 else " " + w)}}))
    evs += [("content_block_stop", {"type": "content_block_stop", "index": 0}),
            ("message_delta", {"type": "message_delta",
                               "delta": {"stop_reason": "end_turn", "stop_sequence": None},
                               "usage": {"output_tokens": len(words)}}),
            ("message_stop", {"type": "message_stop"})]
    return sse(evs)


def openai_compat(method, path, headers, req):
    p = path.split("?")[0].rstrip("/")
    if p.endswith("/models"):
        return j(200, {"object": "list", "data": [{"id": "mock-model", "object": "model"}]})
    if p.endswith("/embeddings"):
        inp = req.get("input", [])
        if isinstance(inp, str):
            inp = [inp]
        return j(200, {"object": "list", "model": req.get("model", "mock"),
                       "data": [{"object": "embedding", "index": i, "embedding": vec(str(t))}
                                for i, t in enumerate(inp)],
                       "usage": {"prompt_tokens": 10, "total_tokens": 10}})
    if p.endswith("/chat/completions"):
        text = reply_text(req)
        model = req.get("model", "mock")
        if not req.get("stream"):
            return j(200, {"id": "cc-gg8", "object": "chat.completion", "model": model,
                           "choices": [{"index": 0, "finish_reason": "stop",
                                        "message": {"role": "assistant", "content": text}}],
                           "usage": {"prompt_tokens": 100, "completion_tokens": 8,
                                     "total_tokens": 108}})
        evs = [{"id": "cc-gg8", "object": "chat.completion.chunk", "model": model,
                "choices": [{"index": 0, "delta": {"role": "assistant", "content": ""},
                             "finish_reason": None}]}]
        for i, w in enumerate(text.split(" ")):
            evs.append({"id": "cc-gg8", "object": "chat.completion.chunk", "model": model,
                        "choices": [{"index": 0, "delta": {"content": w if i == 0 else " " + w},
                                     "finish_reason": None}]})
        evs.append({"id": "cc-gg8", "object": "chat.completion.chunk", "model": model,
                    "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]})
        evs.append({"id": "cc-gg8", "object": "chat.completion.chunk", "model": model,
                    "choices": [], "usage": {"prompt_tokens": 100, "completion_tokens": 8,
                                             "total_tokens": 108}})
        evs.append("[DONE]")
        return sse(evs)
    if p.endswith("/responses"):
        text = reply_text(req)
        resp = {"id": "resp_gg8", "object": "response", "status": "completed",
                "model": req.get("model", "mock"),
                "output": [{"type": "message", "role": "assistant", "id": "msg_gg8",
                            "content": [{"type": "output_text", "text": text,
                                         "annotations": []}]}],
                "usage": {"input_tokens": 100, "output_tokens": 8, "total_tokens": 108}}
        if not req.get("stream"):
            return j(200, resp)
        evs = [{"type": "response.created", "response": {**resp, "status": "in_progress",
                                                         "output": []}}]
        for i, w in enumerate(text.split(" ")):
            evs.append({"type": "response.output_text.delta", "item_id": "msg_gg8",
                        "output_index": 0, "content_index": 0,
                        "delta": w if i == 0 else " " + w})
        evs.append({"type": "response.completed", "response": resp})
        return sse(evs)
    return j(404, {"error": {"message": f"no route {path}"}})


def respond(host, method, path, headers, body, server):
    try:
        req = json.loads(body) if body else {}
    except ValueError:
        req = {}
    for m in EXTRA:
        if host in m.HOSTS:
            return m.respond(host, method, path, headers, body, server)
    if host == "api.anthropic.com":
        return anthropic(method, path, headers, req)
    if host in ("api.openai.com", "api.cloudllm.test") or host.startswith("192.168."):
        return openai_compat(method, path, headers, req)
    return j(404, {"gg8": "unknown host", "host": host, "path": path})
