// GG5 app scenario: graph centred on Mara Okafor with a kind filter (Organizations, then People),
// via "Show in graph" from her entity page. Read-only.
// Usage: node qa/GG5/scenarios/app/ui_graph_kinds.cjs [base=http://127.0.0.1:8765/]
const { chromium } = require("playwright");
const fs = require("fs");
const path = require("path");
const BASE = process.argv[2] || "http://127.0.0.1:8765/";
const OUT = path.join(__dirname, "shots");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const log = [];
const L = (...a) => {
  const s = a.map((x) => (typeof x === "string" ? x : JSON.stringify(x))).join(" ");
  log.push(s);
  console.log(s);
};
(async () => {
  const browser = await chromium.launch();
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 1000 }, locale: "en-US", timezoneId: "UTC" });
  await ctx.addInitScript(() => {
    localStorage.setItem("nugget.onboarding.v1", JSON.stringify({ version: 1, step: "done", completed: [], skipped: [], ai: null, finished: true }));
    localStorage.setItem("nugget.page", "knowledge");
  });
  const page = await ctx.newPage();
  await page.goto(BASE + "index.html");
  const list = page.locator("section.kb-list");
  await list.locator(".entity-row").first().waitFor();
  await page.fill("#kb-search", "Mara Okafor");
  await sleep(700);
  await list.locator(".entity-row").first().click();
  await page.getByRole("button", { name: "Show in graph" }).click();
  await page.locator("canvas.graph-canvas").waitFor();
  await sleep(2500);
  L("centred:", await page.locator(".graph-status").innerText());
  const chips = page.locator("#kb-panel-graph .chips");
  for (const k of ["Organizations", "People"]) {
    await chips.getByRole("button", { name: k }).click();
    await sleep(2500);
    L(`kinds=${k}:`, await page.locator(".graph-status").innerText());
    await page.getByRole("button", { name: "List", exact: true }).click();
    await sleep(400);
    L(
      `  list:`,
      await page.locator(".table-wrap tbody tr").evaluateAll((trs) => trs.map((tr) => Array.from(tr.children).slice(0, 4).map((c) => c.innerText.replace(/\s+/g, " ")).join(" | "))),
    );
    await page.getByRole("button", { name: "Graph", exact: true }).click();
    await sleep(1500);
    await page.screenshot({ path: path.join(OUT, `30-graph-mara-kind-${k}.png`), fullPage: true });
    await chips.getByRole("button", { name: k }).click();
    await sleep(500);
  }
  fs.writeFileSync(path.join(__dirname, "ui_graph_kinds.log"), log.join("\n") + "\n");
  await browser.close();
})().catch((e) => {
  console.error(e);
  process.exit(1);
});
