// Lighthouse (mobile and desktop) for every page of the built site, using the pre-installed Chromium.
// Usage: npm run build && npm run lighthouse   (fails if any category scores under 95)
import { createRequire } from "node:module";
import lighthouse from "lighthouse";
import desktopConfig from "lighthouse/core/config/desktop-config.js";
import { serve, ROUTES } from "./serve.mjs";

const { chromium } = createRequire(import.meta.url)("playwright");
const MIN = 95;
const PORT = 9333;
const routes = ROUTES.filter((r) => r !== "/no-such-page");
const { server, base } = await serve(4176);
const browser = await chromium.launch({ args: [`--remote-debugging-port=${PORT}`] });

let low = 0;
console.log("route".padEnd(16), "form".padEnd(8), "perf a11y best  seo");
for (const r of routes) {
  for (const [form, config] of [
    ["mobile", undefined],
    ["desktop", desktopConfig],
  ]) {
    const result = await lighthouse(
      base + r,
      { port: PORT, output: "json", logLevel: "error", onlyCategories: ["performance", "accessibility", "best-practices", "seo"] },
      config,
    );
    const c = result.lhr.categories;
    const scores = ["performance", "accessibility", "best-practices", "seo"].map((k) => Math.round((c[k]?.score ?? 0) * 100));
    // Private pages are noindex on purpose, which Lighthouse's SEO audit marks down.
    const noindex = /^\/(account|checkout)$/.test(r);
    scores.forEach((s, i) => {
      if (s < MIN && !(i === 3 && noindex)) low++;
    });
    const fails = Object.values(result.lhr.audits)
      .filter((a) => a.score !== null && a.score < 0.9 && a.scoreDisplayMode === "binary")
      .map((a) => a.id);
    console.log(r.padEnd(16), form.padEnd(8), scores.map((s) => String(s).padStart(4)).join(" "), fails.length ? ` ${fails.join(",")}` : "");
  }
}
await browser.close();
server.close();
console.log(low ? `\n${low} score(s) under ${MIN}` : `\nall scores ≥ ${MIN}`);
process.exit(low ? 1 : 0);
