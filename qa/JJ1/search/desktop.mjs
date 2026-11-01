// JJ1 search QA, desktop level: the real Search page (apps/desktop/dist) in headless Chromium
// through the e2e bridge + Tauri shim, against a real nuggetd that run_all.py started on the
// synthetic corpus (gen_corpus.py, files + WhatsApp sources, assume_unflagged_safe).
//
//   node qa/JJ1/search/desktop.mjs <phase> <socket> <port> <storage.json> <out.json>
//
// phase "a": filters, counts, keyboard, screen-reader names, preview, timeline, saved/recent.
// phase "b": after a daemon restart, a new browser context with phase a's storage: saved and
//            recent searches survive; Clear recent survives a reload; hostile stored state.
import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const DESKTOP = path.resolve(here, "../../../apps/desktop");
const { startBridge } = await import(path.join(DESKTOP, "e2e", "bridge.mjs"));
const require = createRequire(path.join(DESKTOP, "package.json"));
const { chromium } = require("playwright");
const AXE = path.join(path.dirname(require.resolve("axe-core/package.json")), "axe.min.js");
const en = JSON.parse(fs.readFileSync(path.join(DESKTOP, "src/i18n/locales/en.json"), "utf8"));
const t = (k) => en[k];

const [phase, socket, port, storagePath, outPath] = process.argv.slice(2);
const ONBOARDED = { version: 1, step: "done", completed: [], skipped: [], ai: null, finished: true };
const results = [];
const STEP = 10_000;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function check(name, fn) {
  const t0 = Date.now();
  try {
    const detail = await fn();
    results.push({ check: name, ok: true, ms: Date.now() - t0, detail: detail ?? null });
    console.log(`  ok   ${name}`);
  } catch (e) {
    results.push({ check: name, ok: false, ms: Date.now() - t0, detail: String(e?.message ?? e).slice(0, 600) });
    console.log(`  FAIL ${name}: ${String(e?.message ?? e).slice(0, 300)}`);
  }
}
function expect(cond, msg) {
  if (!cond) throw new Error(msg);
}
async function until(what, probe, ms = STEP) {
  const end = Date.now() + ms;
  let last;
  while (Date.now() < end) {
    const [ok, v] = await probe();
    last = v;
    if (ok) return v;
    await sleep(150);
  }
  throw new Error(`timed out: ${what}; last: ${JSON.stringify(last)?.slice(0, 400)}`);
}
const countOf = (name) => Number(name.match(/,\s*(\d[\d,]*)\s+results?$/)?.[1]?.replace(/,/g, "") ?? NaN);
const fmt = (k, vars) => t(k).replace(/\{(\w+)\}/g, (_, v) => vars[v]);

const bridge = await startBridge({ socket, port: Number(port) });
const browser = await chromium.launch(process.env.E2E_CHROMIUM ? { executablePath: process.env.E2E_CHROMIUM } : {});
const ctxOpts = { viewport: { width: 1360, height: 900 }, locale: "en-US", timezoneId: "UTC" };
if (phase === "b" && fs.existsSync(storagePath)) ctxOpts.storageState = storagePath;
const ctx = await browser.newContext(ctxOpts);
ctx.setDefaultTimeout(STEP);
await ctx.addInitScript({ path: path.join(DESKTOP, "e2e", "tauri-shim.js") });
const page = await ctx.newPage();
const errors = [];
page.on("pageerror", (e) => errors.push(e.message));
const main = page.locator("main");
const results$ = main.locator("ol.results > li");
const box = page.getByRole("searchbox", { name: t("search.query") });
const active = () => page.evaluate(() => {
  const a = document.activeElement;
  return a ? { tag: a.tagName, id: a.id, label: a.getAttribute("aria-label"), text: (a.textContent || "").trim().slice(0, 60), cls: a.className } : null;
});
const announced = () => page.locator("[role=status][aria-live=polite][aria-atomic=true]").allTextContents();

async function openSearch() {
  await page.goto(`${bridge.url}index.html`);
  await page.evaluate((o) => {
    localStorage.setItem("nugget.onboarding.v1", JSON.stringify(o));
    localStorage.setItem("nugget.page", "search");
  }, ONBOARDED);
  await page.reload();
  await box.waitFor();
}
async function search(q) {
  await box.fill(q);
  await box.press("Enter");
  await until(`results for ${q}`, async () => [(await results$.count()) > 0 && !(await main.locator("ol.results[aria-busy=true]").count()), await results$.count()]);
}
async function facetNames(group) {
  return main.locator("#search-filters").getByRole("group", { name: t(`searchui.facet.${group}`) }).getByRole("checkbox")
    .evaluateAll((els) => els.map((e) => e.getAttribute("aria-label")));
}
/** Tab from the current focus until `pred(active)` holds; returns the number of Tabs. */
async function tabTo(pred, max = 80) {
  for (let i = 1; i <= max; i++) {
    await page.keyboard.press("Tab");
    const a = await active();
    if (a && pred(a)) return i;
  }
  throw new Error(`not reached in ${max} Tabs; at ${JSON.stringify(await active())}`);
}

try {
  if (phase === "a") {
    await openSearch();
    await check("A1 query shows results and announces the count", async () => {
      await search("quokka");
      const n = await results$.count();
      const said = await until("announcement", async () => {
        const a = (await announced()).join(" | ");
        return [/19 results for “quokka”/.test(a), a];
      });
      return { shown: n, said };
    });
    await check("A2 facet checkbox names carry the daemon's counts (Mail 10, Event 3, File 2+, Chat 2)", async () => {
      const kinds = await facetNames("kind");
      const m = Object.fromEntries(kinds.map((k) => [k.replace(/,.*$/, ""), countOf(k)]));
      expect(m.Mail === 10 && m.Event === 3 && m.Chat === 2 && m.File === 4, JSON.stringify(kinds));
      const people = await facetNames("person");
      expect(people.some((p) => /^Alice Varga, 10 results$/.test(p)), JSON.stringify(people));
      return { kinds, people };
    });
    await check("A3 keyboard only: Tab from the search box to the Mail filter, Space ticks it, results narrow to 10, focus stays", async () => {
      await box.focus();
      const tabs = await tabTo((a) => a.tag === "INPUT" && /^Mail, \d+ results?$/.test(a.label ?? ""));
      await page.keyboard.press("Space");
      await until("10 results", async () => [(await results$.count()) === 10, await results$.count()]);
      await sleep(300);
      const a = await active();
      expect(a?.tag === "INPUT" && /^Mail,/.test(a.label ?? ""), `focus after Space: ${JSON.stringify(a)}`);
      const ann = (await announced()).join(" | ");
      expect(/10 results for “quokka” with 1 filter/.test(ann), `announcement: ${ann}`);
      const kinds = await facetNames("kind");
      expect(kinds.includes("Event, 3 results"), `other kinds keep their counts: ${kinds}`);
      return { tabs, kinds, ann };
    });
    await check("A4 sensitive items carry a Sensitive badge (IBAN mail only, among mail)", async () => {
      const badges = await results$.evaluateAll((lis) => lis.filter((li) => [...li.querySelectorAll(".badge-warn")].some((b) => b.textContent.trim() === "Sensitive")).map((li) => li.querySelector(".result-select")?.textContent));
      expect(badges.length === 1 && /m04/.test(badges[0]), JSON.stringify(badges));
      return badges;
    });
    await check("A5 keyboard: Enter on a result opens the preview with focus on its title; Escape closes and returns focus", async () => {
      const btn = results$.filter({ hasText: "m04" }).locator(".result-select");
      await btn.focus();
      await page.keyboard.press("Enter");
      const panel = main.locator("#search-preview");
      await panel.waitFor();
      await until("preview loaded", async () => [(await panel.locator(".sr-preview-text").count()) > 0, null]);
      const a = await active();
      expect(a?.id === "search-preview-title", `focus on open: ${JSON.stringify(a)}`);
      const sens = await panel.locator(".badge-warn", { hasText: "Sensitive" }).count();
      expect(sens === 1, "preview of the IBAN mail has no Sensitive badge");
      const region = await panel.getAttribute("role");
      await page.keyboard.press("Escape");
      await panel.waitFor({ state: "detached" });
      const back = await btn.evaluate((el) => el === document.activeElement);
      expect(back, `focus after Escape: ${JSON.stringify(await active())}`);
      return { region };
    });
    await check("A6 long item preview is cut at 20,000 chars and says so", async () => {
      await page.keyboard.press(`Control+a`).catch(() => {});
      // Clear the Mail filter first.
      await main.getByRole("button", { name: t("searchui.clearFilters") }).click();
      await until("19 results", async () => [(await results$.count()) === 19, await results$.count()]);
      const btn = results$.filter({ hasText: /^long|long$/ }).locator(".result-select").first();
      await btn.click();
      const panel = main.locator("#search-preview");
      const text = await until("long preview", async () => {
        const n = await panel.locator(".sr-preview-text").count();
        return [n > 0, n];
      });
      const len = await panel.locator(".sr-preview-text").evaluate((el) => el.textContent.length);
      const note = await panel.getByText(t("searchui.textTruncated")).count();
      expect(len <= 20000 && note === 1, `len=${len} note=${note}`);
      await page.keyboard.press("Escape");
      return { len };
    });
    await check("A7 Show more pages without duplicates (Show 10, then more)", async () => {
      await main.locator(".search-bar select").selectOption("10");
      await box.press("Enter").catch(() => {});
      await search("quokka");
      await until("10 shown", async () => [(await results$.count()) === 10, await results$.count()]);
      await main.locator("button.sr-more").click();
      await until("19 shown", async () => [(await results$.count()) === 19, await results$.count()]);
      const titles = await results$.locator(".result-select").allTextContents();
      expect(new Set(titles).size === titles.length, `duplicates: ${titles}`);
      await main.locator(".search-bar select").selectOption("25");
      return titles.length;
    });
    await check("A8 Timeline from the People filter (keyboard): heading focused, Alice's items listed, Back returns to results", async () => {
      await search("quokka");
      const tl = main.locator("#search-filters").getByRole("button", { name: fmt("searchui.timelineFor", { name: "Alice Varga" }) });
      await tl.focus();
      await page.keyboard.press("Enter");
      const h = page.locator("h2#timeline-heading");
      await until("timeline heading focused", async () => {
        const a = await active();
        return [a?.id === "timeline-heading" && /Alice Varga/.test(a.text), a];
      });
      const items = await main.locator("li.sr-tl-item").count();
      const status = await main.locator(".sr-timeline [role=status]").first().textContent();
      const sens = await main.locator("li.sr-tl-item .badge-warn").count();
      await main.getByRole("button", { name: t("searchui.backToResults") }).focus();
      await page.keyboard.press("Enter");
      await until("back on results", async () => [(await results$.count()) > 0, null]);
      const a = await active();
      expect(items === 10, `timeline items ${items} (expected 10: 6 mail, 2 events, 2 chats)`);
      return { items, status, sens, focusAfterBack: a };
    });
    await check("A9 (expectation, not promised by the guide) Escape in the timeline goes back to results, as in the preview", async () => {
      await main.locator("#search-filters").getByRole("button", { name: fmt("searchui.timelineFor", { name: "Alice Varga" }) }).click();
      await page.locator("h2#timeline-heading").waitFor();
      await until("heading focused", async () => [(await active())?.id === "timeline-heading", null]);
      await page.keyboard.press("Escape");
      await sleep(500);
      const still = await page.locator("h2#timeline-heading").count();
      if (still) await main.getByRole("button", { name: t("searchui.backToResults") }).click();
      expect(!still, "Escape does nothing in the timeline");
    });
    await check("A10 axe: no serious/critical violations on results+filters, preview, timeline", async () => {
      await search("quokka");
      const run = async (label) => {
        if (!(await page.evaluate(() => typeof window.axe === "object"))) await page.addScriptTag({ path: AXE });
        const v = await page.evaluate(async () => (await window.axe.run(document.querySelector("main"), { resultTypes: ["violations"] })).violations
          .filter((x) => x.impact === "serious" || x.impact === "critical").map((x) => ({ id: x.id, n: x.nodes.length, t: x.nodes[0]?.target })));
        return v.map((x) => ({ ...x, view: label }));
      };
      let all = await run("results");
      await results$.first().locator(".result-select").click();
      await main.locator("#search-preview .sr-preview-text").waitFor();
      all = all.concat(await run("preview"));
      await page.keyboard.press("Escape");
      await main.locator("#search-filters").getByRole("button", { name: fmt("searchui.timelineFor", { name: "Alice Varga" }) }).click();
      await main.locator("li.sr-tl-item").first().waitFor();
      all = all.concat(await run("timeline"));
      await main.getByRole("button", { name: t("searchui.backToResults") }).click();
      expect(all.length === 0, JSON.stringify(all));
    });
    await check("A11 month bar: picking a month sets From/To to that month and narrows", async () => {
      await search("quokka");
      const bar = main.locator("ul.sr-months button").filter({ has: page.locator("xpath=.") }).first();
      const label = await bar.getAttribute("aria-label");
      await bar.click();
      const n = countOf(label.replace(/: /, ", ")) || Number(label.match(/(\d+) results?$/)?.[1]);
      await until(`${n} results`, async () => [(await results$.count()) === n, await results$.count()]);
      const from = await main.locator('#search-filters input[type=date]').first().inputValue();
      await main.getByRole("button", { name: t("searchui.clearFilters") }).click();
      return { label, from };
    });
    await check("A12 save a search (query + Mail filter); stored as a URL-like state", async () => {
      await search("quokka");
      await main.locator("#search-filters").getByRole("checkbox", { name: /^Mail,/ }).check();
      await until("10", async () => [(await results$.count()) === 10, null]);
      await main.getByRole("button", { name: t("searchui.saveSearch") }).click();
      const dialog = page.getByRole("dialog", { name: t("searchui.saveTitle") });
      await dialog.getByRole("textbox", { name: t("searchui.saveName") }).fill("Quokka mail");
      await dialog.getByRole("button", { name: t("searchui.save"), exact: true }).click();
      await dialog.waitFor({ state: "detached" });
      const saved = await page.evaluate(() => JSON.parse(localStorage.getItem("nugget.search.saved.v1")));
      expect(saved?.[0]?.state === "q=quokka&kind=mail", JSON.stringify(saved));
      return saved;
    });
    await check("A13 a query with & = % + and unicode round-trips through a saved search", async () => {
      await main.getByRole("button", { name: t("searchui.clearFilters") }).click().catch(() => {});
      const q = "R&D=50% + kängurú 袋鼠 #1";
      await box.fill(q);
      await box.press("Enter");
      await sleep(800);
      await main.getByRole("button", { name: t("searchui.saveSearch") }).click();
      const dialog = page.getByRole("dialog", { name: t("searchui.saveTitle") });
      await dialog.getByRole("textbox", { name: t("searchui.saveName") }).fill("Odd query");
      await dialog.getByRole("button", { name: t("searchui.save"), exact: true }).click();
      await dialog.waitFor({ state: "detached" });
      await box.fill("");
      await main.getByRole("button", { name: fmt("searchui.runSaved", { name: "Odd query" }) }).first().click();
      await until("query restored", async () => [(await box.inputValue()) === q, await box.inputValue()]);
    });
    await check("A14 recent searches: listed under an empty box, newest first, with filter count", async () => {
      await box.fill("wallaby");
      await box.press("Enter");
      await sleep(600);
      await box.fill("");
      const list = main.locator("section.sr-recent");
      await list.waitFor();
      const names = await list.getByRole("button").evaluateAll((bs) => bs.map((b) => b.getAttribute("aria-label") || b.textContent));
      const raw = await page.evaluate(() => JSON.parse(localStorage.getItem("nugget.search.recent.v1")));
      expect(raw[0] === "q=wallaby", JSON.stringify(raw));
      return { names, raw };
    });
    await check("A15 (observation) a query that is an IBAN is kept in recent searches", async () => {
      await box.fill("DE89 3704 0044 0532 0130 00");
      await box.press("Enter");
      await sleep(800);
      const raw = await page.evaluate(() => JSON.parse(localStorage.getItem("nugget.search.recent.v1")));
      return { stored: raw.some((r) => r.includes("DE89")), raw };
    });
    await check("A16 hostile saved state in storage (junk kinds, ids, dates, XSS-ish name) is normalized; page works", async () => {
      await page.evaluate(() => {
        const s = JSON.parse(localStorage.getItem("nugget.search.saved.v1"));
        s.push({ id: "evil", name: "Evil <img src=x onerror=alert(1)>", state: "q=quokka&kind=bogus&kind=chat&from=2026-13-01&person=%00bad&source=" + "x".repeat(300) + "&timeline=" });
        s.push({ id: 7, name: "", state: 5 });
        localStorage.setItem("nugget.search.saved.v1", JSON.stringify(s));
      });
      await page.reload();
      await box.waitFor();
      await main.getByRole("button", { name: fmt("searchui.runSaved", { name: "Evil <img src=x onerror=alert(1)>" }) }).first().click();
      await until("chat results", async () => [(await results$.count()) === 2, await results$.count()]);
      const bad = bridge.calls.filter((c) => c.cmd === "search_faceted" && c.ok === false);
      expect(bad.length === 0 && errors.length === 0, JSON.stringify({ bad, errors }));
    });
    await check("A17 a stored state with an impossible date (to=2026-02-30) is dropped or clamped, not sent to the daemon", async () => {
      await page.evaluate(() => {
        const s = JSON.parse(localStorage.getItem("nugget.search.saved.v1"));
        s.push({ id: "feb30", name: "Feb 30", state: "q=quokka&to=2026-02-30" });
        localStorage.setItem("nugget.search.saved.v1", JSON.stringify(s));
      });
      await page.reload();
      await box.waitFor();
      const before = bridge.calls.length;
      await main.getByRole("button", { name: fmt("searchui.runSaved", { name: "Feb 30" }) }).first().click();
      await sleep(1500);
      const calls = bridge.calls.slice(before).filter((c) => c.cmd === "search_faceted");
      const shown = await results$.count();
      const err = (await main.locator(".note-error, [role=alert]").allTextContents()).join(" | ");
      expect(calls.every((c) => c.ok !== false) && shown > 0, JSON.stringify({ calls: calls.map((c) => ({ ok: c.ok, code: c.code, message: c.message, args: c.args })), shown, err }));
    });
    await check("A18 DST: a New York user (EDT today) filtering 31 Jan sees the mail sent 23:30 EST on 31 Jan, which the list itself dates 31 Jan", async () => {
      const ny = await browser.newContext({ ...ctxOpts, timezoneId: "America/New_York", storageState: undefined });
      await ny.addInitScript({ path: path.join(DESKTOP, "e2e", "tauri-shim.js") });
      const p2 = await ny.newPage();
      try {
        await p2.goto(`${bridge.url}index.html`);
        await p2.evaluate((o) => { localStorage.setItem("nugget.onboarding.v1", JSON.stringify(o)); localStorage.setItem("nugget.page", "search"); }, ONBOARDED);
        await p2.reload();
        const b2 = p2.getByRole("searchbox", { name: t("search.query") });
        await b2.fill("quokka");
        await b2.press("Enter");
        const li = p2.locator("main ol.results > li");
        await until("results", async () => [(await li.count()) === 19, await li.count()]);
        const shownDate = await li.filter({ hasText: "m02" }).locator("time").first().textContent();
        const offsetSent = await p2.evaluate(() => -new Date().getTimezoneOffset());
        const dates = p2.locator("#search-filters input[type=date]");
        await dates.nth(0).fill("2026-01-31");
        await dates.nth(0).press("Enter");
        await sleep(600);
        await dates.nth(1).fill("2026-01-31");
        await dates.nth(1).press("Enter");
        await sleep(1500);
        const titles = await li.locator(".result-select").allTextContents();
        expect(titles.some((x) => x.includes("m02")), JSON.stringify({ shownDate, offsetSent, titles }));
        return { shownDate, offsetSent, titles };
      } finally {
        await ny.close();
      }
    });
    // Leave a known recent list for phase b.
    await box.fill("quokka");
    await box.press("Enter");
    await sleep(800);
    await ctx.storageState({ path: storagePath });
  } else {
    await page.goto(`${bridge.url}index.html`);
    await box.waitFor();
    await check("B1 after a daemon restart and a new browser context, saved searches are listed (page + sidebar)", async () => {
      await page.locator(".nav-saved").getByRole("button", { name: fmt("searchui.runSaved", { name: "Quokka mail" }) }).waitFor();
      await main.getByRole("button", { name: fmt("searchui.runSaved", { name: "Quokka mail" }) }).first().waitFor();
    });
    await check("B2 running the saved search restores query and Mail filter (10 results)", async () => {
      await main.getByRole("button", { name: fmt("searchui.runSaved", { name: "Quokka mail" }) }).first().click();
      await until("restored", async () => {
        const q = await box.inputValue();
        const ticked = await main.locator("#search-filters input[type=checkbox]:checked").evaluateAll((e) => e.map((x) => x.getAttribute("aria-label")));
        return [q === "quokka" && ticked.length === 1 && /^Mail/.test(ticked[0]) && (await results$.count()) === 10, { q, ticked }];
      });
    });
    await check("B3 recent searches survive (newest first)", async () => {
      await box.fill("");
      const list = main.locator("section.sr-recent");
      await list.waitFor();
      const raw = await page.evaluate(() => JSON.parse(localStorage.getItem("nugget.search.recent.v1")));
      expect(raw[0].startsWith("q=quokka"), JSON.stringify(raw));
      return raw;
    });
    await check("B4 Clear recent empties the list, announces it, and stays cleared after a reload", async () => {
      await main.getByRole("button", { name: t("searchui.clearRecent") }).click();
      await main.locator("section.sr-recent").waitFor({ state: "detached" });
      const ann = (await announced()).join(" | ");
      const focus = await active();
      await page.reload();
      await box.waitFor();
      await sleep(500);
      const n = await main.locator("section.sr-recent").count();
      const raw = await page.evaluate(() => localStorage.getItem("nugget.search.recent.v1"));
      expect(n === 0 && raw === null && /Recent searches cleared/.test(ann), JSON.stringify({ n, raw, ann }));
      return { ann, focus };
    });
    await check("B5 saved search deleted from the Search page stays deleted after reload", async () => {
      await main.getByRole("button", { name: fmt("searchui.deleteSaved", { name: "Odd query" }) }).click();
      await page.reload();
      await box.waitFor();
      const n = await main.getByRole("button", { name: fmt("searchui.runSaved", { name: "Odd query" }) }).count();
      expect(n === 0, `still listed ${n}`);
    });
    await check("B6 no failed search commands, no page errors", async () => {
      const bad = bridge.calls.filter((c) => c.cmd.startsWith("search_") && c.ok === false);
      expect(bad.length === 0 && errors.length === 0, JSON.stringify({ bad, errors }));
    });
  }
} finally {
  await ctx.close().catch(() => {});
  await browser.close().catch(() => {});
  await bridge.close().catch(() => {});
  fs.writeFileSync(outPath, JSON.stringify({ phase, results, errors }, null, 1));
}
process.exit(0);
