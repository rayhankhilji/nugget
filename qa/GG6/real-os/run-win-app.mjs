// GG6 on Windows: the INSTALLED app (NSIS dry-run installer), driven like a user through its own
// WebView2 (the DevTools port opened with WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS), with the real
// sidecar nuggetd it starts, the default per-user data dir, a real model download, then days 2-7.
//
//   node qa/GG6/real-os/run-win-app.mjs --app "%LOCALAPPDATA%\Nugget\nugget-desktop.exe"
import { execFileSync, spawn } from "node:child_process";
import fs from "node:fs";
import { createRequire } from "node:module";
import os from "node:os";
import path from "node:path";
import { parseArgs } from "node:util";
import { rpc } from "../../../apps/desktop/e2e/daemon.mjs";
import { dayBigFolder, dayModelCrash, dayRestart, daySoak } from "./days.mjs";
import { chatQuestions, firstSearch, onboarding, shot } from "./journey.mjs";
import { check, copyCorpus, defaultDataDir, du, friction, log, mark, OUT, procs, results, save, sleep, socketFromLogs, until } from "./lib.mjs";

const { chromium } = createRequire(path.resolve(import.meta.dirname, "../../../apps/desktop/package.json"))("playwright");
const { values: args } = parseArgs({
  options: {
    app: { type: "string" },
    days: { type: "string", default: "all" },
    "soak-min": { type: "string", default: "40" },
    "big-files": { type: "string", default: "20000" },
  },
});
const APP = path.resolve(args.app);
const PORT = 9222;
const dataDir = defaultDataDir();
const logDir = path.join(dataDir, "logs");
results.app = APP;
results.data_dir = dataDir;
const work = fs.mkdtempSync(path.join(os.tmpdir(), "gg6-"));

let browser = null;
let socket = null;

async function launch(label) {
  const t0 = Date.now();
  const p = spawn(APP, [], {
    detached: true,
    stdio: "ignore",
    env: { ...process.env, WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${PORT}` },
  });
  p.unref();
  await until(
    "the app's WebView2 DevTools port",
    async () => {
      const r = await fetch(`http://127.0.0.1:${PORT}/json/version`).catch(() => null);
      return [!!r?.ok, r?.status];
    },
    180_000,
    500,
  );
  browser = await chromium.connectOverCDP(`http://127.0.0.1:${PORT}`);
  const page = await until(
    "the main window's page",
    async () => {
      const pages = browser.contexts().flatMap((c) => c.pages());
      const main = pages.find((pg) => /tauri\.localhost|^tauri:/.test(pg.url()) && !/overlay/.test(pg.url()));
      return [main, pages.map((pg) => pg.url())];
    },
    120_000,
    500,
  );
  page.setDefaultTimeout(20_000);
  await page.setViewportSize({ width: 1080, height: 720 }).catch(() => {});
  results[`launch_${label}_ms`] = Date.now() - t0;
  log(`${label}: app page ${page.url()} after ${Date.now() - t0} ms`);
  return page;
}

async function waitSocket() {
  socket = await until(
    "the daemon answers",
    async () => {
      const s = socketFromLogs(logDir);
      if (!s) return [false, "no ready line yet"];
      await rpc(s, "status", {}, 5000);
      return [s, s];
    },
    180_000,
    1000,
  );
  return socket;
}

function killAll() {
  for (const name of ["nugget-desktop.exe", "nuggetd.exe", "llama-server.exe"]) {
    try {
      execFileSync("taskkill", ["/F", "/T", "/IM", name], { stdio: "ignore" });
    } catch {}
  }
}

async function main() {
  const folder = path.join(os.homedir(), "Documents", "Jordan");
  results.corpus_files = copyCorpus(folder);
  mark("start");
  let page = await launch("first");
  mark("app_window_page");
  await waitSocket();
  mark("daemon_ready");
  results.status_first = await rpc(socket, "status", {});
  results.models = await rpc(socket, "models.list", {}).catch((e) => e.message);
  try {
    await onboarding(page, { folder, ai: "local" });
    await firstSearch(page);
    results.runtime_after_onboarding = await rpc(socket, "runtime.status", {}).catch((e) => e.message);
    await chatQuestions(page, { label: "chat_day1" });
  } catch (e) {
    log(`journey aborted: ${e.stack}`);
    await shot(page, "FAIL-journey");
  }
  results.data_mb_day1 = Math.round(du(dataDir) / 2 ** 20);
  save();

  if (args.days !== "none") {
    const ctx = {
      socket: () => socket,
      // A reboot on Windows: everything stops, the user opens Nugget again.
      restart: async () => {
        await browser?.close().catch(() => {});
        killAll();
        await sleep(3000);
        page = await launch("after_restart");
        await waitSocket();
      },
      dataDir,
      folder,
      workDir: work,
    };
    await dayRestart(ctx);
    await shot(page, "after-restart");
    await check("day 2: the app opens on the main window, not onboarding, after a restart", async () => {
      const onboardingShown = await page.getByRole("heading", { level: 1, name: /Welcome|Set up/i }).count();
      if (onboardingShown) throw new Error("onboarding shown again after a restart");
    });
    await dayModelCrash(ctx);
    await daySoak(ctx, Number(args["soak-min"]));
    await dayBigFolder(ctx, { files: Number(args["big-files"]) });
    await check("day 7: questions still answer", async () => {
      await page.reload();
      await chatQuestions(page, { label: "chat_day7" });
    });
  }

  // Close the window the way a user does, then see what keeps running.
  await check("quit: what keeps running after the window closes", async () => {
    await browser?.close().catch(() => {});
    try {
      execFileSync("taskkill", ["/IM", "nugget-desktop.exe"], { stdio: "ignore" }); // WM_CLOSE, like the X button
    } catch {}
    await sleep(10_000);
    const left = procs(/^(nugget-desktop|nuggetd|llama-server)$/i);
    results.after_close = left;
    if (left.some((p) => /nuggetd|llama/i.test(p.name))) friction(`after closing Nugget, ${left.map((p) => `${p.name} (${p.rss_mb} MB)`).join(", ")} keep running`, { left });
    return left;
  });
  results.data_mb_end = Math.round(du(dataDir) / 2 ** 20);
}

try {
  await main();
} catch (e) {
  log(`FATAL ${e.stack}`);
  results.fatal = String(e.stack);
} finally {
  if (fs.existsSync(logDir)) fs.cpSync(logDir, path.join(OUT, "daemon-logs"), { recursive: true });
  save();
  const failed = results.checks.filter((c) => !c.ok).length;
  log(`done: ${results.checks.length - failed}/${results.checks.length} checks passed, ${results.friction.length} friction notes`);
  process.exit(0);
}
