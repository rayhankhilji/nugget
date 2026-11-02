// Search on the indexed home: filters with counts, a saved search run again from the palette,
// a result's preview, and a person's timeline from Knowledge (search.faceted/.preview/.timeline).
import { startBridge } from "./bridge.mjs";
import { check, DIST, expect, navTo, newContext, t, until } from "./harness.mjs";

const MOD = process.platform === "darwin" ? "Meta" : "Control";
const QUERY = "budget";
const SAVED_NAME = "Budget, one kind";

/** The count in a facet checkbox's accessible name ("Mail, 3 results" → 3; `searchui.facetOption`). */
const countOf = (name) => Number(name.match(/,\s*(\d[\d,]*)\s+results?$/)?.[1]?.replace(/,/g, "") ?? NaN);
/** The value label in a facet checkbox's accessible name ("Mail, 3 results" → "Mail"). */
const labelOf = (name) => name.replace(/,\s*\d[\d,]*\s+results?$/, "");

export async function searchScenario({ browser, socket }) {
  const bridge = await startBridge({ socket, dist: DIST });
  const ctx = await newContext(browser, { onboarded: true });
  const page = await ctx.newPage();
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));
  const main = page.locator("main");
  const results = main.locator("ol.results > li");

  // Through the sidebar, like a person would (the app keeps its state; nothing reloads).
  const openPage = async (id) => {
    await navTo(page, id);
    await page.locator("main h1").first().waitFor();
  };

  try {
    // Start on Search: set the remembered page once, then reload (an init script would run on
    // every navigation).
    await page.goto(`${bridge.url}index.html`);
    await page.evaluate(() => localStorage.setItem("nugget.page", "search"));
    await page.reload();
    const box = page.getByRole("searchbox", { name: t("search.query") });
    await box.waitFor();

    await check("search: a heed://search link types the query in and waits for Enter", async () => {
      bridge.queueDeepLink({ kind: "search", query: QUERY });
      await page.evaluate(() => window.__E2E_EMIT__("nugget:deeplink", null));
      await until("the link's query is typed in", async () => [(await box.inputValue()) === QUERY]);
      const ran = bridge.calls.filter((c) => c.cmd.startsWith("search"));
      expect(ran.length === 0, `a link ran a search by itself: ${JSON.stringify(ran)}`);
    });

    let kindName = null;
    await check("search: a query shows filters with counts", async () => {
      await box.fill(QUERY);
      await box.press("Enter");
      const filters = main.locator("#search-filters");
      const kinds = filters.getByRole("group", { name: t("searchui.facet.kind") }).getByRole("checkbox");
      const names = await until("the Kind filter lists kinds with counts", async () => {
        const n = await kinds.evaluateAll((els) => els.map((e) => e.getAttribute("aria-label") ?? ""));
        return [n.length > 0 && n.every((x) => countOf(x) > 0) && (await results.count()) > 0, n];
      });
      const sources = await filters.getByRole("group", { name: t("searchui.facet.source") }).getByRole("checkbox").count();
      expect(sources > 0, "the Source filter lists no sources");
      // Narrow to the kind with the fewest results, so the filter visibly changes the list.
      kindName = names.reduce((a, b) => (countOf(b) < countOf(a) ? b : a));
    });

    await check("search: ticking a Kind narrows the results and asks the daemon with that filter", async () => {
      expect(kindName, "no kind to filter by");
      const want = countOf(kindName);
      const label = labelOf(kindName);
      const box = main.locator("#search-filters").getByRole("group", { name: t("searchui.facet.kind") }).getByRole("checkbox", { name: kindName, exact: true });
      await box.check();
      await until(`results narrow to ${want} ${label} item(s)`, async () => {
        const n = await results.count();
        const badges = await results.evaluateAll((lis, l) => lis.filter((li) => [...li.querySelectorAll(".badge")].some((b) => b.textContent?.trim() === l)).length, label);
        return [n === Math.min(want, 25) && badges === n, { n, badges }];
      });
      const asked = bridge.calls.filter((c) => c.cmd === "search_faceted" && c.ok !== false);
      expect(asked.length >= 2, `search_faceted was called ${asked.length} time(s)`);
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
      await until("the preview shows the item's text", async () => {
        const s = (await panel.locator("#search-preview-title").textContent())?.trim() ?? "";
        const body = (await panel.textContent()) ?? "";
        return [s.length > 0 && body.length > s.length + 40, s];
      });
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
      // The heading reads "Timeline" until the entity's timeline has loaded, then names it.
      await until("the timeline heading names Ada Wexford", async () => {
        const text = (await h.textContent()) ?? "";
        return [text.includes("Ada Wexford"), text];
      });
      await until("the timeline lists items and facts", async () => {
        const items = await main.locator("li.sr-tl-item").count();
        const facts = await main.locator("li.sr-tl-fact").count();
        return [items > 0 && facts > 0, { items, facts }];
      });
      const months = await main.locator("section.sr-tl-month h3[id^=timeline-month-]").evaluateAll((hs) => hs.map((h) => h.id.slice("timeline-month-".length)));
      const dated = months.filter((m) => /^\d{4}-\d{2}$/.test(m));
      expect(dated.join() === [...dated].sort().join(), `months are not in time order: ${JSON.stringify(months)}`);
      if (dated.length > 1) {
        const target = dated[0];
        await main.getByRole("combobox", { name: t("searchui.jumpToMonth") }).selectOption(target);
        const id = `timeline-month-${target}`;
        await until(`focus moves to ${target}`, async () => [await page.evaluate((id) => document.activeElement?.id === id, id), null]);
      }
      // Escape leaves the timeline for the Search page, with focus on it (not lost to <body>).
      await page.keyboard.press("Escape");
      await h.waitFor({ state: "detached" });
      const focused = await page.evaluate(() => {
        const a = document.activeElement;
        return a && a !== document.body && !!a.closest("main") ? `${a.tagName.toLowerCase()}#${a.id}` : null;
      });
      expect(focused !== null, "focus was lost after Escape closed the timeline");
    });

    await check("search: every search command reached the daemon and was accepted", async () => {
      const bad = bridge.calls.filter((c) => c.cmd.startsWith("search_") && (c.ok === false));
      expect(bad.length === 0, `failed search commands: ${JSON.stringify(bad)}`);
      const used = new Set(bridge.calls.map((c) => c.cmd));
      for (const cmd of ["search_faceted", "search_timeline", "search_preview"]) expect(used.has(cmd), `${cmd} was never called`);
      expect(errors.length === 0, `uncaught page errors: ${JSON.stringify(errors)}`);
    });
  } finally {
    await ctx.close().catch(() => {});
    await bridge.close().catch(() => {});
  }
}
