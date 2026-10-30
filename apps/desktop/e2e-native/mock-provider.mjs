// A fake OpenAI-compatible provider on 127.0.0.1 for the native overlay run: `POST
// /v1/chat/completions` streams a fixed draft as server-sent events, a few words at a time with a
// pause between chunks, so the overlay is seen streaming. Non-streaming requests (the daemon's
// prewarm) get a one-token completion. Every request is recorded. No network beyond loopback.
import http from "node:http";

/** The draft the provider streams, split into the chunks it sends. */
export const DRAFT_CHUNKS = ["Sounds good, ", "noon works ", "for me. ", "I will bring ", "the slides ", "and the notes."];
export const DRAFT_TEXT = DRAFT_CHUNKS.join("");

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/**
 * Start the server. `chunkDelayMs` is the pause before each chunk. Resolves to
 * `{ baseUrl, requests, close }`; `requests` lists `{ at, path, stream, messages }`.
 */
export function startMockProvider({ chunkDelayMs = 250, log = () => {} } = {}) {
  const requests = [];
  const server = http.createServer((req, res) => {
    let body = "";
    req.setEncoding("utf8");
    req.on("data", (c) => (body += c));
    req.on("end", async () => {
      let json = {};
      try {
        json = body ? JSON.parse(body) : {};
      } catch {
        // Recorded as an empty body.
      }
      const entry = { at: Date.now(), method: req.method, path: req.url, stream: json.stream === true, messages: json.messages?.length ?? 0 };
      requests.push(entry);
      log(`mock provider: ${req.method} ${req.url} stream=${entry.stream} messages=${entry.messages}`);
      if (req.method === "GET" && req.url.endsWith("/models")) {
        res.writeHead(200, { "content-type": "application/json" });
        res.end(JSON.stringify({ object: "list", data: [{ id: "fake-draft", object: "model" }] }));
        return;
      }
      if (req.method !== "POST" || !req.url.endsWith("/chat/completions")) {
        res.writeHead(404, { "content-type": "application/json" });
        res.end(JSON.stringify({ error: { message: `no route ${req.url}` } }));
        return;
      }
      const id = `chatcmpl-fake-${requests.length}`;
      if (!entry.stream) {
        res.writeHead(200, { "content-type": "application/json" });
        res.end(
          JSON.stringify({
            id,
            object: "chat.completion",
            model: "fake-draft",
            choices: [{ index: 0, message: { role: "assistant", content: "" }, finish_reason: "length" }],
            usage: { prompt_tokens: 1, completion_tokens: 0, total_tokens: 1 },
          }),
        );
        return;
      }
      res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache", connection: "keep-alive" });
      const send = (obj) => res.write(`data: ${JSON.stringify(obj)}\n\n`);
      const chunk = (delta, finish = null) => ({
        id,
        object: "chat.completion.chunk",
        model: "fake-draft",
        choices: [{ index: 0, delta, finish_reason: finish }],
      });
      let closed = false;
      res.on("close", () => (closed = true));
      send(chunk({ role: "assistant", content: "" }));
      for (const text of DRAFT_CHUNKS) {
        await sleep(chunkDelayMs);
        if (closed) return;
        send(chunk({ content: text }));
      }
      send(chunk({}, "stop"));
      send({ id, object: "chat.completion.chunk", model: "fake-draft", choices: [], usage: { prompt_tokens: 10, completion_tokens: 12, total_tokens: 22 } });
      res.write("data: [DONE]\n\n");
      res.end();
    });
  });
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      resolve({
        baseUrl: `http://127.0.0.1:${port}/v1`,
        requests,
        close: () => new Promise((r) => {
          server.close(() => r());
          server.closeAllConnections();
        }),
      });
    });
  });
}
