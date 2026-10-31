// A2 download failure modes, over the daemon's IPC (models.install + models.progress), with a
// fault-injecting mock download server and nuggetd-e2e (throwaway-signed manifest).
// Run from repo root: node qa/GG6/a2-ai-setup/downloads.mjs [case,case,...]
// Cases: baseline drop-once server-down stall sha-mismatch http404 http500 offline html cancel
//        daemon-restart disk-preflight disk-full
import { execSync } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import path from "node:path";
import { rpc } from "../../../apps/desktop/e2e/daemon.mjs";
import { AI_CONFIG, BIN, SCRATCH, closedPort, dirBytes, log, modelBytes, signManifest, sleep, startFaultServer, startHome, writeResults } from "./lib.mjs";

const MiB = 1024 * 1024;
let rid = 1e6;

/** models.install on its own connection; resolves when installed/failed, or on timeout / abort(). */
function install(socket, id, { timeoutMs = 240_000, onProgress = () => {} } = {}) {
  const t0 = Date.now();
  const conn = net.connect(socket);
  const progress = [];
  let buf = "";
  let reply = null;
  let resolveFn;
  const done = new Promise((r) => (resolveFn = r));
  const finish = (how) => { clearTimeout(timer); conn.destroy(); resolveFn({ how, ms: Date.now() - t0, reply, progress, last: progress.at(-1) ?? null }); };
  const timer = setTimeout(() => finish("timeout"), timeoutMs);
  conn.setEncoding("utf8");
  conn.on("connect", () => conn.write(JSON.stringify({ jsonrpc: "2.0", id: ++rid, method: "models.install", params: { id } }) + "\n"));
  conn.on("data", (c) => {
    buf += c;
    let nl;
    while ((nl = buf.indexOf("\n")) >= 0) {
      const line = buf.slice(0, nl); buf = buf.slice(nl + 1);
      if (!line.trim()) continue;
      const m = JSON.parse(line);
      if (m.id === rid || m.result !== undefined || m.error) { reply = m.error ? { error: m.error } : { result: m.result }; if (m.error) finish("rpc-error"); continue; }
      if (m.method === "models.progress") {
        const p = { ...m.params, at_ms: Date.now() - t0 };
        progress.push(p);
        onProgress(p);
        if (p.phase === "installed" || p.phase === "failed") finish(p.phase);
      }
    }
  });
  conn.on("error", () => finish("conn-error"));
  conn.on("close", () => finish("closed"));
  return { done, abort: () => finish("aborted-by-client") };
}

const summarize = (r) => ({ how: r.how, ms: r.ms, reply: r.reply, n_progress: r.progress.length, last: r.last, max_pct: Math.max(0, ...r.progress.map((p) => (p.total ? Math.round((100 * p.downloaded) / p.total) : 0))) });

/** One daemon + one model + a fault server. */
async function setup(name, { size = 8 * MiB, bps = 2 * MiB, fault, home, minRam = 4, config = AI_CONFIG } = {}) {
  const bytes = modelBytes(size, size);
  const files = {};
  const srv = await startFaultServer(files, { bps, fault });
  const dir = fs.mkdtempSync(path.join(SCRATCH, `dl-${name}-`));
  const s = signManifest(dir, [{ id: `qa-${name}`, bytes, minRam, port: srv.port }]);
  files[s.paths[`qa-${name}`]] = bytes;
  const env = { ...s.env, NUGGET_E2E_LLAMA_SERVER: BIN.fake, NUGGET_E2E_RAM_GB: "16", NUGGET_E2E_GPU: "unknown" };
  const d = await startHome(BIN.e2e, { home, config, env });
  return { id: `qa-${name}`, bytes, srv, d, env, dir };
}

const stagingOf = (home) => {
  try { return execSync(`find ${home} -type d -name staging`).toString().trim().split("\n").filter(Boolean)[0] ?? null; } catch { return null; }
};

const CASES = {
  async baseline() {
    const c = await setup("baseline");
    const r = await install(c.d.socket, c.id).done;
    return { c, out: { ...summarize(r), requests: c.srv.stats.requests.length } };
  },
  async "drop-once"() {
    const c = await setup("dropOnce", { fault: (q) => (q.n <= 1 && q.method === "GET" ? { dropAfter: 3 * MiB } : null) });
    const r = await install(c.d.socket, c.id).done;
    return { c, out: { ...summarize(r), requests: c.srv.stats.requests.map((q) => ({ n: q.n, range: q.range, status: q.status, dropped: q.dropped })) } };
  },
  async "server-down"() {
    // The server disappears at ~40% and stays down: how long until the user hears about it?
    const c = await setup("serverDown", { bps: 1 * MiB });
    const port = c.srv.port;
    const h = install(c.d.socket, c.id, {
      onProgress: (p) => { if (p.downloaded > 3 * MiB && !c.closed) { c.closed = Date.now(); c.srv.close(); } },
    });
    const r = await h.done;
    const staged = dirBytes(stagingOf(c.d.home) || "/nonexistent");
    // Server comes back on the same port; Retry.
    const files = {}; files[Object.keys(c.srv.stats.requests.reduce((a, q) => ((a[q.path] = 1), a), {}))[0]] = c.bytes;
    let srv2 = null;
    try { srv2 = await startFaultServerOn(port, files); } catch (e) { log(`server-down: cannot rebind ${port}: ${e.message}`); }
    let retry = null;
    if (srv2) { retry = summarize(await install(c.d.socket, c.id).done); retry.requests = srv2.stats.requests.map((q) => ({ range: q.range, status: q.status })); await srv2.close(); }
    return { c, out: { ...summarize(r), ms_after_server_down: c.closed ? Date.now() - c.closed : null, staged_bytes_after_fail: staged, retry } };
  },
  async stall() {
    const c = await setup("stall", { fault: (q) => (q.n <= 1 && q.method === "GET" ? { stallAfter: 3 * MiB } : null) });
    const r = await install(c.d.socket, c.id, { timeoutMs: 300_000 }).done;
    return { c, out: { ...summarize(r), requests: c.srv.stats.requests.map((q) => ({ n: q.n, range: q.range, status: q.status, stalled: q.stalled, at: q.at - c.srv.stats.requests[0].at })) } };
  },
  async "sha-mismatch"() {
    const c = await setup("sha", { fault: (q) => (q.method === "GET" ? { wrongBytes: true } : null) });
    const r = await install(c.d.socket, c.id).done;
    const st = stagingOf(c.d.home);
    return { c, out: { ...summarize(r), staging_dir: st, staging_bytes_after: dirBytes(st || "/nonexistent") } };
  },
  async http404() {
    const c = await setup("h404", { fault: () => ({ status: 404 }) });
    const r = await install(c.d.socket, c.id).done;
    return { c, out: { ...summarize(r), requests: c.srv.stats.requests.length } };
  },
  async http500() {
    const c = await setup("h500", { fault: () => ({ status: 500 }) });
    const r = await install(c.d.socket, c.id, { timeoutMs: 300_000 }).done;
    return { c, out: { ...summarize(r), requests: c.srv.stats.requests.length } };
  },
  async offline() {
    const c = await setup("offline");
    await c.srv.close();
    const r = await install(c.d.socket, c.id, { timeoutMs: 300_000 }).done;
    return { c, out: summarize(r) };
  },
  async html() {
    const c = await setup("html", { fault: () => ({ html: true }) });
    const r = await install(c.d.socket, c.id).done;
    return { c, out: summarize(r) };
  },
  async cancel() {
    const c = await setup("cancel", { size: 16 * MiB, bps: 2 * MiB });
    const h = install(c.d.socket, c.id, { onProgress: (p) => { if (p.downloaded > 5 * MiB) h.abort(); } });
    const r = await h.done;
    await sleep(1500);
    const st = stagingOf(c.d.home);
    const staged = dirBytes(st || "/nonexistent");
    const servedBefore = c.srv.stats.served;
    const list = await rpc(c.d.socket, "models.list", {});
    const again = await install(c.d.socket, c.id).done;
    return {
      c,
      out: {
        cancel: summarize(r), staged_bytes_kept: staged, models_list_installed: list.find((m) => m.id === c.id)?.installed,
        reinstall: summarize(again), reinstall_bytes_served: c.srv.stats.served - servedBefore, model_bytes: c.bytes.length,
        reinstall_requests: c.srv.stats.requests.slice(-3).map((q) => q.range),
      },
    };
  },
  async "daemon-restart"() {
    const c = await setup("restart", { size: 48 * MiB, bps: 4 * MiB });
    const h = install(c.d.socket, c.id, { onProgress: (p) => { if (p.downloaded > 24 * MiB && !c.killed) { c.killed = true; c.d.stop("SIGTERM"); } } });
    const r = await h.done;
    await sleep(2000);
    const st = stagingOf(c.d.home);
    const staged = dirBytes(st || "/nonexistent");
    const d2 = await startHome(BIN.e2e, { home: c.d.home, env: c.env });
    c.d2 = d2;
    const list = await rpc(d2.socket, "models.list", {});
    const autoResumed = c.srv.stats.requests.length;
    await sleep(3000);
    const servedBefore = c.srv.stats.served;
    const nReqBefore = c.srv.stats.requests.length;
    const again = await install(d2.socket, c.id).done;
    return {
      c,
      out: {
        first: summarize(r), staged_bytes_on_disk: staged, after_restart_listed_installed: list.find((m) => m.id === c.id)?.installed,
        auto_resumed_after_restart: c.srv.stats.requests.length !== autoResumed,
        reinstall: summarize(again), reinstall_bytes_served: c.srv.stats.served - servedBefore, model_bytes: c.bytes.length,
        reinstall_ranges: c.srv.stats.requests.slice(nReqBefore).map((q) => q.range),
      },
    };
  },
  async "disk-preflight"() {
    const mnt = fs.mkdtempSync(path.join(SCRATCH, "tmpfs20-"));
    execSync(`mount -t tmpfs -o size=20m tmpfs ${mnt}`);
    try {
      const c = await setup("preflight", { home: mnt });
      c.mnt = mnt;
      const fit = await rpc(c.d.socket, "models.fit", {});
      const r = await install(c.d.socket, c.id).done;
      return { c, out: { ...summarize(r), fit_free_disk: fit.free_disk_bytes, fits_disk: fit.models.map((m) => m.fits_disk), requests: c.srv.stats.requests.length } };
    } catch (e) { execSync(`umount -l ${mnt}`); throw e; }
  },
  async "disk-full"() {
    // Room for the preflight (model + 10% + 1 GiB), then the disk fills up mid-download.
    const mnt = fs.mkdtempSync(path.join(SCRATCH, "tmpfsfull-"));
    execSync(`mount -t tmpfs -o size=1200m tmpfs ${mnt}`);
    try {
      const c = await setup("diskfull", { home: mnt, size: 64 * MiB, bps: 8 * MiB });
      c.mnt = mnt;
      const filler = path.join(mnt, "filler.bin");
      const h = install(c.d.socket, c.id, {
        onProgress: (p) => {
          if (p.downloaded > 16 * MiB && !c.filled) {
            c.filled = true;
            const free = Number(execSync(`df -B1 --output=avail ${mnt} | tail -1`).toString().trim());
            execSync(`fallocate -l ${Math.max(0, free - 2 * MiB)} ${filler}`);
          }
        },
      });
      const r = await h.done;
      const st = stagingOf(mnt);
      const staged = dirBytes(st || "/nonexistent");
      fs.rmSync(filler, { force: true });
      const again = await install(c.d.socket, c.id).done;
      return { c, out: { first: summarize(r), staged_bytes_after_fail: staged, after_freeing_space_retry: summarize(again) } };
    } catch (e) { execSync(`umount -l ${mnt}`); throw e; }
  },
};

/** Rebind a plain (no-fault) server on a given port. */
async function startFaultServerOn(port, files) {
  const s = await startFaultServer(files, { bps: 8 * MiB });
  // startFaultServer picks a random port; re-listen on the wanted one.
  await new Promise((r) => s.server.close(r));
  await new Promise((res, rej) => { s.server.once("error", rej); s.server.listen(port, "127.0.0.1", res); });
  return s;
}

const wanted = (process.argv[2] || Object.keys(CASES).join(",")).split(",");
const results = {};
await Promise.all(
  wanted.map(async (name) => {
    const t0 = Date.now();
    let c;
    try {
      const r = await CASES[name]();
      c = r.c;
      results[name] = { ok: true, wall_ms: Date.now() - t0, ...r.out };
      log(`${name}: ${JSON.stringify(results[name]).slice(0, 400)}`);
    } catch (e) {
      results[name] = { ok: false, wall_ms: Date.now() - t0, error: String(e.stack || e) };
      log(`${name}: ERROR ${e.message}`);
    } finally {
      if (c) {
        // Grep logs for anything interesting about the failure.
        results[name].log_tail = (() => { try { return execSync(`grep -h -i "warn\\|error" ${c.d.home}/data/logs/* 2>/dev/null | grep -i "model\\|download\\|install\\|space\\|disk" | tail -4`).toString().split("\n").map((l) => l.slice(0, 400)); } catch { return []; } })();
        await c.d.stop().catch(() => {});
        await c.d2?.stop().catch(() => {});
        await c.srv.close().catch(() => {});
        if (c.mnt) execSync(`umount -l ${c.mnt}`);
        else fs.rmSync(c.d.home, { recursive: true, force: true });
      }
    }
  }),
);
const file = `downloads${process.argv[2] ? "-" + wanted.join("_") : ""}`;
writeResults(file, results);
