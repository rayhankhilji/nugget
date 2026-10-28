// End-to-end: load the unpacked e2e build in Chromium, open a local page, then drive the real
// background router and content script (snapshot -> act -> verify). Native messaging is replaced
// by the e2e-only test hook, so no host install is needed.
//
//   NUGGET_E2E=1 pnpm test:e2e
//
// Uses the preinstalled `playwright` package (global or local); skipped unless NUGGET_E2E=1.

import { createServer } from "node:http";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { execSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

if (!process.env.NUGGET_E2E) {
  console.log("e2e skipped (set NUGGET_E2E=1)");
  process.exit(0);
}

const here = dirname(fileURLToPath(import.meta.url));
const extDir = join(here, "..", "dist", "e2e");

function loadPlaywright() {
  const require = createRequire(import.meta.url);
  try {
    return require("playwright");
  } catch {
    const globalRoot = execSync("npm root -g").toString().trim();
    return createRequire(join(globalRoot, "noop.js"))("playwright");
  }
}
const { chromium } = loadPlaywright();

const page = readFileSync(join(here, "page.html"));
const server = createServer((req, res) => {
  res.writeHead(200, { "content-type": "text/html" });
  res.end(req.url === "/next" ? "<title>Next</title><h1>Next page</h1><button>Ok</button>" : page);
});
await new Promise((r) => server.listen(0, "127.0.0.1", r));
const base = `http://127.0.0.1:${server.address().port}`;

const profile = mkdtempSync(join(tmpdir(), "nugget-e2e-"));
const context = await chromium.launchPersistentContext(profile, {
  headless: true,
  channel: "chromium", // new headless mode, which supports extensions
  args: [`--disable-extensions-except=${extDir}`, `--load-extension=${extDir}`],
});
let failed = false;
try {
  const tab = await context.newPage();
  await tab.goto(`${base}/`);
  const worker = context.serviceWorkers()[0] ?? (await context.waitForEvent("serviceworker"));
  const call = (msg) => worker.evaluate((m) => globalThis.nuggetE2E.handle(m), msg);
  await tab.bringToFront();

  const hello = await call({ id: 1, type: "hello", protocol: 1 });
  assert.equal(hello.result.protocol, 1);

  const snap = (await call({ id: 2, type: "snapshot" })).result;
  assert.ok(snap, "snapshot returned");
  assert.equal(snap.title, "Nugget e2e");
  const byName = (s, n) => s.elements.find((e) => e.name === n);
  assert.equal(byName(snap, "Password").sensitive, "password");
  assert.ok(!JSON.stringify(snap).includes("do-not-read"), "password value leaked");

  // Click and verify through the diff.
  const click = (await call({ id: 3, type: "act", snapshot_id: snap.snapshot_id, action: { kind: "click", element: byName(snap, "Show more").id } })).result;
  assert.equal(click.diff.added.length, 1);
  assert.ok(byName(click.snapshot, "Next page"));

  // Type, then submit, verify the page reacted.
  const typed = (await call({ id: 4, type: "act", snapshot_id: click.snapshot.snapshot_id, action: { kind: "type", element: byName(click.snapshot, "Message").id, text: "hello" } })).result;
  assert.equal(byName(typed.snapshot, "Message").value, "hello");
  const sent = (await call({ id: 5, type: "act", snapshot_id: typed.snapshot.snapshot_id, action: { kind: "submit", element: byName(typed.snapshot, "Message").id } })).result;
  assert.ok(sent.snapshot.text.includes("Sent: hello"), sent.snapshot.text);

  // Stale ids and sensitive fields are refused in the real page.
  const stale = await call({ id: 6, type: "act", snapshot_id: snap.snapshot_id, action: { kind: "click", element: byName(snap, "Show more").id } });
  assert.equal(stale.error.code, "stale_snapshot");
  const pw = await call({ id: 7, type: "act", snapshot_id: sent.snapshot.snapshot_id, action: { kind: "type", element: byName(sent.snapshot, "Password").id, text: "x" } });
  assert.equal(pw.error.code, "sensitive_field");

  // Overlay shows; navigate works; a non-granted site is refused.
  await call({ id: 8, type: "overlay", active: true });
  assert.equal(await tab.locator("[data-nugget-overlay]").count(), 1);
  const nav = (await call({ id: 9, type: "act", snapshot_id: "", action: { kind: "navigate", url: `${base}/next` } })).result;
  assert.equal(nav.snapshot.title, "Next");
  const denied = await call({ id: 10, type: "act", snapshot_id: "", action: { kind: "navigate", url: "https://example.com/" } });
  assert.equal(denied.error.code, "permission_required");

  // Kill switch.
  await call({ id: 11, type: "halt" });
  const halted = await call({ id: 12, type: "act", snapshot_id: nav.snapshot.snapshot_id, action: { kind: "scroll" } });
  assert.equal(halted.error.code, "halted");
  console.log("e2e passed");
} catch (e) {
  failed = true;
  console.error(e);
} finally {
  await context.close();
  server.close();
  rmSync(profile, { recursive: true, force: true });
}
process.exit(failed ? 1 : 0);
