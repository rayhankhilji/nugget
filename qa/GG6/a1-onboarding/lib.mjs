// Shared helpers for the A1 (onboarding + first search) QA scenarios. Builds on the e2e harness in
// apps/desktop/e2e (imported, never modified): the real nuggetd, the real dist/ in headless
// Chromium through bridge.mjs + tauri-shim.js.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const HERE = path.dirname(fileURLToPath(import.meta.url));
export const REPO = path.resolve(HERE, "../../..");
export const E2E = path.join(REPO, "apps/desktop/e2e");
export const SCRATCH = process.env.A1_SCRATCH || path.join(os.tmpdir(), "a1-qa");
fs.mkdirSync(SCRATCH, { recursive: true });
// startDaemon() makes its temp home with os.tmpdir(): keep it in our scratch dir.
process.env.TMPDIR = SCRATCH;
process.env.E2E_ARTIFACTS = process.env.E2E_ARTIFACTS || path.join(SCRATCH, "artifacts");
export const SHOTS = path.join(HERE, "shots");
fs.mkdirSync(SHOTS, { recursive: true });

const { startBridge } = await import(path.join(E2E, "bridge.mjs"));
const { startDaemon, rpc, FIRST_RUN_CONFIG } = await import(path.join(E2E, "daemon.mjs"));
const harness = await import(path.join(E2E, "harness.mjs"));
export const { t, tPattern, sleep } = harness;
export { rpc, startBridge };

/**
 * nuggetd started through `setpriv` without CAP_DAC_OVERRIDE / CAP_DAC_READ_SEARCH: the container
 * runs as root, and root reads chmod-000 folders anyway; a real user's daemon can't.
 */
export function nuggetdWrapper() {
  const bin = process.env.NUGGETD || path.join(REPO, "target/debug/nuggetd");
  if (!fs.existsSync(bin)) throw new Error(`no nuggetd at ${bin}; set NUGGETD`);
  const w = path.join(SCRATCH, "nuggetd-nocap.sh");
  fs.writeFileSync(
    w,
    `#!/bin/sh\nexec setpriv --inh-caps=-dac_override,-dac_read_search --bounding-set=-dac_override,-dac_read_search ${JSON.stringify(bin)} "$@"\n`,
    { mode: 0o755 },
  );
  return w;
}

const T0 = Date.now();
export const log = (...a) => console.log(`[a1 ${((Date.now() - T0) / 1000).toFixed(1)}s]`, ...a);

/** Results: every scenario check, `{ scenario, name, ok, detail }`. */
export const results = [];
export function record(scenario, name, ok, detail = "") {
  results.push({ scenario, name, ok: !!ok, detail });
  log(`${ok ? "PASS" : "FAIL"} [${scenario}] ${name}${detail ? ` :: ${typeof detail === "string" ? detail : JSON.stringify(detail)}` : ""}`);
}

let shotBudget = Number(process.env.A1_MAX_SHOTS || 40);
/** Screenshot into qa/GG6/a1-onboarding/shots (small viewport, at most A1_MAX_SHOTS per run). */
export async function snap(page, name) {
  if (shotBudget <= 0) return null;
  shotBudget--;
  await page
    .evaluate(() => Promise.race([Promise.all(document.getAnimations().filter((a) => a.effect?.getComputedTiming().iterations !== Infinity).map((a) => a.finished.catch(() => {}))), new Promise((r) => setTimeout(r, 1500))]))
    .catch(() => {});
  const file = path.join(SHOTS, `${name}.png`);
  await page.screenshot({ path: file, fullPage: false }).catch((e) => log(`shot ${name} failed: ${e.message}`));
  return path.relative(REPO, file);
}

/** A daemon on an empty home + the bridge + one browser context. `pick` is what the folder picker returns. */
export async function boot(browser, { name, noCap = true } = {}) {
  const bin = noCap ? nuggetdWrapper() : process.env.NUGGETD;
  const t0 = Date.now();
  const daemon = await startDaemon(bin, { stderrPath: path.join(SCRATCH, `${name}-nuggetd.out.log`), config: FIRST_RUN_CONFIG });
  const daemonMs = Date.now() - t0;
  const picker = { next: [] };
  const bridge = await startBridge({ socket: daemon.socket, pickFolders: () => picker.next });
  const ctx = await browser.newContext({ viewport: { width: 1100, height: 760 }, deviceScaleFactor: 1, locale: "en-US", timezoneId: "UTC", colorScheme: "light" });
  ctx.setDefaultTimeout(Number(process.env.E2E_STEP_TIMEOUT_MS || 15000));
  await ctx.addInitScript({ path: path.join(E2E, "tauri-shim.js") });
  const errors = [];
  const open = async () => {
    const page = await ctx.newPage();
    page.on("pageerror", (e) => errors.push(e.message));
    await page.goto(`${bridge.url}index.html`);
    return page;
  };
  const close = async () => {
    await ctx.close().catch(() => {});
    await bridge.close().catch(() => {});
    await daemon.stop({ keep: process.env.E2E_KEEP === "1" });
  };
  return { daemon, bridge, ctx, open, close, picker, errors, daemonMs };
}

const esc = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
export const exact = (key) => new RegExp(`^${esc(t(key))}$`);
export const starts = (key) => new RegExp(`^${esc(t(key))}`);

export const h1 = (page) => page.locator("h1#ob-heading");
export async function heading(page) {
  return (await h1(page).textContent({ timeout: 5000 }).catch(() => null))?.trim() ?? null;
}
export const footerBtn = (page, key) => page.locator("footer.ob-actions").getByRole("button", { name: exact(key) });
export const cont = (page) => footerBtn(page, "onboarding.continue").click();
export const skip = (page) => footerBtn(page, "onboarding.skipStep").click();
export const back = (page) => footerBtn(page, "onboarding.back").click();
export const skipAll = (page) => page.getByRole("button", { name: exact("onboarding.skipAll") }).click();
export async function waitHeading(page, key) {
  await page.getByRole("heading", { level: 1, name: t(key), exact: true }).waitFor();
}
export const saved = (page) => page.evaluate(() => localStorage.getItem("nugget.onboarding.v1"));

/** Visible error notes anywhere (`.note-error`, role=alert, field errors). */
export function visibleErrors(page) {
  return page.evaluate(() =>
    [...document.querySelectorAll(".note-error, [role=alert], .field-error")]
      .filter((el) => el.getClientRects().length > 0 && el.textContent.trim())
      .map((el) => el.textContent.trim().replace(/\s+/g, " ")),
  );
}

/** Rows of the source step's live progress list. */
export function progressRows(page) {
  return page.locator(".ob-index li").evaluateAll((lis) => lis.map((li) => ({ phase: li.dataset.phase, text: li.textContent.trim() })));
}

export function writeJson(file, v) {
  fs.writeFileSync(path.join(HERE, file), JSON.stringify(v, null, 1) + "\n");
}
