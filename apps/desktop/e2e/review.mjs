#!/usr/bin/env node
// Design review screenshots: every page, dialog, state, onboarding step and the Double Tap overlay
// of the real app (the real nuggetd on the fictional corpus, as for `pnpm run e2e`), in light and
// dark, in English, Arabic (right to left) and German (long words), at a small (900×600) and a
// large (1600×1000) window. For a person (or a reviewer) to look at the whole app at once; it
// checks nothing. Written to `e2e/artifacts/review/<locale>-<theme>-<size>/<screen>.png`.
//
//   cd apps/desktop
//   node e2e/review.mjs            # ~5 min once nuggetd is built; uses the existing dist/
//
// Environment: as for `pnpm run e2e` (NUGGETD, E2E_CHROMIUM, E2E_ARTIFACTS), plus
//   REVIEW_LOCALES   comma-separated locales (default en,ar,de)
//   REVIEW_THEMES    light,dark (default both)
//   REVIEW_SIZES     WxH list (default 900x600,1600x1000)
//   REVIEW_ONLY      comma-separated screen-name prefixes to take (default all)
//   REVIEW_STATES=0  skip the empty, loading and error states (taken in en only, both themes, small)
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createRequire } from "node:module";
import { startBridge } from "./bridge.mjs";
import { addCorpusSource, locateNuggetd, rpc, startDaemon, waitForKnowledge, FIRST_RUN_CONFIG } from "./daemon.mjs";
import { DIST, OUT, STEP_TIMEOUT_MS, INDEX_TIMEOUT_MS, here, log, settle, sleep, t, until } from "./harness.mjs";

const list = (v, d) => (v || d).split(",").map((s) => s.trim()).filter(Boolean);
const LOCALES = list(process.env.REVIEW_LOCALES, "en,ar,de");
const THEMES = list(process.env.REVIEW_THEMES, "light,dark");
const SIZES = list(process.env.REVIEW_SIZES, "900x600,1600x1000").map((s) => {
  const [width, height] = s.split("x").map(Number);
  return { name: s, width, height };
});
const ONLY = list(process.env.REVIEW_ONLY, "");
const REVIEW = path.join(OUT, "review");
const ONBOARDED = { version: 1, step: "done", completed: [], skipped: [], ai: null, finished: true };
const escapeRe = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const catalogs = {};
let current = "en";
/** The message `key` in the locale being captured (English when it has no translation). */
function tl(key) {
  catalogs[current] ??= JSON.parse(fs.readFileSync(path.join(here, "..", "src", "i18n", "locales", `${current}.json`), "utf8"));
  const s = catalogs[current][key];
  return typeof s === "string" ? s : t(key);
}
const exactName = (key) => new RegExp(`^${escapeRe(tl(key))}$`);
const wanted = (name) => ONLY.length === 0 || ONLY.some((p) => name.startsWith(p));

const PAGES = [...fs.readFileSync(path.join(here, "..", "src", "App.tsx"), "utf8").matchAll(/\{ id: "(\w+)", label: "([\w.]+)"/g)].map(
  ([, id]) => id,
);

/** Extra views per page: [name, steps(page)]. Locale-independent (roles, classes, data). */
const VIEWS = {
  search: [
    [
      "results",
      async (p) => {
        const box = p.locator("main input[type=search]").first();
        await box.fill("Tidewater budget");
        await box.press("Enter");
        await until("results", async () => [(await p.locator("ol.results li.result").count()) >= 3, null]);
        await box.blur();
      },
    ],
  ],
  knowledge: [
    [
      "entity",
      async (p) => {
        await p.locator("main").getByRole("button", { name: /^Ada Wexford\b/ }).first().click();
        await p.locator("section.entity-detail h2, section.entity-detail h1").first().waitFor();
        await settle(p);
        const why = p.locator("section.entity-detail button[aria-expanded]").first();
        if (await why.count()) await why.click();
      },
    ],
    [
      "dialog-merge",
      async (p) => {
        await p.locator("main").getByRole("button", { name: /^Ada Wexford\b/ }).first().click();
        await p.locator("section.entity-detail").waitFor();
        await p.locator("section.entity-detail").getByRole("button", { name: exactName("entity.merge") }).click();
        const d = p.locator("dialog[open]");
        await d.getByRole("searchbox").fill("");
        await until("candidates", async () => [(await d.locator(".merge-option").count()) > 0, null]);
        await d.locator(".merge-option input[type=radio]").first().check();
        await d.locator(".merge-preview dl").waitFor();
      },
    ],
    [
      "dialog-split",
      async (p) => {
        await p.locator("main").getByRole("button", { name: /^Ada Wexford\b/ }).first().click();
        await p.locator("section.entity-detail").waitFor();
        await p.locator("section.entity-detail").getByRole("button", { name: exactName("entity.split") }).click();
        await p.locator("dialog[open]").getByRole("checkbox").first().check();
      },
    ],
    [
      "graph",
      async (p) => {
        await p.getByRole("tab").nth(1).click();
        await p.locator("canvas.graph-canvas").waitFor();
        await sleep(2500);
      },
    ],
  ],
  sources: [
    [
      "dialog-add-kind",
      async (p) => {
        await p.locator("main").getByRole("button", { name: exactName("sources.add") }).first().click();
        await p.locator("dialog[open]").getByRole("radio").first().waitFor();
      },
    ],
    [
      "dialog-add-folder",
      async (p) => {
        await p.locator("main").getByRole("button", { name: exactName("sources.add") }).first().click();
        const d = p.locator("dialog[open]");
        await d.getByRole("radio").first().waitFor();
        await d.getByRole("button", { name: exactName("common.next") }).click();
        await d.getByRole("button", { name: exactName("addSource.folder.choose") }).waitFor();
      },
    ],
  ],
  apps: [
    [
      "dialog-connect",
      async (p) => {
        await p.locator("main").getByRole("button", { name: exactName("apps.connect") }).first().click();
        await p.locator("dialog[open]").waitFor();
      },
    ],
  ],
  today: [
    [
      "card-sources",
      async (p) => {
        const b = p.locator("li.today-card button[aria-controls]").first();
        if (await b.count()) {
          await b.click();
          await p.locator("li.today-card ul.evidence").first().waitFor();
        }
      },
    ],
    [
      "palette",
      async (p) => {
        await p.keyboard.press("Control+K");
        await p.locator("dialog[open]").getByRole("combobox").fill("se");
      },
    ],
    [
      "shortcuts",
      async (p) => {
        await p.keyboard.press("Shift+?");
        await p.locator("dialog[open]").waitFor();
      },
    ],
  ],
};

const OVERLAY_VIEWS = {
  reading: { phase: "reading", text: "" },
  streaming: { phase: "streaming", text: "Thanks Ines, the Tidewater Billing contract is with Vellum & Thistle; I'll", provider: "local", locality: "local", citations: 2 },
  done: {
    phase: "done",
    text: "Thanks Ines, the Tidewater Billing contract is with Vellum & Thistle LLP for review. Lucia expects to send comments by Friday, and I'll forward them as soon as they land.",
    provider: "local",
    locality: "local",
    citations: 2,
    excluded_sensitive: 1,
  },
  refused: { phase: "refused", text: "", message: "Double Tap is off in password managers." },
  error: { phase: "error", text: "", message: "The model stopped answering. Check Models, then try again." },
  inserted: { phase: "inserted", text: "", verified: true, method: "accessibility" },
};
const overlayView = (v) => ({ provider: null, locality: null, citations: 0, excluded_sensitive: 0, message: null, verified: null, method: null, ...v });

async function main() {
  const require = createRequire(import.meta.url);
  const { chromium } = require("playwright");
  if (!fs.existsSync(path.join(DIST, "index.html"))) throw new Error("no dist/: run `pnpm exec vite build` first");
  fs.rmSync(REVIEW, { recursive: true, force: true });
  fs.mkdirSync(REVIEW, { recursive: true });

  const work = fs.mkdtempSync(path.join(os.tmpdir(), "nugget-review-"));
  const bin = locateNuggetd(log);
  const daemon = await startDaemon(bin, { stderrPath: path.join(work, "nuggetd.out.log") });
  const empty = await startDaemon(bin, { stderrPath: path.join(work, "empty.out.log"), config: FIRST_RUN_CONFIG });
  let browser;
  const bridges = [];
  let n = 0;
  try {
    await addCorpusSource(daemon.socket);
    await waitForKnowledge(daemon.socket, { timeoutMs: INDEX_TIMEOUT_MS, log });
    await rpc(daemon.socket, "proactive.refresh", {}).catch(() => {});
    const full = await startBridge({ socket: daemon.socket, dist: DIST });
    const bare = await startBridge({ socket: empty.socket, dist: DIST });
    bridges.push(full, bare);
    browser = await chromium.launch(process.env.E2E_CHROMIUM ? { executablePath: process.env.E2E_CHROMIUM } : {});

    const newCtx = async ({ locale, theme, size, onboarded = true }) => {
      const ctx = await browser.newContext({
        viewport: { width: size.width, height: size.height },
        deviceScaleFactor: 1,
        locale: "en-US",
        timezoneId: "UTC",
        colorScheme: theme,
        reducedMotion: "reduce",
      });
      ctx.setDefaultTimeout(STEP_TIMEOUT_MS);
      await ctx.addInitScript({ path: path.join(here, "tauri-shim.js") });
      await ctx.addInitScript(
        ([loc, ob]) => {
          localStorage.setItem("nugget.locale", loc);
          if (ob) localStorage.setItem("nugget.onboarding.v1", JSON.stringify(ob));
        },
        [locale, onboarded ? ONBOARDED : null],
      );
      return ctx;
    };
    const snap = async (page, dir, name, { full = true } = {}) => {
      const file = path.join(REVIEW, dir, `${name}.png`);
      fs.mkdirSync(path.dirname(file), { recursive: true });
      const vp = page.viewportSize();
      // The app scrolls inside `main`, not the document: grow the window to the content instead.
      const extra = full
        ? await page.evaluate(() =>
            Math.max(
              0,
              ...[...document.querySelectorAll("*")]
                .filter((el) => /(auto|scroll)/.test(getComputedStyle(el).overflowY) && el.scrollHeight > el.clientHeight + 1)
                .map((el) => el.scrollHeight - el.clientHeight),
            ),
          )
        : 0;
      if (extra > 0) {
        await page.setViewportSize({ width: vp.width, height: Math.min(vp.height + extra, 5000) });
        await sleep(150);
      }
      await page.screenshot({ path: file, animations: "disabled", caret: "hide" });
      if (extra > 0) await page.setViewportSize(vp);
      n++;
    };
    const attempt = async (what, fn) => {
      try {
        await fn();
      } catch (e) {
        log(`  skipped ${what}: ${e.message.split("\n")[0]}`);
      }
    };
    const openPage = async (ctx, bridge, id) => {
      const page = await ctx.newPage();
      await page.addInitScript((p) => localStorage.setItem("nugget.page", p), id);
      await page.goto(`${bridge.url}index.html`);
      await page.locator("main h1").first().waitFor();
      await settle(page);
      return page;
    };

    for (const locale of LOCALES)
      for (const theme of THEMES)
        for (const size of SIZES) {
          const dir = `${locale}-${theme}-${size.name}`;
          current = locale;
          log(`${dir}…`);
          const ctx = await newCtx({ locale, theme, size });
          for (const id of PAGES) {
            if (wanted(`page-${id}`))
              await attempt(`${dir} page-${id}`, async () => {
                const page = await openPage(ctx, full, id);
                await snap(page, dir, `page-${id}`);
                await page.close();
              });
            for (const [view, steps] of VIEWS[id] ?? []) {
              if (!wanted(`page-${id}-${view}`)) continue;
              await attempt(`${dir} page-${id}-${view}`, async () => {
                const page = await openPage(ctx, full, id);
                await steps(page);
                await settle(page);
                await sleep(200);
                await snap(page, dir, `page-${id}-${view}`, { full: !view.startsWith("dialog") && view !== "palette" && view !== "shortcuts" && view !== "graph" });
                await page.close();
              });
            }
          }
          await ctx.close();

          // Onboarding, step by step.
          if (wanted("onboarding"))
            await attempt(`${dir} onboarding`, async () => {
              const octx = await newCtx({ locale, theme, size, onboarded: false });
              const page = await octx.newPage();
              await page.goto(`${bare.url}index.html`);
              const cont = () => page.locator("footer.ob-actions button.btn-primary").last().click();
              const steps = ["welcome", "ai", "source", "permissions", "trigger", "done"];
              for (const [i, s] of steps.entries()) {
                await page.locator("h1").first().waitFor();
                await settle(page);
                await sleep(150);
                if (s === "ai") {
                  await page.getByRole("radio").nth(0).check();
                  await settle(page);
                  await snap(page, dir, `onboarding-2-ai-local`);
                  await page.getByRole("radio").nth(1).check();
                  await snap(page, dir, `onboarding-2-ai-byok`);
                  await page.getByRole("radio").nth(2).check();
                }
                await snap(page, dir, `onboarding-${i + 1}-${s}`);
                if (s === "source") {
                  await page.getByRole("button", { name: exactName("onboarding.source.addFolder") }).click();
                  await page.locator("dialog[open]").waitFor();
                  await snap(page, dir, `onboarding-3-source-folder`, { full: false });
                  await page.keyboard.press("Escape");
                  await page.locator("dialog[open]").waitFor({ state: "detached" });
                }
                if (i < steps.length - 1) await cont();
              }
              await octx.close();
            });

          // The Double Tap overlay, in each phase.
          if (wanted("overlay")) {
            const octx = await browser.newContext({
              viewport: { width: 460, height: 260 },
              deviceScaleFactor: 1,
              locale: "en-US",
              colorScheme: theme,
              reducedMotion: "reduce",
            });
            await octx.addInitScript({ path: path.join(here, "tauri-shim.js") });
            await octx.addInitScript((loc) => localStorage.setItem("nugget.locale", loc), locale);
            for (const [phase, v] of Object.entries(OVERLAY_VIEWS)) {
              await attempt(`${dir} overlay-${phase}`, async () => {
                const page = await octx.newPage();
                await page.route("**/invoke", async (route) => {
                  const body = JSON.parse(route.request().postData() ?? "{}");
                  if (body.cmd === "overlay_state") return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ ok: overlayView(v) }) });
                  return route.continue();
                });
                await page.goto(`${full.url}overlay.html`);
                await settle(page);
                await sleep(300);
                await snap(page, dir, `overlay-${phase}`, { full: false });
                await page.close();
              });
            }
            await octx.close();
          }
        }

    // Empty (a fresh home), loading (the daemon never answers) and error (every daemon command fails).
    if (process.env.REVIEW_STATES !== "0") {
      const SHELL = new Set(["connection_info", "start_daemon", "updates_status", "autostart_get", "doubletap_settings_get", "doubletap_status", "doubletap_default_excluded", "chat_history_load"]);
      for (const locale of LOCALES.includes("en") ? ["en"] : LOCALES.slice(0, 1))
        for (const theme of THEMES) {
          const size = SIZES[0];
          const dir = `states-${locale}-${theme}-${size.name}`;
          log(`${dir}…`);
          for (const [state, bridge, route] of [
            ["empty", bare, null],
            ["loading", full, (r, body) => (SHELL.has(body.cmd) ? r.continue() : new Promise(() => {}))],
            [
              "error",
              full,
              (r, body) =>
                SHELL.has(body.cmd)
                  ? r.continue()
                  : r.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ err: { kind: "rpc", code: -32000, message: "database is locked" } }) }),
            ],
          ]) {
            const ctx = await newCtx({ locale, theme, size });
            if (route)
              await ctx.route("**/invoke", (r) => {
                let body = {};
                try {
                  body = JSON.parse(r.request().postData() ?? "{}");
                } catch {}
                return route(r, body);
              });
            for (const id of PAGES) {
              if (!wanted(`${state}-${id}`)) continue;
              await attempt(`${dir} ${state}-${id}`, async () => {
                const page = await ctx.newPage();
                await page.addInitScript((p) => localStorage.setItem("nugget.page", p), id);
                await page.goto(`${bridge.url}index.html`);
                await page.locator("main h1").first().waitFor();
                if (state === "loading") await sleep(700);
                else await settle(page);
                await snap(page, dir, `${state}-${id}`);
                await page.close();
              });
            }
            await ctx.close();
          }
        }
    }
  } finally {
    await browser?.close().catch(() => {});
    for (const b of bridges) await b.close().catch(() => {});
    await daemon.stop();
    await empty.stop();
  }
  log(`wrote ${n} screenshots to ${path.relative(process.cwd(), REVIEW)}`);
}

main().catch((e) => {
  console.error(`[review] failed: ${e.stack ?? e}`);
  process.exit(1);
});
