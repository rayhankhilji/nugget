// Lead from the real-OS run: a 20k-file folder indexed only 244 items in 60 min. Is it the big
// file or LLM knowledge extraction? Variants on ~5k files nested 12 deep:
//   A: no big files (nuggetd)              B: + 200 MB .log, 200 MB .txt, 50 MB .bin (nuggetd)
//   C: B with a local model installed (nuggetd-e2e + fake llama-server) that is frozen
//      (SIGSTOP) while indexing: an LLM that never answers, the extreme of a slow CPU model.
//   D: as C but the fake model answers instantly.
// usage: node qa/GG6/a3-days/s9-slow-index.mjs [files=5000] [variants=ABC]
import fs from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { signedTestManifest, startModelServer, MODEL_ID } from "../../../apps/desktop/e2e/ai-setup.mjs";
import { generate } from "./gen-corpus.mjs";
import { SCRATCH, BIN, startAt, addFiles, waitIndexed, rpc, rpcStream, children, procStat, log, sleep, now, grepLogs, writeJson } from "./lib.mjs";

const N = Number(process.argv[2] || 5000), VARIANTS = process.argv[3] || "ABC";
const root = path.join(SCRATCH, "s9");
fs.rmSync(root, { recursive: true, force: true });
const nest = (base) => path.join(base, ...Array.from({ length: 10 }, (_, i) => `n${i}`));
const small = path.join(root, "small"), big = path.join(root, "big");
generate(nest(small), N);
generate(nest(big), N);
const line = "2026-10-30T10:00:00Z INFO handled request id=1234 path=/api/v1/items status=200 latency_ms=12\n";
const writeText = (p, mb) => { const fd = fs.openSync(p, "w"); const c = Buffer.from(line.repeat(Math.ceil(1048576 / line.length))); for (let i = 0; i < mb; i++) fs.writeSync(fd, c); fs.closeSync(fd); };
writeText(path.join(big, "server.log"), 200);
writeText(path.join(big, "export.txt"), 200);
execFileSync("fallocate", ["-l", "50M", path.join(big, "disk.bin")]);
const R = { files: N, build: "debug" };

async function run(label, corpus, { model = false, freeze = true } = {}) {
  const home = path.join(root, `home${label}`);
  let served, env = {}, bin = BIN.nuggetd, config;
  if (model) {
    const files = {};
    served = await startModelServer(files, 50 * 1024 * 1024);
    fs.mkdirSync(home, { recursive: true });
    const man = signedTestManifest(home, served.port);
    Object.assign(files, man.files);
    env = { ...man.env, NUGGET_E2E_LLAMA_SERVER: BIN.fake, NUGGET_E2E_RAM_GB: "16", NUGGET_E2E_GPU: "unknown" };
    bin = BIN.e2e;
    config = `[runtime]\nenabled = true\nidle_stop_minutes = 0\n\n[mcp]\nhttp = false\n`;
  }
  const d = await startAt({ home, bin, env, config });
  const out = { label };
  let frozen = null;
  if (model) {
    await rpcStream(d.socket, "models.install", { id: MODEL_ID }, (r, ns) => ns.some((n) => n.method === "models.progress" && ["installed", "failed"].includes(n.params.phase)), 120_000);
    await rpcStream(d.socket, "chat", { provider: null, tier: "synthesis", messages: [{ role: "user", text: "hi" }], items: [] }, (r, ns) => ns.some((n) => n.method === "chat.done"), 60_000);
    frozen = children(d.pid).find((c) => { try { return fs.readFileSync(`/proc/${c}/cmdline`, "utf8").includes("fake-llama"); } catch { return false; } });
    if (freeze) { process.kill(frozen, "SIGSTOP"); out.llm_frozen_pid = frozen; } else frozen = null;
  }
  const t0 = now();
  await addFiles(d.socket, [corpus]);
  const curve = [];
  const res = await waitIndexed(d.socket, { want: N, timeoutMs: 240_000, every: 1000, onPoll: (l, ms) => { if (curve.length === 0 || ms - curve.at(-1).ms >= 5000) curve.push({ ms, items: l.items, queue: l.queue, states: l.states }); } });
  out.result = res;
  out.curve = curve.filter((_, i) => i % 2 === 0);
  out.knowledge = await rpc(d.socket, "knowledge.status");
  out.sync_pass_done = grepLogs(d.logDir, /sync pass done/).map((l) => l.slice(l.indexOf('"fields"'), l.indexOf('"fields"') + 160));
  out.warn = grepLogs(d.logDir, /"level":"(WARN|ERROR)"/).filter((l) => !/keychain|KEYCHAIN/i.test(l)).map((l) => l.slice(0, 260)).slice(0, 6);
  out.rss_mb = Math.round(procStat(d.pid).hwm / 1024);
  out.cpu_s = procStat(d.pid).ticks / 100;
  if (frozen) process.kill(frozen, "SIGCONT");
  await d.stop();
  if (served) await served.close();
  R[label] = out;
  log(label, JSON.stringify(res), "knowledge", JSON.stringify({ e: out.knowledge.entities, p: out.knowledge.pending_items, x: out.knowledge.extractor_provider }));
}
if (VARIANTS.includes("A")) await run("A", small);
if (VARIANTS.includes("B")) await run("B", big);
if (VARIANTS.includes("C")) await run("C", big, { model: true });
// D: as C, but the (instant) fake model answers: the LLM's presence alone is not the problem.
if (VARIANTS.includes("D")) await run("D", big, { model: true, freeze: false });
writeJson(`s9-slow-index-${VARIANTS}.json`, R);
fs.rmSync(root, { recursive: true, force: true });
