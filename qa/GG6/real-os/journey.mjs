// GG6: a new user's first session, driven through the real frontend with Playwright.
// Works on any `page` showing the app: the installed app's own webview (Windows, over CDP) or the
// real frontend in Chromium against a real nuggetd through apps/desktop/e2e/bridge.mjs.
//
// Welcome -> "On this device" -> install the recommended model (a REAL download from the signed
// manifest) -> add a folder (typed path, the wizard's fallback to the native picker) -> wait
// until searchable -> Permissions -> Double Tap trigger -> Done -> Search -> Chat questions with
// "Use my sources". Every step is timed and screenshotted; what the user is told is recorded.
import fs from "node:fs";
import path from "node:path";
import { check, exact, friction, log, mark, OUT, QUESTIONS, results, save, sleep, starts, t, until } from "./lib.mjs";

let shotN = 0;
export async function shot(page, name) {
  shotN++;
  const file = path.join(OUT, "shots", `${String(shotN).padStart(2, "0")}-${name}.png`);
  await page.screenshot({ path: file, fullPage: false }).catch((e) => log(`screenshot ${name} failed: ${e.message}`));
  return file;
}

const visibleText = (page, sel = "main, .onboarding, body") => page.locator(sel).first().innerText({ timeout: 5000 }).catch(() => "");

/**
 * Run onboarding. `folder` is the absolute path of the user's documents folder.
 * `ai`: "local" (install the recommended model) or "later".
 * `installTimeoutMs`: how long the real download may take.
 */
export async function onboarding(page, { folder, ai = "local", installTimeoutMs = 45 * 60_000, indexTimeoutMs = 10 * 60_000 }) {
  const ob = (results.onboarding = { ai, folder });
  const heading = (key) => page.getByRole("heading", { level: 1, name: t(key), exact: true });
  const cont = () => page.locator("footer.ob-actions").getByRole("button", { name: exact("onboarding.continue") }).click();
  const errs = [];
  page.on("pageerror", (e) => errs.push(e.message));
  ob.page_errors = errs;

  await check("onboarding: welcome shows on first launch", async () => {
    await heading("onboarding.welcome.heading").waitFor({ timeout: 120_000 });
    mark("welcome_visible");
    await shot(page, "welcome");
    ob.welcome_text = (await visibleText(page)).slice(0, 2000);
  });
  await cont();

  await check("onboarding: AI step", async () => {
    await heading("onboarding.ai.heading").waitFor();
    await shot(page, "ai-choices");
    if (ai === "later") {
      await page.getByRole("radio", { name: starts("onboarding.ai.later") }).check();
      return;
    }
    await page.getByRole("radio", { name: starts("onboarding.ai.local") }).check();
    const cards = page.locator("li.card");
    await until("the model list loads", async () => [(await cards.count()) > 0, await cards.count()], 60_000);
    await sleep(1500); // the fit report arrives separately
    const listed = await cards.evaluateAll((lis) =>
      lis.map((li) => ({ id: li.querySelector(".card-title")?.textContent?.trim(), badges: li.querySelector(".badges")?.textContent?.trim(), text: li.innerText })),
    );
    ob.models_offered = listed;
    ob.machine_line = await page.locator(".ob-section").first().innerText().catch(() => "");
    await shot(page, "ai-local-models");
    log(`models offered: ${JSON.stringify(listed.map((m) => `${m.id} [${m.badges}]`))}`);
    if (!/download/i.test(listed.map((m) => m.text).join(" "))) friction("the model card shows no download size", { listed });
    if (!/(min|hour|second|time)/i.test(listed.map((m) => m.text).join(" "))) friction("no estimate of how long the model download takes is shown before Install", { cards: listed.map((m) => m.text) });
  });

  if (ai === "local") {
    await check("onboarding: install the recommended model (real download)", async () => {
      const rec = page.locator("li.card", { has: page.locator(".badges", { hasText: t("onboarding.ai.localBadge") }) }).first();
      const id = (await rec.locator(".card-title").textContent()).trim();
      ob.model = id;
      mark("install_clicked", { model: id });
      await rec.getByRole("button", { name: exact("onboarding.models.install") }).click();
      const samples = [];
      let lastShot = 0;
      await until(
        `${id} installs`,
        async () => {
          const s = await rec.evaluate((li) => ({
            value: li.querySelector("progress") ? Number(li.querySelector("progress").value) : null,
            text: li.querySelector(".progress .small")?.textContent?.trim() ?? null,
            badges: li.querySelector(".badges")?.textContent ?? "",
            error: li.querySelector("[role=alert]")?.textContent?.trim() ?? null,
          }));
          if (s.error) {
            const e = new Error(`install failed: ${s.error}`);
            e.fatal = true;
            await shot(page, "install-failed");
            throw e;
          }
          const last = samples[samples.length - 1];
          if (!last || last.value !== s.value || last.text !== s.text) samples.push({ ...s, at_ms: Date.now() });
          if (Date.now() - lastShot > 120_000) {
            lastShot = Date.now();
            await shot(page, "install-progress");
          }
          return [s.badges.includes(t("onboarding.models.installed")), s];
        },
        installTimeoutMs,
        1000,
      );
      ob.install_samples = samples.filter((_, i) => i % Math.max(1, Math.floor(samples.length / 40)) === 0);
      mark("model_installed", { model: id });
      await shot(page, "model-installed");
    });
  }
  await cont();
  mark("ai_step_done");

  await check("onboarding: add the documents folder and wait until searchable", async () => {
    await heading("onboarding.source.heading").waitFor();
    await shot(page, "source-step");
    ob.source_text = (await visibleText(page)).slice(0, 1500);
    await page.getByRole("button", { name: exact("onboarding.source.addFolder") }).click();
    const dialog = page.locator("dialog[open]");
    await dialog.waitFor();
    await dialog.locator("#folder-path").fill(folder);
    await dialog.getByRole("button", { name: exact("addSource.folder.add") }).click();
    await until("the typed folder is listed", async () => {
      const paths = await dialog.locator(".path-list code").allTextContents();
      return [paths.length > 0, paths];
    });
    await shot(page, "folder-form");
    await dialog.getByRole("button", { name: exact("addSource.folder.submit") }).click();
    await dialog.waitFor({ state: "detached", timeout: 30_000 });
    mark("source_added");
    const phases = [];
    const row = await until(
      "the source step says search works",
      async () => {
        const r = await page.locator(".ob-index li").first().evaluate((li) => ({ phase: li.dataset.phase, text: li.textContent.trim() }));
        if (!phases.length || phases[phases.length - 1].text !== r.text) phases.push({ ...r, at_ms: Date.now() });
        if (r.phase === "failed") {
          const e = new Error(`indexing failed: ${r.text}`);
          e.fatal = true;
          throw e;
        }
        return [r.phase === "ready", r];
      },
      indexTimeoutMs,
      500,
    );
    ob.index_phases = phases;
    ob.index_ready_text = row.text;
    mark("source_searchable");
    await shot(page, "source-ready");
  });
  await cont();

  await check("onboarding: permissions step", async () => {
    await heading("onboarding.perm.heading").waitFor();
    ob.permissions_text = (await visibleText(page)).slice(0, 3000);
    await shot(page, "permissions");
  });
  await cont();
  await check("onboarding: Double Tap trigger step", async () => {
    await heading("onboarding.trigger.heading").waitFor();
    ob.trigger_text = (await visibleText(page)).slice(0, 2000);
    await shot(page, "trigger");
  });
  await cont();
  await check("onboarding: done", async () => {
    await heading("onboarding.done.heading").waitFor();
    ob.done_text = (await visibleText(page)).slice(0, 1500);
    await shot(page, "done");
    const finish = new RegExp(`^(${t("onboarding.done.search")}|${t("onboarding.done.start")})$`);
    await page.locator("footer.ob-actions").getByRole("button", { name: finish }).click();
    await page.locator("main[aria-label]").first().waitFor();
    mark("onboarding_done");
  });
  save();
}

export async function nav(page, key) {
  await page.locator("nav .nav-item", { hasText: exact(key) }).first().click();
  await page.locator(`main[aria-label="${t(key)}"]`).waitFor();
}

/** First search, as a user types it. */
export async function firstSearch(page, query = "dentist appointment") {
  return check(`search: "${query}" finds the dentist note`, async () => {
    await nav(page, "app.nav.search");
    const box = page.getByRole("searchbox", { name: t("search.query") });
    await box.fill(query);
    await box.press("Enter");
    const titles = await until(
      "results",
      async () => {
        const all = await page.locator("ol.results li.result .result-title").allTextContents();
        return [all.length > 0, all];
      },
      30_000,
    );
    mark("first_search_results");
    await shot(page, "first-search");
    results.first_search = { query, top: titles.slice(0, 5) };
    if (!titles.slice(0, 3).some((x) => /dentist/i.test(x))) throw new Error(`no dentist note in the top 3: ${JSON.stringify(titles.slice(0, 5))}`);
    return titles.slice(0, 5);
  });
}

/** Ask every question in questions.json in Chat with "Use my sources"; returns the answers. */
export async function chatQuestions(page, { label = "chat", timeoutMs = 300_000, questions = QUESTIONS } = {}) {
  await nav(page, "app.nav.chat");
  const answers = [];
  for (const q of questions) {
    const rec = { id: q.id, q: q.q };
    await check(`${label}: ${q.id} "${q.q}"`, async () => {
      const newChat = page.getByRole("button", { name: /^(New chat|New conversation)$/i });
      if (await newChat.count()) await newChat.first().click().catch(() => {});
      const sources = page.getByRole("checkbox", { name: starts("chat.useSources") });
      if ((await sources.count()) > 0 && !(await sources.isChecked())) await sources.check();
      const input = page.getByRole("textbox", { name: t("chat.message") });
      await input.fill(q.q);
      const before = await page.locator("li.msg-assistant").count();
      const t0 = Date.now();
      await page.getByRole("button", { name: exact("chat.send") }).click();
      const reply = page.locator("li.msg-assistant").nth(before);
      let first = null;
      const r = await until(
        `an answer to ${q.id}`,
        async () => {
          const v = await reply.evaluate((li) => ({
            text: li.querySelector(".msg-body")?.textContent?.trim() ?? "",
            streaming: !!li.querySelector(".caret"),
            citations: [...li.querySelectorAll("li.citation .citation-title")].map((c) => c.textContent.trim()),
            meta: li.querySelector(".msg-meta")?.textContent?.trim() ?? "",
            error: li.querySelector("[role=alert]")?.textContent?.trim() ?? null,
          }));
          if (v.text && first === null) first = Date.now() - t0;
          return [!!v.error || (!v.streaming && v.text.length > 0), v];
        },
        timeoutMs,
        500,
      );
      Object.assign(rec, r, { first_token_ms: first, total_ms: Date.now() - t0 });
      const hay = r.text.toLowerCase();
      rec.correct = q.abstain ? /(don't|do not|can't|cannot|no information|not (in|mention)|couldn't|could not)/i.test(r.text) : q.expect_any.some((e) => hay.includes(e.toLowerCase()));
      rec.cited_expected = q.source ? r.citations.some((c) => c.toLowerCase().includes(q.source.toLowerCase())) : null;
      log(`${q.id}: ${rec.correct ? "correct" : "WRONG"} in ${rec.total_ms} ms (first token ${first} ms): ${r.text.slice(0, 200).replace(/\n/g, " ")}`);
      if (r.error) throw new Error(`error shown: ${r.error}`);
      if (!rec.correct) throw new Error(`wrong answer: ${r.text.slice(0, 300)}`);
      return rec;
    });
    answers.push(rec);
    if (answers.length === 1) await shot(page, `${label}-first-answer`);
  }
  await shot(page, `${label}-last-answer`);
  results[label] = { n: answers.length, correct: answers.filter((a) => a.correct).length, answers };
  save();
  return answers;
}

export function writeJson(name, v) {
  fs.writeFileSync(path.join(OUT, name), JSON.stringify(v, null, 2));
}
