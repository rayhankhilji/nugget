// Backup scenario: back up an indexed home through the Status page's "Backup and export"
// section, export it, wipe the home, and restore the backup into an empty home through the UI.
//
// 1. A daemon on a fresh home indexes the corpus. On the Status page the test sets a backup folder
//    and a passphrase, clicks Back up now and checks that the backup is listed and on disk.
// 2. Export everything to a temp folder; items.jsonl has one line per indexed item.
// 3. The daemon is stopped and its home deleted. A new daemon starts on an empty home (no key in
//    an OS keychain: NUGGET_KEYCHAIN=off, like every scenario), so only the passphrase can open
//    the backup. Through the UI: set the same folder, Restore…, enter the passphrase. The daemon
//    stages the restore and exits; the bridge's `start_daemon` (the shell's job) starts it again
//    on the same home, which applies the restore.
// 4. Search finds corpus text again and the item count matches the original.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { startBridge, METHOD_NOT_FOUND } from "./bridge.mjs";
import { addCorpusSource, rpc, startDaemon, waitForKnowledge } from "./daemon.mjs";
import { check, DIST, expect, INDEX_TIMEOUT_MS, KEEP, log, newContext, OUT, settle, shot, t, tPattern, until } from "./harness.mjs";

/** Backups and restores of the small corpus take seconds; leave room for slow CI disks. */
const BACKUP_TIMEOUT_MS = Number(process.env.E2E_BACKUP_TIMEOUT_MS || 120_000);
const PASSPHRASE = "tidewater lantern quarry";
const ONBOARDED = { version: 1, step: "done", completed: [], skipped: [], ai: null, finished: true };
/** A query the corpus answers (see run.mjs, pages scenario). */
const QUERY = "Tidewater budget";

const escapeRe = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const exactName = (key) => new RegExp(`^${escapeRe(t(key))}$`);

/** Backup directories (complete ones have a manifest) in `dir`. */
function backupsOnDisk(dir) {
  if (!fs.existsSync(dir)) return [];
  return fs
    .readdirSync(dir, { withFileTypes: true })
    .filter((e) => e.isDirectory() && e.name.startsWith("nugget-backup-") && fs.existsSync(path.join(dir, e.name, "manifest.json")))
    .map((e) => path.join(dir, e.name))
    .sort()
    .reverse();
}

export async function backupScenario({ browser, bin }) {
  const work = fs.mkdtempSync(path.join(os.tmpdir(), "nugget-e2e-backup-"));
  const backupDir = path.join(work, "backups");
  const exportDir = path.join(work, "export");
  fs.mkdirSync(backupDir);
  fs.mkdirSync(exportDir);
  const summary = { items: null, backup: null, export: null, restored_items: null, timings: {} };
  const t0 = Date.now();

  // ---- 1. an indexed home ----
  let daemon = await startDaemon(bin, { stderrPath: path.join(OUT, "backup-nuggetd-1.out.log") });
  log(`backup: daemon home ${daemon.home}`);
  let settled;
  try {
    await addCorpusSource(daemon.socket);
    settled = await waitForKnowledge(daemon.socket, { timeoutMs: INDEX_TIMEOUT_MS, log });
  } catch (e) {
    await daemon.stop({ keep: true });
    throw e;
  }
  const items = settled.status.items;
  summary.items = items;
  summary.timings.indexed_ms = Date.now() - t0;
  log(`backup: indexed ${items} items`);

  // The daemon the bridge talks to; `start_daemon` restarts it on its own home (restore).
  const restarts = [];
  const restart = async () => {
    const old = daemon;
    for (let i = 0; i < 150 && !old.exited(); i++) await new Promise((r) => setTimeout(r, 100));
    if (!old.exited()) throw new Error("the daemon did not exit after staging the restore");
    await old.stop({ keep: true });
    daemon = await startDaemon(bin, {
      home: old.home,
      config: null,
      readyTimeoutMs: BACKUP_TIMEOUT_MS,
      stderrPath: path.join(OUT, `backup-nuggetd-restart-${restarts.length + 1}.out.log`),
    });
    restarts.push({ home: daemon.home, at_ms: Date.now() - t0 });
    log(`backup: restarted the daemon on ${daemon.home}`);
    return daemon.socket;
  };

  let bridge;
  let ctx;
  const pageErrors = [];
  try {
    bridge = await startBridge({ socket: daemon.socket, dist: DIST, startDaemon: restart });
    ctx = await newContext(browser);
    await ctx.addInitScript(([key, state]) => localStorage.setItem(key, JSON.stringify(state)), ["nugget.onboarding.v1", ONBOARDED]);
    await ctx.addInitScript(() => localStorage.setItem("nugget.page", "status"));
    const page = await ctx.newPage();
    page.on("pageerror", (e) => pageErrors.push(e.message));
    bridge.deliverEvents(page);

    const section = page.locator('section[aria-labelledby="backup-title"]');
    const field = (key) => section.getByLabel(t(key), { exact: true });
    const button = (key) => section.getByRole("button", { name: exactName(key) });
    const open = async () => {
      await page.goto(`${bridge.url}index.html`);
      await section.getByRole("heading", { level: 2, name: t("backup.title"), exact: true }).waitFor();
      await field("backup.folder").waitFor();
    };
    const setFolder = async (dir) => {
      await field("backup.folder").fill(dir);
      await button("backup.save").click();
      await section.getByText(t("backup.saved"), { exact: true }).waitFor();
    };
    const restoreButtons = section.getByRole("button", { name: tPattern("backup.restore.buttonLabel") });

    await check("backup: set a folder and a passphrase, back up now, see the backup", async () => {
      try {
        await open();
        await setFolder(backupDir);
        await field("backup.passphrase.new").fill(PASSPHRASE);
        await field("backup.passphrase.confirm").fill(PASSPHRASE);
        await section.getByRole("button", { name: new RegExp(`^(${escapeRe(t("backup.passphrase.submit"))}|${escapeRe(t("backup.passphrase.change"))})$`) }).click();
        await section.getByText(t("backup.passphrase.saved"), { exact: true }).waitFor();
        await shot(page, "backup-configured");

        const tBackup = Date.now();
        await button("backup.now").click();
        const done = tPattern("backup.done");
        await until("Back up now reports a finished backup", async () => {
          const texts = await section.locator("[aria-live] p").allTextContents();
          const failed = await section.locator(".note-error").allTextContents();
          if (failed.length) throw new Error(`backup failed: ${failed.join(" | ")}`);
          return [texts.some((x) => done.test(x.trim())), texts];
        }, BACKUP_TIMEOUT_MS);
        summary.timings.backup_ms = Date.now() - tBackup;
        // Setting the passphrase makes the first scheduled backup due at once, so "Back up now"
        // may add a second one; every backup on disk must be listed.
        const onDisk = backupsOnDisk(backupDir);
        expect(onDisk.length >= 1, `expected a complete backup in ${backupDir}, found ${JSON.stringify(fs.readdirSync(backupDir))}`);
        await until("every backup is listed with a Restore button", async () => {
          const n = await restoreButtons.count();
          return [n === backupsOnDisk(backupDir).length, n];
        });
        const manifest = JSON.parse(fs.readFileSync(path.join(onDisk[0], "manifest.json"), "utf8"));
        expect(manifest.format === 1 && manifest.items === items, `manifest ${JSON.stringify(manifest)} should say format 1 and ${items} items`);
        for (const f of ["store.nbk", "key.json"]) expect(fs.existsSync(path.join(onDisk[0], f)), `${f} missing in ${onDisk[0]}`);
        summary.backup = { path: onDisk[0], manifest };
        log(`  backup: ${onDisk[0]} (${manifest.items} items) in ${summary.timings.backup_ms} ms`);
        await settle(page);
        await shot(page, "backup-done");
      } catch (e) {
        await shot(page, "FAIL-backup");
        throw e;
      }
    });

    await check("backup: export everything; items.jsonl has one line per indexed item", async () => {
      try {
        await field("backup.export.folder").fill(exportDir);
        await section.getByRole("button", { name: exactName("backup.export.submit") }).click();
        const done = tPattern("backup.export.done");
        const where = await until("the export reports where it wrote", async () => {
          const failed = await section.locator(".note-error").allTextContents();
          if (failed.length) throw new Error(`export failed: ${failed.join(" | ")}`);
          const ps = await section.locator("[aria-live] p").evaluateAll((els) => els.map((p) => ({ text: p.textContent.trim(), code: p.querySelector("code")?.textContent ?? null })));
          const hit = ps.find((p) => done.test(p.text) && p.code);
          return [Boolean(hit), hit?.code ?? ps];
        }, BACKUP_TIMEOUT_MS);
        expect(path.dirname(where) === exportDir, `the export went to ${where}, not into ${exportDir}`);
        const jsonl = path.join(where, "items.jsonl");
        expect(fs.existsSync(jsonl), `${jsonl} is missing`);
        const lines = fs.readFileSync(jsonl, "utf8").split("\n").filter((l) => l.trim());
        expect(lines.length === items, `items.jsonl has ${lines.length} lines, the daemon indexed ${items} items`);
        JSON.parse(lines[0]);
        for (const f of ["graph.json", "audit.csv", "egress.csv", "README.md"]) expect(fs.existsSync(path.join(where, f)), `${f} missing in ${where}`);
        const shown = (await section.locator("dl.stats > div", { has: page.getByText(t("backup.export.count.items"), { exact: true }) }).locator("dd").textContent())?.replace(/[^\d]/g, "");
        expect(Number(shown) === items, `the UI shows ${shown} exported items, expected ${items}`);
        summary.export = { path: where, items_jsonl_lines: lines.length };
        log(`  export: ${where} (${lines.length} items)`);
        await shot(page, "backup-export");
      } catch (e) {
        await shot(page, "FAIL-backup-export");
        throw e;
      }
    });

    await check("backup: wipe the home, restore into an empty home with the passphrase", async () => {
      try {
        // Wipe: stop the daemon and delete its home (key file, database, config, vault).
        const oldHome = daemon.home;
        fs.cpSync(daemon.logDir, path.join(OUT, "backup-daemon-logs-1"), { recursive: true });
        await daemon.stop({ keep: false });
        expect(!fs.existsSync(oldHome), `${oldHome} still exists after the wipe`);

        // A new computer, as far as Nugget can tell: an empty home, its own new key.
        daemon = await startDaemon(bin, { stderrPath: path.join(OUT, "backup-nuggetd-2.out.log") });
        bridge.setSocket(daemon.socket);
        log(`backup: empty home ${daemon.home}`);
        const before = await rpc(daemon.socket, "status");
        expect(before.items === 0, `the new home already has ${before.items} items`);

        await open();
        await setFolder(backupDir);
        await until("the backups in the folder are listed", async () => {
          const n = await restoreButtons.count();
          return [n >= 1 && n === backupsOnDisk(backupDir).length, n];
        });
        await restoreButtons.first().click();
        const dialog = page.locator("dialog[open]");
        await dialog.getByRole("heading", { name: t("backup.restore.title"), exact: true }).waitFor();
        await dialog.getByText(t("backup.restore.setAside"), { exact: true }).waitFor();
        await dialog.getByLabel(t("backup.restore.passphrase"), { exact: true }).fill(PASSPHRASE);
        await shot(page, "backup-restore-dialog");
        const tRestore = Date.now();
        await dialog.getByRole("button", { name: exactName("backup.restore.confirm") }).click();

        const done = tPattern("backup.restore.done");
        // The message ends with its Close button.
        const closeSuffix = new RegExp(`\\s*${escapeRe(t("common.close"))}$`);
        await until("the page says Nugget is running with the backup", async () => {
          const failed = await page.locator('section[aria-labelledby="backup-title"] .note-error, dialog .note-error').allTextContents();
          if (failed.length) throw new Error(`restore failed: ${failed.join(" | ")}`);
          const texts = await section.locator("[aria-live] p").allTextContents();
          return [texts.some((x) => done.test(x.replace(/\s+/g, " ").trim().replace(closeSuffix, ""))), texts];
        }, BACKUP_TIMEOUT_MS);
        summary.timings.restore_ms = Date.now() - tRestore;
        expect(restarts.length === 1, `start_daemon restarted the daemon ${restarts.length} times, expected once`);
        const after = await rpc(daemon.socket, "status");
        summary.restored_items = after.items;
        expect(after.items === items, `after the restore the daemon has ${after.items} items, the backup had ${items}`);
        log(`  restore: ${after.items} items back in ${summary.timings.restore_ms} ms`);
        await shot(page, "backup-restored");
      } catch (e) {
        await shot(page, "FAIL-backup-restore");
        throw e;
      }
    });

    await check("backup: search finds the corpus again after the restore", async () => {
      try {
        const hits = await rpc(daemon.socket, "search", { query: QUERY, limit: 10 });
        expect(Array.isArray(hits) && hits.length > 0, `search "${QUERY}" over IPC returned ${JSON.stringify(hits)}`);
        await page.getByRole("navigation").getByRole("button", { name: t("app.nav.search"), exact: true }).click();
        const box = page.getByRole("searchbox", { name: t("search.query") });
        await box.fill(QUERY);
        await box.press("Enter");
        const results = page.locator("ol.results li.result");
        const titles = await until(`search "${QUERY}" in the UI shows Tidewater results`, async () => {
          const all = await results.evaluateAll((lis) => lis.map((li) => li.textContent ?? ""));
          return [all.length > 0 && all.some((x) => x.includes("Tidewater")), all.slice(0, 3).map((x) => x.slice(0, 80))];
        });
        log(`  search after restore: ${JSON.stringify(titles)}`);
        await shot(page, "backup-search-after-restore");
      } catch (e) {
        await shot(page, "FAIL-backup-search");
        throw e;
      }
    });

    await check("backup: every backup command the UI called exists and accepted its params", async () => {
      const bad = bridge.calls.filter((c) => c.via === "daemon" && (c.code === METHOD_NOT_FOUND || c.code === -32602));
      expect(bad.length === 0, `wire mismatches: ${JSON.stringify(bad)}`);
      expect(pageErrors.length === 0, `uncaught page errors: ${JSON.stringify(pageErrors)}`);
    });

    summary.restarts = restarts;
    summary.commands = [...new Set(bridge.calls.map((c) => c.cmd))].sort();
    return summary;
  } finally {
    await ctx?.close().catch(() => {});
    await bridge?.close().catch(() => {});
    if (daemon) {
      if (fs.existsSync(daemon.logDir)) fs.cpSync(daemon.logDir, path.join(OUT, "backup-daemon-logs-2"), { recursive: true });
      await daemon.stop({ keep: KEEP });
      if (KEEP) log(`kept the restored home: ${daemon.home}`);
      else fs.rmSync(daemon.home, { recursive: true, force: true });
    }
    if (KEEP) log(`kept the backup and export folders: ${work}`);
    else fs.rmSync(work, { recursive: true, force: true });
  }
}
