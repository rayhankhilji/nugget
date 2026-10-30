// First-run scenario: an empty NUGGET_HOME, the real onboarding UI, the fixture corpus connected
// through the folder picker, and the clock running from app start until Search shows a cited
// answer for a question the corpus answers. Fails above the budget (E2E_FIRST_RUN_BUDGET_MS).
//
// No AI is set up ("Decide later"): onboarding can't configure a provider by itself (it shows a
// config snippet), so the cited answer is the Search page's "Find context" result: numbered
// passages with source titles, which is what an answer or a draft is grounded on.
import fs from "node:fs";
import path from "node:path";
import { startBridge, METHOD_NOT_FOUND } from "./bridge.mjs";
import { CORPUS, FIRST_RUN_CONFIG, startDaemon } from "./daemon.mjs";
import {
  check,
  DIST,
  expect,
  expectNoErrorState,
  INDEX_TIMEOUT_MS,
  KEEP,
  log,
  newContext,
  OUT,
  secs,
  settle,
  shot,
  t,
  tPattern,
  until,
} from "./harness.mjs";

export const FIRST_RUN_BUDGET_MS = Number(process.env.E2E_FIRST_RUN_BUDGET_MS || 120_000);

/** A question the corpus answers: notes/projects/Tidewater Billing.md names the reviewing firm. */
const QUERY = "Tidewater Billing vendor contract";
const EXPECT_TITLE = "Tidewater Billing";
const EXPECT_ANSWER = "Vellum";

const escapeRe = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const exactName = (key) => new RegExp(`^${escapeRe(t(key))}$`);
const startsWith = (key) => new RegExp(`^${escapeRe(t(key))}`);

/**
 * Run the scenario with `browser` (Playwright) and the nuggetd binary `bin`. Records its checks
 * through harness.mjs and returns the timing summary for summary.json.
 */
export async function firstRunScenario({ browser, bin }) {
  const summary = { budget_ms: FIRST_RUN_BUDGET_MS, query: QUERY, total_ms: null, marks: {}, steps: {} };
  const t0 = Date.now();
  let prev = 0;
  const mark = (name) => {
    const at = Date.now() - t0;
    summary.marks[name] = at;
    summary.steps[name] = at - prev;
    prev = at;
    log(`  first run: ${name} at ${secs(at)} (+${secs(summary.steps[name])})`);
  };

  // App start: the shell starts the daemon on an empty home, then the window opens.
  const daemon = await startDaemon(bin, { stderrPath: path.join(OUT, "first-run-nuggetd.out.log"), config: process.env.E2E_FIRST_RUN_NO_CONFIG === "1" ? null : FIRST_RUN_CONFIG });
  mark("daemon_ready");
  log(`first run: daemon home ${daemon.home}`);
  let bridge;
  let ctx;
  const pageErrors = [];
  const consoleErrors = [];
  try {
    bridge = await startBridge({ socket: daemon.socket, dist: DIST, pickFolders: [CORPUS] });
    ctx = await newContext(browser);
    const page = await ctx.newPage();
    page.on("pageerror", (e) => pageErrors.push(e.message));
    page.on("console", (m) => m.type() === "error" && consoleErrors.push(m.text()));
    // Screenshot once the step's entrance animation is over (it fades in).
    const snap = async (name) => {
      await page
        .evaluate(() => {
          const finite = document.getAnimations().filter((a) => a.effect?.getComputedTiming().iterations !== Infinity);
          const done = Promise.all(finite.map((a) => a.finished.catch(() => {})));
          return Promise.race([done, new Promise((r) => setTimeout(r, 2000))]);
        })
        .catch(() => {});
      await shot(page, name);
    };
    const heading = (key) => page.getByRole("heading", { level: 1, name: t(key), exact: true });
    const cont = () => page.locator("footer.ob-actions").getByRole("button", { name: exactName("onboarding.continue") }).click();

    await check("first run: onboarding to a cited answer on an empty home", async () => {
      try {
        // 1. Welcome: shown because nothing is saved, not preset.
        await page.goto(`${bridge.url}index.html`);
        await heading("onboarding.welcome.heading").waitFor();
        mark("app_open");
        await snap("first-run-welcome");
        await cont();

        // 2. AI: decide later (search works without a model).
        await heading("onboarding.ai.heading").waitFor();
        await page.getByRole("radio", { name: startsWith("onboarding.ai.later") }).check();
        await snap("first-run-ai");
        await cont();
        mark("ai_step");

        // 3. First source: the fixture folder through the (stubbed) native picker.
        await heading("onboarding.source.heading").waitFor();
        await page.getByRole("button", { name: exactName("onboarding.source.addFolder") }).click();
        const dialog = page.locator("dialog[open]");
        await dialog.getByRole("button", { name: exactName("addSource.folder.choose") }).click();
        await until("the picked folder is listed in the wizard", async () => {
          const paths = await dialog.locator(".path-list code").allTextContents();
          return [paths.includes(CORPUS), paths];
        });
        await snap("first-run-folder");
        await dialog.getByRole("button", { name: exactName("addSource.folder.submit") }).click();
        await dialog.waitFor({ state: "detached" });
        mark("source_added");

        // Progress while it indexes, then "search works now". Record the phases the user saw.
        const phases = [];
        const rows = page.locator(".ob-index li");
        const readyRe = tPattern("onboarding.source.ready");
        const ready = await until(
          "the source step says the folder is indexed and search works",
          async () => {
            const row = await rows.first().evaluate((li) => ({ phase: li.dataset.phase, text: li.textContent.trim() }));
            const last = phases[phases.length - 1];
            if (!last || last.phase !== row.phase || last.text !== row.text) phases.push({ ...row, at_ms: Date.now() - t0 });
            if (row.phase === "failed") throw new Error(`indexing failed: ${row.text}`);
            return [row.phase === "ready" && readyRe.test(row.text), row];
          },
          INDEX_TIMEOUT_MS,
        );
        summary.index_phases = phases;
        summary.items_indexed = Number(ready.text.match(/(\d[\d,]*)\s+item/)?.[1]?.replace(/,/g, "") ?? NaN);
        mark("source_searchable");
        log(`  first run: "${ready.text}" (${phases.length} progress states shown)`);
        await snap("first-run-indexed");
        await cont();

        // 4-5. Permissions and Double Tap: read and continue with the defaults.
        await heading("onboarding.perm.heading").waitFor();
        await cont();
        await heading("onboarding.trigger.heading").waitFor();
        await cont();

        // 6. Done: points at Search once something is searchable.
        await heading("onboarding.done.heading").waitFor();
        const toSearch = page.locator("footer.ob-actions").getByRole("button", { name: exactName("onboarding.done.search") });
        await toSearch.waitFor();
        await snap("first-run-done");
        await toSearch.click();
        await page.locator(`main[aria-label="${t("app.nav.search")}"]`).waitFor();
        mark("setup_done");

        // Search: the first hit list for a question the corpus answers.
        const box = page.getByRole("searchbox", { name: t("search.query") });
        await box.fill(QUERY);
        await box.press("Enter");
        const hits = page.locator("ol.results li.result");
        const top = await until(`search "${QUERY}" returns "${EXPECT_TITLE}" in the top three`, async () => {
          const all = await hits.evaluateAll((lis) =>
            lis.map((li) => ({ title: li.querySelector(".result-title")?.textContent?.trim() ?? "", text: li.textContent ?? "" })),
          );
          const ok = all.slice(0, 3).some((h) => h.title.includes(EXPECT_TITLE) && h.text.includes(EXPECT_ANSWER));
          return [ok, all.slice(0, 5).map((h) => h.title)];
        });
        summary.top_titles = top;
        mark("first_results");
        await snap("first-run-search");

        // Cited answer: the passages an answer or a draft is built from, numbered, with titles.
        await page.getByRole("button", { name: exactName("search.ask") }).click();
        const panel = page.locator("#ask-panel");
        const question = panel.getByRole("textbox", { name: t("search.question") });
        if (!(await question.inputValue()).trim()) await question.fill(QUERY);
        await panel.getByRole("button", { name: exactName("search.findContext") }).click();
        const cited = await until(`"Find context" cites "${EXPECT_TITLE}" and quotes the answer`, async () => {
          const citations = await panel.locator("ol.citations li.citation").evaluateAll((lis) =>
            lis.map((li) => ({ n: li.querySelector(".cite-n")?.textContent?.trim() ?? "", title: li.querySelector(".citation-title")?.textContent?.trim() ?? "" })),
          );
          const context = (await panel.locator(".context-text").textContent({ timeout: 1000 })) ?? "";
          const marks = await panel.locator(".context-text button.cite").count();
          const ok = citations.some((c) => c.n === "[1]") && citations.some((c) => c.title.includes(EXPECT_TITLE)) && marks > 0 && context.includes(EXPECT_ANSWER);
          return [ok, { citations: citations.slice(0, 5), marks }];
        });
        summary.citations = cited.citations;
        mark("cited_answer");
        summary.total_ms = summary.marks.cited_answer;
        summary.from_app_open_ms = summary.marks.cited_answer - summary.marks.app_open;
        await settle(page);
        await snap("first-run-cited");
        await expectNoErrorState(page, "the Search page after the first run");
        expect(pageErrors.length === 0, `${pageErrors.length} uncaught page error(s): ${JSON.stringify(pageErrors)}`);
      } catch (e) {
        await shot(page, "FAIL-first-run");
        throw e;
      }
    });

    await check(`first run: a cited answer within ${secs(FIRST_RUN_BUDGET_MS)} of app start`, async () => {
      expect(summary.total_ms !== null, "the first run never reached a cited answer");
      expect(
        summary.total_ms <= FIRST_RUN_BUDGET_MS,
        `took ${secs(summary.total_ms)} (budget ${secs(FIRST_RUN_BUDGET_MS)}): ${JSON.stringify(summary.steps)}`,
      );
    });

    await check("first run: every daemon command the UI called exists and accepted its params", async () => {
      const bad = bridge.calls.filter((c) => c.via === "daemon" && (c.code === METHOD_NOT_FOUND || c.code === -32602));
      expect(bad.length === 0, `wire mismatches: ${JSON.stringify(bad)}`);
    });

    summary.rpcErrors = bridge.calls.filter((c) => !c.ok);
    summary.pageErrors = pageErrors;
    summary.consoleErrors = consoleErrors;
    summary.commands = [...new Set(bridge.calls.map((c) => c.cmd))].sort();
  } finally {
    await ctx?.close().catch(() => {});
    await bridge?.close().catch(() => {});
    fs.cpSync(daemon.logDir, path.join(OUT, "first-run-daemon-logs"), { recursive: true });
    await daemon.stop({ keep: KEEP });
    if (KEEP) log(`kept the first-run daemon home: ${daemon.home}`);
  }

  if (summary.total_ms !== null) {
    const parts = Object.entries(summary.steps).map(([k, v]) => `${k} +${secs(v)}`).join(", ");
    log(`first run: cited answer ${secs(summary.total_ms)} after app start (budget ${secs(FIRST_RUN_BUDGET_MS)}): ${parts}`);
  }
  return summary;
}
