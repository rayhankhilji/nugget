// Sources setup regressions (QA GG6): the real daemon on an empty home and the real onboarding UI.
// - GG6-31: the recommended AI option starts selected; GG6-21: "Decide later" is not a skipped step.
// - GG6-16: Welcome makes no fixed time promise.
// - GG6-2: a folder with nothing indexable (only a video and a song) settles within seconds,
//   says why and names the skipped files, and doesn't finish the step.
// - GG6-13: adding the same folder again is refused with a plain message.
// - GG6-12: the source step still lists the folders after Back.
// - GG6-14: Ask on Search with no AI explains what to do, with a button to set up AI.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { startBridge } from "./bridge.mjs";
import { FIRST_RUN_CONFIG, startDaemon } from "./daemon.mjs";
import { check, checkWire, DIST, exactName, expect, INDEX_TIMEOUT_MS, KEEP, newContext, OUT, shot, startsWith, stopDaemon, t, until } from "./harness.mjs";

/** How soon a folder with nothing indexable must say so (GG6-2: it used to spin for ever). */
const SETTLE_MS = 10_000;

export async function sourcesSetupScenario({ browser, bin }) {
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), "nugget-e2e-sources-"));
  const pics = path.join(scratch, "Pictures");
  const notes = path.join(scratch, "Notes");
  fs.mkdirSync(pics);
  fs.mkdirSync(notes);
  // Types Nugget doesn't read (images are read, so none here).
  fs.writeFileSync(path.join(pics, "song.mp3"), Buffer.alloc(64));
  fs.writeFileSync(path.join(pics, "clip.mp4"), Buffer.alloc(64));
  fs.writeFileSync(path.join(notes, "saffron.md"), "# Saffron rice\n\nToast the saffron, then steep it in warm stock for ten minutes.\n");

  const daemon = await startDaemon(bin, { stderrPath: path.join(OUT, "sources-setup-nuggetd.out.log"), config: FIRST_RUN_CONFIG });
  let bridge;
  let ctx;
  let nextPick = [pics];
  const pageErrors = [];
  try {
    bridge = await startBridge({ socket: daemon.socket, dist: DIST, pickFolders: () => nextPick });
    ctx = await newContext(browser);
    const page = await ctx.newPage();
    page.on("pageerror", (e) => pageErrors.push(e.message));
    const heading = (key) => page.getByRole("heading", { level: 1, name: t(key), exact: true });
    const footer = page.locator("footer.ob-actions");
    const cont = () => footer.getByRole("button", { name: exactName("onboarding.continue") }).click();
    const saved = () => page.evaluate(() => JSON.parse(localStorage.getItem("nugget.onboarding.v1") ?? "null"));
    const addFolder = async (folder) => {
      nextPick = [folder];
      await page.locator(".ob-row-start .btn").first().click();
      const dialog = page.locator("dialog[open]");
      await dialog.getByRole("button", { name: exactName("addSource.folder.choose") }).click();
      await until("the picked folder is listed in the wizard", async () => {
        const paths = await dialog.locator(".path-list code").allTextContents();
        return [paths.includes(folder), paths];
      });
      await dialog.getByRole("button", { name: exactName("addSource.folder.submit") }).click();
      return dialog;
    };
    const row = (name) => page.locator(".ob-index > li", { hasText: name });

    await check("sources setup: onboarding is honest about empty folders, duplicates and no AI", async () => {
      try {
        await page.goto(`${bridge.url}index.html`);
        await heading("onboarding.welcome.heading").waitFor();
        await page.locator(".ob-main .ob-lead").waitFor();
        const welcome = (await page.locator(".ob-main").textContent()) ?? "";
        expect(!/two minutes/i.test(welcome), "Welcome still promises a fixed setup time");
        await cont();
        await heading("onboarding.source.heading").waitFor();

        // A folder with only a video and a song settles within seconds and says why.
        const dialog = await addFolder(pics);
        await dialog.waitFor({ state: "detached" });
        const empty = await until(
          "the folder with nothing indexable says so",
          async () => {
            const r = await row("Pictures").first().evaluate((li) => ({ phase: li.dataset.phase, text: li.textContent.trim() }));
            return [r.phase === "empty", r];
          },
          SETTLE_MS,
        );
        expect(empty.text.startsWith(t("onboarding.source.empty").replace("{name}", "Pictures")), `unexpected text: ${empty.text}`);
        expect(empty.text.includes("song.mp3") || empty.text.includes("clip.mp4"), `skipped files not named: ${empty.text}`);
        expect((await page.locator("#ob-continue-hint").count()) === 1, "Continue would finish the step with nothing indexed");

        // The same folder again is refused, in plain words.
        const again = await addFolder(pics);
        await until("adding the same folder again is refused", async () => {
          const text = (await again.locator('[role="alert"]').allTextContents()).join(" ");
          return [/already added/i.test(text), text];
        });
        await again.getByRole("button", { name: t("common.close") }).first().click();
        await again.waitFor({ state: "detached" });
        expect((await row("Pictures").count()) === 1, "the folder is listed twice");

        // A folder with a note indexes and makes the step done.
        const d2 = await addFolder(notes);
        await d2.waitFor({ state: "detached" });
        await until(
          "the notes folder is searchable",
          async () => {
            const r = await row("Notes").first().evaluate((li) => ({ phase: li.dataset.phase, text: li.textContent.trim() }));
            return [r.phase === "ready", r];
          },
          INDEX_TIMEOUT_MS,
        );
        expect((await page.locator("#ob-continue-hint").count()) === 0, "a searchable source doesn't finish the step");

        // Back and forward: the step still lists both folders.
        await footer.getByRole("button", { name: exactName("onboarding.back") }).click();
        await heading("onboarding.welcome.heading").waitFor();
        await cont();
        await heading("onboarding.source.heading").waitFor();
        await until("the source step remembers both folders after Back", async () => {
          const rows = await page.locator(".ob-index > li").allTextContents();
          return [rows.length === 2, rows];
        });
        await cont();

        // AI: the recommended option is preselected; "Decide later" counts as a decision.
        await heading("onboarding.ai.heading").waitFor();
        const local = page.getByRole("radio", { name: startsWith("onboarding.ai.local") });
        await until("the recommended AI option starts selected", async () => [await local.isChecked(), null]);
        await page.getByRole("radio", { name: startsWith("onboarding.ai.later") }).check();
        expect((await page.locator("#ob-continue-hint").count()) === 0, "Decide later is announced as skipping the step");
        await cont();
        await heading("onboarding.perm.heading").waitFor();
        const s1 = await saved();
        expect(s1.completed.includes("ai") && !s1.skipped.includes("ai"), `AI step recorded as ${JSON.stringify(s1)}`);

        await cont();
        await heading("onboarding.trigger.heading").waitFor();
        await cont();
        await page.getByRole("heading", { level: 2, name: t("onboarding.done.heading"), exact: true }).waitFor();
        const done = (await page.locator(".ob-main").textContent()) ?? "";
        expect(done.includes(t("onboarding.done.allDone")), `Done lists a pending step: ${done}`);
        await footer.getByRole("button", { name: exactName("onboarding.done.search") }).click();
        await page.locator(`main[aria-label="${t("app.nav.search")}"]`).waitFor();

        // Ask with no AI: an explanation and a button, not an internal error.
        await page.getByRole("button", { name: exactName("search.ask") }).click();
        const panel = page.locator("#ask-panel");
        await panel.getByRole("textbox", { name: t("search.question") }).fill("saffron rice");
        await panel.getByRole("button", { name: exactName("search.findContext") }).click();
        await until("Ask explains that no AI is set up", async () => {
          const text = (await panel.textContent()) ?? "";
          return [text.includes(t("search.noAi")), text.slice(-200)];
        });
        await panel.getByRole("button", { name: exactName("search.setUpAi") }).click();
        await page.locator(`main[aria-label="${t("app.nav.models")}"]`).waitFor();
        expect(pageErrors.length === 0, `${pageErrors.length} uncaught page error(s): ${JSON.stringify(pageErrors)}`);
      } catch (e) {
        await shot(page, "FAIL-sources-setup");
        throw e;
      }
    });

    // The refused duplicate folder is an invalid-params reply by design (GG6-13).
    await checkWire(bridge, "sources setup", (c) => c.cmd === "sources_add" && c.code === -32602);
  } finally {
    await ctx?.close().catch(() => {});
    await bridge?.close().catch(() => {});
    await stopDaemon(daemon, "sources-setup");
    if (!KEEP) fs.rmSync(scratch, { recursive: true, force: true });
  }
  return {};
}
