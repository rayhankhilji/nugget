// KSTATUS repro: nuggetd + slow OpenAI-compatible mock serving triage/light/synthesis; poll
// ingest.status.triage and knowledge.status every second.
import http from "node:http";
import fs from "node:fs";
import path from "node:path";
import { startDaemon, rpc } from "/home/user/nugget/apps/desktop/e2e/daemon.mjs";
const OUT = process.env.OUT || "/tmp/claude-0/mm1v2/ks/out";
fs.mkdirSync(OUT, { recursive: true });
const TRIAGE_DELAY = Number(process.env.TRIAGE_DELAY_MS || 4000);
const N = Number(process.env.N || 30);
const log = (...a) => { const s = a.join(" "); console.log(s); fs.appendFileSync(path.join(OUT, "poll.log"), s + "\n"); };
const reqs = [];
function inst(s, i) {
  if (!s) return null;
  if (s.enum) return s.enum.includes("work") ? "work" : s.enum[0];
  const t = Array.isArray(s.type) ? s.type[0] : s.type;
  if (t === "object") { const o = {}; for (const k of s.required ?? Object.keys(s.properties ?? {})) o[k] = k === "n" ? i : inst(s.properties?.[k], i); return o; }
  if (t === "array") return Array.from({ length: s.minItems ?? 0 }, (_, j) => inst(s.items, j + 1));
  if (t === "integer" || t === "number") return 7;
  if (t === "boolean") return false;
  if (t === "string") return "apples";
  return null;
}
const server = http.createServer((req, res) => {
  let body = ""; req.on("data", (c) => (body += c)); req.on("end", async () => {
    if (req.method === "GET") return res.writeHead(200, { "Content-Type": "application/json" }).end(JSON.stringify({ object: "list", data: [{ id: "mock", object: "model" }] }));
    if (!req.url.includes("chat/completions")) return res.writeHead(404).end("{}");
    const p = JSON.parse(body);
    const schema = p.response_format?.json_schema?.schema;
    const user = JSON.stringify(p.messages ?? []);
    const isTriage = /label documents/.test(user) || /<document n=/.test(user);
    let text = schema ? JSON.stringify(inst(schema, 1)) : "pong";
    if (schema?.properties?.answer) text = JSON.stringify({ answer: 7, unit: "apples" });
    reqs.push({ t: Date.now(), triage: isTriage, schema: !!schema });
    if (isTriage) await new Promise((r) => setTimeout(r, TRIAGE_DELAY));
    if (!p.stream) return res.writeHead(200, { "Content-Type": "application/json" }).end(JSON.stringify({ id: "x", object: "chat.completion", choices: [{ index: 0, message: { role: "assistant", content: text }, finish_reason: "stop" }], usage: { prompt_tokens: 1, completion_tokens: 1 } }));
    res.writeHead(200, { "Content-Type": "text/event-stream" });
    res.write(`data: ${JSON.stringify({ id: "x", object: "chat.completion.chunk", choices: [{ index: 0, delta: { role: "assistant", content: text }, finish_reason: null }] })}\n\n`);
    res.write(`data: ${JSON.stringify({ id: "x", object: "chat.completion.chunk", choices: [{ index: 0, delta: {}, finish_reason: "stop" }], usage: { prompt_tokens: 1, completion_tokens: 1 } })}\n\n`);
    res.end("data: [DONE]\n\n");
  });
});
await new Promise((r) => server.listen(0, "127.0.0.1", r));
const port = server.address().port;
const notes = path.join(OUT, "notes"); fs.rmSync(notes, { recursive: true, force: true }); fs.mkdirSync(notes, { recursive: true });
for (let i = 0; i < N; i++) fs.writeFileSync(path.join(notes, `note-${i}.md`), `# Meeting ${i} with Ada Lovelace\n\nAda Lovelace from Glimmerfen Labs met Robin on 2026-10-${String(1 + (i % 28)).padStart(2, "0")} about project Kestrel ${i}. Next step: send the draft budget to Ada by Friday.\n`);
const config = `[runtime]\nenabled = false\n\n[mcp]\nhttp = false\n\n[[providers]]\nid = "qa-mock"\nkind = "openai_compatible"\nbase_url = "http://127.0.0.1:${port}/v1"\nmodel = "mock"\ntiers = ["triage", "light", "synthesis"]\nmax_retries = 0\n`;
const home = fs.mkdtempSync(path.join(OUT, "home-"));
const d = await startDaemon(process.env.NUGGETD || "/home/user/nugget/target/debug/nuggetd", { config, home, stderrPath: path.join(OUT, "nuggetd.out.log"), env: { TMPDIR: "/tmp" } });
const t0 = Date.now();
try {
  const pr = await rpc(d.socket, "providers.probe", { provider: "qa-mock" }, 60000).catch((e) => ({ error: e.message }));
  log("probe", JSON.stringify(pr).slice(0, 300));
  log("add", JSON.stringify(await rpc(d.socket, "sources.add", { id: null, kind: "files", name: "notes", settings: { roots: [notes], include: [], exclude: [] } })).slice(0, 200));
  let firstFalseSettle = null;
  for (let s = 0; s < Number(process.env.SECS || 150); s++) {
    const ing = await rpc(d.socket, "ingest.status", {}).catch((e) => ({ error: e.message }));
    const kn = await rpc(d.socket, "knowledge.status", {}).catch((e) => ({ error: e.message }));
    const tr = ing.triage ?? ing.error;
    const items = (await rpc(d.socket, "sources.list", {}).catch(() => [])).map((x) => x.items).join(",");
    const line = `t=${((Date.now() - t0) / 1000).toFixed(1)}s items=${items} triage=${JSON.stringify(tr)} | knowledge pending_items=${kn.pending_items} running=${kn.running} entities=${kn.entities} claims=${kn.claims} | mock triage reqs=${reqs.filter((r) => r.triage).length}`;
    log(line);
    if (firstFalseSettle == null && kn.pending_items === 0 && kn.running === false && (tr?.pending ?? 0) > 0) firstFalseSettle = line;
    await new Promise((r) => setTimeout(r, 1000));
  }
  log("FIRST_FALSE_SETTLE", firstFalseSettle);
} finally { await d.stop({ keep: true }); server.close(); }
