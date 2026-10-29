#!/usr/bin/env node
// Real-app end-to-end test: the real nuggetd indexes the fictional corpus in
// bench/fixtures/knowledge/items, and the real desktop frontend (dist/) runs in headless Chromium
// against it through bridge.mjs, which stands in for the Tauri shell. Every page of the PAGES
// registry in src/App.tsx is opened and checked for real data and uncaught errors.
//
// usage: pnpm run e2e        (see e2e/README.md for the environment variables)
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { startBridge, METHOD_NOT_FOUND } from "./bridge.mjs";
import { CORPUS, addCorpusSource, countFiles, locateNuggetd, rpc, startDaemon, waitForKnowledge } from "./daemon.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const DESKTOP = path.resolve(here, "..");
const DIST = path.join(DESKTOP, "dist");
const OUT = path.resolve(process.env.E2E_ARTIFACTS || path.join(here, "artifacts"));
const KEEP = process.env.E2E_KEEP === "1";
const INDEX_TIMEOUT_MS = Number(process.env.E2E_INDEX_TIMEOUT_MS || 180_000);
const STEP_TIMEOUT_MS = Number(process.env.E2E_STEP_TIMEOUT_MS || 15_000);

const started = Date.now();
const secs = (ms) => `${(ms / 1000).toFixed(1)}s`;
const log = (...a) => console.log(`[e2e ${secs(Date.now() - started)}]`, ...a);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ---- UI strings and the page registry, read from the app source so copy changes don't break this ----

const en = JSON.parse(fs.readFileSync(path.join(DESKTOP, "src", "i18n", "locales", "en.json"), "utf8"));
function t(key) {
  const s = en[key];
  if (typeof s !== "string") throw new Error(`no English string for ${key} in src/i18n/locales/en.json`);
  return s;
}

function pageRegistry() {
  const src = fs.readFileSync(path.join(DESKTOP, "src", "App.tsx"), "utf8");
  const block = src.match(/export const PAGES\b[^=]*=\s*\[([\s\S]*?)\n\];/);
  if (!block) throw new Error("could not find `export const PAGES = [...]` in src/App.tsx");
  const pages = [...block[1].matchAll(/\{\s*id:\s*"([^"]+)",\s*label:\s*"([^"]+)"/g)].map((m) => ({ id: m[1], label: t(m[2]) }));
  if (pages.length < 5) throw new Error(`parsed only ${pages.length} pages from the PAGES registry in src/App.tsx`);
  return pages;
}

// ---- checks ----

const results = [];
let failed = 0;

class CheckError extends Error {}
function expect(cond, message) {
  if (!cond) throw new CheckError(message);
}

/**
 * Poll `probe` (returning `[ok, observed]`) until ok, or fail with `what` and the last observation.
 * Playwright locators auto-wait for elements; this covers data that arrives after them.
 */
async function until(what, probe, timeoutMs = STEP_TIMEOUT_MS) {
  const deadline = Date.now() + timeoutMs;
  let observed;
  for (;;) {
    let ok = false;
    try {
      [ok, observed] = await probe();
    } catch (e) {
      observed = `error: ${e.message.split("\n")[0]}`;
    }
    if (ok) return observed;
    if (Date.now() > deadline) throw new CheckError(`${what}; after ${timeoutMs} ms saw: ${JSON.stringify(observed)}`);
    await sleep(200);
  }
}

async function check(name, fn) {
  const t0 = Date.now();
  try {
    await fn();
    results.push({ name, ok: true, ms: Date.now() - t0 });
    log(`PASS ${name} (${secs(Date.now() - t0)})`);
  } catch (e) {
    failed++;
    const message = e instanceof CheckError ? e.message : (e.stack ?? String(e));
    results.push({ name, ok: false, ms: Date.now() - t0, error: message });
    log(`FAIL ${name}: ${message}`);
  }
}

// ---- browser helpers ----

const ONBOARDED = { version: 1, step: "done", completed: [], skipped: [], ai: null, finished: true };

let shotN = 0;
async function shot(page, name) {
  const file = path.join(OUT, `${String(++shotN).padStart(2, "0")}-${name}.png`);
  await page.screenshot({ path: file }).catch((e) => log(`  screenshot ${name} failed: ${e.message}`));
}

/** Wait until the bridge has been quiet for a moment and nothing is marked busy. */
async function settle(page) {
  await page.waitForLoadState("networkidle", { timeout: 10_000 }).catch(() => {});
  await page
    .waitForFunction(() => !document.querySelector('[aria-busy="true"]'), null, { timeout: 5_000 })
    .catch(() => {});
}

/** Visible error notes (`ErrorNote` and friends render `.note-error` / role=alert). */
async function errorNotes(page) {
  return page.evaluate(() =>
    [...document.querySelectorAll("main .note-error, main [role=alert]")]
      .filter((el) => el.getClientRects().length > 0 && el.textContent.trim())
      .map((el) => el.textContent.trim().replace(/\s+/g, " ")),
  );
}

async function expectNoErrorState(page, what) {
  const notes = await errorNotes(page);
  expect(notes.length === 0, `${what} shows an error state: ${JSON.stringify(notes)}`);
}

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

      // Open each fact's evidence until the cal-011 calendar item shows up with a quote.
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
        if (ev.some((e) => e.title.includes("cal-011") && e.quote)) break;
      }
      const ev = await until('"Why?" shows a quoted source from cal-011', async () => {
        const ev = await quoted();
        return [ev.some((e) => e.title.includes("cal-011") && e.quote.length > 0), ev.map((e) => e.title)];
      });
      log(`  evidence sources: ${[...new Set(ev)].join(", ")}`);
      const cal = detail.locator("ul.evidence > li", { has: page.locator(".evidence-title", { hasText: "cal-011" }) }).first();
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

// ---- main ----

async function main() {
  fs.rmSync(OUT, { recursive: true, force: true });
  fs.mkdirSync(OUT, { recursive: true });

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

  const pages = pageRegistry();
  const expectedItems = countFiles(CORPUS);
  const bin = locateNuggetd(log);
  log(`nuggetd: ${bin}`);
  log(`corpus: ${CORPUS} (${expectedItems} files)`);

  const tSetup = Date.now();
  const daemon = await startDaemon(bin, { stderrPath: path.join(OUT, "nuggetd.out.log") });
  log(`daemon ready, home ${daemon.home}, socket ${daemon.socket}`);
  let bridge;
  let browser;
  const timings = {};
  try {
    const source = await addCorpusSource(daemon.socket);
    log(`added source ${source.id}`);
    const settled = await waitForKnowledge(daemon.socket, { timeoutMs: INDEX_TIMEOUT_MS, log });
    timings.setup_ms = Date.now() - tSetup;
    log(
      `indexed ${settled.status.items} items; ${settled.knowledge.entities} entities, ${settled.knowledge.claims} facts (${secs(timings.setup_ms)})`,
    );

    bridge = await startBridge({ socket: daemon.socket, dist: DIST });
    log(`bridge on ${bridge.url}`);
    browser = await chromium.launch();
    const ctx = await browser.newContext({
      viewport: { width: 1360, height: 860 },
      deviceScaleFactor: 1,
      locale: "en-US",
      timezoneId: "UTC",
      colorScheme: "light",
    });
    ctx.setDefaultTimeout(STEP_TIMEOUT_MS);
    await ctx.addInitScript({ path: path.join(here, "tauri-shim.js") });
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
    fs.writeFileSync(
      path.join(OUT, "summary.json"),
      JSON.stringify({ results, timings, rpcErrors, pageErrors, consoleErrors, commands: [...new Set(bridge.calls.map((c) => c.cmd))].sort() }, null, 2),
    );
    if (rpcErrors.length) log(`note: ${rpcErrors.length} command(s) returned errors (see summary.json): ${[...new Set(rpcErrors.map((c) => `${c.cmd}: ${c.message}`))].join("; ")}`);
    const consoleCount = Object.values(consoleErrors).flat().length;
    if (consoleCount) log(`note: ${consoleCount} console error(s) (see summary.json)`);
  } finally {
    await browser?.close().catch(() => {});
    await bridge?.close().catch(() => {});
    fs.cpSync(daemon.logDir, path.join(OUT, "daemon-logs"), { recursive: true });
    await daemon.stop({ keep: KEEP });
    if (KEEP) log(`kept the daemon home: ${daemon.home}`);
  }

  const passed = results.length - failed;
  log(`${passed}/${results.length} checks passed in ${secs(Date.now() - started)}; screenshots in ${OUT}`);
  return failed === 0;
}

main().then(
  (ok) => process.exit(ok ? 0 : 1),
  (e) => {
    console.error(`[e2e] setup failed: ${e.stack ?? e}`);
    process.exit(1);
  },
);
