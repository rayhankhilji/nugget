// Models → "Your providers" against the mock OpenAI-compatible server: a saved key is used at once
// without a restart (GG6-7), "Replace key" swaps it (GG6-15), Remove (confirmed) leaves config.toml
// byte for byte as before (GG6-11), and an `env:VAR` reference works without a keychain.
import fs from "node:fs";
import path from "node:path";
import { startBridge } from "./bridge.mjs";
import { MODEL, startMockOpenAi } from "./byok.mjs";
import { FIRST_RUN_CONFIG, rpc, startDaemon } from "./daemon.mjs";
import { check, checkWire, DIST, exactName, expect, newContext, OUT, shot, stopDaemon, t, until } from "./harness.mjs";

const GOOD = "sk-e2e-keys-good";
const ROTATED = "sk-e2e-keys-rotated";
const ENV_VAR = "NUGGET_E2E_KEYS_KEY";
const NAME = "e2e-keys";

const fill = (key, vars) => Object.entries(vars).reduce((s, [k, v]) => s.replace(`{${k}}`, v), t(key));

export async function byokKeysScenario({ browser, bin }) {
  const mock = await startMockOpenAi([GOOD, ROTATED]);
  const base = `http://127.0.0.1:${mock.port}/v1`;
  const daemon = await startDaemon(bin, { stderrPath: path.join(OUT, "byok-keys-nuggetd.out.log"), config: FIRST_RUN_CONFIG, env: { [ENV_VAR]: GOOD } });
  const configFile = path.join(daemon.home, "config", "config.toml");
  const original = fs.readFileSync(configFile, "utf8");
  const ids = async () => (await rpc(daemon.socket, "providers.list", {})).map((p) => p.id);
  /** Chat with provider `NAME` and wait until the mock sees the request; returns its auth. */
  const chatAuth = async (what) => {
    const before = mock.requests.length;
    await rpc(daemon.socket, "chat", { provider: NAME, tier: "light", messages: [{ role: "user", text: "hello" }], items: [] });
    return until(`${what}: the chat reaches the mock server`, async () => {
      const r = mock.requests.slice(before).find((x) => x.path === "/v1/chat/completions");
      return [Boolean(r), r?.auth ?? mock.requests.slice(before)];
    });
  };
  let bridge;
  let ctx;
  const pageErrors = [];
  try {
    bridge = await startBridge({ socket: daemon.socket, dist: DIST });
    ctx = await newContext(browser, { onboarded: true, page: "models" });
    const page = await ctx.newPage();
    page.on("pageerror", (e) => pageErrors.push(e.message));
    bridge.deliverEvents(page);
    const section = page.locator("section", { has: page.getByRole("heading", { level: 2, name: t("models.providers.title"), exact: true }) });
    const field = (key) => section.getByLabel(t(`onboarding.byok.form.${key}`), { exact: true });
    const status = (key) => section.locator("[role=status]", { hasText: key });

    await check("byok-keys: a key saved on the Models page is used at once, without a restart", async () => {
      try {
        await page.goto(`${bridge.url}index.html`);
        await section.waitFor();
        await section.getByText(t("models.providers.empty"), { exact: true }).waitFor();
        await section.locator("summary", { hasText: t("models.providers.add") }).click();
        await field("kind").selectOption("compatible");
        await field("baseUrl").fill(base);
        await field("key").fill(GOOD);
        await field("model").fill(MODEL);
        await field("name").fill(NAME);
        await section.getByRole("button", { name: exactName("onboarding.byok.form.save") }).click();
        await status(t("onboarding.byok.form.saved")).waitFor();
        const card = section.locator(`li[data-provider-id="${NAME}"]`);
        await card.getByText(t("models.providers.inUse"), { exact: true }).waitFor();
        expect((await ids()).includes(NAME), `providers.list right after saving lacks ${NAME}: ${JSON.stringify(await ids())}`);
        const auth = await chatAuth("after saving");
        expect(auth === GOOD, `the chat after saving used the ${auth} key`);
        expect(!(await page.locator("body").innerText()).match(/restart nugget/i), "the page still tells the user to restart Nugget");
      } catch (e) {
        await shot(page, "FAIL-byok-keys-save");
        throw e;
      }
    });

    await check("byok-keys: saving the same name again offers Replace key, and the new key is used", async () => {
      try {
        await field("key").fill(ROTATED);
        await section.getByRole("button", { name: exactName("onboarding.byok.form.save") }).click();
        await section.locator("[role=alert]", { hasText: t("onboarding.byok.form.error.exists") }).waitFor();
        await section.getByRole("button", { name: exactName("onboarding.byok.form.replace") }).click();
        await status(t("onboarding.byok.form.replaced")).waitFor();
        const auth = await chatAuth("after replacing");
        expect(auth === ROTATED, `the chat after replacing the key used the ${auth} key`);
        // And from the provider's own card: a dialog with the key field.
        await section.getByRole("button", { name: fill("models.providers.replaceLabel", { name: NAME }) }).click();
        const dialog = page.getByRole("dialog", { name: fill("models.providers.replaceDialog.title", { name: NAME }) });
        await dialog.getByLabel(t("onboarding.byok.form.key"), { exact: true }).fill(GOOD);
        await dialog.getByRole("button", { name: exactName("onboarding.byok.form.replace") }).click();
        await dialog.waitFor({ state: "detached" });
        await status(fill("models.providers.replaced", { name: NAME })).waitFor();
        const again = await chatAuth("after replacing from the card");
        expect(again === GOOD, `the chat after replacing from the card used the ${again} key`);
      } catch (e) {
        await shot(page, "FAIL-byok-keys-replace");
        throw e;
      }
    });

    await check("byok-keys: Remove (confirmed) drops the provider and leaves config.toml as it was", async () => {
      try {
        await section.getByRole("button", { name: fill("models.providers.removeLabel", { name: NAME }) }).click();
        const dialog = page.getByRole("dialog", { name: fill("models.providers.removeDialog.title", { name: NAME }) });
        await dialog.waitFor();
        // Keyboard: focus starts on Cancel; Escape closes without removing.
        expect((await page.evaluate(() => document.activeElement?.textContent)) === t("common.cancel"), "focus is not on Cancel");
        await page.keyboard.press("Escape");
        await dialog.waitFor({ state: "detached" });
        expect((await ids()).includes(NAME), "Escape removed the provider");
        await section.getByRole("button", { name: fill("models.providers.removeLabel", { name: NAME }) }).click();
        await dialog.getByRole("button", { name: exactName("models.providers.remove") }).click();
        await dialog.waitFor({ state: "detached" });
        await status(fill("models.providers.removed", { name: NAME })).waitFor();
        await section.getByText(t("models.providers.empty"), { exact: true }).waitFor();
        expect(!(await ids()).includes(NAME), `providers.list still has ${NAME} after Remove`);
        const now = fs.readFileSync(configFile, "utf8");
        expect(now === original, `config.toml differs from before the save:\n${now}`);
      } catch (e) {
        await shot(page, "FAIL-byok-keys-remove");
        throw e;
      }
    });

    await check("byok-keys: an env: reference is checked with the daemon's variable and the key never reaches the file", async () => {
      const r = await rpc(daemon.socket, "ai.key.save", { name: "from-env", kind: "openai_compatible", base_url: base, key_ref: `env:${ENV_VAR}`, model: MODEL }, 30_000);
      expect(r.saved === true && r.restart_required === false, `env save: ${JSON.stringify(r)}`);
      const config = fs.readFileSync(configFile, "utf8");
      expect(config.includes(`api_key = "env:${ENV_VAR}"`), `no env: reference in config.toml:\n${config}`);
      expect(!config.includes(GOOD), "config.toml contains the key itself");
      expect((await ids()).includes("from-env"), "the env: provider is not routed");
      const saved = await rpc(daemon.socket, "ai.key.saved", {});
      expect(saved.providers.some((p) => p.id === "from-env" && p.key === "env" && p.env_var === ENV_VAR), `ai.key.saved: ${JSON.stringify(saved)}`);
      const missing = await rpc(daemon.socket, "ai.key.save", { name: "x", kind: "openai_compatible", base_url: base, key_ref: "env:NUGGET_E2E_UNSET_VAR", model: MODEL });
      expect(missing.reason === "env_missing" && !missing.saved, `unset variable: ${JSON.stringify(missing)}`);
      const unknown = await rpc(daemon.socket, "ai.key.remove", { name: "nope" });
      expect(unknown.reason === "not_found" && !unknown.removed, `unknown provider: ${JSON.stringify(unknown)}`);
      const removed = await rpc(daemon.socket, "ai.key.remove", { name: "from-env" });
      expect(removed.removed === true, `remove from-env: ${JSON.stringify(removed)}`);
      expect(fs.readFileSync(configFile, "utf8") === original, "config.toml differs after removing the env: provider");
      for (const f of fs.readdirSync(daemon.logDir)) {
        const text = fs.readFileSync(path.join(daemon.logDir, f), "utf8");
        expect(!text.includes(GOOD) && !text.includes(ROTATED), `the daemon log ${f} contains a key`);
      }
    });

    await checkWire(bridge, "byok-keys");
    await check("byok-keys: no uncaught page errors", async () => expect(pageErrors.length === 0, `uncaught page errors: ${JSON.stringify(pageErrors)}`));
  } finally {
    await ctx?.close().catch(() => {});
    await bridge?.close().catch(() => {});
    await mock.close().catch(() => {});
    await stopDaemon(daemon, "byok-keys");
  }
  return {};
}
