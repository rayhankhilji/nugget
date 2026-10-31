// A1 onboarding flows: Continue all the way (with a real personal corpus), the first search and
// "Ask" with no AI, Skip each step, Skip all, Back, quit-and-reopen on every step, "Run setup again".
// Usage (repo root): node qa/GG6/a1-onboarding/flows.mjs [scenario,...]
import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";
import { spawnSync } from "node:child_process";
import * as L from "./lib.mjs";

const { chromium } = createRequire(path.join(L.REPO, "apps/desktop/package.json"))("playwright");
const { t, exact, starts, log, record, snap } = L;
const want = (process.argv[2] || "continue,skip-each,skip-all,back,resume,setup-again").split(",");
const timings = {};

// The synthetic personal corpus (about 150 files) and its 10 day-one questions.
const CORPUS_OUT = path.join(L.SCRATCH, "corpus-flows");
if (!fs.existsSync(path.join(CORPUS_OUT, "questions.json"))) {
  const r = spawnSync("python3", ["-I", path.join(L.HERE, "make_corpus.py"), CORPUS_OUT], { encoding: "utf8" });
  if (r.status !== 0) throw new Error(r.stderr);
}
const CORPUS = path.join(CORPUS_OUT, "corpus");
const QUESTIONS = JSON.parse(fs.readFileSync(path.join(CORPUS_OUT, "questions.json"), "utf8"));

const browser = await chromium.launch();

async function addFolderViaPicker(page, env, dir) {
  env.picker.next = [dir];
  await page.getByRole("button", { name: exact("onboarding.source.addFolder") }).click();
  const dialog = page.locator("dialog[open]");
  await dialog.getByRole("button", { name: exact("addSource.folder.choose") }).click();
  await dialog.getByRole("button", { name: exact("addSource.folder.submit") }).click();
  await dialog.waitFor({ state: "detached" });
}

// ---------------------------------------------------------------------------------------------
async function continueAll() {
  const S = "continue";
  const env = await L.boot(browser, { name: S });
  const m = { daemon_ready_ms: env.daemonMs };
  const t0 = Date.now();
  const mark = (k) => (m[k] = Date.now() - t0);
  try {
    const page = await env.open();
    await L.waitHeading(page, "onboarding.welcome.heading");
    mark("welcome_shown");
    await snap(page, "01-welcome");
    await L.cont(page);
    await L.waitHeading(page, "onboarding.ai.heading");
    mark("ai_shown");
    await snap(page, "02-ai-step");
    // A real user who doesn't read: Continue without picking any option.
    const radiosChecked = await page.getByRole("radio").evaluateAll((rs) => rs.filter((r) => r.checked).map((r) => r.parentElement?.textContent?.trim().slice(0, 40)));
    record(S, "AI step: some option preselected", radiosChecked.length > 0, radiosChecked);
    await L.cont(page);
    await L.waitHeading(page, "onboarding.source.heading");
    mark("source_shown");
    await snap(page, "03-source-step");
    const tAdd = Date.now();
    await addFolderViaPicker(page, env, CORPUS);
    mark("source_added");
    const phases = [];
    let row;
    for (let i = 0; i < 600; i++) {
      [row] = await L.progressRows(page);
      if (row && (!phases.length || phases.at(-1).text !== row.text)) phases.push({ ...row, ms: Date.now() - tAdd });
      if (row?.phase === "ready" || row?.phase === "failed") break;
      await L.sleep(250);
    }
    mark("source_ready");
    record(S, "source step reaches 'Search works now'", row?.phase === "ready", row?.text);
    m.index_phases = phases;
    await snap(page, "04-source-indexed");
    const st = await L.rpc(env.daemon.socket, "status");
    m.items_after_ready = st.items;
    await L.cont(page);
    await L.waitHeading(page, "onboarding.perm.heading");
    await snap(page, "05-permissions");
    await L.cont(page);
    await L.waitHeading(page, "onboarding.trigger.heading");
    await snap(page, "06-trigger");
    await L.cont(page);
    await L.waitHeading(page, "onboarding.done.heading");
    mark("done_shown");
    await snap(page, "07-done");
    const doneText = await page.locator("main.ob-main").innerText();
    record(S, "Done says 'Every step is done.'", doneText.includes(t("onboarding.done.allDone")), doneText.replace(/\s+/g, " ").slice(0, 300));
    const toSearch = L.footerBtn(page, "onboarding.done.search");
    record(S, "Done offers 'Search your sources'", await toSearch.isVisible().catch(() => false));
    await toSearch.click();
    await page.locator(`main[aria-label="${t("app.nav.search")}"]`).waitFor();
    mark("search_open");

    // Let knowledge extraction settle a bit so search is at its day-one best (still record time).
    const ks0 = Date.now();
    for (let i = 0; i < 120; i++) {
      const ks = await L.rpc(env.daemon.socket, "knowledge.status");
      if (ks.pending_items === 0) break;
      await L.sleep(500);
    }
    m.knowledge_settle_extra_ms = Date.now() - ks0;

    // ---- the 10 day-one questions ----
    const box = page.getByRole("searchbox", { name: t("search.query") });
    const qa = [];
    for (const q of QUESTIONS) {
      await box.fill(q.q);
      const tq = Date.now();
      await box.press("Enter");
      let hits = [];
      for (let i = 0; i < 40; i++) {
        await L.sleep(150);
        const busy = await page.locator('[aria-busy="true"]').count();
        hits = await page.locator("ol.results li.result").evaluateAll((lis) => lis.map((li) => ({ title: li.querySelector(".result-title")?.textContent?.trim() ?? "", text: li.textContent ?? "" })));
        const empty = await page.getByText(/^No results for/).count();
        if (!busy && (hits.length || empty)) break;
      }
      const base = path.basename(q.file).replace(/\.[^.]+$/, "");
      const uiMs = Date.now() - tq;
      // Authoritative rank: the daemon's own hit list (same call the UI makes), matched by file path.
      const raw = await L.rpc(env.daemon.socket, "search", { query: q.q, limit: 10 });
      const fname = path.basename(q.file);
      const rank = raw.findIndex((h) => decodeURIComponent(h.url ?? "").endsWith(fname));
      const snippetHasAnswer = rank >= 0 && raw[rank].snippet.includes(q.must);
      qa.push({ q: q.q, expect: q.file, rank: rank < 0 ? null : rank + 1, hit3: rank >= 0 && rank < 3, snippetHasAnswer, ms: uiMs, top3: hits.slice(0, 3).map((h) => h.title) });
    }
    m.qa = qa;
    const hit3 = qa.filter((x) => x.hit3).length;
    record(S, `day-one questions hit@3 = ${hit3}/${qa.length}`, hit3 >= 7, qa.map((x) => `${x.q} -> ${x.rank ?? "miss"}`).join("; "));
    // screenshot one good and one miss
    await box.fill(QUESTIONS[0].q);
    await box.press("Enter");
    await L.sleep(1200);
    await snap(page, "08-search-dentist");
    const miss = qa.find((x) => !x.hit3);
    if (miss) {
      await box.fill(miss.q);
      await box.press("Enter");
      await L.sleep(1200);
      await snap(page, "09-search-miss");
    }

    // ---- Ask with no AI configured ----
    await box.fill(QUESTIONS[1].q);
    await box.press("Enter");
    await L.sleep(800);
    await page.getByRole("button", { name: exact("search.ask") }).click();
    const panel = page.locator("#ask-panel");
    const question = panel.getByRole("textbox", { name: t("search.question") });
    if (!(await question.inputValue()).trim()) await question.fill(QUESTIONS[1].q);
    await panel.getByRole("button", { name: exact("search.findContext") }).click();
    await panel.locator("ol.citations li.citation").first().waitFor({ timeout: 15000 }).catch(() => {});
    const askBtn = panel.getByRole("button", { name: exact("search.askWithContext") });
    const askVisible = await askBtn.isVisible().catch(() => false);
    record(S, "Ask panel: 'Ask with this context' offered with no AI configured", askVisible);
    let askMsg = null;
    if (askVisible) {
      await askBtn.click();
      for (let i = 0; i < 60; i++) {
        await L.sleep(250);
        const errs = await L.visibleErrors(page);
        if (errs.length) {
          askMsg = errs.join(" | ");
          break;
        }
      }
    }
    m.ask_no_ai_message = askMsg;
    record(S, "Ask with no AI explains what to do (points at the Models page)", askMsg && /Models page|choose a model|set up/i.test(askMsg), askMsg);
    await snap(page, "10-ask-no-ai");

    // Chat page with no AI
    await page.getByRole("link", { name: t("app.nav.chat") }).or(page.getByRole("button", { name: t("app.nav.chat") })).first().click().catch(() => {});
    await L.sleep(1000);
    const chatText = (await page.locator("main").innerText().catch(() => "")).replace(/\s+/g, " ").slice(0, 400);
    m.chat_no_ai = chatText;
    await snap(page, "11-chat-no-ai");
    record(S, "no uncaught page errors", env.errors.length === 0, env.errors);
    timings.continue = m;
  } catch (e) {
    record(S, "flow completed", false, e.message.split("\n")[0]);
    timings.continue = m;
  } finally {
    await env.close();
  }
}

// ---------------------------------------------------------------------------------------------
async function skipEach() {
  const S = "skip-each";
  const env = await L.boot(browser, { name: S });
  try {
    const page = await env.open();
    const seen = [];
    for (let i = 0; i < 5; i++) {
      seen.push(await L.heading(page));
      await L.skip(page);
      await L.sleep(200);
    }
    seen.push(await L.heading(page));
    record(S, "Skip walks welcome..done", seen.at(-1) === t("onboarding.done.heading"), seen);
    const doneText = (await page.locator("main.ob-main").innerText()).replace(/\s+/g, " ");
    record(S, "Done lists the skipped steps", /You skipped 4 steps/.test(doneText), doneText.slice(0, 300));
    await snap(page, "12-done-after-skip-each");
    const progress = await page.locator(".ob-progress li").evaluateAll((lis) => lis.map((l) => l.className.replace("ob-step ", "")));
    record(S, "progress marks steps skipped", progress.filter((c) => c.includes("skipped")).length === 5, progress);
    // Welcome is listed as skipped in the progress bar but not as a pending step; harmless.
    // Done with nothing indexed: primary button is "Start using Nugget".
    const btn = await page.locator("footer.ob-actions .btn-primary").textContent();
    record(S, "Done's primary is 'Start using Nugget' when nothing indexed", btn === t("onboarding.done.start"), btn);
    await page.locator("footer.ob-actions .btn-primary").click();
    await page.locator("main").first().waitFor();
    await L.sleep(800);
    const landed = await page.locator("main").first().getAttribute("aria-label");
    record(S, "lands on an app page", !!landed, landed);
    await snap(page, "13-landing-after-skip");
    const mainText = (await page.locator("main").first().innerText()).replace(/\s+/g, " ").slice(0, 300);
    timings.skip_each_landing = { landed, mainText };
    await page.reload();
    await L.sleep(1000);
    record(S, "onboarding not shown again after reload", (await page.locator("h1#ob-heading").count()) === 0);
  } catch (e) {
    record(S, "flow completed", false, e.message.split("\n")[0]);
  } finally {
    await env.close();
  }
}

async function skipAllAndSetupAgain() {
  const S = "skip-all";
  const env = await L.boot(browser, { name: S });
  try {
    const page = await env.open();
    await L.waitHeading(page, "onboarding.welcome.heading");
    await L.skipAll(page);
    await L.sleep(800);
    const landed = await page.locator("main").first().getAttribute("aria-label");
    record(S, "Skip setup exits immediately, no confirm", (await page.locator("h1#ob-heading").count()) === 0, landed);
    await snap(page, "14-after-skip-all");
    const mainText = (await page.locator("main").first().innerText()).replace(/\s+/g, " ").slice(0, 400);
    record(S, "landing page after Skip setup points at adding a source", /source/i.test(mainText), mainText);
    await page.reload();
    await L.sleep(800);
    record(S, "skip-all persists across reload", (await page.locator("h1#ob-heading").count()) === 0);
    // Run setup again (sidebar)
    const again = page.getByRole("button", { name: exact("settings.setupAgain") });
    record(S, "'Run setup again' visible in the sidebar", await again.isVisible().catch(() => false));
    await again.click();
    await L.waitHeading(page, "onboarding.welcome.heading");
    const st = JSON.parse(await L.saved(page));
    record(S, "setup again restarts at Welcome with nothing completed", st.step === "welcome" && st.completed.length === 0, st);
  } catch (e) {
    record(S, "flow completed", false, e.message.split("\n")[0]);
  } finally {
    await env.close();
  }
}

async function backFlow() {
  const S = "back";
  const env = await L.boot(browser, { name: S });
  try {
    const page = await env.open();
    await L.waitHeading(page, "onboarding.welcome.heading");
    record(S, "no Back on Welcome", (await L.footerBtn(page, "onboarding.back").count()) === 0);
    await L.cont(page);
    await page.getByRole("radio", { name: starts("onboarding.ai.later") }).check();
    await L.cont(page);
    await L.waitHeading(page, "onboarding.source.heading");
    // add a source, then go Back and forward: is the progress still shown?
    const smallDir = path.join(L.SCRATCH, "back-small");
    fs.mkdirSync(smallDir, { recursive: true });
    fs.writeFileSync(path.join(smallDir, "note.md"), "# Groceries\n\nOat milk, lentils, limes.\n");
    await addFolderViaPicker(page, env, smallDir);
    await L.sleep(3000);
    const before = await L.progressRows(page);
    await L.back(page);
    await L.waitHeading(page, "onboarding.ai.heading");
    const later = await page.getByRole("radio", { name: starts("onboarding.ai.later") }).isChecked();
    record(S, "AI choice kept after Back", later);
    await L.cont(page);
    await L.waitHeading(page, "onboarding.source.heading");
    const after = await L.progressRows(page);
    const btnLabel = await page.locator(".ob-row-start .btn").first().textContent();
    record(S, "source step still shows the folder added before Back", after.length === before.length && after.length > 0, { before, after, btnLabel });
    await snap(page, "15-source-after-back-forward");
    // Clicking a step in the progress nav jumps without Continue
    await page.locator(".ob-progress li").nth(4).getByRole("button").click();
    record(S, "progress nav jumps to Double Tap", (await L.heading(page)) === t("onboarding.trigger.heading"));
  } catch (e) {
    record(S, "flow completed", false, e.message.split("\n")[0]);
  } finally {
    await env.close();
  }
}

// Quit mid-way on each step (close the page) and reopen in the same profile.
async function resume() {
  const S = "resume";
  const env = await L.boot(browser, { name: S });
  const keys = ["onboarding.welcome.heading", "onboarding.ai.heading", "onboarding.source.heading", "onboarding.perm.heading", "onboarding.trigger.heading", "onboarding.done.heading"];
  try {
    let page = await env.open();
    for (let i = 0; i < keys.length; i++) {
      await L.waitHeading(page, keys[i]);
      if (i === 2) {
        // on the source step: add a folder, quit while it indexes
        const dir = path.join(L.SCRATCH, "resume-src");
        fs.mkdirSync(dir, { recursive: true });
        for (let k = 0; k < 40; k++) fs.writeFileSync(path.join(dir, `n${k}.md`), `# Note ${k}\n\nresume test ${k} heron\n`);
        await addFolderViaPicker(page, env, dir);
      }
      await page.close();
      page = await env.open();
      const h = await L.heading(page);
      record(S, `quit on step ${i + 1} reopens there`, h === t(keys[i]), h);
      if (i === 2) {
        await L.sleep(1500);
        const rows = await L.progressRows(page);
        const btn = await page.locator(".ob-row-start .btn").first().textContent();
        record(S, "source step after reopen shows the folder already added", rows.length > 0 || btn === t("onboarding.source.addAnother"), { rows, btn });
        await snap(page, "16-source-after-reopen");
        // what a user then does: add the same folder again
        await addFolderViaPicker(page, env, path.join(L.SCRATCH, "resume-src"));
        await L.sleep(2500);
        const list = await L.rpc(env.daemon.socket, "sources.list");
        const errs = await L.visibleErrors(page);
        record(S, "adding the same folder again is refused or deduplicated", list.length === 1 || errs.length > 0, { sources: list.map((s) => ({ id: s.id, name: s.name, items: s.items })), errs });
        await snap(page, "17-same-folder-twice");
        const st = await L.rpc(env.daemon.socket, "status");
        const hits = await L.rpc(env.daemon.socket, "search", { query: "heron resume test 7", limit: 5 }).catch((e) => ({ error: e.message }));
        timings.duplicate = { items: st.items, hits: Array.isArray(hits) ? hits.map((h) => h.title ?? h.path ?? h.id) : hits };
      }
      if (i < keys.length - 1) await L.cont(page);
    }
  } catch (e) {
    record(S, "flow completed", false, e.message.split("\n")[0]);
  } finally {
    await env.close();
  }
}

// Finish with a source, then "Run setup again": does the source step know about it?
async function setupAgain() {
  const S = "setup-again";
  const env = await L.boot(browser, { name: S });
  try {
    const page = await env.open();
    await L.waitHeading(page, "onboarding.welcome.heading");
    await L.cont(page);
    await page.getByRole("radio", { name: starts("onboarding.ai.later") }).check();
    await L.cont(page);
    const dir = path.join(L.SCRATCH, "again-src");
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(path.join(dir, "a.md"), "# Alpha\n\nsetup again\n");
    await addFolderViaPicker(page, env, dir);
    await L.sleep(3000);
    await L.skipAll(page);
    await L.sleep(800);
    await page.getByRole("button", { name: exact("settings.setupAgain") }).click();
    await L.waitHeading(page, "onboarding.welcome.heading");
    const ai = await (async () => {
      await L.cont(page);
      return page.getByRole("radio", { name: starts("onboarding.ai.later") }).isChecked();
    })();
    record(S, "setup again keeps the AI choice", ai);
    await L.cont(page);
    await L.waitHeading(page, "onboarding.source.heading");
    const rows = await L.progressRows(page);
    const btn = await page.locator(".ob-row-start .btn").first().textContent();
    record(S, "setup again: source step acknowledges the existing source", rows.length > 0 || btn === t("onboarding.source.addAnother"), { rows, btn });
    await snap(page, "18-setup-again-source");
  } catch (e) {
    record(S, "flow completed", false, e.message.split("\n")[0]);
  } finally {
    await env.close();
  }
}

const all = { continue: continueAll, "skip-each": skipEach, "skip-all": skipAllAndSetupAgain, back: backFlow, resume, "setup-again": setupAgain };
for (const w of want) {
  log(`=== ${w}`);
  await all[w]();
}
await browser.close();
const prev = fs.existsSync(path.join(L.HERE, "results-flows.json")) ? JSON.parse(fs.readFileSync(path.join(L.HERE, "results-flows.json"), "utf8")) : {};
L.writeJson("results-flows.json", { ...prev, ...Object.fromEntries(want.map((w) => [w, L.results.filter((r) => r.scenario === w)])), timings: { ...(prev.timings ?? {}), ...timings } });
const fails = L.results.filter((r) => !r.ok).length;
log(`${L.results.length} checks, ${fails} failed`);
