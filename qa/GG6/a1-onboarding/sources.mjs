// A1 source-step mistakes: what a new user sees when the first folder is empty, unsupported,
// huge, unreadable, oddly named, their whole home, a typed "~", a nonexistent or relative path,
// overlapping folders, or a file instead of a folder.
// Usage (repo root): node qa/GG6/a1-onboarding/sources.mjs [case,...]
import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";
import * as L from "./lib.mjs";

const { chromium } = createRequire(path.join(L.REPO, "apps/desktop/package.json"))("playwright");
const { t, exact, log, record, snap } = L;
const D = path.join(L.SCRATCH, "src-cases");
fs.mkdirSync(D, { recursive: true });
const mk = (rel) => (fs.mkdirSync(path.join(D, rel), { recursive: true }), path.join(D, rel));
const wr = (rel, s) => (fs.mkdirSync(path.dirname(path.join(D, rel)), { recursive: true }), fs.writeFileSync(path.join(D, rel), s), path.join(D, rel));

function fixtures() {
  const empty = mk("empty");
  const unsupported = mk("unsupported");
  for (const f of ["installer.exe", "backup.zip", "holiday.mp4", "IMG_0042.heic", "scan.tiff", "song.mp3", "disk.dmg", "slides.key"]) wr(`unsupported/${f}`, "\0\x01binary" + "x".repeat(200));
  const huge = mk("huge");
  const hugeFile = path.join(huge, "all-my-notes.txt");
  if (!fs.existsSync(hugeFile) || fs.statSync(hugeFile).size < 200 * 1024 * 1024) {
    const line = "Meeting notes: the quarterly plan, the garden budget and the dentist on Thursday.\n";
    const chunk = line.repeat(Math.ceil((1024 * 1024) / line.length));
    const fd = fs.openSync(hugeFile, "w");
    for (let i = 0; i < 200; i++) fs.writeSync(fd, chunk);
    fs.closeSync(fd);
  }
  const locked = mk("locked");
  wr("locked/secret-plans.md", "# Plans\n\nlocked folder note\n");
  fs.chmodSync(locked, 0o000);
  const unicode = mk("My Notes ✨/Café résumé 📁");
  wr("My Notes ✨/Café résumé 📁/重要 meeting — notes.md", "# Kickoff\n\nThe zebrafinch launch moves to May.\n");
  // a synthetic home folder: documents plus the clutter a real home has
  const home = mk("home-sam");
  if (!fs.existsSync(path.join(home, "Documents"))) {
    for (let i = 0; i < 400; i++) wr(`home-sam/Documents/notes/n${i}.md`, `# Note ${i}\n\nhome note ${i} about the kestrel project\n`);
    for (let i = 0; i < 1500; i++) wr(`home-sam/code/webapp/node_modules/pkg${i % 50}/f${i}.js`, `module.exports=${i};\n`);
    for (let i = 0; i < 800; i++) wr(`home-sam/code/webapp/src/c${i}.ts`, `export const c${i} = ${i};\n`);
    for (let i = 0; i < 300; i++) wr(`home-sam/.config/app/s${i}.json`, `{"k":${i}}\n`);
    for (let i = 0; i < 300; i++) wr(`home-sam/Downloads/statement-${i}.csv`, `date,amount\n2026-01-01,${i}\n`);
    for (let i = 0; i < 200; i++) wr(`home-sam/.local/share/Trash/files/old${i}.txt`, `deleted ${i}\n`);
    wr("home-sam/.ssh/id_ed25519", "-----BEGIN OPENSSH PRIVATE KEY-----\nFAKEFAKEFAKE\n-----END OPENSSH PRIVATE KEY-----\n");
    wr("home-sam/.env", "API_TOKEN=fake-token-for-qa\n");
    wr("home-sam/Documents/passwords.txt", "bank: hunter2-fake\n");
  }
  const parent = mk("overlap");
  const child = mk("overlap/sub");
  wr("overlap/sub/a.md", "# a\n");
  const aFile = wr("just-a-file.md", "# I am a file\n");
  const mixed = mk("mixed");
  wr("mixed/ok.md", "# readable\n\nreadable note pelican\n");
  const bad = wr("mixed/unreadable.md", "# nope\n");
  fs.chmodSync(bad, 0o000);
  return { empty, unsupported, huge, locked, unicode, home, parent, child, aFile, mixed };
}

const F = fixtures();
const CASES = {
  empty: { pick: [F.empty] },
  unsupported: { pick: [F.unsupported] },
  huge200mb: { pick: [F.huge], wait: 90_000 },
  chmod000: { pick: [F.locked] },
  unicode: { pick: [F.unicode], search: "zebrafinch" },
  home: { pick: [F.home], wait: 120_000, search: "kestrel project" },
  tilde: { type: ["~"] },
  tildeDocs: { type: ["~/Documents"] },
  nonexistent: { type: ["/Users/sam/Documnets"] },
  relative: { type: ["Documents"] },
  overlap: { pick: [F.parent, F.child] },
  file: { type: [F.aFile] },
  mixedUnreadable: { pick: [F.mixed] },
};

const browser = await chromium.launch();
const out = {};

async function runCase(name, c) {
  const S = `source:${name}`;
  const env = await L.boot(browser, { name: `src-${name}` });
  const r = { name };
  try {
    let page = await env.open();
    await page.evaluate(() => localStorage.setItem("nugget.onboarding.v1", JSON.stringify({ version: 1, step: "source", completed: ["welcome", "ai"], skipped: [], ai: "later", finished: false })));
    await page.reload();
    await L.waitHeading(page, "onboarding.source.heading");
    await page.getByRole("button", { name: exact("onboarding.source.addFolder") }).click();
    const dialog = page.locator("dialog[open]");
    if (c.pick) {
      env.picker.next = c.pick;
      await dialog.getByRole("button", { name: exact("addSource.folder.choose") }).click();
    }
    for (const p of c.type ?? []) {
      await dialog.locator("#folder-path").fill(p);
      await dialog.locator("#folder-path").press("Enter");
    }
    r.formErrorsBeforeSubmit = await L.visibleErrors(page);
    const t0 = Date.now();
    await dialog.getByRole("button", { name: exact("addSource.folder.submit") }).click();
    await L.sleep(800);
    const stillOpen = await dialog.isVisible().catch(() => false);
    r.dialogStayedOpen = stillOpen;
    if (stillOpen) {
      r.dialogErrors = await L.visibleErrors(page);
      r.shot = await snap(page, `20-src-${name}`);
      record(S, "user is told what went wrong (dialog)", r.dialogErrors.length > 0, r.dialogErrors);
      return;
    }
    const phases = [];
    const deadline = Date.now() + (c.wait ?? 25_000);
    let row;
    while (Date.now() < deadline) {
      [row] = await L.progressRows(page);
      if (row && (!phases.length || phases.at(-1).text !== row.text)) phases.push({ ...row, ms: Date.now() - t0 });
      if (row?.phase === "ready" || row?.phase === "failed") break;
      await L.sleep(300);
    }
    r.phases = phases;
    r.finalPhase = row?.phase;
    r.finalText = row?.text;
    r.settleMs = Date.now() - t0;
    const list = await L.rpc(env.daemon.socket, "sources.list");
    r.source = list.map((s) => ({ name: s.name, state: s.state, items: s.items, last_error: s.last_error, health: s.health ?? null }));
    r.errors = await L.visibleErrors(page);
    r.shot = await snap(page, `20-src-${name}`);
    if (c.search) {
      const hits = await L.rpc(env.daemon.socket, "search", { query: c.search, limit: 5 });
      r.searchHits = hits.map((h) => h.title);
    }
    if (name === "home") {
      const s = await L.rpc(env.daemon.socket, "status");
      r.items = s.items;
      for (const q of ["OPENSSH PRIVATE KEY", "API_TOKEN", "hunter2", "deleted", "module.exports"]) {
        const h = await L.rpc(env.daemon.socket, "search", { query: q, limit: 3 });
        r[`hits:${q}`] = h.map((x) => decodeURIComponent(x.url ?? x.title ?? ""));
      }
    }
    const settled = r.finalPhase === "ready" || r.finalPhase === "failed";
    record(S, "the step settles (ready or failed) instead of spinning", settled, `${r.finalPhase} after ${r.settleMs}ms: ${r.finalText}`);
    // Done step: what does the user get told?
    await L.cont(page);
    await L.cont(page);
    await L.cont(page);
    await L.waitHeading(page, "onboarding.done.heading");
    r.doneText = (await page.locator("main.ob-main").innerText()).replace(/\s+/g, " ").slice(0, 300);
    r.donePrimary = await page.locator("footer.ob-actions .btn-primary").textContent();
  } catch (e) {
    r.exception = e.message.split("\n")[0];
    record(S, "case ran", false, r.exception);
  } finally {
    out[name] = r;
    log(JSON.stringify(r).slice(0, 900));
    await env.close();
  }
}

const want = process.argv[2] ? process.argv[2].split(",") : Object.keys(CASES);
for (const w of want) await runCase(w, CASES[w]);
await browser.close();
fs.chmodSync(F.locked, 0o755);
const prev = fs.existsSync(path.join(L.HERE, "results-sources.json")) ? JSON.parse(fs.readFileSync(path.join(L.HERE, "results-sources.json"), "utf8")) : {};
L.writeJson("results-sources.json", { ...prev, ...out, checks: [...(prev.checks ?? []).filter((c) => !want.some((w) => c.scenario === `source:${w}`)), ...L.results] });
log(`${L.results.length} checks, ${L.results.filter((r) => !r.ok).length} failed`);
