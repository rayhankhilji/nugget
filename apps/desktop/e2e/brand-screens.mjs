#!/usr/bin/env node
// The curated brand screenshots in docs/brand/screens/ (for the founder and the website hero): the
// real desktop frontend (dist/) in headless Chromium at 1440x900 @1x, signed in to Heed Cloud, with
// every cloud command answered from a believable working day (e2e/fixtures/day.mjs). Local
// commands go to a real nuggetd through the e2e bridge, as for `pnpm run screenshots`.
//
//   cd apps/desktop
//   node e2e/brand-screens.mjs      # SHOTS_BUILD=0 to reuse dist/, SHOTS_ONLY=home-light,… for some
//
// Each image is a PNG, or a WebP when the PNG would be over 400 KB.
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import { createRequire } from "node:module";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { startBridge } from "./bridge.mjs";
import { REPO, locateNuggetd, startDaemon } from "./daemon.mjs";
import { NOW, TIMEZONE, QUICK_ASK, cloudFixture } from "./fixtures/day.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const DESKTOP = path.resolve(here, "..");
const DIST = path.join(DESKTOP, "dist");
const OUT = path.join(REPO, "docs", "brand", "screens");
const VIEWPORT = { width: 1440, height: 900 };
const BUDGET = 400 * 1024;
const ONLY = new Set((process.env.SHOTS_ONLY || "").split(",").map((s) => s.trim()).filter(Boolean));
const started = Date.now();
const log = (...a) => console.log(`[brand ${((Date.now() - started) / 1000).toFixed(1)}s]`, ...a);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const en = JSON.parse(fs.readFileSync(path.join(DESKTOP, "src", "i18n", "locales", "en.json"), "utf8"));
const ONBOARDED = { version: 1, step: "done", completed: [], skipped: [], ai: null, finished: true };

/** Wait for in-flight requests and entrance animations to finish. */
async function settle(p) {
  await p.waitForLoadState("networkidle").catch(() => {});
  await sleep(500);
}

const SHOTS = [
  { name: "home-light", theme: "light", page: "today", ready: (p) => p.locator(".brief-prose").first().waitFor() },
  { name: "home-dark", theme: "dark", page: "today", ready: (p) => p.locator(".brief-prose").first().waitFor() },
  {
    name: "needs-you",
    theme: "light",
    page: "needs",
    ready: (p) => p.locator(".action-queue .action-item").first().waitFor(),
  },
  {
    name: "search",
    theme: "light",
    page: "search",
    async ready(p) {
      const box = p.locator('input[type="search"]').first();
      await box.fill("harbourline retainer");
      await box.press("Enter");
      await p.locator(".result").first().waitFor();
      await box.blur();
    },
  },
  {
    name: "search-cmdk",
    theme: "light",
    page: "today",
    async ready(p) {
      await p.locator(".brief-prose").first().waitFor();
      await p.keyboard.press("Control+K");
      const box = p.getByRole("combobox");
      await box.fill("harbourline retainer");
      await p.locator(".palette-section-head", { hasText: "Mail" }).waitFor();
      await p.keyboard.press("ArrowDown");
      await p.keyboard.press("ArrowDown");
    },
  },
  { name: "connections", theme: "light", page: "connections", ready: (p) => p.getByText("Slack").first().waitFor() },
  { name: "account", theme: "light", page: "account", ready: (p) => p.locator(".cloud-subscription").waitFor() },
  { name: "welcome", theme: "light", page: "today", onboarding: true, signedOut: true, ready: (p) => p.locator(".ob-main h1").first().waitFor() },
  {
    name: "quickask-dark",
    theme: "dark",
    url: "quickask.html",
    viewport: { width: 680, height: 460 },
    async ready(p) {
      const box = p.getByRole("searchbox", { name: en["quickAsk.inputLabel"] });
      await box.fill(QUICK_ASK.question);
      await box.press("Enter");
      await sleep(300);
      const id = QUICK_ASK.started.stream_id;
      for (const word of QUICK_ASK.answer.match(/\S+\s*/g)) {
        await p.evaluate(([e, d]) => window.__E2E_EMIT__(e, d), ["nugget:chat-delta", { stream_id: id, text: word }]);
      }
      await p.evaluate(([e, d]) => window.__E2E_EMIT__(e, d), ["nugget:chat-done", { stream_id: id, error: null, input_tokens: 812, output_tokens: 96 }]);
      await p.locator(".quickask-answer:not([aria-busy=true])").first().waitFor();
    },
  },
  { name: "ghost-text", theme: "light", url: "__ghost.html", ready: (p) => p.locator(".ghost-chip").waitFor() },
];

/** A neutral mail-compose window (drawn here, not any real mail app) with Heed's ghost text in it. */
function ghostPage(css, sky) {
  const links = css.map((h) => `<link rel="stylesheet" href="${h}">`).join("");
  return `<!doctype html><html><head><meta charset="utf-8">${links}<style>
  body { margin: 0; height: 100vh; display: grid; place-items: center; background:
    linear-gradient(to bottom, rgb(246 243 236 / .35), var(--bg)), url("${sky}") center 20% / cover; font-family: system-ui, sans-serif; }
  .gm-win { width: 860px; border-radius: 14px; background: #fff; box-shadow: 0 30px 80px rgb(7 7 152 / .18), 0 0 0 1px rgb(20 17 15 / .08); overflow: hidden; color: #1d1b19; }
  .gm-bar { display: flex; align-items: center; gap: 8px; padding: 12px 16px; background: #f4f2ee; font-size: 13px; color: #5f5951; }
  .gm-bar i { width: 12px; height: 12px; border-radius: 50%; background: #d9d4cb; }
  .gm-bar b { margin-inline-start: 10px; font-weight: 600; color: #1d1b19; }
  .gm-row { display: flex; gap: 12px; padding: 11px 22px; border-bottom: 1px solid #ece9e3; font-size: 14px; }
  .gm-row span { width: 64px; color: #8a847c; }
  .gm-body { padding: 20px 22px 26px; font-size: 15px; line-height: 1.6; min-height: 300px; }
  .gm-body p { margin: 0 0 12px; }
  .caret { display: inline-block; width: 1.5px; height: 1.15em; margin-inline: 1px -1px; vertical-align: -0.2em; background: var(--heed-signal); }
  .ghost { display: inline; font: inherit; }
  .ghost-text { opacity: .45; }
  .gm-foot { display: flex; justify-content: space-between; align-items: center; padding: 14px 22px; border-top: 1px solid #ece9e3; }
  .gm-send { background: #1d1b19; color: #fff; border-radius: 8px; padding: 8px 18px; font-size: 14px; }
  </style></head><body><div class="gm-win">
  <div class="gm-bar"><i></i><i></i><i></i><b>Re: Halcyon launch timing</b></div>
  <div class="gm-row"><span>To</span>Priya Raman &lt;priya.raman@halcyonfoods.co.uk&gt;</div>
  <div class="gm-row"><span>Subject</span>Re: Halcyon launch timing</div>
  <div class="gm-body"><p>Hi Priya,</p><p>Thanks for checking. Yes, the 14th still holds on our side.<span class="caret"></span><span class="ghost"><span class="ghost-text"> Print proofs land on Thursday, and I'll send photos from the press check the moment they're in so retail can sign off before the weekend.</span><span class="ghost-chip"><i></i><kbd>Tab</kbd></span></span></p></div>
  <div class="gm-foot"><span class="gm-send">Send</span><span style="color:#8a847c;font-size:13px">Draft saved</span></div>
  </div></body></html>`;
}

async function main() {
  const { chromium } = createRequire(import.meta.url)("playwright");
  if (process.env.SHOTS_BUILD !== "0" || !fs.existsSync(path.join(DIST, "index.html"))) {
    log("building the frontend…");
    const r = spawnSync("pnpm", ["exec", "vite", "build", "--logLevel", "warn"], { cwd: DESKTOP, stdio: "inherit" });
    if (r.status !== 0) throw new Error("vite build failed");
  }
  const overlayCss = [...fs.readFileSync(path.join(DIST, "overlay.html"), "utf8").matchAll(/href="([^"]+\.css)"/g)].map((m) => m[1]);
  const sky = `/assets/${fs.readdirSync(path.join(DIST, "assets")).find((f) => /^sky-.*\.webp$/.test(f))}`;
  const shots = SHOTS.filter((s) => ONLY.size === 0 || ONLY.has(s.name));
  fs.mkdirSync(OUT, { recursive: true });

  const work = fs.mkdtempSync(path.join(os.tmpdir(), "heed-brand-"));
  const daemon = await startDaemon(locateNuggetd(log), { stderrPath: path.join(work, "nuggetd.log") });
  const bridge = await startBridge({ socket: daemon.socket, dist: DIST });
  const browser = await chromium.launch(process.env.E2E_CHROMIUM ? { executablePath: process.env.E2E_CHROMIUM } : {});
  try {
    for (const shot of shots) {
      const ctx = await browser.newContext({
        viewport: shot.viewport ?? VIEWPORT,
        deviceScaleFactor: 1,
        locale: "en-GB",
        timezoneId: TIMEZONE,
        colorScheme: shot.theme,
        reducedMotion: "reduce",
      });
      await ctx.addInitScript({ path: path.join(here, "tauri-shim.js") });
      const fixture = cloudFixture();
      if (shot.signedOut) fixture.cloud_session = () => ({ configured: true, signed_in: false, email: null });
      await ctx.route("**/invoke", async (route) => {
        let body = {};
        try {
          body = JSON.parse(route.request().postData() ?? "{}");
        } catch {}
        const answer = fixture[body.cmd];
        if (!answer) return route.continue();
        await route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ ok: answer(body.args?.request ?? body.args) ?? null }) });
      });
      await ctx.route("**/__ghost.html", (route) => route.fulfill({ status: 200, contentType: "text/html", body: ghostPage(overlayCss, sky) }));
      const page = await ctx.newPage();
      const errors = [];
      page.on("pageerror", (e) => errors.push(e.message));
      await page.clock.setFixedTime(new Date(NOW));
      await page.addInitScript(
        ([pageId, onboarded]) => {
          localStorage.setItem("nugget.page", pageId);
          if (onboarded) localStorage.setItem("nugget.onboarding.v1", JSON.stringify(onboarded));
          else localStorage.removeItem("nugget.onboarding.v1");
        },
        [shot.page ?? "today", shot.onboarding ? null : ONBOARDED],
      );
      try {
        await page.goto(`${bridge.url}${shot.url ?? "index.html"}`);
        await settle(page);
        await shot.ready(page);
        await settle(page);
        if (errors.length) throw new Error(`page errors: ${JSON.stringify(errors)}`);
        const png = path.join(OUT, `${shot.name}.png`);
        const webp = path.join(OUT, `${shot.name}.webp`);
        fs.rmSync(png, { force: true });
        fs.rmSync(webp, { force: true });
        await page.screenshot({ path: png, animations: "disabled", caret: "hide" });
        let file = png;
        if (fs.statSync(png).size > BUDGET) {
          const r = spawnSync("python3", ["-c", `from PIL import Image\nfor q in (90, 84, 78, 70):\n  Image.open("${png}").save("${webp}", "WEBP", quality=q, method=6)\n  import os\n  if os.path.getsize("${webp}") <= ${BUDGET}: break`]);
          if (r.status !== 0) throw new Error(`WebP conversion failed: ${r.stderr}`);
          fs.rmSync(png);
          file = webp;
        }
        log(`${path.basename(file)} (${(fs.statSync(file).size / 1024).toFixed(0)} KB)`);
      } catch (e) {
        await page.screenshot({ path: path.join(work, `FAIL-${shot.name}.png`) }).catch(() => {});
        throw new Error(`shot ${shot.name} failed: ${e.message} (see ${work})`);
      } finally {
        await ctx.close();
      }
    }
  } finally {
    await browser.close().catch(() => {});
    await bridge.close().catch(() => {});
    await daemon.stop();
  }
  fs.rmSync(work, { recursive: true, force: true });
}

main().catch((e) => {
  console.error(`[brand] failed: ${e.stack ?? e}`);
  process.exit(1);
});
