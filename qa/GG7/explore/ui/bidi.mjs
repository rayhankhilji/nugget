// GG7: lay out the fragments ui.probe.tsx wrote, in Chromium with the app's CSS and <html dir=rtl lang=ar>,
// and print the computed direction and the visual (left-to-right on screen) order of each text.
//   cd apps/desktop && node ../../qa/GG7/explore/ui/bidi.mjs
import fs from "node:fs";
import { createRequire } from "node:module";
const { chromium } = createRequire(process.cwd() + "/package.json")("playwright");
const S = "/tmp/claude-0/ui-shots";
const css = ["styles.css", "styles-chat.css", "styles-pages.css", "overlay/overlay.css"].map((f) => fs.readFileSync("src/" + f, "utf8")).join("\n");
const b = await chromium.launch();
const p = await b.newPage();
for (const [name, sel] of [["today-ar", ".today-card-title"], ["overlay-ar", ".overlay-text, .overlay-foot kbd"], ["chat-ar", ".msg-body, .msg-body p"]]) {
  await p.setContent(`<html dir="rtl" lang="ar"><style>${css}</style><body>${fs.readFileSync(`${S}/${name}.html`, "utf8")}</body></html>`);
  await p.screenshot({ path: `${S}/${name}.png`, fullPage: true });
  const rows = await p.$$eval(sel, (els) => els.map((el) => {
    const out = []; const w = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
    for (let n; (n = w.nextNode()); ) for (let i = 0; i < n.data.length; i++) {
      const r = document.createRange(); r.setStart(n, i); r.setEnd(n, i + 1); const b = r.getBoundingClientRect();
      if (b.width) out.push([Math.round(b.top / 8), b.left, n.data[i]]);
    }
    out.sort((a, b) => a[0] - b[0] || a[1] - b[1]);
    return { cls: el.className || el.tagName, dir: getComputedStyle(el).direction, align: getComputedStyle(el).textAlign, visual: out.map((x) => x[2]).join("") };
  }));
  for (const r of rows) console.log(`GG7 F.${name}:`, JSON.stringify(r));
}
console.log("GG7 F.digits:", await p.evaluate(() => [new Intl.NumberFormat("ar").format(120), new Intl.DateTimeFormat("ar", { dateStyle: "medium" }).format(new Date(Date.UTC(2026, 10, 3)))]));
await b.close();
