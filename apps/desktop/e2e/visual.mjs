// Visual regression check (e2e scenario "visual"): screenshots of the main screens of the real app
// on the indexed home, in light and dark, at 1280×800 in English, compared pixel by pixel with the
// committed baselines in e2e/visual-baselines/<screen>-<theme>.png. Linux only: font rendering
// differs between operating systems. See "What happens: visual" in e2e/README.md.
//
// Determinism: the browser clock is fixed (FIXED_TIME), motion is reduced and animations are
// disabled, the caret is hidden, fonts are pinned to DejaVu Sans / DejaVu Sans Mono (FONT_CSS),
// Today's cards and brief (which the daemon computes from the wall-clock date) are fixed stubs,
// the Models page sees a fixed machine, and what still changes between runs (relative times,
// temp paths, latencies, uptime) is masked (`markDynamic`).
//
// Diffing needs no dependency: both PNGs are decoded by Chromium itself (an <img> drawn on a
// <canvas>, read back with getImageData) in a blank page, and the diff image is encoded there too.
import fs from "node:fs";
import path from "node:path";
import { startBridge } from "./bridge.mjs";
import { check, CheckError, DIST, expect, here, log, OUT, settle, sinceStart, sleep, STEP_TIMEOUT_MS, until } from "./harness.mjs";

export const BASELINES = path.join(here, "visual-baselines");
/** Rewrite the baselines instead of comparing (E2E_VISUAL_UPDATE=1). */
const UPDATE = process.env.E2E_VISUAL_UPDATE === "1";
/** Largest fraction of differing pixels a screen may have (E2E_VISUAL_TOLERANCE, default 0.5%). */
const TOLERANCE = Number(process.env.E2E_VISUAL_TOLERANCE || 0.005);
/** A pixel differs when any channel (RGBA, 0–255) differs by more than this (about 10%). */
const PIXEL_THRESHOLD = 24;
const THEMES = ["light", "dark"];
const VIEWPORT = { width: 1280, height: 800 };
/** The browser's clock during the shots: relative times of corpus dates come out the same. */
const FIXED_TIME = new Date("2026-10-30T09:00:00Z");
/** Masked regions are painted with this colour. */
const MASK_COLOR = "#ff00ff";

/** Pins the fonts so the shots don't depend on what system-ui resolves to on this machine. */
const FONT_CSS = `
  :root { --font: "DejaVu Sans", sans-serif !important; --mono: "DejaVu Sans Mono", monospace !important; }
  html, body, .overlay, dialog { font-family: var(--font) !important; }
`;

/**
 * Runs in the page: marks what changes between runs with `data-visual-mask` (masked in the shot,
 * along with every `<time>`). Text-based so it covers every page without knowing its markup:
 * relative times, temp-home paths, latencies, durations (uptime).
 */
function markDynamic() {
  const DYNAMIC = [
    /\bago\b|\bjust now\b|^in \d|\bin \d+ (second|minute|hour|day)/i, // relative times
    /\/tmp\/|nugget-e2e-|\/var\/folders\//, // the temp home (vault, socket, logs)
    /\b\d+(\.\d+)?\s?(ms|µs)\b/, // latencies (freshness p50/p95)
    /^\s*(\d+d\s+)?(\d+h\s+)?(\d+m\s+)?\d+s\s*$/, // uptime ("1m 7s")
    /\b[a-z]+_[a-z0-9]{12,}\b/, // ids derived from hashes (ent_…)
  ];
  const marked = new Set();
  const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  for (let n = walker.nextNode(); n; n = walker.nextNode()) {
    const text = n.textContent ?? "";
    const el = n.parentElement;
    if (!el || !text.trim() || !DYNAMIC.some((re) => re.test(text) || re.test(el.textContent ?? ""))) continue;
    // The element holding the text (a <dd>, a <span>), not a whole card.
    if ((el.textContent ?? "").length > 160) continue;
    el.setAttribute("data-visual-mask", "");
    marked.add(el);
  }
  return marked.size + document.querySelectorAll("time").length;
}

// ---- fixed answers for what depends on the wall clock or the machine ----

const STUB_CARD = {
  id: "card_visual_stub",
  kind: "awaiting_reply",
  title: "Reply to Margaret Thistlewood: Moorland sync",
  detail: "Margaret Thistlewood wrote on Saturday and hasn't heard back.",
  summary: null,
  people: ["margaret.thistlewood@cindergate.test"],
  due: null,
  score: 0.8,
  rank: 1,
  factors: [
    { name: "importance", value: 1, weight: 1, contribution: 1 },
    { name: "urgency", value: 0.8, weight: 1, contribution: 0.8 },
  ],
  evidence: [{ item: { source: "files-1", key: "0:mail/mail-036.eml" }, quote: "Robin,", url: null }],
  actions: [
    { index: 0, kind: "draft_reply", label: "Draft a reply to margaret.thistlewood@cindergate.test", risk: "edit", supported: true },
    { index: 1, kind: "archive_thread", label: 'Archive "Moorland sync"', risk: "edit", supported: false },
  ],
  state: "new",
  sensitive: false,
  created_at: "2026-10-24T08:15:00Z",
};

const STUB_BRIEF = {
  date: "2026-10-30",
  generated_at: "2026-10-30T07:00:00Z",
  top: [
    {
      card_id: STUB_CARD.id,
      kind: STUB_CARD.kind,
      title: STUB_CARD.title,
      summary: STUB_CARD.detail,
      due: null,
      rank: 1,
      factors: STUB_CARD.factors,
      actions: ["draft_reply"],
    },
  ],
  meetings: [
    {
      title: "Driftwood API design review",
      start: "2026-10-30T11:00:00Z",
      end: "2026-10-30T12:00:00Z",
      all_day: false,
      location: null,
      attendees: [],
      external: [],
      prep_card: null,
      prep: [],
    },
  ],
  overdue: [],
  renewals: [],
  open_cards: 1,
};

/** The machine the Models page sees (models.fit): 16 GB, no GPU, plenty of disk. */
const MACHINE_RAM_GB = 16;
function pinMachine(fit) {
  return {
    ...fit,
    hardware: { total_ram: MACHINE_RAM_GB * 2 ** 30, available_ram: 12 * 2 ** 30, cpu_cores: 4, gpu: "unknown" },
    ram_gb: MACHINE_RAM_GB,
    free_disk_bytes: 200e9,
    models: fit.models.map((m) => ({
      ...m,
      machine_ram_gb: MACHINE_RAM_GB,
      gpu: "unknown",
      accel: "cpu",
      fits_disk: true,
      reasons: m.reasons.map((r) => (r === "gpu_accelerated" ? "cpu_only_slower" : r)),
    })),
  };
}

/**
 * Timestamps the daemon made during this run (indexing, extraction, sync: "now" when the test ran)
 * become a fixed time just before FIXED_TIME, and the temp home's path a fixed one, so "Synced 5
 * minutes ago", fact recency groups and paths come out the same on every run. Corpus dates are
 * weeks away from the run's window and stay as they are.
 */
function pinRuntime(value, home) {
  const from = Date.now() - sinceStart() - 5 * 60_000;
  const to = Date.now() + 5 * 60_000;
  const pinned = new Date(FIXED_TIME.getTime() - 5 * 60_000).toISOString();
  const walk = (v) => {
    if (typeof v === "string") {
      if (/^\d{4}-\d\d-\d\dT\d\d:\d\d/.test(v)) {
        const ms = Date.parse(v);
        if (ms >= from && ms <= to) return pinned;
      }
      return home && v.includes(home) ? v.split(home).join("/home/robin/.nugget") : v;
    }
    if (Array.isArray(v)) return v.map(walk);
    if (v && typeof v === "object") return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, walk(x)]));
    return v;
  };
  return walk(value);
}

/**
 * Answers `cards_list`/`brief_get` with the stubs (and `proactive_metrics`/`proactive_learning` with a fresh home's), pins `models_fit` to a fixed machine and every
 * other answer's runtime timestamps and paths (`pinRuntime`).
 */
async function routeFixtures(ctx, seen, home) {
  await ctx.route("**/invoke", async (route) => {
    const req = route.request();
    let cmd = null;
    try {
      cmd = JSON.parse(req.postData() || "{}").cmd;
    } catch {}
    const json = (ok) => route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ ok }) });
    if (cmd === "cards_list") return json({ cards: [STUB_CARD], last_refresh: "2026-10-30T07:00:00Z", enabled: true });
    if (cmd === "brief_get") return json(STUB_BRIEF);
    // Other scenarios share the indexed home and leave card feedback behind: pin a fresh home's.
    if (cmd === "proactive_metrics")
      return json({ total: 1, by_state: { new: 1 }, shown: 0, accepted: 0, dismissed: 0, snoozed: 0, precision: null, by_kind: {}, dismiss_reasons: {}, model_updates: 0 });
    if (cmd === "proactive_learning") return json({ entries: [] });
    if (cmd === "models_fit") {
      const res = await route.fetch();
      const body = await res.json();
      if (body.ok) {
        seen.ram_gb = body.ok.ram_gb;
        body.ok = pinMachine(body.ok);
      }
      return route.fulfill({ response: res, json: pinRuntime(body, home) });
    }
    const res = await route.fetch();
    let body;
    try {
      body = await res.json();
    } catch {
      return route.fulfill({ response: res });
    }
    return route.fulfill({ response: res, json: pinRuntime(body, home) });
  });
}

// ---- the screens ----

/**
 * `[name, { page, onboarded, setup }]`: every page of the PAGES registry, plus Search with
 * results, Knowledge with Ada Wexford open and onboarding's Welcome step. Not the knowledge graph:
 * its force layout is not deterministic.
 */
function screens(pages) {
  const list = pages.map(({ id }) => [id, { page: id }]);
  list.push(
    [
      "search-results",
      {
        page: "search",
        setup: async (p) => {
          const box = p.locator("main input[type=search]").first();
          await box.fill("Tidewater budget");
          await box.press("Enter");
          await until("search results", async () => [(await p.locator("ol.results li.result").count()) >= 3, null]);
          await box.blur();
        },
      },
    ],
    [
      "knowledge-entity",
      {
        page: "knowledge",
        setup: async (p) => {
          await p.locator("main").getByRole("button", { name: /^Ada Wexford\b/ }).first().click();
          await p.locator("section.entity-detail").getByRole("heading", { name: "Ada Wexford" }).first().waitFor();
          await p.locator("main").evaluate((m) => m.focus?.());
        },
      },
    ],
    ["onboarding-welcome", { page: null, onboarded: false }],
  );
  return list;
}

// ---- comparing ----

/**
 * Runs in a blank page: decodes both PNGs (base64) with the browser and compares them pixel by
 * pixel. A pixel differs when a channel differs by more than `threshold`. Returns the counts and,
 * when something differs, a diff PNG (base64): differing pixels red over a faded expected image.
 */
async function comparePngs({ actual, expected, threshold }) {
  const load = (b64) =>
    new Promise((resolve, reject) => {
      const img = new Image();
      img.onload = () => resolve(img);
      img.onerror = () => reject(new Error("could not decode a PNG"));
      img.src = `data:image/png;base64,${b64}`;
    });
  const [a, e] = await Promise.all([load(actual), load(expected)]);
  const size = (img) => [img.naturalWidth, img.naturalHeight];
  if (a.naturalWidth !== e.naturalWidth || a.naturalHeight !== e.naturalHeight) return { sizeMismatch: true, actual: size(a), expected: size(e) };
  const w = a.naturalWidth;
  const h = a.naturalHeight;
  const pixels = (img) => {
    const c = document.createElement("canvas");
    c.width = w;
    c.height = h;
    const x = c.getContext("2d", { willReadFrequently: true });
    x.drawImage(img, 0, 0);
    return x.getImageData(0, 0, w, h).data;
  };
  const pa = pixels(a);
  const pe = pixels(e);
  const out = document.createElement("canvas");
  out.width = w;
  out.height = h;
  const ox = out.getContext("2d");
  const diff = ox.createImageData(w, h);
  const d = diff.data;
  let differing = 0;
  for (let i = 0; i < pa.length; i += 4) {
    const delta = Math.max(Math.abs(pa[i] - pe[i]), Math.abs(pa[i + 1] - pe[i + 1]), Math.abs(pa[i + 2] - pe[i + 2]), Math.abs(pa[i + 3] - pe[i + 3]));
    if (delta > threshold) {
      differing++;
      d[i] = 255;
      d[i + 1] = 0;
      d[i + 2] = 0;
    } else {
      // The expected image, greyed and faded, for context.
      const lum = 0.299 * pe[i] + 0.587 * pe[i + 1] + 0.114 * pe[i + 2];
      d[i] = d[i + 1] = d[i + 2] = 255 - (255 - lum) * 0.2;
    }
    d[i + 3] = 255;
  }
  let png = null;
  if (differing > 0) {
    ox.putImageData(diff, 0, 0);
    png = out.toDataURL("image/png").split(",")[1];
  }
  return { sizeMismatch: false, width: w, height: h, differing, total: w * h, png };
}

/** A screenshot that is the same twice in a row (up to 5 tries), so a late repaint isn't baked in. */
async function stableShot(page) {
  const opts = {
    animations: "disabled",
    caret: "hide",
    scale: "css",
    mask: [page.locator("[data-visual-mask], time")],
    maskColor: MASK_COLOR,
  };
  let prev = await page.screenshot(opts);
  for (let i = 0; i < 4; i++) {
    await sleep(250);
    const next = await page.screenshot(opts);
    if (next.equals(prev)) return { png: next, stable: true };
    prev = next;
  }
  return { png: prev, stable: false };
}

// ---- the scenario ----

/**
 * Run on an already indexed daemon (`socket`) for `pages` (`[{ id, label }]`, the PAGES registry).
 * Returns `{ update, tolerance, pixel_threshold, screens, timings }` for summary.json.
 */
export async function visualScenario({ browser, socket, pages, onboarded }) {
  if (process.platform !== "linux") {
    log(`visual: skipped on ${process.platform} (Linux only: font rendering differs between systems)`);
    return { skipped: `platform ${process.platform}` };
  }
  const t0 = Date.now();
  fs.mkdirSync(BASELINES, { recursive: true });
  const bridge = await startBridge({ socket, dist: DIST });
  const diffCtx = await browser.newContext();
  const diffPage = await diffCtx.newPage();
  const results = [];
  const seen = { ram_gb: null };
  try {
    for (const theme of THEMES) {
      const ctx = await browser.newContext({
        viewport: VIEWPORT,
        deviceScaleFactor: 1,
        locale: "en-US",
        timezoneId: "UTC",
        colorScheme: theme,
        reducedMotion: "reduce",
      });
      ctx.setDefaultTimeout(STEP_TIMEOUT_MS);
      await ctx.addInitScript({ path: path.join(here, "tauri-shim.js") });
      // The socket is <home>/data/ipc/nuggetd.sock.
      await routeFixtures(ctx, seen, path.resolve(path.dirname(socket), "..", ".."));
      try {
        for (const [name, screen] of screens(pages)) {
          const file = `${name}-${theme}`;
          await check(`visual ${file}`, async () => {
            const page = await ctx.newPage();
            const errors = [];
            page.on("pageerror", (e) => errors.push(e.message));
            try {
              await page.clock.setFixedTime(FIXED_TIME);
              await page.addInitScript(
                ([id, ob, css]) => {
                  localStorage.setItem("nugget.locale", "en");
                  if (ob) localStorage.setItem("nugget.onboarding.v1", JSON.stringify(ob));
                  else localStorage.removeItem("nugget.onboarding.v1");
                  if (id) localStorage.setItem("nugget.page", id);
                  // Pin the fonts from the first paint (and again before the shot, below).
                  document.addEventListener("DOMContentLoaded", () => {
                    const s = document.createElement("style");
                    s.textContent = css;
                    document.head.append(s);
                  });
                },
                [screen.page, screen.onboarded === false ? null : onboarded, FONT_CSS],
              );
              await page.goto(`${bridge.url}index.html`);
              await page.locator(screen.page ? "main h1" : "h1").first().waitFor();
              await settle(page);
              if (screen.setup) {
                await screen.setup(page);
                await settle(page);
              }
              await page.addStyleTag({ content: FONT_CSS });
              await page.evaluate(() => document.fonts.ready);
              const masked = await page.evaluate(markDynamic);
              // Models: the recommendations follow the real machine's memory; mask them elsewhere.
              if (name === "models" && seen.ram_gb !== null && seen.ram_gb !== MACHINE_RAM_GB) {
                log(`  visual: this machine has ${seen.ram_gb} GB, not ${MACHINE_RAM_GB}: masking the model list`);
                await page.locator("main ul.cards").evaluate((el) => el.setAttribute("data-visual-mask", ""));
              }
              const { png, stable } = await stableShot(page);
              if (!stable) log(`  visual: ${file} still changed after 5 screenshots; comparing the last`);
              expect(errors.length === 0, `uncaught page errors: ${JSON.stringify(errors)}`);
              const row = { name, theme, masked, stable };
              results.push(row);
              await compareOrUpdate(diffPage, file, png, row);
            } finally {
              await page.close();
            }
          });
        }
      } finally {
        await ctx.close().catch(() => {});
      }
    }
  } finally {
    await diffCtx.close().catch(() => {});
    await bridge.close().catch(() => {});
  }
  const ms = Date.now() - t0;
  const worst = results.reduce((m, r) => Math.max(m, r.ratio ?? 0), 0);
  log(
    `visual: ${results.length} screens in ${(ms / 1000).toFixed(1)}s, ` +
      (UPDATE ? `baselines written to ${path.relative(process.cwd(), BASELINES)}` : `worst diff ${(worst * 100).toFixed(3)}% (tolerance ${(TOLERANCE * 100).toFixed(2)}%)`),
  );
  return { update: UPDATE, tolerance: TOLERANCE, pixel_threshold: PIXEL_THRESHOLD, screens: results, timings: { browser_ms: ms } };
}

/** Writes the baseline (E2E_VISUAL_UPDATE=1) or compares with it, recording the ratio in `row`. */
async function compareOrUpdate(diffPage, file, png, row) {
  const baseline = path.join(BASELINES, `${file}.png`);
  const artifact = (kind) => path.join(OUT, `visual-${file}-${kind}.png`);
  if (UPDATE) {
    fs.writeFileSync(baseline, png);
    row.updated = true;
    row.bytes = png.length;
    return;
  }
  if (!fs.existsSync(baseline)) {
    fs.writeFileSync(artifact("actual"), png);
    throw new CheckError(
      `no baseline ${path.relative(process.cwd(), baseline)}; create it on Linux with E2E_VISUAL_UPDATE=1 E2E_SCENARIOS=visual pnpm run e2e ` +
        `(or copy ${path.basename(artifact("actual"))} from the artifacts)`,
    );
  }
  const expected = fs.readFileSync(baseline);
  if (expected.equals(png)) {
    row.ratio = 0;
    row.differing = 0;
    return;
  }
  const r = await diffPage.evaluate(comparePngs, { actual: png.toString("base64"), expected: expected.toString("base64"), threshold: PIXEL_THRESHOLD });
  if (r.sizeMismatch) {
    fs.writeFileSync(artifact("actual"), png);
    fs.copyFileSync(baseline, artifact("expected"));
    row.ratio = 1;
    throw new CheckError(`size ${r.actual.join("×")} differs from the baseline's ${r.expected.join("×")}`);
  }
  row.differing = r.differing;
  row.ratio = r.differing / r.total;
  if (row.ratio > TOLERANCE) {
    fs.writeFileSync(artifact("actual"), png);
    fs.copyFileSync(baseline, artifact("expected"));
    if (r.png) fs.writeFileSync(artifact("diff"), Buffer.from(r.png, "base64"));
    throw new CheckError(
      `${r.differing} of ${r.total} pixels differ (${(row.ratio * 100).toFixed(3)}% > ${(TOLERANCE * 100).toFixed(2)}%); ` +
        `see visual-${file}-diff.png. If the change is intended, refresh the baselines (E2E_VISUAL_UPDATE=1, on Linux)`,
    );
  }
}
