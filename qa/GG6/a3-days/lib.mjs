// Shared helpers for the A3 days-2-7 scenarios: start nuggetd on a persistent home (so it can be
// restarted, killed, stopped), RPC over its socket, /proc sampling. Node built-ins only.
import { spawn } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
export { rpc, rpcRaw, rpcStream } from "../../../apps/desktop/e2e/daemon.mjs";
import { rpc } from "../../../apps/desktop/e2e/daemon.mjs";

export const HERE = path.dirname(fileURLToPath(import.meta.url));
export const REPO = path.resolve(HERE, "../../..");
export const SCRATCH = process.env.A3_SCRATCH || "/tmp/claude-0/-home-user-nugget/35de668b-6103-5633-811a-58fa1eb0313a/scratchpad/a3";
const TARGET = process.env.CARGO_TARGET_DIR || path.join(REPO, "target");
export const BIN = {
  nuggetd: process.env.NUGGETD || path.join(TARGET, "debug", "nuggetd"),
  e2e: process.env.NUGGETD_E2E || path.join(TARGET, "debug", "nuggetd-e2e"),
  fake: process.env.NUGGET_FAKE_LLAMA_SERVER || path.join(TARGET, "debug", "nuggetd-fake-llama-server"),
};
export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
export const now = () => Date.now();
export const log = (...a) => console.log(`[${new Date().toISOString().slice(11, 19)}]`, ...a);

export const BASE_CONFIG = `[runtime]
enabled = false

[mcp]
http = false
`;

/** Count "nuggetd ready" lines across the log dir, and the socket of the last one. */
function readyInfo(logDir) {
  let n = 0, socket = null;
  if (!fs.existsSync(logDir)) return { n, socket };
  for (const f of fs.readdirSync(logDir).sort()) {
    const p = path.join(logDir, f);
    if (!fs.statSync(p).isFile()) continue;
    for (const line of fs.readFileSync(p, "utf8").split("\n")) {
      if (!line.includes("nuggetd ready")) continue;
      n++;
      const m = line.match(/"socket":"([^"]+)"/);
      if (m) socket = m[1];
    }
  }
  return { n, socket };
}

/**
 * Start `bin` on `home` (created if missing; config written only if absent or `config` given).
 * Returns { proc, pid, socket, home, logDir, readyMs, stop(), kill(sig), exited() }.
 */
export async function startAt({ home, bin = BIN.nuggetd, config, env = {}, out, readyTimeoutMs = 120_000, wrap = null }) {
  fs.mkdirSync(path.join(home, "config"), { recursive: true });
  const cfgPath = path.join(home, "config", "config.toml");
  if (config !== undefined && config !== null) fs.writeFileSync(cfgPath, config);
  else if (!fs.existsSync(cfgPath)) fs.writeFileSync(cfgPath, BASE_CONFIG);
  const logDir = path.join(home, "data", "logs");
  const before = readyInfo(logDir).n;
  const fd = fs.openSync(out || path.join(home, "stdout.log"), "a");
  const [cmd, args] = wrap ? [wrap[0], [...wrap.slice(1), bin, "--home", home]] : [bin, ["--home", home]];
  const t0 = now();
  const proc = spawn(cmd, args, { env: { ...process.env, NUGGET_KEYCHAIN: "off", ...env }, stdio: ["ignore", fd, fd] });
  let exited = null;
  proc.on("exit", (code, signal) => (exited = { code, signal, at: now() }));
  let socket = null;
  for (;;) {
    if (exited) throw new Error(`daemon exited before ready: ${JSON.stringify(exited)}; see ${logDir}`);
    if (now() - t0 > readyTimeoutMs) throw new Error(`not ready in ${readyTimeoutMs} ms`);
    const ri = readyInfo(logDir);
    if (ri.n > before && ri.socket) { socket = ri.socket; break; }
    await sleep(100);
  }
  await rpc(socket, "status");
  const readyMs = now() - t0;
  const d = {
    proc, pid: proc.pid, socket, home, logDir, readyMs,
    exited: () => exited,
    kill: (sig) => { try { process.kill(proc.pid, sig); } catch {} },
    async stop(timeoutMs = 15_000) {
      const t = now();
      if (!exited) proc.kill("SIGTERM");
      while (!exited && now() - t < timeoutMs) await sleep(50);
      if (!exited) { proc.kill("SIGKILL"); while (!exited) await sleep(50); return { forced: true, ms: now() - t, exited }; }
      fs.closeSync(fd);
      return { forced: false, ms: now() - t, exited };
    },
    async waitExit(timeoutMs = 15_000) { const t = now(); while (!exited && now() - t < timeoutMs) await sleep(50); return exited; },
  };
  return d;
}

export function addFiles(socket, roots, name = "Docs", extra = {}) {
  return rpc(socket, "sources.add", { id: null, kind: "files", name, settings: { roots, include: [], exclude: [], ...extra } });
}

/** Poll until status.items >= want and ingest queue empty and the source is not syncing, stable twice. */
export async function waitIndexed(socket, { want = 1, timeoutMs = 600_000, every = 500, onPoll } = {}) {
  const t0 = now();
  let prev = null, stable = 0, last;
  for (;;) {
    const [st, ing, srcs] = await Promise.all([rpc(socket, "status"), rpc(socket, "ingest.status"), rpc(socket, "sources.list")]);
    last = { items: st.items, chunks: st.chunks, queue: ing.queue_depth, states: srcs.map((s) => `${s.id}:${s.state}:${s.items}`).join(","), errors: srcs.map((s) => s.last_error).filter(Boolean) };
    onPoll?.(last, now() - t0);
    const key = JSON.stringify(last);
    const settled = st.items >= want && ing.queue_depth === 0 && srcs.every((s) => s.state !== "syncing");
    stable = settled && key === prev ? stable + 1 : 0;
    if (stable >= 2) return { ...last, ms: now() - t0 };
    prev = key;
    if (now() - t0 > timeoutMs) return { ...last, ms: now() - t0, timeout: true };
    await sleep(every);
  }
}

export async function search(socket, query, limit = 20) {
  const t = now();
  const hits = await rpc(socket, "search", { query, limit }, 60_000);
  return { ms: now() - t, hits };
}

/** RSS (kB) and cumulative CPU ticks of a pid; null if gone. */
export function procStat(pid) {
  try {
    const st = fs.readFileSync(`/proc/${pid}/stat`, "utf8");
    const f = st.slice(st.lastIndexOf(")") + 2).split(" ");
    const utime = Number(f[11]), stime = Number(f[12]), threads = Number(f[17]);
    const status = fs.readFileSync(`/proc/${pid}/status`, "utf8");
    const rss = Number(/VmRSS:\s+(\d+)/.exec(status)?.[1] || 0);
    const hwm = Number(/VmHWM:\s+(\d+)/.exec(status)?.[1] || 0);
    const fds = fs.readdirSync(`/proc/${pid}/fd`).length;
    return { ticks: utime + stime, rss, hwm, threads, fds, state: f[0] };
  } catch { return null; }
}
export const CLK_TCK = 100;

/** Children pids of pid (one level). */
export function children(pid) {
  try { return fs.readFileSync(`/proc/${pid}/task/${pid}/children`, "utf8").trim().split(/\s+/).filter(Boolean).map(Number); } catch { return []; }
}

export function du(p) {
  let n = 0;
  try {
    const s = fs.lstatSync(p);
    if (s.isDirectory()) for (const e of fs.readdirSync(p)) n += du(path.join(p, e));
    else n += s.size;
  } catch {}
  return n;
}
export function fileSize(p) { try { return fs.statSync(p).size; } catch { return 0; } }
export const MB = (b) => (b / 1048576).toFixed(1);

/** Lines of the daemon log matching re (all log files). */
export function grepLogs(logDir, re) {
  const out = [];
  if (!fs.existsSync(logDir)) return out;
  for (const f of fs.readdirSync(logDir).sort()) {
    const p = path.join(logDir, f);
    if (!fs.statSync(p).isFile()) continue;
    for (const line of fs.readFileSync(p, "utf8").split("\n")) if (re.test(line)) out.push(`${f}: ${line}`);
  }
  return out;
}

export function writeJson(name, obj) {
  fs.writeFileSync(path.join(HERE, "results", name), JSON.stringify(obj, null, 2) + "\n");
}
fs.mkdirSync(path.join(HERE, "results"), { recursive: true });
