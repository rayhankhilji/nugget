// A build with no NEXT_PUBLIC_* settings (a fresh Vercel import) must still be a good public site:
// prices fall back to the built-in plans, and the account flows show "Heed Cloud isn't open yet".
import { exec } from "node:child_process";
import { promisify } from "node:util";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { serve, ROUTES } from "./serve.mjs";

const OUT = "dist-bare";
const env = Object.fromEntries(Object.entries(process.env).filter(([k]) => !/^(NEXT_PUBLIC_|PUBLIC_|SITE_URL$)/.test(k)));
await promisify(exec)("npx astro build", { env: { ...env, ASTRO_OUT_DIR: OUT } });
const { chromium } = createRequire(import.meta.url)("playwright");

let failures = 0;
const check = (ok, what) => {
  console.log(`${ok ? "ok  " : "FAIL"} ${what}`);
  if (!ok) failures++;
};

const { server, base } = await serve(4177, fileURLToPath(new URL(`../${OUT}/`, import.meta.url)));
const browser = await chromium.launch();
const page = await browser.newPage();
const errors = [];
page.on("pageerror", (e) => errors.push(e.message));
page.on("console", (m) => m.type() === "error" && !m.text().includes("status of 404") && errors.push(m.text()));

for (const r of ROUTES) {
  const res = await page.goto(base + r, { waitUntil: "networkidle" });
  check(res?.status() === (r === "/no-such-page" ? 404 : 200) && errors.length === 0, `bare ${r} renders without errors${errors.length ? `: ${errors.join(" | ")}` : ""}`);
  errors.length = 0;
}
for (const r of ["/account", "/checkout?plan=heed&interval=month", "/signin", "/signup"]) {
  await page.goto(base + r, { waitUntil: "networkidle" });
  const closed = await page.locator("[data-cloud-closed] h1").textContent().catch(() => null);
  check(closed === "Heed Cloud isn’t open yet." && page.url() === base + r, `bare ${r} shows the calm closed state`);
}
await page.goto(base + "/download");
check((await page.locator('a[href$=".dmg"], [data-dl]').count()) === 0, "bare /download links to no binary");
check((await page.locator('a[href^="mailto:"]', { hasText: "Notify me" }).count()) === 1, "bare /download offers Notify me");
check((await page.textContent("body"))?.includes("scripts/build-macos-app.sh") === true, "bare /download gives the build-from-source steps");
await page.goto(base + "/pricing");
check((await page.getAttribute(".pricing", "data-plans-source")) === "fallback", "bare pricing uses the built-in plans");
check((await page.locator('[data-tier="heed"] .amt:visible').first().textContent()) === "£39", "bare pricing shows £39");

await browser.close();
server.close();
console.log(failures ? `\n${failures} failure(s)` : "\nall bare-build checks passed");
process.exit(failures ? 1 : 0);
