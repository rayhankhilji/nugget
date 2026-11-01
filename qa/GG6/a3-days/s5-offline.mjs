// Scenario 5: no network. The daemon runs in an empty network namespace (`unshare -rn`), so
// only loopback-less isolation; the Unix socket still works. Exercises every network feature.
// usage: node qa/GG6/a3-days/s5-offline.mjs
import fs from "node:fs";
import path from "node:path";
import { generate } from "./gen-corpus.mjs";
import { SCRATCH, startAt, addFiles, waitIndexed, search, rpc, rpcRaw, rpcStream, log, sleep, now, grepLogs, writeJson } from "./lib.mjs";

const root = path.join(SCRATCH, "s5");
fs.rmSync(root, { recursive: true, force: true });
const corpus = path.join(root, "corpus");
generate(corpus, 300);
const CONFIG = `[runtime]
enabled = true

[mcp]
http = false

[[providers]]
id = "anthropic"
kind = "anthropic"
model = "claude-opus-5-5"
api_key = "env:QA_FAKE_KEY"
tiers = ["synthesis", "agentic"]
`;
const R = { build: "debug" };
const d = await startAt({ home: path.join(root, "home"), config: CONFIG, wrap: ["unshare", "-rn"], env: { QA_FAKE_KEY: "sk-ant-qa-not-a-real-key" } });
R.ready_ms = d.readyMs;
await addFiles(d.socket, [corpus]);
R.index = await waitIndexed(d.socket, { want: 300 });
R.search = { ms: (await search(d.socket, "Harbor Ledger")).ms, hits: (await search(d.socket, "Harbor Ledger")).hits.length };
const timed = async (label, fn) => { const t = now(); try { const r = await fn(); R[label] = { ms: now() - t, r: JSON.stringify(r).slice(0, 600) }; } catch (e) { R[label] = { ms: now() - t, error: e.message.slice(0, 600) }; } log(label, JSON.stringify(R[label])); };
await timed("status", () => rpc(d.socket, "status"));
await timed("providers_list", () => rpc(d.socket, "providers.list"));
await timed("probe_cloud", () => rpcRaw(d.socket, "providers.probe", { provider: "anthropic" }, 120_000));
await timed("ask", async () => {
  const { result, notes } = await rpcStream(d.socket, "retrieve.ask", { query: "Harbor Ledger vendor contract", budget_tokens: 2000 }, (r, ns) => ns.some((n) => n.method === "chat.done"), 180_000);
  return { provider: result.provider, citations: result.citations?.length, done: notes.find((n) => n.method === "chat.done").params };
});
await timed("chat", async () => {
  const { result, notes } = await rpcStream(d.socket, "chat", { provider: null, tier: "synthesis", messages: [{ role: "user", text: "hi" }], items: [] }, (r, ns) => ns.some((n) => n.method === "chat.done"), 180_000);
  return { provider: result.provider, done: notes.find((n) => n.method === "chat.done").params };
});
await timed("models_list", async () => (await rpc(d.socket, "models.list")).map((m) => [m.id, m.recommended, m.installed]).slice(0, 6));
const models = await rpc(d.socket, "models.list").catch(() => []);
const rec = models.find((m) => m.recommended) || models[0];
if (rec) await timed("models_install", async () => {
  const { notes } = await rpcStream(d.socket, "models.install", { id: rec.id }, (r, ns) => ns.some((n) => n.method === "models.progress" && ["installed", "failed"].includes(n.params.phase)), 180_000);
  return notes.filter((n) => n.method === "models.progress").slice(-1)[0].params;
});
await timed("runtime_status", () => rpc(d.socket, "runtime.status"));
await timed("imap_add", () => rpc(d.socket, "sources.add", { id: null, kind: "imap", name: "Mail", settings: { host: "imap.example.com", username: "qa@example.com" } }));
await timed("imap_secret", () => rpc(d.socket, "sources.secret.set", { id: "imap-1", secret: "app-password" }));
await timed("caldav_add", () => rpc(d.socket, "sources.add", { id: null, kind: "caldav", name: "Cal", settings: { url: "https://dav.example.com/cal/", username: "qa" } }));
await timed("caldav_secret", () => rpc(d.socket, "sources.secret.set", { id: "caldav-1", secret: "pw" }));
await sleep(20_000);
await timed("sources_after_20s", async () => (await rpc(d.socket, "sources.list")).map((s) => ({ id: s.id, state: s.state, last_error: s.last_error })));
await timed("gmail_auth", async () => { await rpc(d.socket, "sources.add", { id: null, kind: "gmail", name: "G", settings: { client_id: "x.apps.googleusercontent.com" } }); return rpcRaw(d.socket, "sources.auth.begin", { id: "gmail-1" }); });
await timed("egress", () => rpc(d.socket, "egress.recent", { limit: 20 }));
await timed("proactive_refresh", () => rpc(d.socket, "proactive.refresh", {}, 60_000));
R.log_warn = grepLogs(d.logDir, /"level":"(ERROR|WARN)"/).filter((l) => !/keychain|KEYCHAIN/.test(l)).map((l) => l.slice(0, 350)).slice(0, 20);
await d.stop();
writeJson("s5-offline.json", R);
