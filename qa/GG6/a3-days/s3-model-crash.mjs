// Scenario 3: the local model server (fake llama-server) crashes: kill -9 while idle, kill -9
// mid-answer, then a crash loop (the binary exits 1). Uses nuggetd-e2e + a signed test manifest +
// a local mock download server, like apps/desktop/e2e/ai-setup.mjs.
// usage: node qa/GG6/a3-days/s3-model-crash.mjs
import fs from "node:fs";
import path from "node:path";
import { signedTestManifest, startModelServer, MODEL_ID } from "../../../apps/desktop/e2e/ai-setup.mjs";
import { generate } from "./gen-corpus.mjs";
import { SCRATCH, BIN, startAt, addFiles, waitIndexed, rpc, rpcStream, children, log, sleep, now, grepLogs, writeJson } from "./lib.mjs";

const root = path.join(SCRATCH, "s3");
fs.rmSync(root, { recursive: true, force: true });
fs.mkdirSync(root, { recursive: true });
const corpus = path.join(root, "corpus");
generate(corpus, 300);
const files = {};
const served = await startModelServer(files, 50 * 1024 * 1024);
const man = signedTestManifest(root, served.port);
Object.assign(files, man.files);
// Wrapper: logs every launch; exits 1 while the flag file exists (crash loop).
const flag = path.join(root, "crashloop.flag");
const launches = path.join(root, "launches.log");
const wrapper = path.join(root, "llama-server-wrapper.sh");
fs.writeFileSync(wrapper, `#!/bin/bash\necho "$(date +%s.%N) $$ $*" >> ${launches}\nif [ -e ${flag} ]; then echo "simulated crash" >&2; exit 1; fi\nexec ${BIN.fake} "$@"\n`, { mode: 0o755 });
const CONFIG = `[runtime]\nenabled = true\nidle_stop_minutes = 0\n\n[mcp]\nhttp = false\n`;
const home = path.join(root, "home");
const R = { build: "debug" };
const d = await startAt({ home, bin: BIN.e2e, config: CONFIG, env: { ...man.env, NUGGET_E2E_LLAMA_SERVER: wrapper, NUGGET_E2E_RAM_GB: "16", NUGGET_E2E_GPU: "unknown" } });
await addFiles(d.socket, [corpus]);
await waitIndexed(d.socket, { want: 300 });
// Install the model.
const inst = await rpcStream(d.socket, "models.install", { id: MODEL_ID }, (r, ns) => ns.some((n) => n.method === "models.progress" && ["installed", "failed"].includes(n.params.phase)), 120_000);
R.install = inst.notes.filter((n) => n.method === "models.progress").slice(-1)[0]?.params;
log("install", JSON.stringify(R.install));

const rt = async () => (await rpc(d.socket, "runtime.status")).find((r) => r.role === "chat") || null;
const server = () => {
  // The llama-server is a child of the daemon (wrapper execs the fake server).
  for (const c of children(d.pid)) {
    try { if (fs.readFileSync(`/proc/${c}/cmdline`, "utf8").includes("fake-llama-server")) return c; } catch {}
  }
  return null;
};
const waitServer = async (label, ms = 60_000) => {
  const t = now();
  while (now() - t < ms) { const r = await rt(); const s = server(); if (r?.state === "ready" && s) return { label, waited_ms: now() - t, pid: s }; await sleep(100); }
  return { label, waited_ms: now() - t, pid: null, rt: await rt() };
};
const ask = async (label, q = "Harbor Ledger vendor contract", timeoutMs = 240_000) => {
  const t = now();
  try {
    const { result, notes } = await rpcStream(d.socket, "retrieve.ask", { query: q, budget_tokens: 2000 }, (r, ns) => ns.some((n) => n.method === "chat.done" && n.params.stream_id === r.stream_id), timeoutMs);
    const done = notes.find((n) => n.method === "chat.done").params;
    const text = notes.filter((n) => n.method === "chat.delta").map((n) => n.params.text).join("");
    return { label, ms: now() - t, provider: result.provider, error: done.error, text: text.slice(0, 120) };
  } catch (e) { return { label, ms: now() - t, rpc_error: e.message.slice(0, 400) }; }
};
const chat = async (label, timeoutMs = 240_000) => {
  const t = now();
  try {
    const { result, notes } = await rpcStream(d.socket, "chat", { provider: null, tier: "synthesis", messages: [{ role: "user", text: "Say hello" }], items: [] }, (r, ns) => ns.some((n) => n.method === "chat.done" && n.params.stream_id === r.stream_id), timeoutMs);
    const done = notes.find((n) => n.method === "chat.done").params;
    return { label, ms: now() - t, provider: result.provider, error: done.error, text: notes.filter((n) => n.method === "chat.delta").map((n) => n.params.text).join("").slice(0, 80) };
  } catch (e) { return { label, ms: now() - t, rpc_error: e.message.slice(0, 400) }; }
};

R.first_ask = await ask("first");
R.rt_after_first = await rt();
log("first", JSON.stringify(R.first_ask), JSON.stringify(R.rt_after_first));

// A: kill -9 while idle
let pid = server();
R.idle_kill_pid = pid;
let t = now();
process.kill(pid, "SIGKILL");
const states = [];
for (let i = 0; i < 200; i++) {
  const r = await rt();
  states.push({ ms: now() - t, state: r?.state, error: r?.error });
  if (r?.state === "ready" && server() && server() !== pid) break;
  await sleep(100);
}
R.idle_kill_states = states.filter((s, i) => i === 0 || s.state !== states[i - 1].state);
R.idle_kill_recover_ms = states.at(-1).ms;
R.ask_after_idle_kill = await ask("after idle kill");
log("idle kill", JSON.stringify(R.idle_kill_states), JSON.stringify(R.ask_after_idle_kill));

// B: kill -9 mid-answer (server frozen so the request is in flight, then killed)
pid = server();
process.kill(pid, "SIGSTOP");
const pending = ask("mid-answer");
await sleep(1500);
t = now();
process.kill(pid, "SIGKILL");
R.mid_answer = await pending;
R.mid_answer_ms_after_kill = now() - t;
R.rt_after_mid = await rt();
R.ask_after_mid = await ask("after mid-answer kill");
log("mid", JSON.stringify(R.mid_answer), JSON.stringify(R.ask_after_mid));
// B2: same with plain chat
R.wait_before_chat_mid = await waitServer("before chat mid");
pid = server();
process.kill(pid, "SIGSTOP");
const pc = chat("chat mid-answer");
await sleep(1500);
process.kill(pid, "SIGKILL");
R.chat_mid_answer = await pc;
log("chat mid", JSON.stringify(R.chat_mid_answer));
await sleep(3000);

// B3: hung server (SIGSTOP, never killed): how long does a request wait?
R.wait_after_chat_mid = await waitServer("after chat mid");
pid = server();
process.kill(pid, "SIGSTOP");
R.hung_server_ask = await ask("hung server", undefined, 200_000);
R.rt_while_hung = await rt();
log("hung", JSON.stringify(R.hung_server_ask), JSON.stringify(R.rt_while_hung));
process.kill(pid, "SIGKILL");
await sleep(5000);

// C: crash loop
fs.writeFileSync(flag, "");
fs.writeFileSync(launches, "");
R.wait_before_loop = await waitServer("before loop");
pid = server();
t = now();
if (pid) process.kill(pid, "SIGKILL");
const loopStates = [];
const askDuring = ask("ask during crash loop", undefined, 240_000);
for (let i = 0; i < 90; i++) {
  const r = await rt();
  loopStates.push({ s: Math.round((now() - t) / 1000), state: r?.state, error: r?.error?.slice(0, 160) });
  await sleep(1000);
}
R.crashloop_ask = await askDuring;
R.crashloop_states = loopStates.filter((s, i) => i === 0 || s.state !== loopStates[i - 1].state || s.error !== loopStates[i - 1].error);
R.crashloop_launch_times = fs.readFileSync(launches, "utf8").trim().split("\n").filter(Boolean).map((l) => +(Number(l.split(" ")[0]) - t / 1000).toFixed(1));
R.crashloop_status_providers = (await rpc(d.socket, "status")).providers;
R.crashloop_ask2 = await ask("second ask after give-up", undefined, 240_000);
R.crashloop_launches_total = fs.readFileSync(launches, "utf8").trim().split("\n").filter(Boolean).length;
log("crashloop", JSON.stringify(R.crashloop_states), JSON.stringify(R.crashloop_ask), JSON.stringify(R.crashloop_ask2));
// Recovery: remove the flag, ask again.
fs.rmSync(flag);
t = now();
R.recover_ask = await ask("after fix");
R.rt_after_fix = await rt();
log("recover", JSON.stringify(R.recover_ask));
R.logs = grepLogs(d.logDir, /local runtime|llama-server|gave up/i).map((l) => l.slice(0, 400)).slice(-25);
await d.stop();
await served.close();
writeJson("s3-model-crash.json", R);
