// GG6 on macOS / Linux: the INSTALLED nuggetd (from the release dry-run bundle) on a fresh home
// with no config (exactly what a new user gets), the real frontend in Chromium through
// apps/desktop/e2e/bridge.mjs, a real model download, then days 2-7.
//
//   node qa/GG6/real-os/run-bridge.mjs --nuggetd <installed nuggetd> [--days all|none] [--soak-min 40]
//   node qa/GG6/real-os/run-bridge.mjs --nuggetd <bin> --scenario diskfull --home <dir on a small fs>
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { parseArgs } from "node:util";
import { createRequire } from "node:module";
import { startBridge } from "../../../apps/desktop/e2e/bridge.mjs";
import { rpc } from "../../../apps/desktop/e2e/daemon.mjs";
import { dayBigFolder, dayModelCrash, dayRestart, daySleep, daySoak } from "./days.mjs";
import { chatQuestions, firstSearch, onboarding, shot } from "./journey.mjs";
import { check, copyCorpus, du, friction, log, mark, OUT, results, save, sleep, socketFromLogs, until } from "./lib.mjs";

// Playwright comes from the desktop app's devDependencies.
const { chromium } = createRequire(path.resolve(import.meta.dirname, "../../../apps/desktop/package.json"))("playwright");
const { values: args } = parseArgs({
  options: {
    nuggetd: { type: "string" },
    scenario: { type: "string", default: "full" },
    days: { type: "string", default: "all" },
    "soak-min": { type: "string", default: "40" },
    "big-files": { type: "string", default: "20000" },
    home: { type: "string" },
    ai: { type: "string", default: "local" },
  },
});
const BIN = path.resolve(args.nuggetd);
const work = fs.mkdtempSync(path.join(os.tmpdir(), "gg6-"));
const home = args.home ? path.resolve(args.home) : path.join(work, "nugget-home");
fs.mkdirSync(home, { recursive: true });
const logDir = path.join(home, "data", "logs");
results.nuggetd = BIN;
results.home = home;

let proc = null;
let socket = null;
const readyCount = () => {
  if (!fs.existsSync(logDir)) return 0;
  return fs.readdirSync(logDir).reduce((n, f) => n + (fs.readFileSync(path.join(logDir, f), "utf8").match(/nuggetd ready/g) || []).length, 0);
};

async function startD() {
  const before = readyCount();
  const out = fs.openSync(path.join(OUT, "nuggetd.out.log"), "a");
  // A real first run: no config.toml (the daemon writes its default), the OS keychain if any.
  proc = spawn(BIN, ["--home", home], { env: { ...process.env, NUGGET_LOG: "info" }, stdio: ["ignore", out, out] });
  let exited = null;
  proc.on("exit", (c, s) => (exited = { c, s }));
  await until(
    "nuggetd ready",
    async () => {
      if (exited) {
        const e = new Error(`nuggetd exited ${JSON.stringify(exited)}`);
        e.fatal = true;
        throw e;
      }
      return [readyCount() > before, readyCount()];
    },
    120_000,
    200,
  );
  socket = socketFromLogs(logDir);
  await rpc(socket, "status", {});
  return socket;
}
async function stopD() {
  if (!proc || proc.exitCode !== null) return null;
  const t0 = Date.now();
  proc.kill("SIGTERM");
  for (let i = 0; i < 300 && proc.exitCode === null && proc.signalCode === null; i++) await sleep(100);
  const clean = proc.exitCode !== null || proc.signalCode !== null;
  if (!clean) proc.kill("SIGKILL");
  const logs = fs.readdirSync(logDir).map((f) => fs.readFileSync(path.join(logDir, f), "utf8")).join("");
  const stopped = /"stopped"/.test(logs.slice(-20000));
  log(`nuggetd stop: ${Date.now() - t0} ms, exit ${proc.exitCode} ${proc.signalCode}, logged stopped: ${stopped}`);
  return { ms: Date.now() - t0, exit: proc.exitCode, signal: proc.signalCode, logged_stopped: stopped };
}

async function full() {
  const folder = path.join(work, "Documents", "Jordan");
  results.corpus_files = copyCorpus(folder);
  mark("start");
  await startD();
  mark("daemon_ready");
  const st0 = await rpc(socket, "status", {});
  results.status_first = st0;
  results.providers_first = await rpc(socket, "providers.list", {}).catch((e) => e.message);
  results.config_written = fs.existsSync(path.join(home, "config", "config.toml")) ? fs.readFileSync(path.join(home, "config", "config.toml"), "utf8") : null;
  results.models = await rpc(socket, "models.list", {}).catch((e) => e.message);

  const bridge = await startBridge({ socket, pickFolders: [folder] });
  const browser = await chromium.launch();
  const ctx = await browser.newContext({ viewport: { width: 1080, height: 720 } });
  const page = await ctx.newPage();
  bridge.deliverEvents(page);
  await page.goto(`${bridge.url}index.html`);
  try {
    await onboarding(page, { folder, ai: args.ai });
    await firstSearch(page);
    // Before any question: what Status says about the model.
    results.runtime_after_onboarding = await rpc(socket, "runtime.status", {}).catch((e) => e.message);
    if (args.ai !== "later") await chatQuestions(page, { label: "chat_day1" });
  } catch (e) {
    log(`journey aborted: ${e.stack}`);
    await shot(page, "FAIL-journey");
  }
  results.data_mb_day1 = Math.round(du(path.join(home, "data")) / 2 ** 20);
  save();

  if (args.days !== "none") {
    const dctx = {
      socket: () => socket,
      restart: async () => {
        results.days.stop = await stopD();
        await startD();
      },
      dataDir: path.join(home, "data"),
      folder,
      workDir: work,
    };
    results.days = results.days ?? {};
    await dayRestart(dctx);
    // What the open app shows after the daemon restarted under it.
    await page.reload().catch(() => {});
    await sleep(5000);
    await shot(page, "after-daemon-restart");
    await dayModelCrash(dctx);
    await daySleep(dctx, 180);
    await daySoak(dctx, Number(args["soak-min"]));
    await dayBigFolder(dctx, { files: Number(args["big-files"]) });
    await check("day 7: the questions still answer after the week", async () => {
      await page.reload();
      await chatQuestions(page, { label: "chat_day7" });
    });
  }
  await browser.close();
  await bridge.close?.();
  results.final_stop = await stopD();
  results.data_mb_end = Math.round(du(path.join(home, "data")) / 2 ** 20);
}

/** The disk fills up while the model downloads (the home is on a small filesystem). */
async function diskfull() {
  await startD();
  const models = await rpc(socket, "models.list", {});
  const rec = models.find((m) => m.recommended && m.tiers?.includes("synthesis")) ?? models.find((m) => m.recommended);
  results.diskfull = { model: rec?.id, size_bytes: rec?.size_bytes, free_before: os.freemem() };
  const { rpcStream } = await import("../../../apps/desktop/e2e/daemon.mjs");
  await check("disk full during the model download: a clear error, nothing half-installed, daemon healthy", async () => {
    let last = null;
    const r = await rpcStream(
      socket,
      "models.install",
      { id: rec.id },
      (_res, notes) => {
        const p = notes.filter((n) => n.method === "models.progress").pop()?.params;
        last = p ?? last;
        return p && (p.phase === "failed" || p.phase === "installed");
      },
      20 * 60_000,
    ).catch((e) => ({ error: e.message }));
    results.diskfull.last_progress = last;
    results.diskfull.rpc = r.error ?? null;
    const st = await rpc(socket, "status", {}).catch((e) => ({ error: e.message }));
    results.diskfull.status_after = st;
    results.diskfull.models_dir_mb = Math.round(du(path.join(home, "data", "models")) / 2 ** 20);
    results.diskfull.free_listing = fs.readdirSync(path.join(home, "data", "models"), { recursive: true }).slice(0, 30);
    const search = await rpc(socket, "search", { query: "anything", limit: 3 }).catch((e) => ({ error: e.message }));
    results.diskfull.search_after = search?.error ?? "ok";
    log(`diskfull: ${JSON.stringify(results.diskfull).slice(0, 1500)}`);
    if (last?.phase !== "failed") throw new Error(`expected a failed install, got ${JSON.stringify(last)}`);
    if (!/space|disk|full/i.test(last.error ?? "")) friction(`a full disk during the model download is reported as: "${last.error}"`, { last });
    if (results.diskfull.models_dir_mb > 50) friction(`a failed download leaves ${results.diskfull.models_dir_mb} MB of partial files on a full disk`, {});
    if (st.error) throw new Error(`daemon unhealthy after disk full: ${st.error}`);
  });
  results.final_stop = await stopD();
}

try {
  if (args.scenario === "diskfull") await diskfull();
  else await full();
} catch (e) {
  log(`FATAL ${e.stack}`);
  results.fatal = String(e.stack);
} finally {
  if (proc && proc.exitCode === null) proc.kill("SIGKILL");
  // Copy the daemon logs (small) next to the results.
  if (fs.existsSync(logDir)) fs.cpSync(logDir, path.join(OUT, "daemon-logs"), { recursive: true });
  save();
  const failed = results.checks.filter((c) => !c.ok).length;
  log(`done: ${results.checks.length - failed}/${results.checks.length} checks passed, ${results.friction.length} friction notes`);
  process.exit(0);
}
