// Runs a real `nuggetd` for the end-to-end test: locate or build the binary, start it in a
// throwaway home with a fixed config, index the fictional corpus in bench/fixtures/knowledge/items
// and wait until knowledge extraction has settled. Node built-ins only.
import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
export const REPO = path.resolve(here, "../../..");
export const CORPUS = path.join(REPO, "bench", "fixtures", "knowledge", "items");

/** The corpus owner; mail and calendar facts hang off this entity. Matches the fixtures. */
const CONFIG = `# Written by apps/desktop/e2e/daemon.mjs.
[runtime]
enabled = false

[knowledge.me]
name = "Robin Ashgrove"
emails = ["robin.ashgrove@glimmerfen.test", "robin@ashgrove.test"]

[mcp]
http = false
`;

/**
 * A first run: nothing the user would have to write by hand. Only what a test machine needs:
 * no local model runtime (no models, nothing to start) and no MCP HTTP listener (a fixed port
 * that parallel runs would fight over). No `[knowledge.me]`: onboarding doesn't ask for it.
 */
export const FIRST_RUN_CONFIG = `# Written by apps/desktop/e2e/daemon.mjs (first-run scenario).
[runtime]
enabled = false

[mcp]
http = false
`;

/** The config the all-pages scenario uses (the corpus owner set, so mail and calendar facts attach). */
export const E2E_CONFIG = CONFIG;

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** Files under `dir`, recursively (the corpus item count). */
export function countFiles(dir) {
  let n = 0;
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    n += e.isDirectory() ? countFiles(path.join(dir, e.name)) : e.isFile() ? 1 : 0;
  }
  return n;
}

/**
 * `$NUGGETD` if set, else `cargo build -p nuggetd` (a no-op when up to date) and the debug binary
 * in `$CARGO_TARGET_DIR` (default `<repo>/target`).
 */
export function locateNuggetd(log = console.log) {
  const exe = process.platform === "win32" ? "nuggetd.exe" : "nuggetd";
  if (process.env.NUGGETD) {
    const bin = path.resolve(process.env.NUGGETD);
    if (!fs.existsSync(bin)) throw new Error(`NUGGETD points to ${bin}, which does not exist`);
    return bin;
  }
  log("building nuggetd (cargo build --locked -p nuggetd)…");
  const r = spawnSync("cargo", ["build", "--locked", "-p", "nuggetd"], { cwd: REPO, stdio: "inherit" });
  if (r.error) throw new Error(`could not run cargo (${r.error.message}); install Rust or set NUGGETD to a built nuggetd`);
  if (r.status !== 0) throw new Error(`cargo build -p nuggetd failed (exit ${r.status})`);
  const target = process.env.CARGO_TARGET_DIR ? path.resolve(REPO, process.env.CARGO_TARGET_DIR) : path.join(REPO, "target");
  const bin = path.join(target, "debug", exe);
  if (!fs.existsSync(bin)) throw new Error(`cargo build succeeded but ${bin} is missing`);
  return bin;
}

/**
 * The daemon with test overrides and the fake llama-server, for the local-model scenario:
 * `$NUGGETD_E2E` and `$NUGGET_FAKE_LLAMA_SERVER` if both are set, else
 * `cargo build --locked -p nuggetd --features fake-runtime --bins` (a no-op when up to date) and
 * the debug binaries in `$CARGO_TARGET_DIR` (default `<repo>/target`).
 */
export function locateE2eBinaries(log = console.log) {
  const exe = (name) => (process.platform === "win32" ? `${name}.exe` : name);
  const given = [process.env.NUGGETD_E2E, process.env.NUGGET_FAKE_LLAMA_SERVER];
  if (given.every(Boolean)) {
    const [nuggetd, server] = given.map((p) => path.resolve(p));
    for (const b of [nuggetd, server]) if (!fs.existsSync(b)) throw new Error(`${b} (from NUGGETD_E2E / NUGGET_FAKE_LLAMA_SERVER) does not exist`);
    return { nuggetd, server };
  }
  log("building nuggetd-e2e and the fake llama-server (cargo build --locked -p nuggetd --features fake-runtime --bins)…");
  const r = spawnSync("cargo", ["build", "--locked", "-p", "nuggetd", "--features", "fake-runtime", "--bins"], { cwd: REPO, stdio: "inherit" });
  if (r.error) throw new Error(`could not run cargo (${r.error.message}); set NUGGETD_E2E and NUGGET_FAKE_LLAMA_SERVER`);
  if (r.status !== 0) throw new Error(`cargo build -p nuggetd --features fake-runtime failed (exit ${r.status})`);
  const target = process.env.CARGO_TARGET_DIR ? path.resolve(REPO, process.env.CARGO_TARGET_DIR) : path.join(REPO, "target");
  const nuggetd = path.join(target, "debug", exe("nuggetd-e2e"));
  const server = path.join(target, "debug", exe("nuggetd-fake-llama-server"));
  for (const b of [nuggetd, server]) if (!fs.existsSync(b)) throw new Error(`cargo build succeeded but ${b} is missing`);
  return { nuggetd, server };
}

let nextId = 1;

/** One newline-delimited JSON-RPC request over the daemon's Unix socket. Resolves to the raw reply. */
export function rpcRaw(socket, method, params = null, timeoutMs = 30_000) {
  return new Promise((resolve, reject) => {
    const id = nextId++;
    const conn = net.connect(socket);
    let buf = "";
    const timer = setTimeout(() => {
      conn.destroy();
      reject(new Error(`${method}: no reply within ${timeoutMs} ms`));
    }, timeoutMs);
    const done = (fn, v) => {
      clearTimeout(timer);
      conn.destroy();
      fn(v);
    };
    conn.setEncoding("utf8");
    conn.on("connect", () => conn.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n"));
    conn.on("data", (chunk) => {
      buf += chunk;
      let nl;
      while ((nl = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, nl);
        buf = buf.slice(nl + 1);
        if (!line.trim()) continue;
        let msg;
        try {
          msg = JSON.parse(line);
        } catch (e) {
          return done(reject, new Error(`${method}: unreadable reply: ${e.message}`));
        }
        if (msg.id === id) return done(resolve, msg); // anything else is a notification
      }
    });
    conn.on("error", (e) => done(reject, e));
    conn.on("close", () => done(reject, new Error(`${method}: the daemon closed the connection`)));
  });
}

/** Like `rpcRaw`, but returns `result` and throws on a JSON-RPC error. */
export async function rpc(socket, method, params = null, timeoutMs) {
  const msg = await rpcRaw(socket, method, params, timeoutMs);
  if (msg.error) throw new Error(`${method} failed: ${msg.error.message} (code ${msg.error.code})`);
  return msg.result;
}

/**
 * A request whose answer streams as notifications on the same connection (`draft.begin`,
 * `retrieve.ask`, `chat`): sends it, keeps the connection open and resolves to
 * `{ result, notes }` once `done(result, notes)` is true. Throws on a JSON-RPC error or timeout.
 */
export function rpcStream(socket, method, params, done, timeoutMs = 60_000) {
  return new Promise((resolve, reject) => {
    const id = nextId++;
    const conn = net.connect(socket);
    const notes = [];
    let result;
    let buf = "";
    const finish = (fn, v) => {
      clearTimeout(timer);
      conn.destroy();
      fn(v);
    };
    const timer = setTimeout(
      () => finish(reject, new Error(`${method}: not done within ${timeoutMs} ms; result ${JSON.stringify(result)}, ${notes.length} notification(s): ${JSON.stringify(notes.slice(-5))}`)),
      timeoutMs,
    );
    conn.setEncoding("utf8");
    conn.on("connect", () => conn.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n"));
    conn.on("data", (chunk) => {
      buf += chunk;
      let nl;
      while ((nl = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, nl);
        buf = buf.slice(nl + 1);
        if (!line.trim()) continue;
        let msg;
        try {
          msg = JSON.parse(line);
        } catch {
          continue;
        }
        if (msg.id === id) {
          if (msg.error) return finish(reject, new Error(`${method} failed: ${msg.error.message} (code ${msg.error.code})`));
          result = msg.result;
        } else if (msg.method) notes.push(msg);
        if (result !== undefined && done(result, notes)) return finish(resolve, { result, notes });
      }
    });
    conn.on("error", (e) => finish(reject, e));
    conn.on("close", () => finish(reject, new Error(`${method}: the daemon closed the connection`)));
  });
}

function readyLine(logDir) {
  if (!fs.existsSync(logDir)) return null;
  for (const f of fs.readdirSync(logDir)) {
    for (const line of fs.readFileSync(path.join(logDir, f), "utf8").split("\n")) {
      if (!line.includes("nuggetd ready")) continue;
      try {
        const socket = JSON.parse(line)?.fields?.socket;
        if (socket) return socket;
      } catch {
        const m = line.match(/"socket":"([^"]+)"/);
        if (m) return m[1];
      }
    }
  }
  return null;
}

/**
 * Start `bin` in a fresh temp home. Resolves once its log says "nuggetd ready" and the socket
 * answers `status`. `stderrPath` receives the daemon's stdout and stderr. `config` is the
 * `config/config.toml` to write (default: the all-pages config; `null` writes none). `env` is
 * added to the daemon's environment (e.g. `nuggetd-e2e`'s `NUGGET_E2E_*` overrides). `home` reuses an
 * existing home instead (a restart; pass `config: null` to keep its config), and `stop()` then
 * keeps it unless asked with `{ keep: false }`.
 */
export async function startDaemon(bin, { stderrPath, readyTimeoutMs = 30_000, config = CONFIG, env = {}, home: reuse = null } = {}) {
  const home = reuse ?? fs.mkdtempSync(path.join(os.tmpdir(), "nugget-e2e-"));
  fs.mkdirSync(path.join(home, "config"), { recursive: true });
  if (config !== null) fs.writeFileSync(path.join(home, "config", "config.toml"), config);
  const out = stderrPath ? fs.openSync(stderrPath, "w") : "ignore";
  const proc = spawn(bin, ["--home", home], {
    // No OS keychain in CI; the store key goes to a 0600 file in the temp home.
    env: { ...process.env, NUGGET_KEYCHAIN: "off", ...env },
    stdio: ["ignore", out, out],
  });
  let exited = null;
  proc.on("exit", (code, signal) => (exited = { code, signal }));
  const logDir = path.join(home, "data", "logs");

  const stop = async ({ keep = reuse !== null } = {}) => {
    if (!exited) {
      proc.kill("SIGTERM");
      for (let i = 0; i < 100 && !exited; i++) await sleep(100);
      if (!exited) proc.kill("SIGKILL");
    }
    if (typeof out === "number") fs.closeSync(out);
    if (!keep) fs.rmSync(home, { recursive: true, force: true });
  };

  const deadline = Date.now() + readyTimeoutMs;
  let socket = null;
  while (!socket) {
    if (exited) {
      await stop({ keep: true });
      throw new Error(`nuggetd exited before it was ready (code ${exited.code}, signal ${exited.signal}); logs in ${logDir}`);
    }
    if (Date.now() > deadline) {
      await stop({ keep: true });
      throw new Error(`nuggetd did not log "nuggetd ready" within ${readyTimeoutMs} ms; logs in ${logDir}`);
    }
    socket = readyLine(logDir);
    if (!socket) await sleep(100);
  }
  // A reused home's log already has an earlier "nuggetd ready": wait until this daemon answers.
  for (;;) {
    try {
      await rpc(socket, "status");
      break;
    } catch (e) {
      if (exited || Date.now() > deadline) {
        await stop({ keep: true });
        throw new Error(`nuggetd did not answer status within ${readyTimeoutMs} ms (${e.message}); logs in ${logDir}`);
      }
      await sleep(100);
    }
  }
  return { home, socket, logDir, proc, stop, exited: () => exited };
}

/** Add the corpus as a `files` source. Returns the `SourceInfo`. */
export function addCorpusSource(socket, roots = [CORPUS]) {
  return rpc(socket, "sources.add", {
    id: null,
    kind: "files",
    name: "Fixture corpus",
    settings: { roots, include: [], exclude: [] },
  });
}

/**
 * Poll until extraction has settled: more than `minEntities` entities, no pending items, and the
 * item, entity and fact counts unchanged across two polls. Throws with the last counts on timeout.
 */
export async function waitForKnowledge(socket, { minEntities = 50, timeoutMs = 180_000, log = console.log } = {}) {
  const deadline = Date.now() + timeoutMs;
  let prev = "";
  let last = null;
  for (;;) {
    const [st, ks] = await Promise.all([rpc(socket, "status"), rpc(socket, "knowledge.status")]);
    last = { items: st.items, entities: ks.entities, claims: ks.claims, pending_items: ks.pending_items };
    const key = JSON.stringify(last);
    if (ks.entities > minEntities && ks.pending_items === 0 && key === prev) return { status: st, knowledge: ks };
    if (key !== prev) log(`  indexing: ${key}`);
    prev = key;
    if (Date.now() > deadline) {
      throw new Error(`knowledge did not settle within ${timeoutMs} ms (want entities > ${minEntities}, pending_items == 0); last ${key}`);
    }
    await sleep(750);
  }
}
