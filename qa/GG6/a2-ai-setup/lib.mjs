// Shared helpers for the A2 (AI setup) QA scenarios. Imports the repo's e2e modules unchanged.
// Everything runs on 127.0.0.1; no real provider or Hugging Face calls.
import crypto from "node:crypto";
import fs from "node:fs";
import http from "node:http";
import net from "node:net";
import path from "node:path";
import { spawn } from "node:child_process";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

export const HERE = path.dirname(fileURLToPath(import.meta.url));
export const REPO = path.resolve(HERE, "../../..");
export const SHOTS = path.join(HERE, "shots");
export const SCRATCH = process.env.A2_SCRATCH || "/tmp/claude-0/-home-user-nugget/35de668b-6103-5633-811a-58fa1eb0313a/scratchpad/a2";
fs.mkdirSync(SHOTS, { recursive: true });
fs.mkdirSync(SCRATCH, { recursive: true });
process.env.E2E_ARTIFACTS ||= path.join(SCRATCH, "e2e-artifacts");
fs.mkdirSync(process.env.E2E_ARTIFACTS, { recursive: true });

export const BIN = {
  nuggetd: path.join(REPO, "target/debug/nuggetd"),
  e2e: path.join(REPO, "target/debug/nuggetd-e2e"),
  fake: path.join(REPO, "target/debug/nuggetd-fake-llama-server"),
};

const require = createRequire(path.join(REPO, "apps/desktop/package.json"));
export const playwright = require("playwright");

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
export const sha256 = (b) => crypto.createHash("sha256").update(b).digest("hex");
const t0 = Date.now();
export const log = (...a) => console.log(`[a2 ${((Date.now() - t0) / 1000).toFixed(1)}s]`, ...a);

/** Deterministic bytes starting with GGUF. */
export function modelBytes(n, seed = 0x9e3779b9) {
  const buf = Buffer.alloc(n);
  buf.write("GGUF", 0, "latin1");
  let x = seed >>> 0 || 1;
  for (let i = 4; i < n; i++) {
    x ^= x << 13; x ^= x >>> 17; x ^= x << 5;
    buf[i] = x & 0xff;
  }
  return buf;
}

/**
 * A model server with fault injection. `handler(ctx)` may return a fault for each request:
 * ctx = { n (request index), path, range, start }. Faults: { status }, { dropAfter: bytes },
 * { stallAfter: bytes }, { wrongBytes: true }, { html: true }. `bps` throttles.
 */
export function startFaultServer(files, { bps = 4 * 1024 * 1024, fault = () => null } = {}) {
  const stats = { requests: [], served: 0 };
  const sockets = new Set();
  const server = http.createServer(async (req, res) => {
    const url = new URL(req.url, "http://127.0.0.1");
    const body = files[url.pathname];
    const range = req.headers.range ?? null;
    const m = range && /^bytes=(\d+)-(\d*)$/.exec(range);
    const start = m ? Number(m[1]) : 0;
    const entry = { n: stats.requests.length, method: req.method, path: url.pathname, range, status: 0, at: Date.now() };
    stats.requests.push(entry);
    const f = fault({ ...entry, start }) || {};
    if (f.status) { entry.status = f.status; return res.writeHead(f.status, { "Content-Type": "text/plain" }).end(`error ${f.status}`); }
    if (f.html) { entry.status = 200; return res.writeHead(200, { "Content-Type": "text/html" }).end("<html><body>Please sign in to the Wi-Fi</body></html>"); }
    if (!body) { entry.status = 404; return res.writeHead(404).end("not found"); }
    const end = m && m[2] ? Math.min(Number(m[2]), body.length - 1) : body.length - 1;
    entry.status = m ? 206 : 200;
    res.writeHead(entry.status, {
      "Content-Type": "application/octet-stream",
      "Content-Length": end - start + 1,
      "Accept-Ranges": "bytes",
      ...(m ? { "Content-Range": `bytes ${start}-${end}/${body.length}` } : {}),
    });
    if (req.method === "HEAD") return res.end();
    const src = f.wrongBytes ? Buffer.alloc(body.length, 0x41) : body;
    const slice = 64 * 1024;
    const ts = Date.now();
    let sent = 0;
    for (let at = start; at <= end && !res.destroyed; at += slice) {
      if (f.dropAfter != null && sent >= f.dropAfter) { entry.dropped = sent; req.socket.destroy(); return; }
      if (f.stallAfter != null && sent >= f.stallAfter) { entry.stalled = sent; await new Promise(() => {}); }
      const chunk = src.subarray(at, Math.min(at + slice, end + 1));
      if (!res.write(chunk)) await new Promise((r) => res.once("drain", r));
      sent += chunk.length;
      stats.served += chunk.length;
      const due = (sent / bps) * 1000 - (Date.now() - ts);
      if (due > 0) await sleep(due);
    }
    res.end();
  });
  server.on("connection", (s) => { sockets.add(s); s.on("close", () => sockets.delete(s)); });
  return new Promise((resolve) =>
    server.listen(0, "127.0.0.1", () =>
      resolve({
        port: server.address().port,
        stats,
        server,
        close: () => new Promise((r) => { for (const s of sockets) s.destroy(); server.close(() => r()); }),
      }),
    ),
  );
}

/** A free localhost port (bound then released). */
export function closedPort() {
  return new Promise((resolve) => {
    const s = net.createServer();
    s.listen(0, "127.0.0.1", () => { const { port } = s.address(); s.close(() => resolve(port)); });
  });
}

/**
 * Sign a manifest `{version:1, models}` with a fresh ed25519 key; returns env for nuggetd-e2e.
 * `models`: [{ id, bytes?, size?, sha?, minRam, tiers, url }].
 */
export function signManifest(dir, models, extra = {}) {
  fs.mkdirSync(dir, { recursive: true });
  const manifest = {
    version: 1,
    models: models.map((m) => {
      const rev = crypto.createHash("sha1").update(m.id).digest("hex");
      const file = m.file || `${m.id}.gguf`;
      return {
        id: m.id,
        family: m.family || "qwen3",
        file_name: file,
        url: m.url || `http://127.0.0.1:${m.port}/${rev}/${file}`,
        mirrors: [],
        revision: rev,
        size: m.size ?? m.bytes.length,
        sha256: m.sha ?? sha256(m.bytes),
        min_ram_gb: m.minRam,
        tiers: m.tiers || ["triage", "light", "synthesis", "agentic"],
        license: m.license || "Apache-2.0",
        min_app_version: "0.0.1",
        _path: `/${rev}/${file}`,
      };
    }),
    ...extra,
  };
  const paths = {};
  for (const m of manifest.models) { paths[m.id] = m._path; delete m._path; }
  const json = Buffer.from(JSON.stringify(manifest, null, 2) + "\n");
  const { publicKey, privateKey } = crypto.generateKeyPairSync("ed25519");
  const sig = crypto.sign(null, json, privateKey);
  const pub = publicKey.export({ format: "der", type: "spki" }).subarray(-32).toString("hex");
  fs.writeFileSync(path.join(dir, "manifest.json"), json);
  fs.writeFileSync(path.join(dir, "manifest.json.sig"), sig.toString("hex") + "\n");
  return {
    manifest,
    paths,
    env: {
      NUGGET_E2E_MANIFEST: path.join(dir, "manifest.json"),
      NUGGET_E2E_MANIFEST_SIG: path.join(dir, "manifest.json.sig"),
      NUGGET_E2E_MANIFEST_PUBKEY: pub,
    },
  };
}

export const AI_CONFIG = `[runtime]\nenabled = true\nidle_stop_minutes = 0\n\n[mcp]\nhttp = false\n`;
export const NO_RT_CONFIG = `[runtime]\nenabled = false\n\n[mcp]\nhttp = false\n`;

function readyLine(logDir) {
  if (!fs.existsSync(logDir)) return null;
  for (const f of fs.readdirSync(logDir)) {
    if (f.includes(".prev-")) continue;
    for (const line of fs.readFileSync(path.join(logDir, f), "utf8").split("\n")) {
      if (!line.includes("nuggetd ready")) continue;
      const m = line.match(/"socket":"([^"]+)"/);
      if (m) return m[1];
    }
  }
  return null;
}

/** Start `bin --home home` (home created if missing; config written only if `config` given and absent or `overwrite`). */
export async function startHome(bin, { home, config, env = {}, keychainOff = true, readyTimeoutMs = 40_000, overwrite = false } = {}) {
  home ||= fs.mkdtempSync(path.join(SCRATCH, "home-"));
  fs.mkdirSync(path.join(home, "config"), { recursive: true });
  const cfg = path.join(home, "config", "config.toml");
  if (config != null && (overwrite || !fs.existsSync(cfg))) fs.writeFileSync(cfg, config);
  const logDir = path.join(home, "data", "logs");
  // Remove old ready lines so we wait for this start.
  if (fs.existsSync(logDir)) for (const f of fs.readdirSync(logDir)) fs.renameSync(path.join(logDir, f), path.join(logDir, `${f}.prev-${Date.now()}`));
  const outPath = path.join(home, `daemon-out-${Date.now()}.log`);
  const out = fs.openSync(outPath, "w");
  const baseEnv = { ...process.env, ...env };
  if (keychainOff) baseEnv.NUGGET_KEYCHAIN = "off"; else delete baseEnv.NUGGET_KEYCHAIN;
  const proc = spawn(bin, ["--home", home], { env: baseEnv, stdio: ["ignore", out, out] });
  let exited = null;
  proc.on("exit", (code, signal) => (exited = { code, signal }));
  const deadline = Date.now() + readyTimeoutMs;
  let socket = null;
  while (!socket) {
    if (exited) throw new Error(`daemon exited (${JSON.stringify(exited)}): ${fs.readFileSync(outPath, "utf8").slice(-800)}`);
    if (Date.now() > deadline) throw new Error("daemon not ready");
    socket = readyLine(logDir);
    if (!socket) await sleep(100);
  }
  const stop = async (sig = "SIGTERM") => {
    if (!exited) {
      proc.kill(sig);
      for (let i = 0; i < 100 && !exited; i++) await sleep(100);
      if (!exited) proc.kill("SIGKILL");
      for (let i = 0; i < 30 && !exited; i++) await sleep(100);
    }
    try { fs.closeSync(out); } catch {}
  };
  return { home, socket, logDir, proc, stop, outPath, exited: () => exited };
}

/** All text logged by a home (daemon logs + stdout). */
export function allLogs(home) {
  let s = "";
  const walk = (d) => {
    if (!fs.existsSync(d)) return;
    for (const e of fs.readdirSync(d, { withFileTypes: true })) {
      const p = path.join(d, e.name);
      if (e.isDirectory()) walk(p);
      else if (/\.log|logs/.test(p)) s += fs.readFileSync(p, "utf8");
    }
  };
  walk(home);
  return s;
}

/** Recursive byte size of a dir (0 when missing). */
export function dirBytes(d) {
  if (!fs.existsSync(d)) return 0;
  let n = 0;
  for (const e of fs.readdirSync(d, { withFileTypes: true })) {
    const p = path.join(d, e.name);
    n += e.isDirectory() ? dirBytes(p) : fs.statSync(p).size;
  }
  return n;
}

/** Screenshot to qa/GG6/a2-ai-setup/shots/<name>.png (small: viewport only). */
export async function snap(page, name) {
  const file = path.join(SHOTS, `${name}.png`);
  await page.screenshot({ path: file }).catch((e) => log(`shot ${name} failed: ${e.message}`));
  return path.relative(REPO, file);
}

/** Write a JSON results file next to the scripts. */
export function writeResults(name, data) {
  const file = path.join(HERE, "results", `${name}.json`);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, JSON.stringify(data, null, 2));
  log(`wrote ${path.relative(REPO, file)}`);
}
