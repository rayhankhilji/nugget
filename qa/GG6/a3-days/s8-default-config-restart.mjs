// Lead from the real-OS run: the first-run default config.toml has an enabled Ollama provider
// `local` (127.0.0.1:11434). With a local model installed (nuggetd-e2e + fake llama-server), does
// chat/ask route to the dead Ollama provider, before or after a restart?
// usage: node qa/GG6/a3-days/s8-default-config-restart.mjs
import fs from "node:fs";
import path from "node:path";
import { signedTestManifest, startModelServer, MODEL_ID } from "../../../apps/desktop/e2e/ai-setup.mjs";
import { generate } from "./gen-corpus.mjs";
import { SCRATCH, BIN, startAt, addFiles, waitIndexed, rpc, rpcStream, log, now, grepLogs, writeJson } from "./lib.mjs";

const root = path.join(SCRATCH, "s8");
fs.rmSync(root, { recursive: true, force: true });
fs.mkdirSync(root, { recursive: true });
const corpus = path.join(root, "corpus");
generate(corpus, 200);
const files = {};
const served = await startModelServer(files, 50 * 1024 * 1024);
const man = signedTestManifest(root, served.port);
Object.assign(files, man.files);
const env = { ...man.env, NUGGET_E2E_LLAMA_SERVER: BIN.fake, NUGGET_E2E_RAM_GB: "16", NUGGET_E2E_GPU: "unknown" };
const home = path.join(root, "home");
const R = { build: "debug" };
const chat = async (d, label) => {
  const t = now();
  try {
    const { result, notes } = await rpcStream(d.socket, "chat", { provider: null, tier: "synthesis", messages: [{ role: "user", text: "Say hello" }], items: [] }, (r, ns) => ns.some((n) => n.method === "chat.done"), 120_000);
    return { label, ms: now() - t, provider: result.provider, error: notes.find((n) => n.method === "chat.done").params.error };
  } catch (e) { return { label, ms: now() - t, rpc_error: e.message.slice(0, 300) }; }
};
const ask = async (d, label) => {
  const t = now();
  try {
    const { result, notes } = await rpcStream(d.socket, "retrieve.ask", { query: "Harbor Ledger vendor contract", budget_tokens: 2000 }, (r, ns) => ns.some((n) => n.method === "chat.done"), 120_000);
    return { label, ms: now() - t, provider: result.provider, error: notes.find((n) => n.method === "chat.done").params.error };
  } catch (e) { return { label, ms: now() - t, rpc_error: e.message.slice(0, 300) }; }
};
let d = await startAt({ home, bin: BIN.e2e, config: false, env });
R.default_config = fs.readFileSync(path.join(home, "config", "config.toml"), "utf8").split("\n").filter((l) => /^\[|^id|^base_url|^enabled|^idle/.test(l));
R.providers_first = (await rpc(d.socket, "providers.list")).map((p) => `${p.id}:${p.locality}:${p.tiers}:${p.error}`);
await addFiles(d.socket, [corpus]);
await waitIndexed(d.socket, { want: 200 });
R.chat_before_model = await chat(d, "before model");
const inst = await rpcStream(d.socket, "models.install", { id: MODEL_ID }, (r, ns) => ns.some((n) => n.method === "models.progress" && ["installed", "failed"].includes(n.params.phase)), 120_000);
R.install = inst.notes.filter((n) => n.method === "models.progress").at(-1).params.phase;
R.providers_after_install = (await rpc(d.socket, "providers.list")).map((p) => `${p.id}:${p.locality}:${p.tiers}:${p.error}`);
R.chat_after_install = await chat(d, "after install");
R.ask_after_install = await ask(d, "ask after install");
await d.stop();
d = await startAt({ home, bin: BIN.e2e, env });
R.providers_after_restart = (await rpc(d.socket, "providers.list")).map((p) => `${p.id}:${p.locality}:${p.tiers}:${p.error}`);
R.runtime_after_restart = await rpc(d.socket, "runtime.status");
R.chat_after_restart = await chat(d, "after restart");
R.chat_after_restart_2 = await chat(d, "after restart (2nd)");
R.ask_after_restart = await ask(d, "ask after restart");
R.config_after = fs.readFileSync(path.join(home, "config", "config.toml"), "utf8").split("\n").filter((l) => /^\[|^id|^base_url|^chat_model|^triage|^synthesis/.test(l));
R.logs = grepLogs(d.logDir, /trying next|routed|11434/).map((l) => l.slice(0, 300)).slice(-10);
await d.stop();
await served.close();
writeJson("s8-default-config-restart.json", R);
console.log(JSON.stringify(R, null, 1).slice(0, 5000));
