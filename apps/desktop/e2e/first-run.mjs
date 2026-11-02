// First run: an empty home, the real onboarding (the fork: run it yourself, "Decide later" for AI,
// then the corpus folder through the (stubbed) picker), then Search's "Find context" cites the note that
// answers the query.
import path from "node:path";
import { startBridge } from "./bridge.mjs";
import { CORPUS, FIRST_RUN_CONFIG, startDaemon } from "./daemon.mjs";
import { check, checkWire, DIST, exactName, expect, expectNoErrorState, log, newContext, onboardingAddFolder, onboardingRunItYourself, OUT, settle, shot, startsWith, stopDaemon, t, until } from "./harness.mjs";

/** notes/projects/Tidewater Billing.md names the reviewing firm. */
const QUERY = "Tidewater Billing vendor contract";
const EXPECT_TITLE = "Tidewater Billing";
const EXPECT_ANSWER = "Vellum";

export async function firstRunScenario({ browser, bin }) {
  const summary = { query: QUERY };
  const daemon = await startDaemon(bin, { stderrPath: path.join(OUT, "first-run-nuggetd.out.log"), config: FIRST_RUN_CONFIG });
  log(`first run: daemon home ${daemon.home}`);
  let bridge;
  let ctx;
  const pageErrors = [];
  try {
    bridge = await startBridge({ socket: daemon.socket, dist: DIST, pickFolders: [CORPUS] });
    ctx = await newContext(browser);
    const page = await ctx.newPage();
    page.on("pageerror", (e) => pageErrors.push(e.message));
    const heading = (key) => page.getByRole("heading", { level: 1, name: t(key), exact: true });
    const cont = () => page.locator("footer.ob-actions").getByRole("button", { name: exactName("onboarding.continue") }).click();

    await check("first run: onboarding to a cited answer on an empty home", async () => {
      try {
        await page.goto(`${bridge.url}index.html`);
        // The fork first (no cloud in the e2e build): run it yourself → AI → a local source.
        const fork = await onboardingRunItYourself(page);
        expect(fork.includes(t("onboarding.fork.local.body")), `the fork lacks the local-path copy: ${fork.slice(0, 300)}`);
        expect(fork.includes(t("onboarding.fork.cloud.trial")), `the fork shows a price without plans: ${fork.slice(0, 300)}`);
        await page.getByRole("radio", { name: startsWith("onboarding.ai.later") }).check();
        await cont();

        await heading("onboarding.source.heading").waitFor();
        const ready = await onboardingAddFolder(page, CORPUS);
        log(`  first run: "${ready.text}"`);
        await cont();

        await heading("onboarding.perm.heading").waitFor();
        await cont();
        await heading("onboarding.trigger.heading").waitFor();
        await cont();

        // The last step asks the first question; what setup did is its section below.
        await page.getByRole("heading", { level: 2, name: t("onboarding.done.heading"), exact: true }).waitFor();
        await page.locator("footer.ob-actions").getByRole("button", { name: exactName("onboarding.done.search") }).click();
        await page.locator(`main[aria-label="${t("app.nav.search")}"]`).waitFor();

        const box = page.getByRole("searchbox", { name: t("search.query") });
        await box.fill(QUERY);
        await box.press("Enter");
        summary.top_titles = await until(`search "${QUERY}" returns "${EXPECT_TITLE}" in the top three`, async () => {
          const all = await page.locator("ol.results li.result").evaluateAll((lis) =>
            lis.map((li) => ({ title: li.querySelector(".result-title")?.textContent?.trim() ?? "", text: li.textContent ?? "" })),
          );
          return [all.slice(0, 3).some((h) => h.title.includes(EXPECT_TITLE) && h.text.includes(EXPECT_ANSWER)), all.slice(0, 5).map((h) => h.title)];
        });

        await page.getByRole("button", { name: exactName("search.ask") }).click();
        const panel = page.locator("#ask-panel");
        const question = panel.getByRole("textbox", { name: t("search.question") });
        if (!(await question.inputValue()).trim()) await question.fill(QUERY);
        await panel.getByRole("button", { name: exactName("search.findContext") }).click();
        await until(`"Find context" cites "${EXPECT_TITLE}" and quotes the answer`, async () => {
          const citations = await panel.locator("ol.citations li.citation").evaluateAll((lis) =>
            lis.map((li) => ({ n: li.querySelector(".cite-n")?.textContent?.trim() ?? "", title: li.querySelector(".citation-title")?.textContent?.trim() ?? "" })),
          );
          const context = (await panel.locator(".context-text").textContent({ timeout: 1000 })) ?? "";
          const marks = await panel.locator(".context-text button.cite").count();
          const ok = citations.some((c) => c.n === "[1]") && citations.some((c) => c.title.includes(EXPECT_TITLE)) && marks > 0 && context.includes(EXPECT_ANSWER);
          return [ok, { citations: citations.slice(0, 5), marks }];
        });
        await settle(page);
        await shot(page, "first-run-cited");
        await expectNoErrorState(page, "the Search page after the first run");
        expect(pageErrors.length === 0, `${pageErrors.length} uncaught page error(s): ${JSON.stringify(pageErrors)}`);
      } catch (e) {
        await shot(page, "FAIL-first-run");
        throw e;
      }
    });
    await checkWire(bridge, "first run");
    summary.rpcErrors = bridge.calls.filter((c) => !c.ok);
  } finally {
    await ctx?.close().catch(() => {});
    await bridge?.close().catch(() => {});
    await stopDaemon(daemon, "first-run");
  }
  return summary;
}
