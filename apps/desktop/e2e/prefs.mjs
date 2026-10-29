// Display preferences (e2e scenario "prefs"), on the indexed home the "pages" scenario uses:
// - reduced motion: with `reducedMotion: "reduce"`, no page runs an infinite or longer-than-instant
//   animation or transition, spinners stay visible but still, and the knowledge graph draws its
//   settled layout once and then stops (a control run without the preference shows the probes
//   would notice motion);
// - more contrast and forced colours: every page renders, and the stylesheet's overrides apply;
// - 200% zoom: half the CSS viewport at twice the pixel density, the same thing a browser zoom
//   does; no page scrolls sideways (Z2's overflow measurement from i18n.mjs, plus `main`, which is
//   the app's scroll container) and labels still fit.
// See e2e/README.md.
import fs from "node:fs";
import path from "node:path";
import { startBridge } from "./bridge.mjs";
import { check, DIST, expect, here, log, OUT, settle, sleep, STEP_TIMEOUT_MS, t, until } from "./harness.mjs";
import { ALLOW, LABELLED, measure } from "./i18n.mjs";

/** The harness's default window (newContext in harness.mjs). */
const VIEWPORT = { width: 1360, height: 860 };

/**
 * At 200% zoom: places where an offender other than a page scroll is accepted, with the reason
 * (on top of i18n.mjs's ALLOW).
 */
const ZOOM_ALLOW = [];

/** Longest animation or transition (seconds) that counts as "no motion": styles.css's 0.01ms. */
const INSTANT_S = 0.011;

/** A context like harness.mjs's newContext with `options` on top, onboarding already finished. */
async function prefsContext(browser, onboarded, options = {}) {
  const ctx = await browser.newContext({
    viewport: VIEWPORT,
    deviceScaleFactor: 1,
    locale: "en-US",
    timezoneId: "UTC",
    colorScheme: "light",
    ...options,
  });
  ctx.setDefaultTimeout(STEP_TIMEOUT_MS);
  await ctx.addInitScript({ path: path.join(here, "tauri-shim.js") });
  await ctx.addInitScript(
    ([key, state]) => {
      localStorage.setItem(key, JSON.stringify(state));
      // Counts animation frames, so a test can tell whether something keeps drawing.
      window.__rafCount = 0;
      const raf = window.requestAnimationFrame.bind(window);
      window.requestAnimationFrame = (cb) => {
        window.__rafCount++;
        return raf(cb);
      };
    },
    ["nugget.onboarding.v1", onboarded],
  );
  return ctx;
}

/** Open page `id` in `ctx` and wait for it to settle. Returns the page and its uncaught errors. */
async function openPage(ctx, bridge, id) {
  const page = await ctx.newPage();
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.addInitScript((p) => localStorage.setItem("nugget.page", p), id);
  await page.goto(`${bridge.url}index.html`);
  await page.locator("main h1").first().waitFor();
  await settle(page);
  await page.evaluate(() => document.fonts.ready);
  return { page, errors };
}

/**
 * Runs in the page. Everything that moves or would move: running animations that never end or
 * last longer than `instant` seconds, and visible elements (or their ::before/::after) whose
 * computed animation or transition is longer than that, and smooth scrolling.
 */
function motionOffenders(instant) {
  const out = [];
  const secs = (list) => Math.max(0, ...list.split(",").map((v) => (v.trim().endsWith("ms") ? parseFloat(v) / 1000 : parseFloat(v)) || 0));
  const pathOf = (el) => {
    const parts = [];
    for (let e = el; e && e !== document.body && parts.length < 4; e = e.parentElement) {
      let p = e.tagName.toLowerCase();
      if (typeof e.className === "string" && e.className.trim()) p += `.${e.className.trim().split(/\s+/).slice(0, 2).join(".")}`;
      parts.unshift(p);
    }
    return parts.join(" > ");
  };
  const visible = (el) => {
    const r = el.getBoundingClientRect();
    return r.width > 0 && r.height > 0 && getComputedStyle(el).visibility !== "hidden";
  };
  for (const a of document.getAnimations()) {
    const target = a.effect?.target;
    if (!target || a.playState !== "running" || !visible(target)) continue;
    const timing = a.effect.getComputedTiming();
    if (timing.iterations === Infinity || timing.activeDuration > instant * 1000) {
      out.push({ kind: "running", what: a.animationName ?? a.transitionProperty ?? a.constructor.name, path: pathOf(target), ms: timing.activeDuration });
    }
  }
  for (const el of document.querySelectorAll("body *")) {
    if (!visible(el)) continue;
    for (const pseudo of [null, "::before", "::after"]) {
      const s = getComputedStyle(el, pseudo);
      if (pseudo && (s.content === "none" || s.content === "normal")) continue;
      const where = pathOf(el) + (pseudo ?? "");
      if (s.animationName !== "none" && (s.animationIterationCount === "infinite" || secs(s.animationDuration) * Number(s.animationIterationCount) > instant)) {
        out.push({ kind: "animation", what: s.animationName, path: where, duration: s.animationDuration, iterations: s.animationIterationCount });
      }
      if (s.transitionProperty !== "none" && secs(s.transitionDuration) > instant) {
        out.push({ kind: "transition", what: s.transitionProperty, path: where, duration: s.transitionDuration });
      }
      if (!pseudo && s.scrollBehavior === "smooth") out.push({ kind: "smooth-scroll", what: "scroll-behavior", path: where });
    }
  }
  if (getComputedStyle(document.documentElement).scrollBehavior === "smooth") out.push({ kind: "smooth-scroll", what: "scroll-behavior", path: "html" });
  return out;
}

/**
 * Runs in the page. Puts one of each busy indicator into `main` (they only show while something
 * loads, which a settled page never does) and reports their computed style.
 */
function busyProbes() {
  const host = document.createElement("div");
  host.className = "pg-loading";
  host.innerHTML = '<span class="spinner"></span><div class="state-loading"><span class="state-spinner"></span><span class="state-skeleton"></span></div><span class="pg-skeleton"></span><span class="caret"></span>';
  document.querySelector("main").appendChild(host);
  const out = {};
  for (const el of host.querySelectorAll("span")) {
    const s = getComputedStyle(el);
    const r = el.getBoundingClientRect();
    out[el.className] = {
      animation: s.animationName,
      iterations: s.animationIterationCount,
      visible: r.width > 0 && r.height > 0 && s.visibility !== "hidden" && Number(s.opacity) > 0,
      border: s.borderTopStyle,
    };
  }
  host.remove();
  return out;
}

/** A hash of the graph canvas's pixels (every 7th pixel), to see whether it is redrawn differently. */
function canvasHash(canvas) {
  const ctx = canvas.getContext("2d");
  if (!ctx || !canvas.width || !canvas.height) return "empty";
  const d = ctx.getImageData(0, 0, canvas.width, canvas.height).data;
  let h = 0x811c9dc5;
  for (let i = 0; i < d.length; i += 4 * 7) {
    h ^= d[i] | (d[i + 1] << 8) | (d[i + 2] << 16);
    h = Math.imul(h, 0x01000193);
  }
  return (h >>> 0).toString(16);
}

/** Open Knowledge's Graph tab and wait for nodes on the canvas. Returns the canvas locator. */
async function openGraph(page) {
  await page.getByRole("tab", { name: t("knowledge.tab.graph") }).click();
  const canvas = page.locator("canvas.graph-canvas");
  await until("Graph tab draws nodes on its canvas", async () => {
    const label = (await canvas.getAttribute("aria-label", { timeout: 1000 })) ?? "";
    const colours = await canvas.evaluate((c) => {
      const ctx = c.getContext("2d");
      if (!ctx || !c.width || !c.height) return 0;
      const d = ctx.getImageData(0, 0, c.width, c.height).data;
      const seen = new Set();
      for (let i = 0; i < d.length && seen.size < 50; i += 4 * 97) seen.add(`${d[i]},${d[i + 1]},${d[i + 2]}`);
      return seen.size;
    });
    return [Number(label.match(/(\d+)\s+entit/)?.[1] ?? 0) > 0 && colours > 1, { label, colours }];
  });
  return canvas;
}

/**
 * Drag a node of the graph sideways with the mouse. A node is found by its fill: a canvas pixel in
 * exactly one of the entity kind colours (edges are translucent, labels in the text colour).
 */
async function dragNode(page, canvas) {
  const at = await canvas.evaluate((c) => {
    const css = getComputedStyle(c);
    const probe = document.createElement("span");
    document.body.appendChild(probe);
    const kinds = new Set();
    for (const k of ["person", "organization", "project", "commitment", "preference", "event", "place", "topic"]) {
      probe.style.color = css.getPropertyValue(`--kind-${k}`).trim();
      const m = getComputedStyle(probe).color.match(/\d+/g);
      if (m) kinds.add(`${m[0]},${m[1]},${m[2]}`);
    }
    probe.remove();
    const d = c.getContext("2d").getImageData(0, 0, c.width, c.height).data;
    // Middle rows first: a node away from the edges, so the drag stays on the canvas.
    for (let y = Math.floor(c.height / 3); y < c.height; y += 2) {
      for (let x = Math.floor(c.width / 3); x < c.width; x += 2) {
        const i = (y * c.width + x) * 4;
        if (kinds.has(`${d[i]},${d[i + 1]},${d[i + 2]}`)) {
          const r = c.getBoundingClientRect();
          return { x: r.left + (x * r.width) / c.width, y: r.top + (y * r.height) / c.height };
        }
      }
    }
    return null;
  });
  expect(at, "found no node on the graph canvas to drag");
  await page.mouse.move(at.x, at.y);
  await page.mouse.down();
  for (let i = 1; i <= 8; i++) await page.mouse.move(at.x + i * 10, at.y + i * 5);
  await page.mouse.up();
}

/** Frames requested and whether the canvas changed over `ms`. */
async function watchCanvas(page, canvas, ms) {
  const before = { frames: await page.evaluate(() => window.__rafCount), hash: await canvas.evaluate(canvasHash) };
  await sleep(ms);
  const after = { frames: await page.evaluate(() => window.__rafCount), hash: await canvas.evaluate(canvasHash) };
  return { frames: after.frames - before.frames, changed: before.hash !== after.hash };
}

/** Runs in the page: the computed values the contrast and forced-colours checks compare. */
function colourProbe() {
  const probe = document.createElement("span");
  probe.style.color = "Highlight";
  document.body.appendChild(probe);
  const highlight = getComputedStyle(probe).color;
  probe.remove();
  const sidebar = document.querySelector("aside.sidebar");
  const button = document.querySelector("main button.btn");
  const current = document.querySelector('aside.sidebar .nav-item[aria-current="page"]');
  const active = document.activeElement;
  const a = active && active !== document.body ? getComputedStyle(active) : null;
  return {
    forced: matchMedia("(forced-colors: active)").matches,
    more: matchMedia("(prefers-contrast: more)").matches,
    sidebarBorder: sidebar ? getComputedStyle(sidebar).borderInlineEndColor : null,
    muted: getComputedStyle(document.documentElement).getPropertyValue("--muted").trim(),
    currentOutline: current ? getComputedStyle(current).outlineStyle : null,
    buttonBorder: button ? `${getComputedStyle(button).borderTopStyle} ${getComputedStyle(button).borderTopWidth}` : null,
    highlight,
    focus: a ? { tag: active.tagName.toLowerCase(), style: a.outlineStyle, color: a.outlineColor, width: a.outlineWidth } : null,
  };
}

/**
 * Run the scenario on an already indexed daemon (`socket`) for every page in `pages`
 * (`[{ id }]`, from the PAGES registry). Returns a summary for summary.json.
 */
export async function prefsScenario({ browser, socket, pages, onboarded }) {
  const t0 = Date.now();
  const summary = { motion: {}, graph: {}, contrast: {}, zoom: { offenders: [] } };
  const bridge = await startBridge({ socket, dist: DIST });
  const contexts = [];
  const context = async (options) => {
    const ctx = await prefsContext(browser, onboarded, options);
    contexts.push(ctx);
    return ctx;
  };
  try {
    // ---- (a) reduced motion ----
    const still = await context({ reducedMotion: "reduce" });
    for (const { id } of pages) {
      await check(`prefs reduced motion ${id}: nothing animates, spinners stay still`, async () => {
        const { page, errors } = await openPage(still, bridge, id);
        try {
          await sleep(300);
          const moving = await page.evaluate(motionOffenders, INSTANT_S);
          summary.motion[id] = moving;
          expect(moving.length === 0, `${moving.length} moving element(s): ${moving.map((m) => `${m.kind} ${m.what} on ${m.path}`).join("; ")}`);
          const probes = await page.evaluate(busyProbes);
          for (const [name, p] of Object.entries(probes)) {
            expect(p.animation === "none", `.${name} still animates (${p.animation}) under reduced motion`);
            expect(p.visible, `.${name} disappears under reduced motion`);
          }
          expect(probes.spinner.border !== "none", "the spinner has no ring under reduced motion");
          expect(errors.length === 0, `uncaught page errors: ${JSON.stringify(errors)}`);
        } finally {
          await page.close();
        }
      });
    }

    await check("prefs reduced motion: the knowledge graph draws once and stops", async () => {
      const { page, errors } = await openPage(still, bridge, "knowledge");
      try {
        const start = await page.evaluate(() => window.__rafCount);
        const canvas = await openGraph(page);
        const toFirstDraw = (await page.evaluate(() => window.__rafCount)) - start;
        const idle = await watchCanvas(page, canvas, 600);
        await page.screenshot({ path: path.join(OUT, "prefs-graph-reduced-motion.png") });
        // Dragging a node moves that node only: once the mouse is up nothing moves on.
        await dragNode(page, canvas);
        const afterDrag = await watchCanvas(page, canvas, 600);
        summary.graph.reduced = { frames_to_first_draw: toFirstDraw, idle_frames: idle.frames, changed: idle.changed, after_drag: afterDrag };
        expect(!idle.changed, "the graph canvas kept changing 600 ms after its first draw");
        expect(idle.frames === 0, `${idle.frames} animation frame(s) requested in 600 ms after the graph was drawn`);
        expect(!afterDrag.changed && afterDrag.frames === 0, `the layout kept moving after a node was dragged: ${JSON.stringify(afterDrag)}`);
        // The layout is settled before the first paint: a handful of frames (paint, resize), not a loop.
        expect(toFirstDraw <= 5, `${toFirstDraw} animation frames before the first draw (the layout should not animate)`);
        expect(errors.length === 0, `uncaught page errors: ${JSON.stringify(errors)}`);
      } finally {
        await page.close();
      }
    });

    // The control: without the preference the same probes see motion, so they can.
    const moving = await context({ reducedMotion: "no-preference" });
    await check("prefs motion control: without reduced motion, spinners spin and the graph animates", async () => {
      const { page } = await openPage(moving, bridge, "knowledge");
      try {
        const probes = await page.evaluate(busyProbes);
        expect(probes.spinner.animation !== "none" && probes.spinner.iterations === "infinite", `the spinner doesn't spin: ${JSON.stringify(probes.spinner)}`);
        const canvas = await openGraph(page);
        await dragNode(page, canvas);
        const afterDrag = await watchCanvas(page, canvas, 600);
        summary.graph.control = { after_drag: afterDrag };
        // The same drag that leaves a reduced-motion layout still sets this one moving.
        expect(afterDrag.changed && afterDrag.frames > 0, `the layout didn't move on after a drag: ${JSON.stringify(afterDrag)}`);
      } finally {
        await page.close();
      }
    });

    // ---- (b) more contrast and forced colours ----
    const plain = await context({});
    const { page: basePage } = await openPage(plain, bridge, pages[0].id);
    const base = await basePage.evaluate(colourProbe);
    await basePage.close();
    summary.contrast.default = base;

    for (const [mode, options] of [
      ["contrast", { contrast: "more" }],
      ["forced", { forcedColors: "active" }],
    ]) {
      const ctx = await context(options);
      summary.contrast[mode] = {};
      for (const { id } of pages) {
        await check(`prefs ${mode === "contrast" ? "more contrast" : "forced colours"} ${id}: renders with the overrides`, async () => {
          const { page, errors } = await openPage(ctx, bridge, id);
          try {
            // Keyboard focus on a known control (a key press first, so :focus-visible applies).
            await page.keyboard.press("Shift");
            await page.locator("aside.sidebar .nav-item").first().focus();
            const seen = await page.evaluate(colourProbe);
            summary.contrast[mode][id] = seen;
            await page.screenshot({ path: path.join(OUT, `prefs-${mode}-${id}.png`) });
            expect(errors.length === 0, `uncaught page errors: ${JSON.stringify(errors)}`);
            if (mode === "contrast") {
              expect(seen.more, "prefers-contrast: more doesn't match");
              expect(seen.sidebarBorder !== base.sidebarBorder, `the sidebar border is unchanged (${seen.sidebarBorder})`);
              expect(seen.muted !== base.muted, `--muted is unchanged (${seen.muted})`);
              expect(seen.focus && seen.focus.style !== "none" && parseFloat(seen.focus.width) >= 3, `the focus ring isn't thicker: ${JSON.stringify(seen.focus)}`);
            } else {
              expect(seen.forced, "forced-colors: active doesn't match");
              expect(seen.focus && seen.focus.style !== "none" && seen.focus.color === seen.highlight, `the focus ring isn't Highlight (${seen.highlight}): ${JSON.stringify(seen.focus)}`);
              // Its background is dropped in forced colours; the stylesheet outlines it instead.
              expect(seen.currentOutline === "solid", `the current page's nav item has no outline (${seen.currentOutline})`);
              if (seen.buttonBorder) expect(/^solid [1-9]/.test(seen.buttonBorder), `buttons lose their border: ${seen.buttonBorder}`);
            }
          } finally {
            await page.close();
          }
        });
      }
    }

    // ---- (c) 200% zoom ----
    const zoomed = await context({
      viewport: { width: VIEWPORT.width / 2, height: VIEWPORT.height / 2 },
      deviceScaleFactor: 2,
      colorScheme: "light",
    });
    for (const { id } of pages) {
      await check(`prefs 200% zoom ${id}: no sideways scrolling, labels fit`, async () => {
        const { page, errors } = await openPage(zoomed, bridge, id);
        try {
          await sleep(300);
          await page.screenshot({ path: path.join(OUT, `zoom200-${id}.png`) });
          const found = await page.evaluate(measure, { labelled: LABELLED, allow: [...ALLOW, ...ZOOM_ALLOW] });
          // The app scrolls inside `main`, so the window never does; `main` must not scroll sideways either.
          const main = await page.evaluate(() => {
            const m = document.querySelector("main");
            return { scrollWidth: m.scrollWidth, clientWidth: m.clientWidth };
          });
          if (main.scrollWidth > main.clientWidth + 1) found.push({ kind: "main-scrolls", path: "main", text: "", ...main });
          const offenders = found.map((o) => ({ page: id, ...o }));
          summary.zoom.offenders.push(...offenders);
          expect(errors.length === 0, `uncaught page errors: ${JSON.stringify(errors)}`);
          expect(
            offenders.length === 0,
            `${offenders.length} offender(s): ${offenders.map((o) => `${o.kind} ${o.path} "${o.text}" (${o.scrollWidth}>${o.clientWidth})`).join("; ")}`,
          );
        } finally {
          await page.close();
        }
      });
    }
  } finally {
    for (const ctx of contexts) await ctx.close().catch(() => {});
    await bridge.close().catch(() => {});
  }
  const ms = Date.now() - t0;
  fs.writeFileSync(path.join(OUT, "prefs-zoom-offenders.json"), JSON.stringify(summary.zoom.offenders, null, 2));
  log(`prefs: ${pages.length} pages x reduced motion, more contrast, forced colours, 200% zoom in ${(ms / 1000).toFixed(1)}s; ${summary.zoom.offenders.length} zoom offender(s)`);
  return { ...summary, timings: { browser_ms: ms } };
}
