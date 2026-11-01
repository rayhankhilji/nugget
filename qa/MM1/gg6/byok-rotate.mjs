// MM1 re-check of GG6-7 (a saved API key can't be changed or removed). Extends
// qa/GG6/a2-ai-setup/byok.mjs (whose rotate case only shows the `exists` refusal) with the fix's
// path: replace and remove without a restart, over IPC and in the UI (onboarding "Replace key",
// Models page "Your providers" Replace/Remove). Mock provider on 127.0.0.1 records which key
// each chat used. Synthetic keys only. NUGGET_KEYCHAIN=off as in byok.mjs (no Secret Service here).
// usage (repo root): A2_SCRATCH=<dir> TMPDIR=/tmp node qa/MM1/gg6/byok-rotate.mjs
import fs from "node:fs";
import http from "node:http";
import path from "node:path";
import { startBridge } from "../../../apps/desktop/e2e/bridge.mjs";
import { rpc } from "../../../apps/desktop/e2e/daemon.mjs";
import { newContext, t } from "../../../apps/desktop/e2e/harness.mjs";
import { BIN, NO_RT_CONFIG, allLogs, log, playwright, sleep, startHome } from "../../GG6/a2-ai-setup/lib.mjs";

const OLD = "sk-qa-old-0123456789", NEW = "sk-qa-new-9876543210", NEW2 = "sk-qa-ui-5555555555";
const used = [];
const srv = http.createServer((req, res) => {
  const auth = (req.headers.authorization || "").replace("Bearer ", "");
  const ok = [OLD, NEW, NEW2].includes(auth);
  const json = (s, b) => res.writeHead(s, { "Content-Type": "application/json" }).end(JSON.stringify(b));
  if (req.url.endsWith("/models")) return ok ? json(200, { object: "list", data: [{ id: "qa-model" }] }) : json(401, { error: { message: "Incorrect API key" } });
  if (req.url.endsWith("/chat/completions")) {
    used.push(auth);
    if (!ok) return json(401, { error: { message: "Incorrect API key" } });
    return json(200, { id: "x", object: "chat.completion", model: "qa-model", choices: [{ index: 0, message: { role: "assistant", content: `key=${auth.slice(6, 9)}` }, finish_reason: "stop" }], usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 } });
  }
  json(404, {});
});
await new Promise((r) => srv.listen(0, "127.0.0.1", r));
const BASE = `http://127.0.0.1:${srv.address().port}/v1`;
const R = { ipc: {}, ui: {} };
const d = await startHome(BIN.nuggetd, { config: NO_RT_CONFIG });
const chat = async () => {
  const n = used.length;
  const r = await rpc(d.socket, "chat", { provider: "custom", tier: "light", messages: [{ role: "user", text: "hi" }], items: [] }).then((x) => ({ provider: x.provider }), (e) => ({ error: e.message.slice(0, 200) }));
  await sleep(300);
  return { ...r, key_used: used.slice(n).map((k) => k.slice(0, 9)) };
};
const ids = async () => (await rpc(d.socket, "providers.list", {})).map((p) => p.id);
const cfg = () => fs.readFileSync(path.join(d.home, "config/config.toml"), "utf8");
try {
  R.ipc.save = await rpc(d.socket, "ai.key.save", { name: "custom", kind: "openai_compatible", base_url: BASE, api_key: OLD, model: "qa-model" });
  R.ipc.chat_after_save = await chat();
  R.ipc.update_bad = await rpc(d.socket, "ai.key.update", { name: "custom", api_key: "sk-qa-revoked" }).catch((e) => ({ rpc_error: e.message }));
  R.ipc.chat_after_bad_update = await chat();
  R.ipc.update_new = await rpc(d.socket, "ai.key.update", { name: "custom", api_key: NEW }).catch((e) => ({ rpc_error: e.message }));
  R.ipc.chat_after_update = await chat();
  R.ipc.saved_list = await rpc(d.socket, "ai.key.saved", {}).catch((e) => ({ rpc_error: e.message }));
  R.ipc.update_missing = await rpc(d.socket, "ai.key.update", { name: "nope", api_key: NEW }).catch((e) => ({ rpc_error: e.message }));
  R.ipc.remove = await rpc(d.socket, "ai.key.remove", { name: "custom" }).catch((e) => ({ rpc_error: e.message }));
  R.ipc.providers_after_remove = await ids();
  R.ipc.config_mentions_custom_after_remove = /id\s*=\s*"custom"/.test(cfg());
  R.ipc.chat_after_remove = await chat();
  R.ipc.remove_again = await rpc(d.socket, "ai.key.remove", { name: "custom" }).catch((e) => ({ rpc_error: e.message }));
  log("ipc", JSON.stringify(R.ipc));

  // UI: onboarding BYOK save, save again with a new key -> "Replace key" -> used at once.
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
  const notes = async () => (await page.locator("form.ob-byok-fields [role=status], form.ob-byok-fields [role=alert]").allTextContents()).map((s) => s.trim()).filter(Boolean);
  const idle = () => page.waitForFunction(() => document.querySelector("form.ob-byok-fields")?.getAttribute("aria-busy") !== "true", null, { timeout: 60_000 });
  await field("baseUrl").fill(BASE); await field("key").fill(OLD); await field("model").fill("qa-model");
  await page.getByRole("button", { name: t("onboarding.byok.form.save"), exact: true }).click(); await idle(); await sleep(300);
  R.ui.save = await notes();
  await field("key").fill(NEW2);
  await page.getByRole("button", { name: t("onboarding.byok.form.save"), exact: true }).click(); await idle(); await sleep(300);
  R.ui.save_again = await notes();
  const rep = page.getByRole("button", { name: t("onboarding.byok.form.replace"), exact: true });
  R.ui.replace_button_visible = await rep.isVisible();
  if (R.ui.replace_button_visible) { await rep.click(); await idle(); await sleep(300); R.ui.after_replace = await notes(); }
  R.ui.chat_after_ui_replace = await chat();
  // Models page: Remove from "Your providers".
  const p2 = await ctx.newPage();
  bridge.deliverEvents(p2);
  await p2.addInitScript(() => { localStorage.setItem("nugget.onboarding.v1", JSON.stringify({ version: 1, step: "done", completed: [], skipped: [], ai: "byok", finished: true })); localStorage.setItem("nugget.page", "models"); });
  await p2.goto(`${bridge.url}index.html`);
  const section = p2.locator("section", { has: p2.getByRole("heading", { level: 2, name: t("models.providers.title"), exact: true }) });
  await section.waitFor();
  const card = section.locator('li[data-provider-id="custom"]');
  R.ui.models_card_text = (await card.innerText({ timeout: 10_000 }).catch((e) => `ERR ${e.message.split("\n")[0]}`)).replace(/\s+/g, " ");
  await card.getByRole("button", { name: t("models.providers.remove"), exact: false }).click();
  await p2.locator("dialog[open]").getByRole("button", { name: t("models.providers.remove"), exact: true }).click();
  await sleep(1500);
  R.ui.models_after_remove = (await section.innerText()).replace(/\s+/g, " ").slice(0, 300);
  R.ui.providers_after_ui_remove = await ids();
  R.ui.chat_after_ui_remove = await chat();
  await ctx.close(); await bridge.close(); await browser.close();
  R.keys_in_logs = [OLD, NEW, NEW2].filter((k) => allLogs(d.home).includes(k));
  R.keys_in_config = [OLD, NEW, NEW2].filter((k) => cfg().includes(k));
} catch (e) { R.exception = e.message.split("\n")[0]; }
finally { await d.stop(); srv.close(); }
console.log(JSON.stringify(R, null, 1));
