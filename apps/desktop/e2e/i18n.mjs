// Translation layout check (e2e scenario "i18n"): every page of the PAGES registry is opened in a
// few locales that stress the layout (the en-XA pseudo-locale, about 35% longer than English;
// German, long words; Japanese, no spaces to wrap at; Arabic, right to left) on the indexed home
// the "pages" scenario uses. Each page is screenshotted and its labelled elements are measured
// for text that is cut off or escapes its box. See e2e/README.md.
import fs from "node:fs";
import path from "node:path";
import { startBridge } from "./bridge.mjs";
import { check, DIST, expect, log, newContext, OUT, settle, sleep, until } from "./harness.mjs";

/** Locales to check (E2E_I18N_LOCALES, comma-separated). */
export const I18N_LOCALES = (process.env.E2E_I18N_LOCALES || "en-XA,de,ja,ar").split(",").map((s) => s.trim()).filter(Boolean);

/** Elements whose text is a label: what a translation must fit into. */
const LABELLED = [
  "button",
  "a",
  "[role=button]",
  "[role=tab]",
  "[role=menuitem]",
  "[role=option]",
  ".nav-item",
  "h1",
  "h2",
  "h3",
  "h4",
  "h5",
  "h6",
  "label",
  "legend",
  "summary",
  "th",
  "select",
  "dt",
  "[class*=badge]",
  "[class*=chip]",
  "[class*=pill]",
  "[aria-label]",
].join(", ");

/**
 * Where overflow is intended, with the reason. An element inside one of these is not measured.
 */
const ALLOW = [
  // The knowledge graph draws its own labels on a canvas.
  "canvas",
  // Code, paths and patterns are shown verbatim; their blocks scroll. (`kbd` is measured: key
  // names like "Ctrl" are translated.)
  "pre",
  "code",
  // Chat history titles are the user's own questions, cut with an ellipsis (full text on hover).
  ".chat-history-open",
  // Command palette labels are cut with an ellipsis by design.
  ".palette-label",
  // Screen-reader-only text is clipped to 1px on purpose.
  ".visually-hidden",
  // Form fields: their value and placeholder can't be measured as labels.
  "input",
  "textarea",
];

/**
 * Runs in the page. Returns the overflowing elements:
 * - "clipped": the element (or a descendant holding its text) clips (overflow hidden/clip, which
 *   `text-overflow: ellipsis` needs) and its content is larger than its box, by more than 1px;
 * - "escapes": a `white-space: nowrap` element that is wider than its nearest block container;
 * - "spills": the label, a box inside it, or its nearest flex/grid/block ancestor doesn't clip
 *   and its content is wider than it (a hint pushed out of its button, buttons past their card).
 * Scroll containers (overflow auto/scroll) are never offenders: scrolling is a choice.
 */
export function measure({ labelled, allow }) {
  const allowSel = allow.join(", ");
  const out = [];
  const seen = new Set();
  const visible = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width < 2 || r.height < 2) return false;
    const s = getComputedStyle(el);
    return s.visibility !== "hidden" && s.display !== "none" && Number(s.opacity) > 0;
  };
  const pathOf = (el) => {
    const parts = [];
    for (let e = el; e && e !== document.body && parts.length < 4; e = e.parentElement) {
      let p = e.tagName.toLowerCase();
      if (e.id) p += `#${e.id}`;
      else if (typeof e.className === "string" && e.className.trim()) p += `.${e.className.trim().split(/\s+/).slice(0, 2).join(".")}`;
      parts.unshift(p);
    }
    return parts.join(" > ");
  };
  const textOf = (el) => (el.innerText || el.getAttribute("aria-label") || "").trim().replace(/\s+/g, " ").slice(0, 80);
  const report = (kind, el, extra = {}) => {
    if (seen.has(el)) return;
    seen.add(el);
    out.push({
      kind,
      path: pathOf(el),
      text: textOf(el),
      scrollWidth: el.scrollWidth,
      clientWidth: el.clientWidth,
      scrollHeight: el.scrollHeight,
      clientHeight: el.clientHeight,
      ...extra,
    });
  };
  const clips = (v) => v === "hidden" || v === "clip";
  const isBlock = (s) => !/^inline$|^contents$/.test(s.display);
  // Containers that are not labels themselves (an aria-label on a landmark names a region).
  const container = (el) =>
    /^(MAIN|NAV|SECTION|ASIDE|FORM|UL|OL|DL|TABLE|TBODY|CANVAS|DIALOG|INPUT|TEXTAREA|IMG|SVG|svg)$/.test(el.tagName) ||
    /^(region|group|list|dialog|tabpanel|navigation|toolbar|tablist|log|status|img|progressbar|meter|listbox)$/.test(el.getAttribute("role") ?? "");

  for (const el of document.querySelectorAll(labelled)) {
    if (el.closest(allowSel) || container(el) || !visible(el)) continue;
    if (!el.innerText?.trim() && el.tagName !== "SELECT") continue; // icon-only: nothing to translate
    // The label and the elements inside it that hold its text.
    const parts = [el, ...el.querySelectorAll("*")].filter((x) => !x.closest(allowSel) && visible(x));
    for (const x of parts) {
      const s = getComputedStyle(x);
      if (clips(s.overflowX) && x.scrollWidth > x.clientWidth + 1 && x.clientWidth > 0) report("clipped", x);
      else if (clips(s.overflowY) && x.scrollHeight > x.clientHeight + 1 && x.clientHeight > 0) report("clipped", x);
      else if (/nowrap|pre$/.test(s.whiteSpace) && !clips(s.overflowX)) {
        let block = x.parentElement;
        while (block && !isBlock(getComputedStyle(block))) block = block.parentElement;
        const w = Math.max(x.scrollWidth, x.getBoundingClientRect().width);
        if (block && !block.closest(allowSel) && w > block.clientWidth + 1 && block.clientWidth > 0) {
          report("escapes", x, { container: pathOf(block), containerWidth: block.clientWidth, width: Math.round(w) });
        }
      }
      if (s.overflowX === "visible" && isBlock(s) && s.display !== "inline-block" && x.clientWidth > 0 && x.scrollWidth > x.clientWidth + 1) {
        report("spills", x);
      }
    }
    // The nearest ancestor that neither clips nor scrolls but has content wider than itself.
    for (let a = el.parentElement; a && a !== document.body; a = a.parentElement) {
      const s = getComputedStyle(a);
      if (s.overflowX !== "visible") break; // clipped (measured above) or scrolls (intended)
      if (!isBlock(s) || a.closest(allowSel)) continue;
      if (a.scrollWidth > a.clientWidth + 1 && a.clientWidth > 0) {
        const r = el.getBoundingClientRect();
        const box = a.getBoundingClientRect();
        // Only when this label is the part that sticks out.
        if (r.right > box.right + 1 || r.left < box.left - 1) report("spills", a, { label: textOf(el) });
        break;
      }
    }
  }
  // The window itself must not scroll sideways.
  const root = document.scrollingElement;
  if (root.scrollWidth > root.clientWidth + 1) report("page-scrolls", root);
  return out;
}

/**
 * Run the scenario on an already indexed daemon (`socket`) for every page in `pages`
 * (`[{ id }]`, from the PAGES registry). Returns `{ locales, offenders, timings }` for summary.json.
 */
export async function i18nScenario({ browser, socket, pages, onboarded }) {
  const t0 = Date.now();
  const offenders = [];
  const bridge = await startBridge({ socket, dist: DIST });
  const ctx = await newContext(browser);
  try {
    await ctx.addInitScript(
      ([key, state]) => {
        localStorage.setItem(key, JSON.stringify(state));
      },
      ["nugget.onboarding.v1", onboarded],
    );
    for (const locale of I18N_LOCALES) {
      for (const { id } of pages) {
        await check(`i18n ${locale} ${id}: nothing cut off or overflowing`, async () => {
          const page = await ctx.newPage();
          const errors = [];
          page.on("pageerror", (e) => errors.push(e.message));
          await page.addInitScript(
            ([p, l]) => {
              localStorage.setItem("nugget.page", p);
              localStorage.setItem("nugget.locale", l);
            },
            [id, locale],
          );
          try {
            await page.goto(`${bridge.url}index.html`);
            await page.locator("main h1").first().waitFor();
            await until(`<html lang> is ${locale}`, async () => {
              const lang = await page.evaluate(() => document.documentElement.lang);
              return [lang === locale, lang];
            });
            await settle(page);
            await page.evaluate(() => document.fonts.ready);
            await sleep(300); // entrance transitions
            await page.screenshot({ path: path.join(OUT, `i18n-${locale}-${id}.png`) });
            if (locale === "ar") {
              const dir = await page.evaluate(() => document.documentElement.dir);
              expect(dir === "rtl", `<html dir> is "${dir}" in ar, expected "rtl"`);
            }
            const found = (await page.evaluate(measure, { labelled: LABELLED, allow: ALLOW })).map((o) => ({ locale, page: id, ...o }));
            offenders.push(...found);
            expect(errors.length === 0, `uncaught page errors: ${JSON.stringify(errors)}`);
            expect(
              found.length === 0,
              `${found.length} overflowing element(s): ${found.map((o) => `${o.kind} ${o.path} "${o.text}" (${o.scrollWidth}>${o.clientWidth})`).join("; ")}`,
            );
          } finally {
            await page.close();
          }
        });
      }
    }
  } finally {
    await ctx.close().catch(() => {});
    await bridge.close().catch(() => {});
  }
  const ms = Date.now() - t0;
  log(`i18n: ${I18N_LOCALES.length} locales x ${pages.length} pages in ${(ms / 1000).toFixed(1)}s, ${offenders.length} offender(s)`);
  fs.writeFileSync(path.join(OUT, "i18n-offenders.json"), JSON.stringify(offenders, null, 2));
  return { locales: I18N_LOCALES, offenders, timings: { browser_ms: ms } };
}
