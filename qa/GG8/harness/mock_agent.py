"""GG8 area B: an Anthropic mock that drives the agent with tool calls. First turn: call the first
tool whose name mentions search/retrieve/knowledge (else `note`) about the Acme renewal; once a
tool_result is in the conversation: call `finish`. Requests without tools fall back to text."""
import json
import mocks

HOSTS = ("api.anthropic.com",)


def pick(tools):
    names = [t.get("name", "") for t in tools]
    for n in names:
        if any(k in n.lower() for k in ("search", "retriev", "knowledge", "context")):
            return n, {"query": "Acme renewal payment card", "q": "Acme renewal payment card"}
    return "note", {"text": "checking the Acme renewal"}


def respond(host, method, path, headers, body, server):
    req = json.loads(body) if body else {}
    tools = req.get("tools") or []
    if not path.startswith("/v1/messages") or not tools:
        return mocks.anthropic(method, path, headers, req)
    has_result = "tool_result" in json.dumps(req.get("messages", []))
    name, inp = ("finish", {"summary": "Done: the Acme renewal is due 14 November.",
                            "result": "Done", "answer": "Done"}) if has_result else pick(tools)
    if not req.get("stream"):
        return mocks.j(200, {"id": "msg_gg8", "type": "message", "role": "assistant",
                             "model": req.get("model"), "stop_reason": "tool_use",
                             "content": [{"type": "tool_use", "id": "toolu_gg8", "name": name, "input": inp}],
                             "usage": {"input_tokens": 100, "output_tokens": 10}})
    evs = [("message_start", {"type": "message_start", "message": {
        "id": "msg_gg8", "type": "message", "role": "assistant", "model": req.get("model"),
        "content": [], "stop_reason": None, "usage": {"input_tokens": 100, "output_tokens": 1}}}),
        ("content_block_start", {"type": "content_block_start", "index": 0, "content_block": {
            "type": "tool_use", "id": "toolu_gg8", "name": name, "input": {}}}),
        ("content_block_delta", {"type": "content_block_delta", "index": 0, "delta": {
            "type": "input_json_delta", "partial_json": json.dumps(inp)}}),
        ("content_block_stop", {"type": "content_block_stop", "index": 0}),
        ("message_delta", {"type": "message_delta", "delta": {"stop_reason": "tool_use"},
                           "usage": {"output_tokens": 10}}),
        ("message_stop", {"type": "message_stop"})]
    return mocks.sse(evs)
