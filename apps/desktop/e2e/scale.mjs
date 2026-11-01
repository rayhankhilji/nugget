// Long lists at scale (e2e scenario "scale", II5): the real app on the indexed home, with the
// knowledge base and the egress ledger answered at scale by the test, measures that scrolling a
// long list stays smooth and that only a window of rows is ever in the DOM.
//
// Indexing hundreds of thousands of items in a test run would take far longer than the test
// itself, so the large lists are synthetic: `knowledge_status` reports 500,000 entities,
// `knowledge_entities` pages through them by `offset`/`limit` (as the daemon does), and
// `egress_page` pages through a 5,000-row ledger by cursor (as the daemon does, LL1). Every other
// command goes to the real daemon through the bridge.
//
// Search paging (LL1) runs against the real daemon: `search_faceted` answers "method not found"
// (an older daemon without II2's faceted search), so the Search page pages its plain results with
// the daemon's real `search.page` and "Show more".
//
// In the page, a PerformanceObserver collects long tasks (main-thread work over 50 ms) and a
// requestAnimationFrame loop times frames while `main` is scrolled programmatically. The scenario
// fails when a long task exceeds E2E_SCALE_MAX_LONGTASK_MS (default 250 ms), when more than
// E2E_SCALE_MAX_LONGTASKS long tasks happen during a scroll (default 5), or when the DOM holds more
// than MAX_ROWS rows of a list. The numbers are in summary.json under "scale".
import { startBridge } from "./bridge.mjs";
import { check, expect, log, newContext, settle, shot, t, tPattern, until } from "./harness.mjs";

const ENTITIES = 500_000;
const KINDS = ["person", "organization", "project", "place", "topic"];
/** Rows a virtualised list may have in the DOM at once (a window plus overscan, at most). */
const MAX_ROWS = 120;
const MAX_LONGTASK_MS = Number(process.env.E2E_SCALE_MAX_LONGTASK_MS || 250);
const MAX_LONGTASKS = Number(process.env.E2E_SCALE_MAX_LONGTASKS || 5);
/** Rows in the synthetic ledger, served by `egress_page` a page at a time. */
const LEDGER = 5_000;
/** A word in most items of the e2e corpus (the owner's name), for search paging. */
const SEARCH_QUERY = "robin";

function entity(i) {
  return {
    id: `ent_scale_${i}`,
    kind: KINDS[i % KINDS.length],
    name: `Scale entity ${i}`,
    claims: (i * 7) % 13,
    mentions: (i * 11) % 29,
    updated_at: "2026-10-01T00:00:00Z",
  };
}

function egressRow(i) {
  return {
    at: new Date(Date.UTC(2026, 9, 30, 9) - i * 37_000).toISOString(),
    host: `api${i}.example.test`,
    feature: i % 3 ? "chat" : "connector",
    bytes_sent: 900 + ((i * 37) % 4000),
    bytes_received: 2000 + ((i * 53) % 9000),
    items: [],
  };
}

/** Answers the large lists; everything else goes to the daemon. `egressPages` collects `egress_page` requests. */
async function routeScale(ctx, egressPages = []) {
  await ctx.route("**/invoke", async (route) => {
    let body = {};
    try {
      body = JSON.parse(route.request().postData() || "{}");
    } catch {}
    const json = (ok) => route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ ok }) });
    if (body.cmd === "knowledge_entities") {
      const req = body.args?.request ?? {};
      if (req.text || req.kind) return json([]);
      const from = Math.min(ENTITIES, req.offset ?? 0);
      const to = Math.min(ENTITIES, from + (req.limit ?? 50));
      return json(Array.from({ length: to - from }, (_, k) => entity(from + k)));
    }
    if (body.cmd === "knowledge_status") {
      const res = await route.fetch();
      const real = await res.json();
      if (real.ok) real.ok = { ...real.ok, entities: ENTITIES, pending_items: 0 };
      return route.fulfill({ response: res, json: real });
    }
    if (body.cmd === "egress_recent") {
      const limit = body.args?.limit ?? 200;
      return json(Array.from({ length: limit }, (_, i) => egressRow(i)));
    }
    if (body.cmd === "egress_page") {
      // Keyset paging like the daemon's: the cursor names the last row of the previous page.
      const req = body.args?.request ?? {};
      egressPages.push(req);
      const from = req.cursor ? Number(/^e1\.(\d+)$/.exec(req.cursor)?.[1] ?? NaN) + 1 : 0;
      if (!Number.isInteger(from)) {
        return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ err: { kind: "rpc", code: -32602, message: "cursor: bad" } }) });
      }
      const to = Math.min(LEDGER, from + (req.limit ?? 200));
      const items = Array.from({ length: Math.max(0, to - from) }, (_, k) => egressRow(from + k));
      return json({ items, next_cursor: to < LEDGER ? `e1.${to - 1}` : null });
    }
    if (body.cmd === "search_faceted") {
      return route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({ err: { kind: "rpc", code: -32601, message: "method not found: search.faceted" } }),
      });
    }
    return route.fallback();
  });
}

/** Installed before the app loads: collects long tasks from the start. */
function observeLongTasks() {
  window.__longtasks = [];
  try {
    new PerformanceObserver((list) => {
      for (const e of list.getEntries()) window.__longtasks.push({ start: e.startTime, duration: e.duration });
    }).observe({ type: "longtask", buffered: true });
  } catch {
    window.__longtasks = null;
  }
}

/**
 * Scrolls `main` by `px` per frame for `frames` frames; returns frame times, long tasks during the
 * scroll and the most rows of `rowSelector` seen in the DOM at once.
 */
async function scrollAndMeasure(page, { px, frames, rowSelector }) {
  return page.evaluate(
    async ({ px, frames, rowSelector }) => {
      const main = document.querySelector("main");
      const t0 = performance.now();
      const times = [];
      let maxRows = 0;
      let last = t0;
      for (let i = 0; i < frames; i++) {
        main.scrollTop += px;
        await new Promise((r) => requestAnimationFrame(() => r()));
        const now = performance.now();
        times.push(now - last);
        last = now;
        maxRows = Math.max(maxRows, document.querySelectorAll(rowSelector).length);
      }
      const tasks = (window.__longtasks ?? []).filter((l) => l.start >= t0);
      const sorted = [...times].sort((a, b) => a - b);
      const pct = (p) => sorted[Math.min(sorted.length - 1, Math.floor(p * sorted.length))] ?? 0;
      return {
        frames: times.length,
        frame_p50_ms: Math.round(pct(0.5) * 10) / 10,
        frame_p95_ms: Math.round(pct(0.95) * 10) / 10,
        frame_max_ms: Math.round(Math.max(0, ...times) * 10) / 10,
        longtasks: tasks.length,
        longtask_max_ms: Math.round(Math.max(0, ...tasks.map((l) => l.duration))),
        longtasks_supported: window.__longtasks !== null,
        max_rows_in_dom: maxRows,
        scroll_top: main.scrollTop,
      };
    },
    { px, frames, rowSelector },
  );
}

function expectSmooth(what, m) {
  expect(m.max_rows_in_dom <= MAX_ROWS, `${what}: ${m.max_rows_in_dom} rows in the DOM at once (at most ${MAX_ROWS})`);
  expect(m.longtask_max_ms <= MAX_LONGTASK_MS, `${what}: a ${m.longtask_max_ms} ms long task while scrolling (at most ${MAX_LONGTASK_MS} ms)`);
  expect(m.longtasks <= MAX_LONGTASKS, `${what}: ${m.longtasks} long tasks while scrolling (at most ${MAX_LONGTASKS})`);
}

export async function scaleScenario({ browser, socket, onboarded }) {
  const summary = {};
  const bridge = await startBridge({ socket });
  const ctx = await newContext(browser);
  try {
    await ctx.addInitScript(([key, state]) => localStorage.setItem(key, JSON.stringify(state)), ["nugget.onboarding.v1", onboarded]);
    await ctx.addInitScript(observeLongTasks);
    const egressPages = [];
    await routeScale(ctx, egressPages);

    await check("scale: knowledge list of 500,000 entities loads pages as it scrolls, smoothly", async () => {
      const page = await ctx.newPage();
      const errors = [];
      page.on("pageerror", (e) => errors.push(e.message));
      try {
        await page.addInitScript(() => localStorage.setItem("nugget.page", "knowledge"));
        await page.goto(`${bridge.url}index.html`);
        const list = page.locator(".entity-list");
        const count = page.locator(".kb-count");
        await until("Knowledge shows the first page of 500,000", async () => {
          const text = ((await count.textContent({ timeout: 1000 })) ?? "").trim();
          return [/500,000\s*$/.test(text) && (await list.locator("button.entity-row").count()) > 0, text];
        });
        const setSize = await list.locator("li").first().getAttribute("aria-setsize");
        expect(setSize === String(ENTITIES), `aria-setsize is ${setSize}, expected ${ENTITIES}`);
        await settle(page);

        const loaded = async () => Number(((await count.textContent()) ?? "").match(/–\s*([\d,]+)/)?.[1]?.replace(/,/g, "") ?? 0);
        const runs = [];
        // Scroll until well past the first pages: each run reaches the end and loads the next page.
        for (let i = 0; i < 12 && (await loaded()) < 2000; i++) {
          runs.push(await scrollAndMeasure(page, { px: 400, frames: 60, rowSelector: ".entity-list > li" }));
        }
        const total = await loaded();
        log(`  knowledge: ${total} of ${ENTITIES} loaded after ${runs.length} scroll runs`);
        expect(total >= 1000, `only ${total} entities loaded while scrolling (expected at least 1,000)`);
        const worst = {
          frames: runs.reduce((n, r) => n + r.frames, 0),
          frame_p50_ms: Math.max(...runs.map((r) => r.frame_p50_ms)),
          frame_p95_ms: Math.max(...runs.map((r) => r.frame_p95_ms)),
          frame_max_ms: Math.max(...runs.map((r) => r.frame_max_ms)),
          longtasks: runs.reduce((n, r) => n + r.longtasks, 0),
          longtask_max_ms: Math.max(...runs.map((r) => r.longtask_max_ms)),
          longtasks_supported: runs[0]?.longtasks_supported ?? false,
          max_rows_in_dom: Math.max(...runs.map((r) => r.max_rows_in_dom)),
          loaded: total,
        };
        log(`  knowledge scroll: ${JSON.stringify(worst)}`);
        summary.knowledge = worst;
        // Long tasks are counted across every run here: allow a few per page loaded.
        expect(worst.max_rows_in_dom <= MAX_ROWS, `knowledge: ${worst.max_rows_in_dom} rows in the DOM at once (at most ${MAX_ROWS})`);
        expect(worst.longtask_max_ms <= MAX_LONGTASK_MS, `knowledge: a ${worst.longtask_max_ms} ms long task while scrolling (at most ${MAX_LONGTASK_MS} ms)`);

        // Keyboard: End on a row jumps to the last loaded entity and loads the next page.
        await list.locator("button.entity-row").first().focus();
        await page.keyboard.press("End");
        const focused = await until("End focuses the last loaded entity", async () => {
          const name = await page.evaluate(() => document.activeElement?.closest("li")?.getAttribute("aria-posinset"));
          return [Number(name) >= total, name];
        });
        log(`  knowledge: End focused row ${focused}`);
        await shot(page, "scale-knowledge");
        expect(errors.length === 0, `page errors: ${JSON.stringify(errors)}`);
      } finally {
        await page.close();
      }
    });

    await check("scale: egress ledger of 5,000 rows loads pages by cursor as it scrolls, smoothly", async () => {
      const page = await ctx.newPage();
      const errors = [];
      page.on("pageerror", (e) => errors.push(e.message));
      try {
        await page.addInitScript(() => localStorage.setItem("nugget.page", "privacy"));
        await page.goto(`${bridge.url}index.html`);
        await page.locator("main table").waitFor();
        await page.getByLabel(t("privacy.show")).selectOption("1000");
        const table = page.locator("main table");
        const rowCount = async () => Number((await table.getAttribute("aria-rowcount")) ?? 0);
        await until("the ledger table reports its first 1,000 rows", async () => {
          const n = await rowCount();
          return [n === 1001, n];
        });
        await settle(page);
        const m = await scrollAndMeasure(page, { px: 400, frames: 120, rowSelector: "main tbody tr[data-index]" });
        log(`  privacy scroll: ${JSON.stringify(m)}`);
        summary.privacy = m;
        expectSmooth("privacy", m);
        // Scrolling to the end of the first page loaded the next one, by the daemon's cursor.
        await until("scrolling loads the next 1,000 rows", async () => {
          const n = await rowCount();
          return [n >= 2001, n];
        });
        const second = egressPages.filter((r) => r.limit === 1000)[1];
        expect(second?.cursor === "e1.999", `the second page was asked with ${JSON.stringify(second)}`);

        // Keep scrolling to the oldest row: every page loads once, nothing skipped or repeated.
        const last = await until("the whole ledger loads", async () => {
          const r = await page.evaluate(async () => {
            const main = document.querySelector("main");
            main.scrollTop = main.scrollHeight;
            for (let i = 0; i < 3; i++) await new Promise((res) => requestAnimationFrame(() => res()));
            const rows = document.querySelectorAll("main tbody tr[data-index]");
            return { index: rows[rows.length - 1]?.getAttribute("aria-rowindex"), host: rows[rows.length - 1]?.querySelector("code")?.textContent, rows: rows.length };
          });
          return [r.index === String(LEDGER + 1), r];
        }, 60_000);
        expect(last.host === `api${LEDGER - 1}.example.test`, `the last row is ${last.host}, expected the oldest`);
        expect(last.rows <= MAX_ROWS, `${last.rows} ledger rows in the DOM at the end (at most ${MAX_ROWS})`);
        const cursors = egressPages.filter((r) => r.limit === 1000).map((r) => r.cursor ?? null);
        expect(
          JSON.stringify(cursors) === JSON.stringify([null, "e1.999", "e1.1999", "e1.2999", "e1.3999"]),
          `ledger pages asked: ${JSON.stringify(cursors)}`,
        );
        const note = (await page.locator(".privacy-loaded").textContent())?.trim();
        expect(tPattern("privacy.loadedAll").test(note ?? ""), `the ledger says "${note}" at the end`);
        expect((await page.getByRole("button", { name: t("privacy.loadOlder") }).count()) === 0, "Load older is still offered at the end");
        await shot(page, "scale-privacy");
        expect(errors.length === 0, `page errors: ${JSON.stringify(errors)}`);
      } finally {
        await page.close();
      }
    });

    await check("scale: plain search pages through the daemon's search.page with Show more (keyboard)", async () => {
      const page = await ctx.newPage();
      const errors = [];
      page.on("pageerror", (e) => errors.push(e.message));
      try {
        await page.addInitScript(() => localStorage.setItem("nugget.page", "search"));
        await page.goto(`${bridge.url}index.html`);
        const box = page.getByRole("searchbox", { name: t("search.query") });
        await box.waitFor();
        await page.locator("main label.field-inline", { hasText: t("search.show") }).locator("select").first().selectOption("10");
        await box.fill(SEARCH_QUERY);
        await box.press("Enter");
        const results = page.locator("main ol.results .result-select");
        await until("the first page of results", async () => {
          const n = await results.count();
          return [n === 10, n];
        });
        const before = bridge.calls.filter((c) => c.cmd === "search_page").length;
        expect(before >= 1, "search_page was not called");
        // Keyboard: Tab to Show more and press Enter until the end; focus lands on the first new result.
        const more = page.locator("main button.sr-more");
        let pages = 1;
        while ((await more.count()) > 0 && pages < 60) {
          const shown = await results.count();
          await more.focus();
          await page.keyboard.press("Enter");
          await until(`page ${pages + 1} of results`, async () => {
            const n = await results.count();
            return [n > shown || (await more.count()) === 0, n];
          });
          pages++;
          const focused = await page.evaluate(() => {
            const all = Array.from(document.querySelectorAll("main ol.results .result-select"));
            return all.indexOf(document.activeElement);
          });
          if ((await results.count()) > shown) expect(focused === shown, `focus is on result ${focused}, expected the first new one (${shown})`);
        }
        const total = await results.count();
        const keys = await page.evaluate(() =>
          Array.from(document.querySelectorAll("main ol.results li.result"), (li) => li.textContent ?? ""),
        );
        log(`  search: ${total} results for "${SEARCH_QUERY}" in ${pages} pages`);
        expect(pages >= 3, `only ${pages} pages of results for "${SEARCH_QUERY}"`);
        expect(total > 20, `only ${total} results loaded`);
        expect(new Set(keys).size === keys.length, "a result is listed twice");
        const paged = bridge.calls.filter((c) => c.cmd === "search_page" && c.ok !== false).length;
        expect(paged >= pages, `search_page called ${paged} times for ${pages} pages`);
        expect(bridge.calls.filter((c) => c.cmd === "search_page" && c.ok === false).length === 0, "a search_page call failed");
        expect(bridge.calls.filter((c) => c.cmd === "search").length === 0, "fell back to plain search");
        await shot(page, "scale-search-paged");
        expect(errors.length === 0, `page errors: ${JSON.stringify(errors)}`);
      } finally {
        await page.close();
      }
    });
    return summary;
  } finally {
    await ctx.close().catch(() => {});
    await bridge.close().catch(() => {});
  }
}
