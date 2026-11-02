// Shared by the e2e scenarios: settings from the environment, UI strings, check/until/shot helpers.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const here = path.dirname(fileURLToPath(import.meta.url));
export const DESKTOP = path.resolve(here, "..");
export const DIST = path.resolve(process.env.E2E_DIST || path.join(DESKTOP, "dist"));
export const OUT = path.resolve(process.env.E2E_ARTIFACTS || path.join(here, "artifacts"));
export const KEEP = process.env.E2E_KEEP === "1";
export const INDEX_TIMEOUT_MS = Number(process.env.E2E_INDEX_TIMEOUT_MS || 180_000);
export const STEP_TIMEOUT_MS = Number(process.env.E2E_STEP_TIMEOUT_MS || 15_000);

const started = Date.now();
export const secs = (ms) => `${(ms / 1000).toFixed(1)}s`;
export const log = (...a) => console.log(`[e2e ${secs(Date.now() - started)}]`, ...a);
export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
export const sinceStart = () => Date.now() - started;

// ---- UI strings, read from the app source so copy changes don't break the test ----

const en = JSON.parse(fs.readFileSync(path.join(DESKTOP, "src", "i18n", "locales", "en.json"), "utf8"));
export function t(key) {
  const s = en[key];
  if (typeof s !== "string") throw new Error(`no English string for ${key} in src/i18n/locales/en.json`);
  return s;
}

/** A RegExp matching the English message `key` exactly, with `{args}` and plurals as wildcards. */
export function tPattern(key) {
  let out = "";
  // Text content never holds the markup some messages carry (`<code>{path}</code>`).
  const s = t(key).replace(/<\/?[a-zA-Z][^>]*>/g, "");
  for (let i = 0; i < s.length; ) {
    if (s[i] === "{") {
      // Skip the whole (possibly nested) placeholder.
      let depth = 0;
      do {
        if (s[i] === "{") depth++;
        else if (s[i] === "}") depth--;
        i++;
      } while (depth > 0 && i < s.length);
      out += ".*?";
    } else {
      out += s[i].replace(/[.*+?^$()|[\]\\]/g, "\\$&");
      i++;
    }
  }
  return new RegExp(`^${out}$`);
}

// ---- checks ----

/** Every check's outcome, in order: `{ name, ok, ms, error? }`. */
export const results = [];

export class CheckError extends Error {}
export function expect(cond, message) {
  if (!cond) throw new CheckError(message);
}

/**
 * Poll `probe` (returning `[ok, observed]`) until ok, or fail with `what` and the last observation.
 * Playwright locators auto-wait for elements; this covers data that arrives after them.
 */
export async function until(what, probe, timeoutMs = STEP_TIMEOUT_MS) {
  const deadline = Date.now() + timeoutMs;
  let observed;
  for (;;) {
    let ok = false;
    try {
      [ok, observed] = await probe();
    } catch (e) {
      observed = `error: ${e.message.split("\n")[0]}`;
    }
    if (ok) return observed;
    if (Date.now() > deadline) throw new CheckError(`${what}; after ${timeoutMs} ms saw: ${JSON.stringify(observed)}`);
    await sleep(200);
  }
}

export async function check(name, fn) {
  const t0 = Date.now();
  try {
    await fn();
    results.push({ name, ok: true, ms: Date.now() - t0 });
    log(`PASS ${name} (${secs(Date.now() - t0)})`);
  } catch (e) {
    const message = e instanceof CheckError ? e.message : (e.stack ?? String(e));
    results.push({ name, ok: false, ms: Date.now() - t0, error: message });
    log(`FAIL ${name}: ${message}`);
  }
}

// ---- browser helpers ----

let shotN = 0;
export async function shot(page, name) {
  const file = path.join(OUT, `${String(++shotN).padStart(2, "0")}-${name}.png`);
  await page.screenshot({ path: file }).catch((e) => log(`  screenshot ${name} failed: ${e.message}`));
}

/** Wait until the bridge has been quiet for a moment and nothing is marked busy. */
export async function settle(page) {
  await page.waitForLoadState("networkidle", { timeout: 10_000 }).catch(() => {});
  await page
    .waitForFunction(() => !document.querySelector('[aria-busy="true"]'), null, { timeout: 5_000 })
    .catch(() => {});
}

/** Visible error notes (`ErrorNote` and friends render `.note-error` / role=alert). */
export async function errorNotes(page, scope = "main") {
  return page.evaluate(
    (scope) =>
      [...document.querySelectorAll(`${scope} .note-error, ${scope} [role=alert]`)]
        .filter((el) => el.getClientRects().length > 0 && el.textContent.trim())
        .map((el) => el.textContent.trim().replace(/\s+/g, " ")),
    scope,
  );
}

export async function expectNoErrorState(page, what, scope) {
  const notes = await errorNotes(page, scope);
  expect(notes.length === 0, `${what} shows an error state: ${JSON.stringify(notes)}`);
}

/** Onboarding state of a user who finished it. */
export const ONBOARDED = { version: 1, step: "done", completed: [], skipped: [], ai: null, finished: true };

/**
 * The browser context every scenario uses: fixed viewport, locale, time zone and the Tauri shim.
 * `onboarded` (true or a state) skips onboarding; `page` opens that page; the rest are Playwright options.
 */
export async function newContext(browser, { onboarded, page, ...options } = {}) {
  const ctx = await browser.newContext({
    viewport: { width: 1360, height: 860 },
    deviceScaleFactor: 1,
    locale: "en-US",
    timezoneId: "UTC",
    colorScheme: "light",
    ...options,
  });
  ctx.setDefaultTimeout(STEP_TIMEOUT_MS);
  await ctx.addInitScript({ path: path.join(here, "tauri-shim.js") });
  if (onboarded || page) {
    const state = onboarded === true ? ONBOARDED : (onboarded ?? null);
    await ctx.addInitScript(
      ([state, page]) => {
        if (state) localStorage.setItem("nugget.onboarding.v1", JSON.stringify(state));
        if (page) localStorage.setItem("nugget.page", page);
      },
      [state, page ?? null],
    );
  }
  return ctx;
}

const escapeRe = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
/** Accessible-name matchers for the English message `key`. */
export const exactName = (key) => new RegExp(`^${escapeRe(t(key))}$`);
export const startsWith = (key) => new RegExp(`^${escapeRe(t(key))}`);

/** Onboarding's source step: add `folder` through the stubbed picker and wait until it's indexed. */
export async function onboardingAddFolder(page, folder) {
  await page.getByRole("button", { name: exactName("onboarding.source.addFolder") }).click();
  const dialog = page.locator("dialog[open]");
  await dialog.getByRole("button", { name: exactName("addSource.folder.choose") }).click();
  await until("the picked folder is listed in the wizard", async () => {
    const paths = await dialog.locator(".path-list code").allTextContents();
    return [paths.includes(folder), paths];
  });
  await dialog.getByRole("button", { name: exactName("addSource.folder.submit") }).click();
  await dialog.waitFor({ state: "detached" });
  const readyRe = tPattern("onboarding.source.ready");
  return until(
    "the source step says the folder is indexed and search works",
    async () => {
      const row = await page.locator(".ob-index li").first().evaluate((li) => ({ phase: li.dataset.phase, text: li.textContent.trim() }));
      if (row.phase === "failed") throw new Error(`indexing failed: ${row.text}`);
      return [row.phase === "ready" && readyRe.test(row.text), row];
    },
    INDEX_TIMEOUT_MS,
  );
}

/** Check that every daemon command the UI called exists and accepted its params. */
export function checkWire(bridge, label, skip = () => false) {
  return check(`${label}: every daemon command the UI called exists and accepted its params`, async () => {
    const bad = bridge.calls.filter((c) => c.via === "daemon" && (c.code === -32601 || c.code === -32602) && !skip(c));
    expect(bad.length === 0, `wire mismatches: ${JSON.stringify(bad)}`);
  });
}

/** Copy a scenario daemon's logs into the artifacts and stop it (E2E_KEEP=1 keeps its home). */
export async function stopDaemon(daemon, name) {
  if (fs.existsSync(daemon.logDir)) fs.cpSync(daemon.logDir, path.join(OUT, `${name}-daemon-logs`), { recursive: true });
  await daemon.stop({ keep: KEEP });
  if (KEEP) log(`kept the ${name} daemon home: ${daemon.home}`);
}

/**
 * Open a page through the sidebar, as a person would. The sidebar lists the main pages (and
 * Chat) or, on a Settings page, the Settings pages: it goes Back or into Settings first when the
 * page is listed in the other mode.
 */
export async function navTo(page, id) {
  const label = t(`app.nav.${id}`);
  const esc = label.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const item = page.locator("aside.sidebar nav .nav-item").filter({ hasText: new RegExp(`^\\s*${esc}\\s*(\\d+\\+?)?\\s*$`) }).first();
  if (!(await item.isVisible())) {
    const back = page.locator("aside.sidebar .nav-back");
    if (await back.isVisible()) await back.click();
    else await page.locator("aside.sidebar .account").click();
  }
  // Advanced settings pages sit in a collapsed group.
  if (!(await item.isVisible())) await page.locator("aside.sidebar details.nav-advanced:not([open]) > summary").click();
  await item.click();
  await page.locator(`main[aria-label="${label}"]`).waitFor();
}
