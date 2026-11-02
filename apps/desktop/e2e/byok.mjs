// Bring your own key in onboarding: the key is checked against a mock OpenAI-compatible server on
// 127.0.0.1 (bad key, wrong URL and no network are reported), saved to the secret store (never to
// config.toml or the logs), and the requests show in the egress ledger.
import fs from "node:fs";
import http from "node:http";
import net from "node:net";
import path from "node:path";
import { startBridge } from "./bridge.mjs";
import { FIRST_RUN_CONFIG, rpc, startDaemon } from "./daemon.mjs";
import { check, checkWire, DIST, exactName, expect, log, newContext, OUT, shot, startsWith, stopDaemon, t, tPattern, until } from "./harness.mjs";

const GOOD_KEY = "sk-e2e-good";
const BAD_KEY = "sk-e2e-bad";
export const MODEL = "mock-model";

/**
 * A mock OpenAI-compatible server on 127.0.0.1: `GET /v1/models` and `POST /v1/chat/completions` (a
 * short stream) for any of `keys`, 401 for another key, 404 for other paths. `requests` records
 * `{ method, path, auth }`, auth being the key used, "other" or "none".
 */
export function startMockOpenAi(keys) {
  const requests = [];
  const server = http.createServer((req, res) => {
    const bearer = req.headers.authorization?.replace(/^Bearer /, "");
    const auth = !bearer ? "none" : keys.includes(bearer) ? bearer : "other";
    const p = new URL(req.url, "http://127.0.0.1").pathname;
    requests.push({ method: req.method, path: p, auth });
    const json = (status, body) => res.writeHead(status, { "Content-Type": "application/json" }).end(JSON.stringify(body));
    if (!keys.includes(auth)) return json(401, { error: { message: "Incorrect API key provided", code: "invalid_api_key" } });
    if (req.method === "GET" && p === "/v1/models") return json(200, { object: "list", data: [{ id: MODEL, object: "model" }] });
    if (req.method === "POST" && p === "/v1/chat/completions") {
      req.resume();
      const chunk = (delta, finish = null) =>
        `data: ${JSON.stringify({ id: "c", object: "chat.completion.chunk", model: MODEL, choices: [{ index: 0, delta, finish_reason: finish }] })}\n\n`;
      res.writeHead(200, { "Content-Type": "text/event-stream" });
      return res.end(chunk({ role: "assistant", content: "Hello from the mock." }) + chunk({}, "stop") + "data: [DONE]\n\n");
    }
    json(404, { error: { message: "Not found" } });
  });
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () =>
      resolve({ port: server.address().port, requests, close: () => new Promise((r) => (server.closeAllConnections?.(), server.close(() => r()))) }),
    );
  });
}

/** A localhost port nothing listens on (bound, then released). */
function closedPort() {
  return new Promise((resolve, reject) => {
    const s = net.createServer();
    s.once("error", reject);
    s.listen(0, "127.0.0.1", () => {
      const { port } = s.address();
      s.close(() => resolve(port));
    });
  });
}

export async function byokScenario({ browser, bin }) {
  const summary = {};
  const mock = await startMockOpenAi([GOOD_KEY]);
  const deadPort = await closedPort();
  const daemon = await startDaemon(bin, { stderrPath: path.join(OUT, "byok-nuggetd.out.log"), config: FIRST_RUN_CONFIG });
  log(`byok: daemon home ${daemon.home}, mock server on 127.0.0.1:${mock.port}`);
  let bridge;
  let ctx;
  const pageErrors = [];
  try {
    bridge = await startBridge({ socket: daemon.socket, dist: DIST });
    ctx = await newContext(browser);
    const page = await ctx.newPage();
    page.on("pageerror", (e) => pageErrors.push(e.message));
    bridge.deliverEvents(page);
    const heading = (key) => page.getByRole("heading", { level: 1, name: t(key), exact: true });
    const cont = () => page.locator("footer.ob-actions").getByRole("button", { name: exactName("onboarding.continue") }).click();

    const field = (key) => page.getByLabel(t(`onboarding.byok.form.${key}`), { exact: true });
    const button = (key) => page.getByRole("button", { name: exactName(`onboarding.byok.form.${key}`) });
    /** Fill the form; `action` is "test" or "save"; resolves to the alert/status text shown. */
    const attempt = async (name, { baseUrl, key, action = "test", expectKey }) => {
      await field("baseUrl").fill(baseUrl);
      await field("key").fill(key);
      await button(action).click();
      const role = expectKey.endsWith(".saved") ? "status" : "alert";
      const want = tPattern(expectKey);
      // The note may add the server's own message after the string; match its fixed start too.
      const head = t(expectKey).split("{")[0].trim();
      return until(`${name}: the form shows "${t(expectKey)}" (role=${role})`, async () => {
        const texts = (await page.locator(`[role=${role}]`).allTextContents()).map((s) => s.trim()).filter(Boolean);
        return [texts.some((s) => want.test(s) || (head.length > 3 && s.includes(head))), texts];
      });
    };

    await check("byok: onboarding checks keys against the server before saving", async () => {
      try {
        await page.goto(`${bridge.url}index.html`);
        await heading("onboarding.welcome.heading").waitFor();
        await cont();
        // Connect apps comes before the AI step; continue without a source.
        await heading("onboarding.source.heading").waitFor();
        await cont();
        await heading("onboarding.ai.heading").waitFor();
        await page.getByRole("radio", { name: startsWith("onboarding.ai.byok") }).check();

        const kind = field("kind");
        await kind.waitFor();
        await kind.selectOption("openai_compatible").catch(async () => {
          // The option's value may differ; pick it by its label instead.
          const options = await kind.locator("option").evaluateAll((os) => os.map((o) => ({ value: o.value, label: o.textContent ?? "" })));
          const o = options.find((x) => /compatible/i.test(x.value) || /compatible/i.test(x.label));
          expect(o, `no OpenAI-compatible option in the kind select: ${JSON.stringify(options)}`);
          await kind.selectOption(o.value);
        });
        if ((await field("model").count()) > 0) await field("model").fill(MODEL);
        if ((await field("name").count()) > 0) await field("name").fill("e2e-mock");

        const base = `http://127.0.0.1:${mock.port}/v1`;
        await attempt("bad-key", { baseUrl: base, key: BAD_KEY, expectKey: "onboarding.byok.form.error.bad_key" });
        await attempt("wrong-url", { baseUrl: `http://127.0.0.1:${mock.port}/nope`, key: GOOD_KEY, expectKey: "onboarding.byok.form.error.wrong_url" });
        await attempt("no-network", { baseUrl: `http://127.0.0.1:${deadPort}/v1`, key: GOOD_KEY, expectKey: "onboarding.byok.form.error.no_network" });
        await attempt("good-key", { baseUrl: base, key: GOOD_KEY, action: "save", expectKey: "onboarding.byok.form.saved" });
        expect(mock.requests.some((r) => r.path === "/v1/models" && r.auth === GOOD_KEY), `the mock server never saw the good key: ${JSON.stringify(mock.requests)}`);
        expect(mock.requests.some((r) => r.auth === "other"), `the bad key never reached the mock server: ${JSON.stringify(mock.requests)}`);
      } catch (e) {
        await shot(page, "FAIL-byok");
        throw e;
      }
    });

    await check("byok: config.toml names the key in the secret store and never holds it", async () => {
      const file = path.join(daemon.home, "config", "config.toml");
      const config = fs.readFileSync(file, "utf8");
      fs.writeFileSync(path.join(OUT, "byok-config.toml"), config.replaceAll(GOOD_KEY, "<LEAKED KEY>"));
      expect(/^\s*\[\[providers\]\]/m.test(config), `no [[providers]] table in ${file}`);
      expect(/api_key\s*=\s*"keychain:/.test(config), `the provider's api_key is not a keychain: reference in ${file}`);
      expect(!config.includes(GOOD_KEY), `${file} contains the API key itself`);
      // Nor the logs.
      for (const f of fs.readdirSync(daemon.logDir)) {
        expect(!fs.readFileSync(path.join(daemon.logDir, f), "utf8").includes(GOOD_KEY), `the daemon log ${f} contains the API key`);
      }
    });

    await check("byok: the egress ledger shows the key checks (egress.recent and Privacy)", async () => {
      const recent = await rpc(daemon.socket, "egress.recent", { limit: 100 });
      const rows = recent.filter((e) => e.host === "127.0.0.1" || e.host.startsWith("127.0.0.1:"));
      expect(rows.length > 0, `no egress.recent row for 127.0.0.1: ${JSON.stringify(recent.slice(0, 5))}`);
      const pctx = await newContext(browser, { onboarded: true, page: "privacy" });
      const p = await pctx.newPage();
      try {
        await p.goto(`${bridge.url}index.html`);
        await p.locator(`main[aria-label="${t("app.nav.privacy")}"]`).waitFor();
        await until("the Privacy page lists a request to 127.0.0.1", async () => {
          const text = (await p.locator("main table").allTextContents()).join(" ");
          return [text.includes("127.0.0.1"), text.slice(0, 300)];
        });
      } finally {
        await pctx.close();
      }
    });

    await checkWire(bridge, "byok");
    await check("byok: no uncaught page errors", async () => expect(pageErrors.length === 0, `uncaught page errors: ${JSON.stringify(pageErrors)}`));
    summary.rpcErrors = bridge.calls.filter((c) => !c.ok);
  } finally {
    await ctx?.close().catch(() => {});
    await bridge?.close().catch(() => {});
    await mock.close().catch(() => {});
    await stopDaemon(daemon, "byok");
  }
  return summary;
}
