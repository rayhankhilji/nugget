// Accessibility-tree checks shared by the a11y scenario: what a screen reader user meets on a page.
// Playwright's ARIA snapshot of the page is saved as `aria-<name>.yml` (review it in a diff), and
// the DOM is checked for:
// - landmarks: exactly one main; at least one navigation, each named, names unique per role;
//   complementary and explicit regions named;
// - headings inside main: exactly one h1, no skipped level going down (h1 then h3), dialogs aside;
// - names: every button, link and [role=button|link|tab|menuitem|switch|checkbox|option] that is
//   exposed has an accessible name; every open dialog is named; SVGs are hidden or named images;
// - live regions: polite (or status) only, assertive only as role=alert, and none holding content
//   that is still streaming (aria-busy or data-streaming): that would be read token by token.
import fs from "node:fs";
import path from "node:path";
import { OUT } from "./harness.mjs";

/** Check landmarks, headings and names on an open page; returns a list of problems (strings). */
export async function treeChecks(page, name) {
  const problems = [];
  const snapshot = await page.locator("body").ariaSnapshot();
  fs.mkdirSync(OUT, { recursive: true });
  fs.writeFileSync(path.join(OUT, `aria-${name.replace(/[^\w-]+/g, "-")}.yml`), snapshot);

  // The snapshot's own names (Playwright computes them per the accname spec): a control listed
  // without a quoted name has none. The DOM pass below says which element it is.
  const unnamed = snapshot
    .split("\n")
    .filter((line) => /^\s*- (button|link|tab|menuitem|switch|checkbox|option)(\s+\[[^\]]*\])*:?\s*$/.test(line))
    .map((line) => line.trim());

  const dom = await page.evaluate(inspect);
  problems.push(...dom);
  if (unnamed.length > 0 && !dom.some((p) => p.startsWith("no accessible name"))) {
    problems.push(`ARIA snapshot has controls without a name: ${unnamed.join(", ")} (see aria-${name}.yml)`);
  }
  return problems;
}

/** Runs in the page. */
function inspect() {
  const out = [];
  const hiddenStyle = (el) => {
    for (let e = el; e && e.nodeType === 1; e = e.parentElement) {
      if (e.hidden || e.getAttribute("aria-hidden") === "true" || e.inert) return true;
      const s = getComputedStyle(e);
      if (s.display === "none" || s.visibility === "hidden") return true;
    }
    return false;
  };
  const exposed = (el) => !hiddenStyle(el);
  const describe = (el) => {
    const id = el.id ? `#${el.id}` : "";
    const cls = typeof el.className === "string" && el.className.trim() ? `.${el.className.trim().split(/\s+/).slice(0, 2).join(".")}` : "";
    const text = (el.textContent || "").trim().replace(/\s+/g, " ").slice(0, 40);
    return `<${el.tagName.toLowerCase()}${id}${cls}>${text ? ` "${text}"` : ""}`;
  };

  // A pragmatic accessible-name computation: aria-labelledby, aria-label, label/alt/value, then
  // content (skipping hidden parts, using a child's own aria-label), then title.
  const textOf = (node) => {
    if (node.nodeType === 3) return node.textContent;
    if (node.nodeType !== 1) return "";
    const el = node;
    if (el.getAttribute("aria-hidden") === "true" || el.hidden) return "";
    const s = getComputedStyle(el);
    if (s.display === "none" || s.visibility === "hidden") return "";
    const label = el.getAttribute("aria-label");
    if (label && label.trim()) return label;
    if (el.tagName === "IMG") return el.getAttribute("alt") || "";
    return [...el.childNodes].map(textOf).join(" ");
  };
  const byIds = (el, attr) =>
    (el.getAttribute(attr) || "")
      .split(/\s+/)
      .filter(Boolean)
      .map((id) => document.getElementById(id))
      .filter(Boolean)
      .map((ref) => [...ref.childNodes].map(textOf).join(" ") || ref.getAttribute("aria-label") || "")
      .join(" ");
  const nameOf = (el) => {
    const parts = [
      byIds(el, "aria-labelledby"),
      el.getAttribute("aria-label") || "",
      el.labels ? [...el.labels].map((l) => textOf(l)).join(" ") : "",
      el.tagName === "INPUT" && /^(button|submit|reset)$/.test(el.type) ? el.value : "",
    ];
    for (const p of parts) if (p && p.trim()) return p.trim();
    const content = textOf(el).trim();
    if (content) return content;
    return (el.getAttribute("title") || "").trim();
  };
  /** A landmark's or dialog's name: only from aria-labelledby or aria-label, never its content. */
  const labelOf = (el) => (byIds(el, "aria-labelledby") || el.getAttribute("aria-label") || "").trim();

  // ---- landmarks ----
  const mains = [...document.querySelectorAll("main, [role=main]")].filter(exposed);
  if (mains.length !== 1) out.push(`expected exactly one main landmark, found ${mains.length}`);
  const main = mains[0];

  const navs = [...document.querySelectorAll("nav, [role=navigation]")].filter(exposed);
  if (navs.length === 0) out.push("no navigation landmark");
  const navNames = navs.map(labelOf);
  navs.forEach((nav, i) => {
    if (!navNames[i]) out.push(`navigation landmark without a name: ${describe(nav)}`);
  });
  const dupes = navNames.filter((n, i) => n && navNames.indexOf(n) !== i);
  if (dupes.length) out.push(`navigation landmarks share a name: ${[...new Set(dupes)].join(", ")}`);

  // An <aside> is complementary unless it sits inside another sectioning element.
  const asides = [...document.querySelectorAll("aside, [role=complementary]")].filter(
    (el) => exposed(el) && (el.getAttribute("role") === "complementary" || !el.parentElement?.closest("article, aside, nav, section, main")),
  );
  for (const el of asides) if (!labelOf(el)) out.push(`complementary landmark without a name: ${describe(el)}`);
  for (const el of document.querySelectorAll("[role=region]")) {
    if (exposed(el) && !labelOf(el)) out.push(`region without a name: ${describe(el)}`);
  }

  // ---- headings inside main (a dialog's headings start their own outline) ----
  if (main) {
    const headings = [...main.querySelectorAll("h1, h2, h3, h4, h5, h6, [role=heading]")].filter(
      (h) => exposed(h) && !h.closest("dialog, [role=dialog], [role=alertdialog]"),
    );
    const level = (h) => (h.getAttribute("role") === "heading" ? Number(h.getAttribute("aria-level") || 2) : Number(h.tagName[1]));
    const h1s = headings.filter((h) => level(h) === 1);
    if (h1s.length !== 1) out.push(`expected exactly one h1 in main, found ${h1s.length}${h1s.length ? `: ${h1s.map(describe).join(", ")}` : ""}`);
    let prev = 0;
    for (const h of headings) {
      const l = level(h);
      if (l > prev + 1) out.push(`heading level skipped (h${prev || "-"} then h${l}): ${describe(h)}`);
      prev = l;
    }
  }

  // ---- names ----
  const controls = document.querySelectorAll(
    "button, a[href], [role=button], [role=link], [role=tab], [role=menuitem], [role=switch], [role=checkbox], [role=option]",
  );
  for (const el of controls) {
    if (!exposed(el)) continue;
    // Native checkboxes are named by their <label>; anything else needs a name of its own.
    if (!nameOf(el)) out.push(`no accessible name: ${describe(el)}`);
  }
  for (const d of document.querySelectorAll("dialog[open], [role=dialog], [role=alertdialog]")) {
    if (!exposed(d)) continue;
    if (!labelOf(d)) out.push(`dialog without a name: ${describe(d)}`);
    // A dialog's outline starts at its first heading (its title) and doesn't skip levels below it.
    let prev = 0;
    for (const h of d.querySelectorAll("h1, h2, h3, h4, h5, h6")) {
      if (!exposed(h)) continue;
      const l = Number(h.tagName[1]);
      if (prev && l > prev + 1) out.push(`heading level skipped in a dialog (h${prev} then h${l}): ${describe(h)}`);
      prev = l;
    }
  }
  for (const svg of document.querySelectorAll("svg")) {
    if (!exposed(svg)) continue;
    const named = svg.getAttribute("role") === "img" && (svg.getAttribute("aria-label") || svg.getAttribute("aria-labelledby") || svg.querySelector(":scope > title"));
    if (!named) out.push(`SVG neither hidden (aria-hidden) nor a named image: inside ${describe(svg.parentElement)}`);
  }

  // ---- live regions ----
  const LIVE_ROLES = new Set(["status", "log", "alert", "marquee"]);
  for (const el of document.querySelectorAll("[aria-live], [role]")) {
    const role = el.getAttribute("role") || "";
    const liveAttr = el.getAttribute("aria-live");
    const live = liveAttr ? liveAttr !== "off" : LIVE_ROLES.has(role);
    if (!live || !exposed(el)) continue;
    if (liveAttr === "assertive" && role !== "alert") out.push(`assertive live region that is not an alert: ${describe(el)}`);
    if (liveAttr && !["polite", "assertive", "off"].includes(liveAttr)) out.push(`unknown aria-live value "${liveAttr}": ${describe(el)}`);
    const busy = '[aria-busy="true"], [data-streaming]';
    if (el.matches(busy) || el.querySelector(busy)) out.push(`live region holds streaming content (announced piece by piece): ${describe(el)}`);
  }
  return out;
}
