// Screenshots of every page at desktop and mobile widths, in light and dark, saved small under screenshots/.
import { createRequire } from "node:module";
import sharp from "sharp";
import { mkdir } from "node:fs/promises";
import { buildTestSite, signedIn, CLOUD, ME } from "./mocks.mjs";

await buildTestSite();
const { serve, ROUTES } = await import("./serve.mjs");
// Playwright is pre-installed globally (with its Chromium), so it is resolved via require.
const { chromium } = createRequire(import.meta.url)("playwright");

const OUT = new URL("../screenshots/", import.meta.url);
const only = process.argv.slice(2);
const VIEWS = [
  { name: "desktop", width: 1440, height: 900, scale: 1, out: 1200 },
  { name: "mobile", width: 390, height: 844, scale: 2, out: 480 },
];
const routes = ROUTES.filter((r) => r !== "/no-such-page");

await mkdir(OUT, { recursive: true });
const { server, base } = await serve(4174);
const browser = await chromium.launch();
for (const v of VIEWS) {
  for (const scheme of ["dark", "light"]) {
    const opts = {
      viewport: { width: v.width, height: v.height },
      deviceScaleFactor: v.scale,
      colorScheme: scheme,
      reducedMotion: "reduce",
    };
    const anon = await browser.newContext(opts);
    // Account and checkout are shown signed in; checkout is caught mid-redirect to Stripe.
    const authed = await browser.newContext(opts);
    await signedIn(authed);
    await authed.route(`${CLOUD}/**`, (route) =>
      new URL(route.request().url()).pathname === "/v1/me" ? route.fulfill({ json: ME }) : new Promise(() => {}),
    );
    const pages = { anon: await anon.newPage(), authed: await authed.newPage() };
    for (const r of routes) {
      const name = `${r === "/" ? "home" : r.slice(1).replace(/\//g, "-")}-${v.name}-${scheme}`;
      if (only.length && !only.some((o) => name.includes(o))) continue;
      const priv = r === "/account" || r === "/checkout";
      const page = priv ? pages.authed : pages.anon;
      const url = r === "/checkout" ? "/checkout?plan=heed_max&interval=year" : r;
      await page.goto(base + url, { waitUntil: r === "/checkout" ? "load" : "networkidle" });
      // Scroll through once so scroll-triggered reveals have run, then return to the top.
      await page.evaluate(async () => {
        for (let y = 0; y < document.body.scrollHeight; y += 500) {
          window.scrollTo(0, y);
          await new Promise((r) => setTimeout(r, 40));
        }
        window.scrollTo(0, 0);
      });
      // Checkout's API call is held open on purpose (to catch the redirect state), so it never goes idle.
      if (r !== "/checkout") await page.waitForLoadState("networkidle");
      await page.waitForTimeout(900);
      const full = r !== "/legal/privacy" && r !== "/legal/terms";
      const png = await page.screenshot({ fullPage: full });
      const file = new URL(`${name}.webp`, OUT);
      await sharp(png).resize({ width: r === "/" ? (v.name === "desktop" ? 1040 : 420) : v.out }).webp({ quality: r === "/" ? (v.name === "mobile" ? 26 : 40) : v.name === "mobile" ? 62 : 70 }).toFile(file.pathname);
      console.log(name);
    }
    await anon.close();
    await authed.close();
  }
}
await browser.close();
server.close();
