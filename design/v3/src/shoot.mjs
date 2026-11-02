// Screenshots every .html in a dir to .png (1280x800; brand.html full page). Uses apps/desktop's Playwright.
import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";
const require = createRequire("/home/user/nugget/apps/desktop/package.json");
const { chromium } = require("playwright");
const dir = process.argv[2];
const b = await chromium.launch();
const p = await b.newPage({ viewport: { width: 1280, height: 800 } });
for (const f of fs.readdirSync(dir).filter((f) => f.endsWith(".html"))) {
  await p.goto("file://" + path.join(dir, f));
  await p.evaluate(() => document.fonts.ready);
  const out = path.join(dir, f.replace(".html", ".png"));
  if (f === "brand.html") await p.locator("body").screenshot({ path: out });
  else await p.screenshot({ path: out });
}
await b.close();
