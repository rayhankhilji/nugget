#!/usr/bin/env node
// Real-app end-to-end test: the real nuggetd indexes the fictional corpus in
// bench/fixtures/knowledge/items, and the real desktop frontend (dist/) runs in headless Chromium
// against it through bridge.mjs, which stands in for the Tauri shell. Every page of the PAGES
// registry in src/App.tsx is opened and checked for real data and uncaught errors. A second
// scenario (first-run.mjs) starts from an empty home, goes through onboarding and times how long
// it takes from app start to a cited answer. ai-setup.mjs installs the recommended local model in
// onboarding (signed test manifest, mock download, fake llama-server) and times app start to a
// cited answer streamed by that model; byok.mjs checks and saves an API key in onboarding.
// i18n.mjs reopens every page in long, CJK and right-to-left locales on the same indexed home
// and measures text that doesn't fit.
//
// usage: pnpm run e2e        (see e2e/README.md for the environment variables)
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { startBridge, METHOD_NOT_FOUND } from "./bridge.mjs";
import { CORPUS, addCorpusSource, countFiles, locateNuggetd, rpc, startDaemon, waitForKnowledge } from "./daemon.mjs";
import { firstRunScenario } from "./first-run.mjs";
import { aiSetupScenario } from "./ai-setup.mjs";
import { byokScenario } from "./byok.mjs";
import { i18nScenario } from "./i18n.mjs";
import {
  check,
  DESKTOP,
  DIST,
  expect,
  expectNoErrorState,
  INDEX_TIMEOUT_MS,
  KEEP,
  log,
  newContext,
  OUT,
  results,
  secs,
  settle,
  shot,
  sinceStart,
  sleep,
  STEP_TIMEOUT_MS,
  t,
  until,
} from "./harness.mjs";

/**
 * Scenarios to run (E2E_SCENARIOS, comma-separated): every page on an indexed home, the same pages
 * in other locales (sharing that home), a first run, a first run with the recommended local model,
 * and bring-your-own-key.
 */
const SCENARIOS = ["pages", "i18n", "first-run", "ai-setup", "byok"];
/** Scenarios that use the shared indexed home. */
const INDEXED = ["pages", "i18n"];

// ---- the page registry, read from the app source so new pages are covered automatically ----

function pageRegistry() {
  const src = fs.readFileSync(path.join(DESKTOP, "src", "App.tsx"), "utf8");
  const block = src.match(/export const PAGES\b[^=]*=\s*\[([\s\S]*?)\n\];/);
  if (!block) throw new Error("could not find `export const PAGES = [...]` in src/App.tsx");
  const pages = [...block[1].matchAll(/\{\s*id:\s*"([^"]+)",\s*label:\s*"([^"]+)"/g)].map((m) => ({ id: m[1], label: t(m[2]) }));
  if (pages.length < 5) throw new Error(`parsed only ${pages.length} pages from the PAGES registry in src/App.tsx`);
  return pages;
}

const ONBOARDED = { version: 1, step: "done", completed: [], skipped: [], ai: null, finished: true };

// ---- per-page assertions ----

function pageChecks({ expectedItems }) {
  return {
    async status(page) {
      const dd = page.locator("dt", { hasText: new RegExp(`^${t("status.items")}$`) }).first().locator("xpath=following-sibling::dd[1]");
      await until(`Status shows ${expectedItems} items`, async () => {
        const v = (await dd.textContent({ timeout: 1000 }))?.trim();
        return [v === String(expectedItems), v];
      });
    },

    async search(page) {
      const box = page.getByRole("searchbox", { name: t("search.query") });
      await box.fill("Tidewater budget");
      await box.press("Enter");
      // Text-only retrieval ranks the "Q4 budget" note (both words; "Tidewater" in its body) level
      // with the "Tidewater Billing" project note, so the first hit must mention Tidewater and a
      // Tidewater-titled hit must be in the top three.
      const hits = page.locator("ol.results li.result");
      const got = await until('search "Tidewater budget" returns Tidewater results at the top', async () => {
        const all = await hits.evaluateAll((lis) =>
          lis.map((li) => ({ title: li.querySelector(".result-title")?.textContent?.trim() ?? "", text: li.textContent ?? "" })),
        );
        const titles = all.map((h) => h.title);
        const ok = all.length > 0 && all[0].text.includes("Tidewater") && titles.slice(0, 3).some((x) => x.includes("Tidewater"));
        return [ok, titles.slice(0, 5)];
      });
      log(`  search: top titles ${JSON.stringify(got)}`);
      await settle(page);
      await shot(page, "search-results");
    },

    async knowledge(page) {
      const list = page.getByRole("region", { name: t("knowledge.entitiesLabel") });
      const total = await until("Knowledge lists at least 60 entities", async () => {
        const text = (await list.locator(".kb-count").textContent({ timeout: 1000 })) ?? "";
        const rows = await list.locator(".entity-row").count();
        const n = Number((text.match(/(\d[\d,]*)\s*$/)?.[1] ?? "0").replace(/,/g, ""));
        return [n >= 60 && rows > 0, { count: text.trim(), rows }];
      });
      log(`  knowledge: ${total.count}, ${total.rows} rows on the first page`);

      await list.getByRole("button", { name: /^Ada Wexford\b/ }).first().click();
      const detail = page.locator("section.entity-detail");
      await detail.getByRole("heading", { name: "Ada Wexford" }).first().waitFor({ timeout: STEP_TIMEOUT_MS });
      const why = detail.getByRole("button", { name: new RegExp(`^${t("entity.why").replace(/[?]/g, "\\?")}`) });
      const facts = await until("Ada Wexford shows facts (Why? buttons)", async () => {
        const n = await why.count();
        return [n > 0, n];
      });
      log(`  Ada Wexford: ${facts} facts`);
      await settle(page);
      await shot(page, "knowledge-entity");

      // Open each fact's evidence until the "Wren Mobile usability test" calendar item (cal-011.ics) shows up with a quote.
      const quoted = async () =>
        detail.locator("ul.evidence > li").evaluateAll((lis) =>
          lis.map((li) => ({
            title: li.querySelector(".evidence-title")?.textContent?.trim() ?? "",
            quote: li.querySelector("blockquote.evidence-quote")?.textContent?.trim() ?? "",
          })),
        );
      for (let i = 0; i < facts; i++) {
        const b = why.nth(i);
        if ((await b.getAttribute("aria-expanded")) !== "true") await b.click();
        const ev = await quoted();
        if (ev.some((e) => e.title.includes("Wren Mobile usability test") && e.quote)) break;
      }
      const ev = await until('"Why?" shows a quoted source from the Wren Mobile usability test event', async () => {
        const ev = await quoted();
        return [ev.some((e) => e.title.includes("Wren Mobile usability test") && e.quote.length > 0), ev.map((e) => e.title)];
      });
      log(`  evidence sources: ${[...new Set(ev)].join(", ")}`);
      const cal = detail.locator("ul.evidence > li", { has: page.locator(".evidence-title", { hasText: "Wren Mobile usability test" }) }).first();
      await cal.scrollIntoViewIfNeeded();
      await shot(page, "knowledge-why");

      await page.getByRole("tab", { name: t("knowledge.tab.graph") }).click();
      const canvas = page.locator("canvas.graph-canvas");
      const graph = await until("Graph tab renders a canvas with nodes", async () => {
        const label = (await canvas.getAttribute("aria-label", { timeout: 1000 })) ?? "";
        const nodes = Number(label.match(/(\d+)\s+entit/)?.[1] ?? 0);
        // Something was drawn: more than one colour on the canvas.
        const colours = await canvas.evaluate((c) => {
          const ctx = c.getContext("2d");
          if (!ctx || !c.width || !c.height) return 0;
          const d = ctx.getImageData(0, 0, c.width, c.height).data;
          const seen = new Set();
          for (let i = 0; i < d.length && seen.size < 50; i += 4 * 97) seen.add(`${d[i]},${d[i + 1]},${d[i + 2]},${d[i + 3]}`);
          return seen.size;
        });
        return [nodes > 0 && colours > 1, { label, colours }];
      });
      log(`  graph: ${graph.label}`);
      await sleep(1500); // let the force layout cool down for the screenshot
      await shot(page, "knowledge-graph");
    },

    async sources(page) {
      const src = await until('Sources lists the "files" source with items > 0', async () => {
        const rows = await page.locator("li.source").evaluateAll((lis) =>
          lis.map((li) => ({
            name: li.querySelector(".source-name")?.textContent?.trim() ?? "",
            id: li.querySelector("code")?.textContent?.trim() ?? "",
            meta: li.querySelector("p.source-meta:not(.muted)")?.textContent?.trim() ?? "",
          })),
        );
        const files = rows.find((r) => r.id.startsWith("files-"));
        const items = Number(files?.meta.match(/(\d[\d,]*)\s+items?/)?.[1]?.replace(/,/g, "") ?? 0);
        return [items > 0, rows];
      });
      log(`  sources: ${JSON.stringify(src)}`);
    },

    async safety(page) {
      const region = page.getByRole("region", { name: t("safety.defaults.title") });
      const rows = await until("Safety shows the built-in rules table", async () => {
        const n = await region.locator("table tbody tr").count();
        return [n > 0, n];
      });
      log(`  safety: ${rows} built-in rules`);
    },

    async agent(page) {
      await until("Agent shows the empty task history", async () => {
        const v = await page.getByText(t("agent.history.emptyTitle"), { exact: true }).isVisible();
        return [v, v];
      });
    },

    async plugins(page) {
      // A fresh home has no plugins: the empty state proves plugins.list answered.
      await until("Plugins shows its (empty) plugin list", async () => {
        const v = await page.getByText(t("plugins.emptyTitle"), { exact: true }).isVisible();
        return [v, v];
      });
      await expectNoErrorState(page, "Plugins");
    },

    async today(page) {
      await until("Today shows the morning brief", async () => {
        const v = await page.getByText(new RegExp(`^${t("today.brief.title")}`)).first().isVisible();
        return [v, v];
      });
      await expectNoErrorState(page, "Today");
    },
  };
}

// ---- the indexed home the pages and i18n scenarios share (indexed once) ----

let indexed = null;

/** Start nuggetd on a temp home and index the corpus, once; later calls reuse it. */
function indexedHome(bin) {
  indexed ??= (async () => {
    const tSetup = Date.now();
    const daemon = await startDaemon(bin, { stderrPath: path.join(OUT, "nuggetd.out.log") });
    log(`daemon ready, home ${daemon.home}, socket ${daemon.socket}`);
    try {
      const source = await addCorpusSource(daemon.socket);
      log(`added source ${source.id}`);
      const settled = await waitForKnowledge(daemon.socket, { timeoutMs: INDEX_TIMEOUT_MS, log });
      const setup_ms = Date.now() - tSetup;
      log(`indexed ${settled.status.items} items; ${settled.knowledge.entities} entities, ${settled.knowledge.claims} facts (${secs(setup_ms)})`);
      return { daemon, settled, setup_ms };
    } catch (e) {
      await stopIndexedHome(daemon);
      throw e;
    }
  })();
  return indexed;
}

async function stopIndexedHome(daemon) {
  fs.cpSync(daemon.logDir, path.join(OUT, "daemon-logs"), { recursive: true });
  await daemon.stop({ keep: KEEP });
  if (KEEP) log(`kept the daemon home: ${daemon.home}`);
}

// ---- scenario: every page on an indexed home ----

async function pagesScenario({ browser, bin }) {
  const pages = pageRegistry();
  const expectedItems = countFiles(CORPUS);
  log(`corpus: ${CORPUS} (${expectedItems} files)`);

  const { daemon, settled, setup_ms } = await indexedHome(bin);
  let bridge;
  let ctx;
  const timings = { setup_ms };
  try {
    bridge = await startBridge({ socket: daemon.socket, dist: DIST });
    log(`bridge on ${bridge.url}`);
    ctx = await newContext(browser);
    await ctx.addInitScript(
      ([key, state]) => {
        localStorage.setItem(key, JSON.stringify(state));
      },
      ["nugget.onboarding.v1", ONBOARDED],
    );

    const vaultDir = settled.knowledge.vault?.path ?? path.join(daemon.home, "data", "vault");
    const specific = pageChecks({ expectedItems });
    const pageErrors = {};
    const consoleErrors = {};
    const tBrowser = Date.now();

    for (const { id, label } of pages) {
      await check(`page ${id}`, async () => {
        const page = await ctx.newPage();
        const errors = (pageErrors[id] = []);
        const consoleErrs = (consoleErrors[id] = []);
        page.on("pageerror", (e) => errors.push(e.message));
        page.on("console", (m) => m.type() === "error" && consoleErrs.push(m.text()));
        await page.addInitScript((p) => localStorage.setItem("nugget.page", p), id);
        try {
          await page.goto(`${bridge.url}index.html`);
          await page.locator(`main[aria-label="${label}"]`).waitFor();
          const current = (await page.locator('nav .nav-item[aria-current="page"]').textContent())?.trim();
          expect(current === label, `the sidebar marks "${current}" as current, expected "${label}"`);
          await page.locator("main h1").first().waitFor();
          await settle(page);
          await shot(page, id);
          if (specific[id]) await specific[id](page);
          await settle(page);
          await expectNoErrorState(page, `page ${id}`);
          expect(errors.length === 0, `${errors.length} uncaught page error(s) on ${id}: ${JSON.stringify(errors)}`);
        } catch (e) {
          await shot(page, `FAIL-${id}`);
          throw e;
        } finally {
          await page.close();
        }
      });
    }
    timings.browser_ms = Date.now() - tBrowser;

    await check("no uncaught page errors on any page", async () => {
      const bad = Object.entries(pageErrors).filter(([, e]) => e.length);
      expect(bad.length === 0, `page errors: ${JSON.stringify(Object.fromEntries(bad))}`);
    });

    await check("every daemon command the UI called exists and accepted its params", async () => {
      const bad = bridge.calls.filter((c) => c.via === "daemon" && (c.code === METHOD_NOT_FOUND || c.code === -32602));
      expect(bad.length === 0, `wire mismatches: ${JSON.stringify(bad)}`);
    });

    await check("vault has a page for Ada Wexford with footnote citations", async () => {
      const find = (dir) => {
        if (!fs.existsSync(dir)) return null;
        for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
          const p = path.join(dir, e.name);
          if (e.isDirectory()) {
            const hit = find(p);
            if (hit) return hit;
          } else if (e.name.startsWith("Ada Wexford") && e.name.endsWith(".md")) return p;
        }
        return null;
      };
      const file = await until(`a Markdown page for Ada Wexford under ${vaultDir}`, async () => {
        const f = find(vaultDir);
        return [f && /\[\^1\]/.test(fs.readFileSync(f, "utf8")), f];
      });
      const md = fs.readFileSync(file, "utf8");
      expect(/^# Ada Wexford$/m.test(md), `${file} has no "# Ada Wexford" heading`);
      expect(/^\[\^1\]:\s*\S/m.test(md), `${file} cites [^1] but has no [^1]: footnote definition`);
      fs.copyFileSync(file, path.join(OUT, "vault-Ada-Wexford.md"));
      log(`  vault: ${path.relative(vaultDir, file)}`);
    });

    await check("daemon still running", async () => {
      expect(!daemon.exited(), `nuggetd exited during the test: ${JSON.stringify(daemon.exited())}`);
      await rpc(daemon.socket, "status");
    });

    const rpcErrors = bridge.calls.filter((c) => !c.ok);
    if (rpcErrors.length) log(`note: ${rpcErrors.length} command(s) returned errors (see summary.json): ${[...new Set(rpcErrors.map((c) => `${c.cmd}: ${c.message}`))].join("; ")}`);
    const consoleCount = Object.values(consoleErrors).flat().length;
    if (consoleCount) log(`note: ${consoleCount} console error(s) (see summary.json)`);
    return { timings, rpcErrors, pageErrors, consoleErrors, commands: [...new Set(bridge.calls.map((c) => c.cmd))].sort() };
  } finally {
    await ctx?.close().catch(() => {});
    await bridge?.close().catch(() => {});
  }
}

// ---- main ----

async function main() {
  fs.rmSync(OUT, { recursive: true, force: true });
  fs.mkdirSync(OUT, { recursive: true });

  const wanted = (process.env.E2E_SCENARIOS || SCENARIOS.join(",")).split(",").map((s) => s.trim()).filter(Boolean);
  const unknown = wanted.filter((s) => !SCENARIOS.includes(s));
  if (unknown.length) throw new Error(`unknown E2E_SCENARIOS ${JSON.stringify(unknown)}; known: ${SCENARIOS.join(", ")}`);

  const require = createRequire(import.meta.url);
  let chromium;
  try {
    ({ chromium } = require("playwright"));
  } catch {
    throw new Error("playwright is not installed: run `pnpm install` in apps/desktop (and `pnpm exec playwright install chromium` once)");
  }

  if (!fs.existsSync(path.join(DIST, "index.html")) || process.env.E2E_BUILD === "1") {
    log("building the frontend (pnpm run build)…");
    const r = spawnSync("pnpm", ["run", "build"], { cwd: DESKTOP, stdio: "inherit", shell: process.platform === "win32" });
    if (r.status !== 0) throw new Error(`pnpm run build failed (exit ${r.status ?? r.error?.message})`);
  }

  // ai-setup runs nuggetd-e2e (located by the scenario); the others the plain nuggetd.
  const bin = wanted.some((s) => s !== "ai-setup") ? locateNuggetd(log) : null;
  if (bin) log(`nuggetd: ${bin}`);

  // E2E_CHROMIUM: a Chromium binary to use instead of Playwright's own download.
  const browser = await chromium.launch(process.env.E2E_CHROMIUM ? { executablePath: process.env.E2E_CHROMIUM } : {});
  const summary = { results };
  try {
    for (const [i, name] of wanted.entries()) {
      log(`---- scenario: ${name} ----`);
      try {
        if (name === "pages") Object.assign(summary, await pagesScenario({ browser, bin }));
        else if (name === "i18n") {
          const { daemon } = await indexedHome(bin);
          summary.i18n = await i18nScenario({ browser, socket: daemon.socket, pages: pageRegistry(), onboarded: ONBOARDED });
        } else if (name === "first-run") summary.first_run = await firstRunScenario({ browser, bin });
        else if (name === "ai-setup") summary.first_run_ai = await aiSetupScenario({ browser });
        else summary.byok = await byokScenario({ browser, bin });
      } catch (e) {
        // A setup failure (daemon, indexing) fails the scenario and lets the next one run.
        results.push({ name: `scenario ${name}`, ok: false, ms: 0, error: e.stack ?? String(e) });
        log(`FAIL scenario ${name}: ${e.stack ?? e}`);
      }
      // Stop the shared indexed home once no later scenario needs it.
      if (indexed && !wanted.slice(i + 1).some((s) => INDEXED.includes(s))) {
        const home = await indexed.catch(() => null);
        indexed = null;
        if (home) await stopIndexedHome(home.daemon);
      }
    }
  } finally {
    const home = indexed && (await indexed.catch(() => null));
    if (home) await stopIndexedHome(home.daemon);
    await browser.close().catch(() => {});
    // The first-run summary carries the time to a cited model answer when both ran.
    const aiMs = summary.first_run_ai?.time_to_cited_model_answer_ms ?? null;
    if (summary.first_run) summary.first_run.time_to_cited_model_answer_ms = aiMs;
    fs.writeFileSync(path.join(OUT, "summary.json"), JSON.stringify(summary, null, 2));
    if (summary.first_run || summary.first_run_ai) {
      const cited = summary.first_run?.total_ms ?? null;
      log(
        `first run: cited answer (search, no AI) ${cited === null ? "not reached" : secs(cited)}; ` +
          `cited model answer (local model installed in onboarding) ${aiMs === null ? "not reached" : secs(aiMs)} after app start`,
      );
    }
  }

  const failed = results.filter((r) => !r.ok).length;
  log(`${results.length - failed}/${results.length} checks passed in ${secs(sinceStart())}; screenshots in ${OUT}`);
  return failed === 0;
}

main().then(
  (ok) => process.exit(ok ? 0 : 1),
  (e) => {
    console.error(`[e2e] setup failed: ${e.stack ?? e}`);
    process.exit(1);
  },
);
