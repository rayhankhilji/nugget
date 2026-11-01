// MM1 targeted re-check of GG6-5's crash loop (as GG6's verifier did: start from a HEALTHY server,
// not right after other induced crashes). Same building blocks as qa/GG6/a3-days/s3-model-crash.mjs
// (nuggetd-e2e + signed test manifest + fake llama-server through a wrapper that exits 1 while a
// flag file exists). Then: Ask while backing off, after give-up, a burst of 10 asks (relaunch
// storm?), 20 s idle (relaunches?), fix the cause, `runtime.retry`, Ask.
// usage (repo root): A3_SCRATCH=<dir> node qa/MM1/gg6/crashloop.mjs
import fs from "node:fs";
import path from "node:path";
import { signedTestManifest, startModelServer, MODEL_ID } from "../../../apps/desktop/e2e/ai-setup.mjs";
import { generate } from "../../GG6/a3-days/gen-corpus.mjs";
import { SCRATCH, BIN, startAt, addFiles, waitIndexed, rpc, rpcStream, children, log, sleep, now } from "../../GG6/a3-days/lib.mjs";

const root = path.join(SCRATCH, "mm1-crashloop");
fs.rmSync(root, { recursive: true, force: true });
fs.mkdirSync(root, { recursive: true });
const corpus = path.join(root, "corpus");
generate(corpus, 50);
const files = {};
const served = await startModelServer(files, 50 * 1024 * 1024);
const man = signedTestManifest(root, served.port);
Object.assign(files, man.files);
const flag = path.join(root, "crashloop.flag"), launches = path.join(root, "launches.log"), wrapper = path.join(root, "w.sh");
fs.writeFileSync(wrapper, `#!/bin/bash\necho "$(date +%s.%N)" >> ${launches}\nif [ -e ${flag} ]; then echo "simulated crash" >&2; exit 1; fi\nexec ${BIN.fake} "$@"\n`, { mode: 0o755 });
const d = await startAt({ home: path.join(root, "home"), bin: BIN.e2e, config: `[runtime]\nenabled = true\nidle_stop_minutes = 0\n\n[mcp]\nhttp = false\n`, env: { ...man.env, NUGGET_E2E_LLAMA_SERVER: wrapper, NUGGET_E2E_RAM_GB: "16", NUGGET_E2E_GPU: "unknown" } });
await addFiles(d.socket, [corpus]);
await waitIndexed(d.socket, { want: 50 });
await rpcStream(d.socket, "models.install", { id: MODEL_ID }, (r, ns) => ns.some((n) => n.method === "models.progress" && ["installed", "failed"].includes(n.params.phase)), 120_000);
const rt = async () => (await rpc(d.socket, "runtime.status")).find((r) => r.role === "chat") || null;
const server = () => { for (const c of children(d.pid)) { try { if (fs.readFileSync(`/proc/${c}/cmdline`, "utf8").includes("fake-llama-server")) return c; } catch {} } return null; };
const ask = async (label) => {
  const t = now();
  try {
    const { result, notes } = await rpcStream(d.socket, "retrieve.ask", { query: "Harbor Ledger vendor contract", budget_tokens: 2000 }, (r, ns) => ns.some((n) => n.method === "chat.done" && n.params.stream_id === r.stream_id), 240_000);
    const done = notes.find((n) => n.method === "chat.done").params;
    return { label, ms: now() - t, error: done.error, error_len: done.error?.length ?? 0, ok: !done.error };
  } catch (e) { return { label, ms: now() - t, rpc_error: e.message.slice(0, 300) }; }
};
const nLaunch = () => (fs.existsSync(launches) ? fs.readFileSync(launches, "utf8").trim().split("\n").filter(Boolean).length : 0);
const R = {};
R.healthy_first = await ask("healthy");
// Stay healthy past stable_after (120 s) so earlier failures don't count.
await sleep(125_000);
R.rt_before = await rt();
fs.writeFileSync(flag, "");
fs.writeFileSync(launches, "");
const t0 = now();
process.kill(server(), "SIGKILL");
await sleep(300);
R.ask_during_backoff = await ask("during backoff");
const states = [];
while (now() - t0 < 90_000) { const r = await rt(); states.push({ s: +((now() - t0) / 1000).toFixed(1), state: r?.state, gave_up: r?.gave_up, error: r?.error?.slice(0, 120) }); if (r?.gave_up) break; await sleep(500); }
R.states = states.filter((s, i) => i === 0 || s.state !== states[i - 1].state || s.error !== states[i - 1].error || s.gave_up !== states[i - 1].gave_up);
R.gave_up_after_s = states.at(-1).s;
R.launch_times_s = fs.readFileSync(launches, "utf8").trim().split("\n").filter(Boolean).map((l) => +(Number(l) - t0 / 1000).toFixed(1));
R.ask_after_give_up = await ask("after give-up");
const before = nLaunch();
R.burst = [];
for (let i = 0; i < 10; i++) R.burst.push((await ask(`burst ${i}`)).ms);
await sleep(20_000);
R.launches_during_burst_and_20s_idle = nLaunch() - before;
R.status_provider_error = (await rpc(d.socket, "status")).providers?.find((p) => p.id === "local-chat")?.error;
fs.rmSync(flag);
R.ask_after_fix_without_retry = await ask("fixed, no retry");
const tr = now();
R.retry = await rpc(d.socket, "runtime.retry", { role: "chat" }).catch((e) => ({ rpc_error: e.message }));
let r2;
while (now() - tr < 60_000) { r2 = await rt(); if (r2?.state === "ready") break; await sleep(200); }
R.ready_after_retry_ms = now() - tr;
R.ask_after_retry = await ask("after retry");
R.launches_total = nLaunch();
await d.stop();
await served.close();
console.log(JSON.stringify(R, null, 1));
