// A2 after install: the local llama-server fails (exits 1 at start / crashes after start / no
// runtime binary) - what do Chat (IPC + UI), Ask and Status say? Plus "Decide later": what Chat,
// Ask and Double Tap say with no AI, and where the user can set AI up afterwards.
// Run from repo root: node qa/GG6/a2-ai-setup/runtime-later.mjs
import fs from "node:fs";
import path from "node:path";
import { startBridge } from "../../../apps/desktop/e2e/bridge.mjs";
import { CORPUS, addCorpusSource, rpc, rpcStream } from "../../../apps/desktop/e2e/daemon.mjs";
import { newContext, t } from "../../../apps/desktop/e2e/harness.mjs";
import { AI_CONFIG, BIN, NO_RT_CONFIG, SCRATCH, log, modelBytes, playwright, signManifest, sleep, snap, startFaultServer, startHome, writeResults } from "./lib.mjs";

const results = { runtime: {}, later: {} };
const MiB = 1024 * 1024;

const wrapper = (name, args) => {
  const p = path.join(SCRATCH, `llama-${name}.sh`);
  fs.writeFileSync(p, `#!/bin/sh\nexec ${BIN.fake} ${args} "$@"\n`, { mode: 0o755 });
  return p;
};
const VARIANTS = {
  "exit-1": wrapper("exit1", "--fake-exit-immediately 1"),
  "crash-after-start": wrapper("crash", "--fake-crash-after-ms 300"),
  "slow-load-120s": wrapper("slow", "--fake-start-delay-ms 120000"),
};

async function askOnce(socket, method, params) {
  const t0 = Date.now();
  try {
    const { result, notes } = await rpcStream(socket, method, params, (r, ns) => ns.some((n) => n.method === "chat.done"), 150_000);
    const done = notes.find((n) => n.method === "chat.done")?.params;
    return { ms: Date.now() - t0, provider: result.provider, text: notes.filter((n) => n.method === "chat.delta").map((n) => n.params.text).join("").slice(0, 200), done_error: done?.error ?? null };
  } catch (e) {
    return { ms: Date.now() - t0, error: e.message.slice(0, 400) };
  }
}

const browser = await playwright.chromium.launch();
try {
  for (const [name, server] of Object.entries(VARIANTS)) {
    const bytes = modelBytes(2 * MiB, 7);
    const files = {};
    const srv = await startFaultServer(files, { bps: 64 * MiB });
    const dir = fs.mkdtempSync(path.join(SCRATCH, "rt-"));
    const s = signManifest(dir, [{ id: "qa-chat", bytes, minRam: 4, port: srv.port }]);
    files[s.paths["qa-chat"]] = bytes;
    const d = await startHome(BIN.e2e, { config: AI_CONFIG, env: { ...s.env, NUGGET_E2E_LLAMA_SERVER: server, NUGGET_E2E_RAM_GB: "16", NUGGET_E2E_GPU: "unknown" } });
    try {
      await rpcStream(d.socket, "models.install", { id: "qa-chat" }, (r, ns) => ns.some((n) => n.params?.phase === "installed" || n.params?.phase === "failed"), 60_000);
      const r = { chat: await askOnce(d.socket, "chat", { provider: null, tier: "synthesis", messages: [{ role: "user", text: "hello" }], items: [] }) };
      r.runtime_status = (await rpc(d.socket, "runtime.status", {})).map((x) => ({ model: x.model_id, role: x.role, state: x.state, error: x.error?.slice(0, 300) }));
      if (name === "exit-1") {
        r.ask = await askOnce(d.socket, "retrieve.ask", { query: "hello", budget_tokens: 2000 });
        const bridge = await startBridge({ socket: d.socket });
        const ctx = await newContext(browser);
        await ctx.addInitScript(() => {
          localStorage.setItem("nugget.onboarding.v1", JSON.stringify({ version: 1, step: "done", completed: [], skipped: [], ai: "local", finished: true }));
          localStorage.setItem("nugget.page", "chat");
        });
        const page = await ctx.newPage();
        bridge.deliverEvents(page);
        await page.goto(`${bridge.url}index.html`);
        await page.getByRole("textbox", { name: t("chat.message") }).fill("Hello?");
        await page.getByRole("button", { name: t("chat.send"), exact: true }).click();
        await sleep(15_000);
        r.ui_chat = (await page.locator("li.msg-assistant").last().innerText().catch(() => "")).slice(0, 400);
        r.ui_alerts = (await page.locator("[role=alert], .note-error").allTextContents()).map((x) => x.trim()).filter(Boolean);
        r.shot = await snap(page, "runtime-exit1-chat");
        await ctx.close(); await bridge.close();
      }
      results.runtime[name] = r;
      log(`${name}: ${JSON.stringify(r).slice(0, 600)}`);
    } finally {
      await d.stop(); await srv.close();
      fs.rmSync(d.home, { recursive: true, force: true });
    }
  }

  // Decide later.
  const d = await startHome(BIN.nuggetd, { config: NO_RT_CONFIG });
  try {
    await addCorpusSource(d.socket, [CORPUS]);
    await sleep(4000);
    results.later.providers = await rpc(d.socket, "providers.list", {});
    results.later.chat_rpc = await askOnce(d.socket, "chat", { provider: null, tier: "light", messages: [{ role: "user", text: "hello" }], items: [] });
    results.later.ask_rpc = await askOnce(d.socket, "retrieve.ask", { query: "Tidewater Billing vendor contract", budget_tokens: 2000 });
    results.later.draft_rpc = await askOnce(d.socket, "draft.begin", { context: { app: { id: "qa.mail", name: "Mail", window_title: "Re: hi" }, field: { role: "text_area", label: "Body", value: "" }, thread: [{ author: "Ada", text: "Any news?", from_me: false }], recipients: [], subject: "Re: hi" }, instruction: null, provider: null, max_tokens: null });
    const bridge = await startBridge({ socket: d.socket });
    const ctx = await newContext(browser);
    await ctx.addInitScript(() => localStorage.setItem("nugget.onboarding.v1", JSON.stringify({ version: 1, step: "done", completed: [], skipped: ["ai"], ai: "later", finished: true })));
    const page = await ctx.newPage();
    bridge.deliverEvents(page);
    await page.goto(`${bridge.url}index.html`);
    const go = async (key) => { await page.locator("nav .nav-item", { hasText: t(key) }).first().click(); await sleep(1500); return (await page.locator("main").innerText()).replace(/\s+/g, " ").slice(0, 600); };
    results.later.chat_page = await go("app.nav.chat");
    await page.getByRole("textbox", { name: t("chat.message") }).fill("What is the Tidewater Billing vendor contract?").catch(() => {});
    await page.getByRole("button", { name: t("chat.send"), exact: true }).click({ timeout: 3000 }).catch((e) => (results.later.chat_send = `send not possible: ${e.message.split("\n")[0]}`));
    await sleep(4000);
    results.later.chat_after_send = (await page.locator("main").innerText()).replace(/\s+/g, " ").slice(0, 800);
    results.later.chat_shot = await snap(page, "later-chat");
    results.later.search_page = await go("app.nav.search");
    results.later.doubletap_page = await go("app.nav.doubletap");
    results.later.models_page = await go("app.nav.models");
    results.later.status_page = await go("app.nav.status");
    results.later.sidebar_setup_button = await page.getByRole("button", { name: t("settings.setupAgain") }).count();
    await ctx.close(); await bridge.close();
  } finally {
    await d.stop();
    fs.rmSync(d.home, { recursive: true, force: true });
  }
} finally {
  await browser.close();
}
writeResults("runtime-later", results);
