// A2 bring-your-own-key: realistic mistakes against a mock provider on 127.0.0.1 (no real
// provider calls), over IPC (ai.key.test / ai.key.save) and through the onboarding form (UI text).
// Run from repo root: node qa/GG6/a2-ai-setup/byok.mjs
import { execSync } from "node:child_process";
import fs from "node:fs";
import http from "node:http";
import path from "node:path";
import { startBridge } from "../../../apps/desktop/e2e/bridge.mjs";
import { rpc } from "../../../apps/desktop/e2e/daemon.mjs";
import { newContext, t } from "../../../apps/desktop/e2e/harness.mjs";
import { BIN, NO_RT_CONFIG, allLogs, log, playwright, sleep, snap, startHome, writeResults } from "./lib.mjs";

const GOOD = "sk-qa-good-0123456789";
const ANTH = "sk-ant-qa-good-0123456789";
const NEW = "sk-qa-rotated-9876543210";

function startMock() {
  const seen = [];
  const server = http.createServer(async (req, res) => {
    const u = new URL(req.url, "http://x");
    const auth = req.headers.authorization || "";
    const xkey = req.headers["x-api-key"] || "";
    seen.push({ path: u.pathname, method: req.method });
    const json = (s, b) => res.writeHead(s, { "Content-Type": "application/json" }).end(JSON.stringify(b));
    const list = { object: "list", data: [{ id: "qa-model" }, { id: "qa-model-large" }] };
    const p = u.pathname;
    if (p === "/good/v1/models") return [`Bearer ${GOOD}`, `Bearer ${NEW}`].includes(auth) ? json(200, list) : json(401, { error: { message: "Incorrect API key" } });
    if (p === "/good/v1/chat/completions") {
      if (![`Bearer ${GOOD}`, `Bearer ${NEW}`].includes(auth)) return json(401, { error: { message: "Incorrect API key" } });
      return json(200, { id: "x", object: "chat.completion", model: "qa-model", choices: [{ index: 0, message: { role: "assistant", content: "Hello from the QA mock provider." }, finish_reason: "stop" }], usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 } });
    }
    if (p === "/anth/v1/models") return xkey === ANTH ? json(200, { data: [{ id: "qa-claude", type: "model" }], has_more: false }) : json(401, { type: "error", error: { type: "authentication_error", message: "invalid x-api-key" } });
    if (p === "/r429/v1/models") return res.writeHead(429, { "Retry-After": "20", "Content-Type": "application/json" }).end('{"error":{"message":"Rate limit reached"}}');
    if (p === "/r500/v1/models") return json(500, { error: { message: "internal" } });
    if (p === "/slow/v1/models") { await sleep(30_000); return json(200, list); }
    if (p === "/portal/v1/models") return res.writeHead(200, { "Content-Type": "text/html" }).end("<html><body>Sign in to Guest Wi-Fi</body></html>");
    json(404, { error: { message: "Not found" } });
  });
  return new Promise((r) => server.listen(0, "127.0.0.1", () => r({ port: server.address().port, seen, close: () => new Promise((c) => { server.closeAllConnections(); server.close(c); }) })));
}

const mock = await startMock();
const B = `http://127.0.0.1:${mock.port}`;
const results = { test: {}, save: {}, ui: {} };
let d = await startHome(BIN.nuggetd, { config: NO_RT_CONFIG });
const home = d.home;
const test = async (name, req) => {
  const t0 = Date.now();
  const r = await rpc(d.socket, "ai.key.test", { model: null, ...req }, 60_000).catch((e) => ({ rpc_error: e.message }));
  results.test[name] = { ms: Date.now() - t0, ok: r.ok, reason: r.reason, message: r.message, models: r.models?.length, rpc_error: r.rpc_error };
  log(`test ${name}: ${JSON.stringify(results.test[name])}`);
};
try {
  // OpenAI-compatible
  await test("compat-good", { kind: "openai_compatible", base_url: `${B}/good/v1`, api_key: GOOD });
  await test("compat-trailing-slash", { kind: "openai_compatible", base_url: `${B}/good/v1/`, api_key: GOOD });
  await test("compat-missing-v1", { kind: "openai_compatible", base_url: `${B}/good`, api_key: GOOD });
  await test("compat-key-whitespace-around", { kind: "openai_compatible", base_url: `${B}/good/v1`, api_key: `  ${GOOD}\n` });
  await test("compat-key-newline-inside", { kind: "openai_compatible", base_url: `${B}/good/v1`, api_key: `sk-qa-good-01234\n56789` });
  await test("compat-anthropic-key", { kind: "openai_compatible", base_url: `${B}/good/v1`, api_key: ANTH });
  await test("compat-revoked-401", { kind: "openai_compatible", base_url: `${B}/good/v1`, api_key: "sk-qa-revoked" });
  await test("compat-429", { kind: "openai_compatible", base_url: `${B}/r429/v1`, api_key: GOOD });
  await test("compat-500", { kind: "openai_compatible", base_url: `${B}/r500/v1`, api_key: GOOD });
  await test("compat-slow-30s", { kind: "openai_compatible", base_url: `${B}/slow/v1`, api_key: GOOD });
  await test("compat-captive-portal-html", { kind: "openai_compatible", base_url: `${B}/portal/v1`, api_key: GOOD });
  await test("compat-http-not-loopback", { kind: "openai_compatible", base_url: "http://192.0.2.10/v1", api_key: GOOD });
  await test("compat-no-scheme", { kind: "openai_compatible", base_url: `127.0.0.1:${mock.port}/good/v1`, api_key: GOOD });
  await test("compat-model-typo", { kind: "openai_compatible", base_url: `${B}/good/v1`, api_key: GOOD, model: "qa-modle" });
  // Anthropic (base URL overridden to the mock)
  await test("anthropic-good", { kind: "anthropic", base_url: `${B}/anth`, api_key: ANTH });
  await test("anthropic-base-with-v1", { kind: "anthropic", base_url: `${B}/anth/v1`, api_key: ANTH });
  await test("anthropic-openai-key", { kind: "anthropic", base_url: `${B}/anth`, api_key: GOOD });
  // OpenAI (responses) kind against the mock
  await test("openai-good", { kind: "openai_responses", base_url: `${B}/good/v1`, api_key: GOOD });

  // Save, then what the user sees.
  const save = async (name, req) => {
    const r = await rpc(d.socket, "ai.key.save", req, 60_000).catch((e) => ({ rpc_error: e.message }));
    results.save[name] = r;
    log(`save ${name}: ${JSON.stringify(r)}`);
  };
  await save("first", { name: "custom", kind: "openai_compatible", base_url: `${B}/good/v1`, api_key: GOOD, model: "qa-model" });
  results.providers_before_restart = (await rpc(d.socket, "providers.list", {})).map((p) => ({ id: p.id, error: p.error }));
  await save("rotate-key-same-name", { name: "custom", kind: "openai_compatible", base_url: `${B}/good/v1`, api_key: NEW, model: "qa-model" });
  await save("bad-name", { name: "My OpenAI", kind: "openai_compatible", base_url: `${B}/good/v1`, api_key: GOOD, model: "qa-model" });
  const cfg = fs.readFileSync(path.join(home, "config/config.toml"), "utf8");
  results.config_providers = cfg.slice(cfg.indexOf("# Added by"));
  results.key_in_config = cfg.includes(GOOD);
  results.key_in_logs_before_restart = allLogs(home).includes(GOOD);
  results.key_anywhere_on_disk = (() => { try { return execSync(`grep -rl -a ${GOOD} ${home} || true`).toString().trim(); } catch { return "grep error"; } })();

  // Restart with NUGGET_KEYCHAIN=off (what the e2e does): is the saved key usable?
  await d.stop();
  d = await startHome(BIN.nuggetd, { home });
  results.providers_after_restart_keychain_off = (await rpc(d.socket, "providers.list", {})).map((p) => ({ id: p.id, error: p.error }));
  results.status_after_restart = (await rpc(d.socket, "status", {})).providers?.map((p) => ({ id: p.id, error: p.error }));
  results.chat_after_restart = await rpc(d.socket, "chat", { provider: null, tier: "light", messages: [{ role: "user", text: "hello" }], items: [] }).then((r) => r, (e) => ({ error: e.message }));
  await d.stop();

  // Real keychain path (no NUGGET_KEYCHAIN): on this Linux container there is no Secret Service.
  const d2 = await startHome(BIN.nuggetd, { config: NO_RT_CONFIG, keychainOff: false });
  results.save.keychain_on_no_secret_service = await rpc(d2.socket, "ai.key.save", { name: "custom", kind: "openai_compatible", base_url: `${B}/good/v1`, api_key: GOOD, model: "qa-model" }, 60_000).catch((e) => ({ rpc_error: e.message }));
  log(`keychain on: ${JSON.stringify(results.save.keychain_on_no_secret_service)}`);
  await d2.stop();
  fs.rmSync(d2.home, { recursive: true, force: true });

  // UI: what the onboarding form says for a few of the above.
  d = await startHome(BIN.nuggetd, { config: NO_RT_CONFIG });
  const browser = await playwright.chromium.launch();
  const bridge = await startBridge({ socket: d.socket });
  const ctx = await newContext(browser);
  const page = await ctx.newPage();
  bridge.deliverEvents(page);
  await page.goto(`${bridge.url}index.html`);
  await page.getByRole("heading", { level: 1, name: t("onboarding.welcome.heading") }).waitFor();
  await page.locator("footer.ob-actions").getByRole("button", { name: t("onboarding.continue"), exact: true }).click();
  await page.getByRole("radio", { name: new RegExp(`^${t("onboarding.ai.byok")}`) }).check();
  await page.getByLabel(t("onboarding.byok.provider"), { exact: true }).first().selectOption("compatible");
  const field = (k) => page.getByLabel(t(`onboarding.byok.form.${k}`), { exact: true });
  const uiCase = async (name, { base, key, model = "qa-model", action = "test", shot }) => {
    await field("baseUrl").fill(base);
    await field("key").fill(key);
    await field("model").fill(model);
    const t0 = Date.now();
    await page.getByRole("button", { name: t(`onboarding.byok.form.${action}`), exact: true }).click();
    await page.waitForFunction(() => document.querySelector("form.ob-byok-fields")?.getAttribute("aria-busy") !== "true", null, { timeout: 60_000 });
    await sleep(200);
    const notes = (await page.locator("form.ob-byok-fields [role=status], form.ob-byok-fields [role=alert]").allTextContents()).map((s) => s.trim()).filter(Boolean);
    results.ui[name] = { ms: Date.now() - t0, notes };
    log(`ui ${name}: ${JSON.stringify(results.ui[name])}`);
    if (shot) results.ui[name].shot = await snap(page, shot);
  };
  await uiCase("http-not-loopback", { base: "http://192.0.2.10/v1", key: GOOD, shot: "byok-http-wrongurl" });
  await uiCase("429", { base: `${B}/r429/v1`, key: GOOD });
  await uiCase("newline-in-key", { base: `${B}/good/v1`, key: "sk-qa-good-01234 56789" });
  await uiCase("model-typo", { base: `${B}/good/v1`, key: GOOD, model: "qa-modle" });
  await uiCase("slow", { base: `${B}/slow/v1`, key: GOOD });
  await uiCase("save", { base: `${B}/good/v1`, key: GOOD, action: "save" });
  await uiCase("save-again-rotated", { base: `${B}/good/v1`, key: NEW, action: "save", shot: "byok-rotate-exists" });
  await ctx.close(); await bridge.close(); await browser.close();
  await d.stop();
  results.key_in_logs_final = allLogs(home).includes(GOOD) || allLogs(home).includes(NEW);
} finally {
  await d.stop().catch(() => {});
  await mock.close();
}
writeResults("byok", results);
