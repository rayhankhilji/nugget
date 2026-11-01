// GG5 app scenario: drive the real desktop frontend (apps/desktop/dist, via e2e/bridge.mjs on
// :8765, standalone mode with the shim inlined) against the populated persona daemon.
// Read-only pass: entity list, hard-case entity views, Why? evidence, open-in-vault and evidence
// Open URLs (captured, not opened), graph overview, centred on Mara, list mode, sensitive nodes.
// Usage: node qa/GG5/scenarios/app/ui_explore.cjs [base=http://127.0.0.1:8765/]
const { chromium } = require("playwright");
const fs = require("fs");
const path = require("path");

const BASE = process.argv[2] || "http://127.0.0.1:8765/";
const OUT = path.join(__dirname, "shots");
fs.mkdirSync(OUT, { recursive: true });
const log = [];
const L = (...a) => {
  const s = a.map((x) => (typeof x === "string" ? x : JSON.stringify(x))).join(" ");
  log.push(s);
  console.log(s);
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

(async () => {
  const browser = await chromium.launch();
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 1000 }, locale: "en-US", timezoneId: "UTC" });
  await ctx.addInitScript(() => {
    localStorage.setItem("nugget.onboarding.v1", JSON.stringify({ version: 1, step: "done", completed: [], skipped: [], ai: null, finished: true }));
    localStorage.setItem("nugget.page", "knowledge");
  });
  const opened = [];
  const calls = [];
  await ctx.route("**/invoke", async (route) => {
    const body = JSON.parse(route.request().postData() || "{}");
    calls.push(body.cmd);
    if (body.cmd === "open_url") {
      opened.push(body.args.url);
      return route.fulfill({ contentType: "application/json", body: JSON.stringify({ ok: { opened: true } }) });
    }
    return route.continue();
  });
  const page = await ctx.newPage();
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));
  page.on("console", (m) => m.type() === "error" && errors.push(m.text()));
  await page.goto(BASE + "index.html");
  await page.locator("main h1").first().waitFor();
  const list = page.locator("section.kb-list");
  await list.locator(".entity-row").first().waitFor();
  await sleep(800);
  const shot = async (n) => page.screenshot({ path: path.join(OUT, n + ".png"), fullPage: true });
  await shot("01-list");
  const rows = async () =>
    list.locator(".entity-row").evaluateAll((bs) => bs.map((b) => b.innerText.replace(/\s+/g, " ").trim()));
  L("count:", await list.locator(".kb-count").innerText());
  L("rows:", await rows());

  const search = async (q) => {
    await page.fill("#kb-search", q);
    await sleep(700);
    return rows();
  };
  const detail = page.locator("section.entity-detail");
  const openEntity = async (q, nth = 0, tag = q) => {
    const r = await search(q);
    L(`search "${q}":`, r);
    await list.locator(".entity-row").nth(nth).click();
    await detail.locator("h2.entity-name").waitFor();
    await sleep(600);
    const head = await detail.locator(".entity-detail-head, .entity-props").evaluateAll((els) => els.map((e) => e.innerText.replace(/\s+/g, " ")));
    const facts = await detail.locator("li.fact").evaluateAll((lis) =>
      lis.map((li) => ({ cls: li.className, text: li.querySelector(".fact-text")?.innerText, meta: li.querySelector(".fact-meta")?.innerText.replace(/\s+/g, " ") })),
    );
    const groups = await detail.locator(".fact-predicate").allInnerTexts();
    L(`== entity ${tag}[${nth}] head:`, head);
    L("   groups:", groups);
    for (const f of facts) L("   fact:", f);
    await shot(`02-entity-${tag.replace(/[^A-Za-z0-9]+/g, "_")}-${nth}`);
    return facts;
  };

  await openEntity("Chen");
  await openEntity("Sam", 0);
  await openEntity("Sam", 1);
  await openEntity("Gus", 0);
  await openEntity("Gus", 1);
  await openEntity("Tessellate");
  await openEntity("Lumenfold");
  await openEntity("Harbor", 0);
  await openEntity("Harbor", 1);
  await search("Harbor");
  await shot("03-list-harbor");
  await openEntity("Dana");
  // Why? on every fact of Dana, capture evidence
  const why = detail.getByRole("button", { name: /^Why\?/ });
  const n = await why.count();
  for (let i = 0; i < n; i++) await why.nth(i).click();
  await sleep(500);
  const ev = await detail.locator("ul.evidence > li").evaluateAll((lis) =>
    lis.map((li) => ({
      quote: li.querySelector("blockquote")?.innerText ?? null,
      title: li.querySelector(".evidence-title")?.innerText,
      src: li.querySelector(".evidence-source")?.innerText.replace(/\s+/g, " "),
      open: !!li.querySelector("button"),
    })),
  );
  L("Dana evidence:", ev);
  await shot("04-dana-why");
  // click every evidence Open button, capture URLs
  const openBtns = detail.locator("ul.evidence button");
  for (let i = 0; i < (await openBtns.count()); i++) await openBtns.nth(i).click();
  await detail.getByRole("button", { name: "Open in vault" }).click();
  await sleep(500);
  L("opened URLs:", opened);
  await shot("05-dana-opened");

  await openEntity("Mara Okafor");
  await openEntity("Jonas");
  await openEntity("Counselling session");
  await openEntity("Theo");

  // ---- graph ----
  await page.getByRole("tab", { name: "Graph" }).click();
  const canvas = page.locator("canvas.graph-canvas");
  await canvas.waitFor();
  await sleep(2500);
  L("overview status:", await page.locator(".graph-status").innerText(), "| canvas label:", await canvas.getAttribute("aria-label"));
  await shot("10-graph-overview");
  await page.getByRole("button", { name: "List", exact: true }).click();
  await sleep(500);
  const tableRows = async () =>
    page.locator(".table-wrap tbody tr").evaluateAll((trs) => trs.map((tr) => Array.from(tr.children).slice(0, 4).map((c) => c.innerText.replace(/\s+/g, " "))));
  L("overview list:", await tableRows());
  await shot("11-graph-overview-list");
  await page.getByRole("button", { name: "Graph", exact: true }).click();

  const centre = async (q, tag) => {
    await page.locator(".center-picker input").fill(q);
    await page.locator(".center-picker button[type=submit]").click();
    await page.locator(".center-results li").first().waitFor();
    L(`centre results "${q}":`, await page.locator(".center-results").innerText());
    await page.locator(".center-results li button").first().click();
    await sleep(3000);
    L(`centred ${tag}:`, await page.locator(".graph-controls").innerText(), "|", await page.locator(".graph-status").innerText(), "| canvas:", await canvas.getAttribute("aria-label"));
    await shot(`12-graph-${tag}-canvas`);
    await page.getByRole("button", { name: "List", exact: true }).click();
    await sleep(500);
    L(`${tag} list:`, await tableRows());
    await shot(`13-graph-${tag}-list`);
    await page.getByRole("button", { name: "Graph", exact: true }).click();
    await sleep(300);
  };
  await centre("Mara Okafor", "mara-d2");
  // depth 1
  await page.locator(".graph-controls select").first().selectOption("1");
  await sleep(2500);
  L("mara depth1:", await page.locator(".graph-status").innerText());
  await shot("14-graph-mara-d1");
  await page.locator(".graph-controls select").first().selectOption("3");
  await sleep(2500);
  L("mara depth3:", await page.locator(".graph-status").innerText());
  await shot("15-graph-mara-d3");
  await page.locator(".graph-controls select").first().selectOption("2");
  await centre("Jonas", "jonas");
  await centre("Counselling session", "counselling");
  await centre("Gus", "gus");
  L("errors:", errors);
  L("invoked cmds:", [...new Set(calls)]);
  fs.writeFileSync(path.join(__dirname, "ui_explore.log"), log.join("\n") + "\n");
  await browser.close();
})().catch((e) => {
  console.error(e);
  fs.writeFileSync(path.join(__dirname, "ui_explore.log"), log.join("\n") + "\nFAILED: " + e.stack + "\n");
  process.exit(1);
});
