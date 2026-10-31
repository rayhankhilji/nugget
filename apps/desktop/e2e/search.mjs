// Faceted search, saved searches, timelines and previews (e2e scenario "search", II2), on the
// indexed home the "pages" scenario uses. The real daemon answers `search.faceted`,
// `search.timeline` and `search.preview`; the test drives the real Search page:
// - a query shows filters with counts; ticking a Kind narrows the results to that count and the
//   daemon was asked with that filter;
// - the search is saved under a name, shows in the sidebar, and runs again from the command
//   palette on another page with its query and filter restored;
// - from the Knowledge page, Ada Wexford's Timeline lists dated items and facts in time order,
//   and Jump to month moves focus to that month;
// - a result's preview shows the item's text and closes with Escape, returning focus.
// See e2e/README.md.
import path from "node:path";
import { startBridge, METHOD_NOT_FOUND } from "./bridge.mjs";
import { check, DIST, expect, here, log, settle, shot, STEP_TIMEOUT_MS, t, until } from "./harness.mjs";

const MOD = process.platform === "darwin" ? "Meta" : "Control";
const QUERY = "budget";
const SAVED_NAME = "Budget, one kind";

/** The integer at the end of a facet checkbox's accessible name ("Mail 3" → 3). */
const countOf = (name) => Number(name.match(/(\d[\d,]*)\s*$/)?.[1]?.replace(/,/g, "") ?? NaN);

export async function searchScenario({ browser, socket, onboarded }) {
  const bridge = await startBridge({ socket, dist: DIST });
  const ctx = await browser.newContext({ viewport: { width: 1360, height: 860 }, locale: "en-US", timezoneId: "UTC" });
  ctx.setDefaultTimeout(STEP_TIMEOUT_MS);
  await ctx.addInitScript({ path: path.join(here, "tauri-shim.js") });
  await ctx.addInitScript(([key, state]) => localStorage.setItem(key, JSON.stringify(state)), ["nugget.onboarding.v1", onboarded]);
  const page = await ctx.newPage();
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));
  const out = {};
  const main = page.locator("main");
  const results = main.locator("ol.results > li");
  const heading = main.locator("#search-results-heading");

  const openPage = async (id) => {
    await page.evaluate((p) => localStorage.setItem("nugget.page", p), id).catch(() => {});
    await page.goto(`${bridge.url}index.html`);
    await page.locator("main h1").first().waitFor();
  };

  try {
    await page.addInitScript(() => localStorage.setItem("nugget.page", "search"));
    await page.goto(`${bridge.url}index.html`);
    const box = page.getByRole("searchbox", { name: t("search.query") });
    await box.waitFor();

    let kindName = null;
    await check("search: a query shows filters with counts", async () => {
      await box.fill(QUERY);
      await box.press("Enter");
      const filters = main.locator("#search-filters");
      const kinds = filters.getByRole("group", { name: t("searchui.facet.kind") }).getByRole("checkbox");
      const names = await until("the Kind filter lists kinds with counts", async () => {
        const n = await kinds.evaluateAll((els) => els.map((e) => e.getAttribute("aria-label") || e.closest("label")?.textContent?.trim() || ""));
        return [n.length > 0 && n.every((x) => countOf(x) > 0) && (await results.count()) > 0, n];
      });
      const sources = await filters.getByRole("group", { name: t("searchui.facet.source") }).getByRole("checkbox").count();
      expect(sources > 0, "the Source filter lists no sources");
      out.kinds = names;
      log(`  kinds: ${JSON.stringify(names)}; ${sources} source(s); ${await results.count()} result(s)`);
      // Narrow to the kind with the fewest results, so the filter visibly changes the list.
      kindName = names.reduce((a, b) => (countOf(b) < countOf(a) ? b : a));
      await settle(page);
      await shot(page, "search-facets");
    });

    await check("search: ticking a Kind narrows the results and asks the daemon with that filter", async () => {
      expect(kindName, "no kind to filter by");
      const want = countOf(kindName);
      const label = kindName.replace(/\s*\d[\d,]*\s*$/, "");
      const box = main.locator("#search-filters").getByRole("group", { name: t("searchui.facet.kind") }).getByRole("checkbox", { name: new RegExp(`^${label}\\b`) });
      await box.check();
      const got = await until(`results narrow to ${want} ${label} item(s)`, async () => {
        const n = await results.count();
        const badges = await results.evaluateAll((lis, l) => lis.filter((li) => [...li.querySelectorAll(".badge")].some((b) => b.textContent?.trim() === l)).length, label);
        return [n === Math.min(want, 25) && badges === n, { n, badges }];
      });
      const asked = bridge.calls.filter((c) => c.cmd === "search_faceted" && c.ok !== false);
      expect(asked.length >= 2, `search_faceted was called ${asked.length} time(s)`);
      out.filtered = got;
      await settle(page);
      await shot(page, "search-filtered");
    });

    await check("search: save the search; it shows in the sidebar", async () => {
      await main.getByRole("button", { name: t("searchui.saveSearch") }).click();
      const dialog = page.getByRole("dialog", { name: t("searchui.saveTitle") });
      await dialog.getByRole("textbox", { name: t("searchui.saveName") }).fill(SAVED_NAME);
      await dialog.getByRole("button", { name: t("searchui.save"), exact: true }).click();
      await dialog.waitFor({ state: "detached" });
      const run = t("searchui.runSaved").replace("{name}", SAVED_NAME);
      await page.locator(".nav-saved").getByRole("button", { name: run }).waitFor();
      await main.getByRole("button", { name: run }).first().waitFor();
    });

    await check("search: the saved search runs again from the command palette on another page", async () => {
      await openPage("today");
      await page.keyboard.press(`${MOD}+K`);
      const input = page.getByRole("combobox", { name: t("palette.inputLabel") });
      await input.waitFor();
      await input.fill(SAVED_NAME);
      const option = page.getByRole("option", { name: new RegExp(t("searchui.paletteSaved").replace("{name}", SAVED_NAME).replace(/[.*+?^${}()|[\]\\]/g, "\\$&")) });
      await option.first().waitFor();
      await page.keyboard.press("Enter");
      await until("Search reopens with the saved query and filter", async () => {
        const q = await page.getByRole("searchbox", { name: t("search.query") }).inputValue().catch(() => "");
        const ticked = await main.locator("#search-filters input[type=checkbox]:checked").count().catch(() => 0);
        return [q === QUERY && ticked === 1 && (await results.count()) > 0, { q, ticked }];
      });
    });

    await check("search: a result's preview shows its text and closes with Escape", async () => {
      const first = results.first().locator(".result-select");
      await first.click();
      const panel = main.locator("#search-preview");
      await panel.waitFor();
      const text = await until("the preview shows the item's text", async () => {
        const s = (await panel.locator("#search-preview-title").textContent())?.trim() ?? "";
        const body = (await panel.textContent()) ?? "";
        return [s.length > 0 && body.length > s.length + 40, s];
      });
      log(`  preview: ${text}`);
      await settle(page);
      await shot(page, "search-preview");
      await page.keyboard.press("Escape");
      await panel.waitFor({ state: "detached" });
      await until("focus returns to the result", async () => [await first.evaluate((el) => el === document.activeElement), null]);
    });

    await check("search: Ada Wexford's timeline from the Knowledge page, in time order, with jump to month", async () => {
      await openPage("knowledge");
      const list = page.getByRole("region", { name: t("knowledge.entitiesLabel") });
      await list.getByRole("button", { name: /^Ada Wexford\b/ }).first().click();
      const detail = page.locator("section.entity-detail");
      await detail.getByRole("heading", { name: "Ada Wexford" }).first().waitFor();
      await detail.getByRole("button", { name: t("searchui.timelineFor").replace("{name}", "Ada Wexford") }).click();
      const h = page.locator("h2#timeline-heading");
      await h.waitFor();
      expect(((await h.textContent()) ?? "").includes("Ada Wexford"), "the timeline heading does not name Ada Wexford");
      const events = await until("the timeline lists items and facts", async () => {
        const items = await main.locator("li.sr-tl-item").count();
        const facts = await main.locator("li.sr-tl-fact").count();
        return [items > 0 && facts > 0, { items, facts }];
      });
      const months = await main.locator("section.sr-tl-month h3[id^=timeline-month-]").evaluateAll((hs) => hs.map((h) => h.id.slice("timeline-month-".length)));
      const dated = months.filter((m) => /^\d{4}-\d{2}$/.test(m));
      expect(dated.join() === [...dated].sort().join(), `months are not in time order: ${JSON.stringify(months)}`);
      log(`  timeline: ${events.items} item(s), ${events.facts} fact(s) over ${JSON.stringify(months)}`);
      await settle(page);
      await shot(page, "search-timeline");
      if (dated.length > 1) {
        const target = dated[0];
        await main.getByRole("combobox", { name: t("searchui.jumpToMonth") }).selectOption(target);
        const id = `timeline-month-${target}`;
        await until(`focus moves to ${target}`, async () => [await page.evaluate((id) => document.activeElement?.id === id, id), null]);
      }
      out.timeline = { ...events, months };
    });

    await check("search: every search command reached the daemon and was accepted", async () => {
      const bad = bridge.calls.filter((c) => c.cmd.startsWith("search_") && (c.code === METHOD_NOT_FOUND || c.code === -32602 || c.ok === false));
      expect(bad.length === 0, `failed search commands: ${JSON.stringify(bad)}`);
      const used = new Set(bridge.calls.map((c) => c.cmd));
      for (const cmd of ["search_faceted", "search_timeline", "search_preview"]) expect(used.has(cmd), `${cmd} was never called`);
      expect(errors.length === 0, `uncaught page errors: ${JSON.stringify(errors)}`);
    });
    return out;
  } finally {
    await ctx.close().catch(() => {});
    await bridge.close().catch(() => {});
  }
}
