// axe-core on every route, in light and dark, at desktop and phone widths. Fails on any violation.
import { createRequire } from "node:module";
const require = createRequire(import.meta.url);
const AXE = require.resolve("axe-core/axe.min.js");

/** Runs axe on the current page; returns violations as short strings. */
export async function axe(page) {
  await page.addScriptTag({ path: AXE });
  const res = await page.evaluate(async () => {
    // @ts-expect-error injected
    const r = await window.axe.run(document, { resultTypes: ["violations"] });
    return r.violations.map((v) => `${v.id} (${v.impact}): ${v.nodes.slice(0, 2).map((n) => n.target.join(" ")).join(", ")}`);
  });
  return res;
}
