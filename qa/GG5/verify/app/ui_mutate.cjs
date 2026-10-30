// GG5 app scenario (mutating): reject/undo, restore of a superseded fact, merge of the two Gus
// Ferreira entities and of Tessellate into Lumenfold, all through the real UI; daemon state is
// read back over IPC (python helper) after each step.
// Usage: node qa/GG5/scenarios/app/ui_mutate.cjs <socket> [base=http://127.0.0.1:8765/]
const { chromium } = require("playwright");
const { execFileSync } = require("child_process");
const fs = require("fs");
const path = require("path");

const SOCK = process.argv[2];
const BASE = process.argv[3] || "http://127.0.0.1:8765/";
const OUT = path.join(process.env.OUTDIR, "shots");
fs.mkdirSync(OUT, { recursive: true });
const log = [];
const L = (...a) => {
  const s = a.map((x) => (typeof x === "string" ? x : JSON.stringify(x))).join(" ");
  log.push(s);
  console.log(s);
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const rpc = (method, params) =>
  JSON.parse(
    execFileSync("python3", [
      "-c",
      `import sys,json; sys.path.insert(0,'${"/home/user/nugget/qa/GG5/harness"}')
from nugget_qa import Client
c=Client(sys.argv[1]); print(json.dumps(c.call(sys.argv[2], json.loads(sys.argv[3]))))`,
      SOCK,
      method,
      JSON.stringify(params ?? {}),
    ]).toString(),
  );

(async () => {
  const browser = await chromium.launch();
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 1000 }, locale: "en-US", timezoneId: "UTC" });
  await ctx.addInitScript(() => {
    localStorage.setItem("nugget.onboarding.v1", JSON.stringify({ version: 1, step: "done", completed: [], skipped: [], ai: null, finished: true }));
    localStorage.setItem("nugget.page", "knowledge");
  });
  const page = await ctx.newPage();
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.goto(BASE + "index.html");
  const list = page.locator("section.kb-list");
  await list.locator(".entity-row").first().waitFor();
  const detail = page.locator("section.entity-detail");
  const shot = async (n) => page.screenshot({ path: path.join(OUT, n + ".png"), fullPage: true });
  const open = async (q, nth = 0) => {
    await page.fill("#kb-search", q);
    await sleep(700);
    await list.locator(".entity-row").nth(nth).click();
    await detail.locator("h2.entity-name").waitFor();
    await sleep(600);
  };
  const facts = () =>
    detail.locator("li.fact").evaluateAll((lis) =>
      lis.map((li) => `${li.className.replace("fact ", "").replace("fact", "")}|${li.querySelector(".fact-text")?.innerText}|${li.querySelector(".fact-meta")?.innerText.replace(/\s+/g, " ")}`),
    );
  const factLi = (text) => detail.locator("li.fact", { has: page.locator(".fact-text", { hasText: text }) }).first();
  const claimStatus = (entity, text) => rpc("knowledge.entity", { id: entity }).claims.filter((c) => c.text.includes(text)).map((c) => [c.id, c.status]);
  const graphHas = (claimIds) => {
    const g = rpc("knowledge.graph", { center: null, depth: 1, limit: 200, kinds: [] });
    return claimIds.map((id) => g.edges.some((e) => e.claim === id));
  };

  // ---- reject / undo on R. Chen "works on Harbor" ----
  const byName = (n) => rpc("knowledge.entities", { limit: 500, offset: 0 }).filter((e) => e.name === n)[0].id;
  const chen = byName("R. Chen");
  await open("Chen");
  const target = "Robert Chen works on Harbor.";
  await factLi(target).getByRole("button", { name: /^Reject/ }).click();
  await sleep(1500);
  L("after reject, ui:", await facts());
  L("after reject, live note:", await detail.locator(".entity-live").innerText());
  L("after reject, ipc:", claimStatus(chen, "works on Harbor"), "edge in overview:", graphHas(claimStatus(chen, "works on Harbor").map((x) => x[0])));
  await shot("20-chen-rejected");
  await detail.getByRole("button", { name: "Undo" }).click();
  await sleep(1500);
  L("after undo, ui:", await facts());
  L("after undo, ipc:", claimStatus(chen, "works on Harbor"), "edge:", graphHas(claimStatus(chen, "works on Harbor").map((x) => x[0])));
  await shot("21-chen-undone");
  // reject again and reload the page: how does a rejected fact look later?
  await factLi(target).getByRole("button", { name: /^Reject/ }).click();
  await sleep(1500);
  await page.reload();
  await list.locator(".entity-row").first().waitFor();
  await open("Chen");
  L("rejected after reload, ui:", await facts());
  L("facts heading:", await detail.locator("h3.entity-section").first().innerText());
  await shot("22-chen-rejected-reload");
  const vaultChen = fs.readFileSync(path.join(process.env.VAULT, "Person", `R. Chen (${chen}).md`), "utf8");
  L("vault R. Chen has 'works on Harbor':", vaultChen.includes("works on [["), vaultChen.split("\n").filter((l) => l.includes("Harbor")).join(" // "));
  // restore it (Restore button)
  await factLi(target).getByRole("button", { name: /^Restore/ }).click();
  await sleep(1500);
  L("after restore, ipc:", claimStatus(chen, "works on Harbor"));

  // ---- Restore on a superseded fact (Dana works at Brightwell) ----
  const dana = byName("Dana Whitfield");
  await open("Dana Whitfield");
  const sup = "works at Brightwell";
  L("dana before:", await facts());
  const btn = factLi(sup).locator(".fact-meta button").last();
  L("superseded fact action button:", await btn.innerText(), "| aria:", await btn.getAttribute("aria-label"), "| class:", await btn.getAttribute("class"));
  await btn.click();
  await sleep(2000);
  L("dana after restore, ui:", await facts());
  L("dana after restore, live:", await detail.locator(".entity-live").innerText());
  L("dana after restore, ipc works_at:", rpc("knowledge.entity", { id: dana }).claims.filter((c) => c.predicate === "works_at").map((c) => [c.id, c.status, c.text]));
  await shot("23-dana-superseded-restored");
  await sleep(3000);
  const vaultDana = fs.readFileSync(path.join(process.env.VAULT, "Person", `Dana Whitfield (${dana}).md`), "utf8");
  L("vault Dana works_at lines:", vaultDana.split("\n").filter((l) => l.includes("works at")));

  // ---- merge Gus Ferreira (tessellate) into Gus Ferreira (lumenfold) ----
  await open("Gus", 0);
  const gusKeep = await detail.locator(".entity-sub code").innerText();
  await detail.getByRole("button", { name: /^Merge/ }).click();
  const dlg = page.getByRole("dialog");
  await dlg.waitFor();
  await sleep(1200);
  L("merge options:", await dlg.locator(".merge-results").innerText());
  await shot("24-merge-gus-options");
  await dlg.locator(".merge-option input").first().check();
  await sleep(1200);
  L("merge preview:", await dlg.locator(".merge-preview").innerText());
  await shot("25-merge-gus-preview");
  await dlg.getByRole("button", { name: "Merge", exact: true }).click();
  await sleep(2500);
  L("after merge, detail:", await detail.locator(".entity-detail-head, .entity-props").allInnerTexts(), await facts());
  await page.fill("#kb-search", "Gus");
  await sleep(800);
  L("after merge, list:", await list.locator(".entity-row").allInnerTexts());
  await shot("26-merge-gus-done");

  // ---- merge Tessellate into Lumenfold ----
  await open("Lumenfold");
  await detail.getByRole("button", { name: /^Merge/ }).click();
  await dlg.waitFor();
  await dlg.locator("input[type=search]").fill("Tessellate");
  await sleep(1200);
  L("merge options (Tessellate):", await dlg.locator(".merge-results").innerText());
  await dlg.locator(".merge-option input").first().check();
  await sleep(1200);
  L("merge preview:", await dlg.locator(".merge-preview").innerText());
  await shot("27-merge-org-preview");
  await dlg.getByRole("button", { name: "Merge", exact: true }).click();
  await sleep(2500);
  L("after org merge, detail:", await detail.locator(".entity-detail-head, .entity-props").allInnerTexts(), await facts());
  await shot("28-merge-org-done");
  // graph after merges, centred on Mara, list mode
  await page.getByRole("tab", { name: "Graph" }).click();
  await page.locator(".center-picker input").fill("Mara Okafor");
  await page.locator(".center-picker button[type=submit]").click();
  await page.locator(".center-results li button").first().click();
  await sleep(3000);
  L("graph after merges:", await page.locator(".graph-status").innerText());
  await shot("29-graph-after-merge");
  L("errors:", errors);
  fs.writeFileSync(path.join(process.env.OUTDIR, "ui_mutate.log"), log.join("\n") + "\n");
  await browser.close();
})().catch((e) => {
  console.error(e);
  fs.writeFileSync(path.join(process.env.OUTDIR, "ui_mutate.log"), log.join("\n") + "\nFAILED: " + e.stack + "\n");
  process.exit(1);
});
