// Shared by the e2e scenarios (run.mjs, first-run.mjs): paths and settings from the environment,
// the log, UI strings from en.json, and the check/until/shot helpers. Node built-ins only.
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

/** The browser context every scenario uses: fixed viewport, locale, time zone and the Tauri shim. */
export async function newContext(browser) {
  const ctx = await browser.newContext({
    viewport: { width: 1360, height: 860 },
    deviceScaleFactor: 1,
    locale: "en-US",
    timezoneId: "UTC",
    colorScheme: "light",
  });
  ctx.setDefaultTimeout(STEP_TIMEOUT_MS);
  await ctx.addInitScript({ path: path.join(here, "tauri-shim.js") });
  return ctx;
}
