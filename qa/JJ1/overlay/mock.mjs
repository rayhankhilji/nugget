// JJ1 overlay QA: a programmable fake OpenAI-compatible provider (same wire shape as
// apps/desktop/e2e-native/mock-provider.mjs, which streams one fixed draft). Here the driver sets
// the next draft (`p.next = {text, chunks, delayMs}` or a function of the request body) and every
// request's full body is kept, so a refinement's instruction and context can be checked.
import http from "node:http";

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** Split `text` into `n` roughly equal chunks on word boundaries. */
export function chunksOf(text, n = 6) {
  const words = text.split(/(?<= )/);
  const per = Math.max(1, Math.ceil(words.length / n));
  const out = [];
  for (let i = 0; i < words.length; i += per) out.push(words.slice(i, i + per).join(""));
  return out;
}

export function startMock({ log = () => {} } = {}) {
  const p = {
    requests: [],
    /** `{text, chunks?, delayMs?}` or `(body) => {text, ...}`. */
    next: { text: "Default mock draft." },
  };
  const server = http.createServer((req, res) => {
    let body = "";
    req.setEncoding("utf8");
    req.on("data", (c) => (body += c));
    req.on("end", async () => {
      let json = {};
      try {
        json = body ? JSON.parse(body) : {};
      } catch {}
      const entry = { at: Date.now(), path: req.url, stream: json.stream === true, messages: json.messages ?? [], closed: false, sent: 0 };
      p.requests.push(entry);
      log(`mock: ${req.method} ${req.url} stream=${entry.stream} msgs=${entry.messages.length}`);
      if (req.method === "GET" && req.url.endsWith("/models")) {
        res.writeHead(200, { "content-type": "application/json" });
        return res.end(JSON.stringify({ object: "list", data: [{ id: "fake-draft", object: "model" }] }));
      }
      if (req.method !== "POST" || !req.url.endsWith("/chat/completions")) {
        res.writeHead(404);
        return res.end("{}");
      }
      const id = `chatcmpl-qa-${p.requests.length}`;
      if (!entry.stream) {
        res.writeHead(200, { "content-type": "application/json" });
        return res.end(JSON.stringify({ id, object: "chat.completion", model: "fake-draft", choices: [{ index: 0, message: { role: "assistant", content: "" }, finish_reason: "length" }], usage: { prompt_tokens: 1, completion_tokens: 0, total_tokens: 1 } }));
      }
      const plan = typeof p.next === "function" ? p.next(json, entry) : p.next;
      entry.reply = plan.text;
      const chunks = plan.chunkList ?? chunksOf(plan.text, plan.chunks ?? 6);
      res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache", connection: "keep-alive" });
      const send = (o) => res.write(`data: ${JSON.stringify(o)}\n\n`);
      const ch = (delta, finish = null) => ({ id, object: "chat.completion.chunk", model: "fake-draft", choices: [{ index: 0, delta, finish_reason: finish }] });
      res.on("close", () => (entry.closed = true));
      send(ch({ role: "assistant", content: "" }));
      for (const c of chunks) {
        await sleep(plan.delayMs ?? 250);
        if (entry.closed) return;
        send(ch({ content: c }));
        entry.sent++;
      }
      send(ch({}, "stop"));
      send({ id, object: "chat.completion.chunk", model: "fake-draft", choices: [], usage: { prompt_tokens: 10, completion_tokens: 12, total_tokens: 22 } });
      res.write("data: [DONE]\n\n");
      res.end();
      entry.finished = true;
    });
  });
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      p.baseUrl = `http://127.0.0.1:${server.address().port}/v1`;
      p.streams = () => p.requests.filter((r) => r.stream);
      p.close = () => new Promise((r) => { server.close(() => r()); server.closeAllConnections(); });
      resolve(p);
    });
  });
}
