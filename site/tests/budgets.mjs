// Performance budgets, checked against two builds: the bare production build (dist-bare, from
// tests/bare.mjs; budgets.json) and the build with Heed Cloud configured (dist-test, from
// tests/smoke.mjs; budgets-cloud.json, where account and checkout load the Supabase client):
// - per route, the JavaScript a page loads (inline scripts + module scripts + everything they import)
//   must stay within tests/budgets.json, which is today's size plus 10%;
// - no image anywhere in the build may exceed 300 KB.
// `node tests/budgets.mjs --update` rewrites budgets.json from the current build (+10%).
import { readFileSync, readdirSync, statSync, writeFileSync, existsSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const cloud = process.argv.includes("--cloud");
const ROOT = fileURLToPath(new URL(cloud ? "../dist-test/" : "../dist-bare/", import.meta.url));
const BUDGETS = fileURLToPath(new URL(cloud ? "./budgets-cloud.json" : "./budgets.json", import.meta.url));
const IMAGE_MAX = 300 * 1024;
const PAGES = ["index", "pricing", "download", "privacy", "docs/self-host", "signin", "account", "checkout", "legal/privacy"];

if (!existsSync(ROOT)) {
  console.error(`${ROOT} is missing: run node tests/bare.mjs and tests/smoke.mjs first (npm test does)`);
  process.exit(2);
}

/** Bytes of a JS module plus its static and dynamic imports, each counted once. */
function moduleBytes(file, seen) {
  if (seen.has(file)) return 0;
  seen.add(file);
  const src = readFileSync(file, "utf8");
  let total = Buffer.byteLength(src);
  for (const m of src.matchAll(/(?:import|export)\s*(?:[^"'()]*?from\s*)?["'](\.{1,2}\/[^"']+\.js)["']|import\(\s*["'](\.{1,2}\/[^"']+\.js)["']\s*\)/g)) {
    const rel = m[1] ?? m[2];
    total += moduleBytes(join(dirname(file), rel), seen);
  }
  return total;
}

function pageJs(page) {
  const html = readFileSync(join(ROOT, `${page}.html`), "utf8");
  const seen = new Set();
  let total = 0;
  for (const m of html.matchAll(/<script\b([^>]*)>([\s\S]*?)<\/script>/g)) {
    const attrs = m[1];
    if (/type="application\/(ld\+)?json"/.test(attrs)) continue; // data, not code
    const src = /src="(\/_astro\/[^"]+\.js)"/.exec(attrs)?.[1];
    total += src ? moduleBytes(join(ROOT, src), seen) : Buffer.byteLength(m[2]);
  }
  return total;
}

const sizes = Object.fromEntries(PAGES.map((p) => [p, pageJs(p)]));

if (process.argv.includes("--update")) {
  writeFileSync(BUDGETS, `${JSON.stringify(Object.fromEntries(Object.entries(sizes).map(([k, v]) => [k, Math.ceil(v * 1.1)])), null, 2)}\n`);
  console.log("budgets.json updated:", sizes);
  process.exit(0);
}

const budgets = JSON.parse(readFileSync(BUDGETS, "utf8"));
let failures = 0;
const check = (ok, what) => {
  console.log(`${ok ? "ok  " : "FAIL"} ${what}`);
  if (!ok) failures++;
};
for (const [page, bytes] of Object.entries(sizes)) {
  const max = budgets[page];
  check(max !== undefined && bytes <= max, `JS on /${page === "index" ? "" : page}: ${(bytes / 1024).toFixed(1)} KB (budget ${max ? (max / 1024).toFixed(1) : "?"} KB)`);
}

function* files(dir) {
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    const p = join(dir, e.name);
    if (e.isDirectory()) yield* files(p);
    else yield p;
  }
}
const big = [...files(ROOT)].filter((f) => /\.(png|jpe?g|webp|avif|gif|svg)$/i.test(f) && statSync(f).size > IMAGE_MAX);
check(big.length === 0, `no image over 300 KB${big.length ? `: ${big.map((f) => f.replace(ROOT, "")).join(", ")}` : ""}`);

console.log(failures ? `\n${failures} budget failure(s)` : `\nall budgets met (${cloud ? "cloud" : "bare"} build)`);
process.exit(failures ? 1 : 0);
