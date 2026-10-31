// A scripted OpenAI-compatible chat-completions server (the smallest realistic stand-in for a
// BYOK / LAN model that calls tools). The repo's fake llama-server cannot emit tool calls (it only
// answers "Hello from fake" or a cited quote), so the agent can never reach a confirmation with it.
//
// Script: an array of turns; the turn used for a request is the number of assistant messages
// already in it (turn 0 = first model call of a run). A turn is
//   { calls: [{ name, args }], delayMs?, text? }   -> tool call(s)
//   { text, delayMs? }                             -> plain text
// Past the end of the script, the last turn's fallback is `finish` (outcome cannot).
// `stream: true` requests get SSE (headers first, then the delay, then the chunks), others JSON.
import http from "node:http";
import { now } from "./ipc.mjs";

export function startMockModel() {
  const state = { script: [], requests: [], seq: 0 };
  const server = http.createServer((req, res) => {
    const url = new URL(req.url, "http://127.0.0.1");
    let body = "";
    req.on("data", (c) => (body += c));
    req.on("end", async () => {
      const json = (status, v) => res.writeHead(status, { "Content-Type": "application/json" }).end(JSON.stringify(v));
      if (req.method === "GET" && url.pathname === "/v1/models") {
        return json(200, { object: "list", data: [{ id: "mock-agent", object: "model", owned_by: "qa" }] });
      }
      if (req.method !== "POST" || url.pathname !== "/v1/chat/completions") {
        return json(404, { error: { message: "not found" } });
      }
      let parsed;
      try {
        parsed = JSON.parse(body);
      } catch {
        return json(400, { error: { message: "bad json" } });
      }
      const turn = (parsed.messages ?? []).filter((m) => m.role === "assistant").length;
      const tools = (parsed.tools ?? []).map((t) => t.function?.name);
      const rec = { t: now(), turn, tools, stream: !!parsed.stream, aborted: false, answeredAt: null };
      state.requests.push(rec);
      req.on("close", () => {
        if (!res.writableEnded) rec.aborted = true;
      });
      res.on("close", () => {
        if (!res.writableEnded) rec.aborted = true;
      });
      const step = state.script[turn] ?? { calls: [{ name: "finish", args: { outcome: "cannot", summary: "script ended" } }] };
      rec.step = step;
      const id = `chatcmpl-qa-${++state.seq}`;
      const calls = (step.calls ?? []).map((c, i) => ({
        index: i,
        id: `call_${state.seq}_${i}`,
        type: "function",
        function: { name: c.name, arguments: JSON.stringify(c.args ?? {}) },
      }));
      const finish = calls.length ? "tool_calls" : "stop";
      const wait = (ms) => new Promise((r) => setTimeout(r, ms));
      if (!parsed.stream) {
        if (step.delayMs) await wait(step.delayMs);
        rec.answeredAt = now();
        return json(200, {
          id,
          object: "chat.completion",
          model: "mock-agent",
          choices: [{ index: 0, finish_reason: finish, message: { role: "assistant", content: step.text ?? null, tool_calls: calls.length ? calls : undefined } }],
          usage: { prompt_tokens: 100, completion_tokens: 20, total_tokens: 120 },
        });
      }
      res.writeHead(200, { "Content-Type": "text/event-stream", "Cache-Control": "no-cache" });
      const send = (obj) => res.write(`data: ${JSON.stringify(obj)}\n\n`);
      const chunk = (delta, finish_reason = null) => ({ id, object: "chat.completion.chunk", model: "mock-agent", choices: [{ index: 0, delta, finish_reason }] });
      send(chunk({ role: "assistant" }));
      if (step.delayMs) await wait(step.delayMs);
      if (res.destroyed) return;
      if (step.text) send(chunk({ content: step.text }));
      for (const c of calls) send(chunk({ tool_calls: [c] }));
      send(chunk({}, finish));
      send({ id, object: "chat.completion.chunk", model: "mock-agent", choices: [], usage: { prompt_tokens: 100, completion_tokens: 20, total_tokens: 120 } });
      res.write("data: [DONE]\n\n");
      rec.answeredAt = now();
      res.end();
    });
  });
  return new Promise((resolve) =>
    server.listen(0, "127.0.0.1", () =>
      resolve({
        port: server.address().port,
        state,
        setScript(script) {
          state.script = script;
          state.requests = [];
        },
        close: () => new Promise((r) => (server.closeAllConnections?.(), server.close(() => r()))),
      }),
    ),
  );
}
