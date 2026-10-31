"""GG8 area B: Anthropic and OpenAI-compatible mocks that honour structured output (answer with a
minimal instance of the requested JSON schema) and the probe's forced tool call, so that providers
probe as json_schema- and tools-capable and features that need them (knowledge, the Today judge)
actually send their requests."""
import json
import re
import mocks

HOSTS = ("api.anthropic.com", "api.cloudllm.test")


def instance(s):
    if not isinstance(s, dict):
        return None
    if "enum" in s:
        return s["enum"][0]
    if "const" in s:
        return s["const"]
    for k in ("anyOf", "oneOf"):
        if k in s:
            return instance(s[k][0])
    t = s.get("type")
    if isinstance(t, list):
        t = [x for x in t if x != "null"][0] if [x for x in t if x != "null"] else "null"
    if t == "object" or "properties" in s:
        return {k: instance(v) for k, v in (s.get("properties") or {}).items()}
    if t == "array":
        return []
    if t == "integer":
        return 7
    if t == "number":
        return 0.5
    if t == "boolean":
        return False
    if t == "string":
        return "apples"
    return None


def anth_schema(req):
    oc = req.get("output_config") or {}
    f = oc.get("format") or req.get("output_format") or {}
    return f.get("schema")


def oai_schema(req):
    rf = req.get("response_format") or {}
    js = rf.get("json_schema") or {}
    return js.get("schema")


def text_reply(req, text, anth):
    req = dict(req)
    req.pop("output_config", None)
    req.pop("response_format", None)
    req.pop("output_format", None)
    old = mocks.REPLY
    mocks.REPLY = text
    try:
        return mocks.anthropic("POST", "/v1/messages", {}, req) if anth else \
            mocks.openai_compat("POST", "/v1/chat/completions", {}, req)
    finally:
        mocks.REPLY = old


def respond(host, method, path, headers, body, server):
    req = json.loads(body) if body else {}
    anth = host == "api.anthropic.com"
    if anth:
        sch = anth_schema(req)
        tools = req.get("tools") or []
        if sch is not None:
            return text_reply(req, json.dumps(instance(sch)), True)
        if tools and tools[0].get("name") == "record_value":
            m = re.search(r'exactly \\?"([^"\\]+)', json.dumps(req.get("messages")))
            val = m.group(1) if m else "x"
            evs = [("message_start", {"type": "message_start", "message": {"id": "m", "type": "message", "role": "assistant", "model": "m", "content": [], "stop_reason": None, "usage": {"input_tokens": 1, "output_tokens": 1}}}),
                   ("content_block_start", {"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "t1", "name": "record_value", "input": {}}}),
                   ("content_block_delta", {"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": json.dumps({"value": val})}}),
                   ("content_block_stop", {"type": "content_block_stop", "index": 0}),
                   ("message_delta", {"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 5}}),
                   ("message_stop", {"type": "message_stop"})]
            return mocks.sse(evs)
        if "pong" in json.dumps(req.get("messages")):
            return text_reply(req, "pong", True)
        return mocks.anthropic(method, path, headers, req)
    sch = oai_schema(req)
    if sch is not None and path.endswith("/chat/completions"):
        return text_reply(req, json.dumps(instance(sch)), False)
    if "pong" in json.dumps(req.get("messages")) and path.endswith("/chat/completions"):
        return text_reply(req, "pong", False)
    return mocks.openai_compat(method, path, headers, req)
