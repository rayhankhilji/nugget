// A2 recommendation matrix: the REAL manifests/default.json model list (ids, sizes, hashes, min RAM,
// tiers) re-signed with a throwaway key (URLs point at a closed localhost port: nothing downloads),
// run through nuggetd-e2e for 4/8/16/32/64 GB x unknown/apple_silicon/nvidia. Records models.list
// (recommended flags) and models.fit, and screenshots the onboarding AI step for 4, 8 and 16 GB.
// Run from repo root: node qa/GG6/a2-ai-setup/recommend.mjs
import fs from "node:fs";
import path from "node:path";
import { startBridge } from "../../../apps/desktop/e2e/bridge.mjs";
import { rpc } from "../../../apps/desktop/e2e/daemon.mjs";
import { newContext, t } from "../../../apps/desktop/e2e/harness.mjs";
import { BIN, NO_RT_CONFIG, REPO, SCRATCH, closedPort, log, playwright, signManifest, snap, startHome, writeResults } from "./lib.mjs";

const real = JSON.parse(fs.readFileSync(path.join(REPO, "manifests/default.json"), "utf8"));
const dead = await closedPort();
const dir = fs.mkdtempSync(path.join(SCRATCH, "rec-"));
const signed = signManifest(
  dir,
  real.models.map((m) => ({
    id: m.id, size: m.size, sha: m.sha256, minRam: m.min_ram_gb, tiers: m.tiers, family: m.family, file: m.file_name,
    license: m.license, port: dead,
  })),
);

const RAMS = [4, 8, 16, 32, 64];
const GPUS = ["unknown", "apple_silicon", "nvidia"];
const matrix = [];
const browser = await playwright.chromium.launch();
try {
  for (const ram of RAMS) {
    for (const gpu of GPUS) {
      const d = await startHome(BIN.e2e, { config: NO_RT_CONFIG, env: { ...signed.env, NUGGET_E2E_RAM_GB: String(ram), NUGGET_E2E_GPU: gpu } });
      try {
        const list = await rpc(d.socket, "models.list", {});
        const fit = await rpc(d.socket, "models.fit", {}).catch((e) => ({ error: e.message }));
        const row = {
          ram, gpu,
          recommended: list.filter((m) => m.recommended).map((m) => m.id),
          recommended_bytes: list.filter((m) => m.recommended).reduce((a, m) => a + m.size_bytes, 0),
          fit: (fit.models || []).map((f) => ({ id: f.id, recommended: f.recommended, fits_ram: f.fits_ram, accel: f.accel, reasons: f.reasons })),
          machine_ram_gb: fit.ram_gb,
        };
        matrix.push(row);
        log(`${ram} GB ${gpu}: ${row.recommended.join(", ") || "(none)"}`);
        if (gpu === "unknown" && [4, 8, 16].includes(ram)) {
          const bridge = await startBridge({ socket: d.socket });
          const ctx = await newContext(browser);
          const page = await ctx.newPage();
          bridge.deliverEvents(page);
          await page.goto(`${bridge.url}index.html`);
          await page.getByRole("heading", { level: 1, name: t("onboarding.welcome.heading") }).waitFor();
          await page.locator("footer.ob-actions").getByRole("button", { name: t("onboarding.continue"), exact: true }).click();
          await page.getByRole("radio", { name: new RegExp(`^${t("onboarding.ai.local")}`) }).check();
          await page.waitForTimeout(1500);
          const cards = await page.locator("li.card").evaluateAll((ls) => ls.map((l) => l.innerText.replace(/\s+/g, " ").trim()));
          const body = await page.locator("main, body").first().innerText();
          row.ui = { cards, mentions_time: /minute|second|hour|time/i.test(cards.join(" ")), machine: (body.match(/This computer:[^\n]*/) || [""])[0] };
          await page.setViewportSize({ width: 1100, height: 1000 });
          row.shot = await snap(page, `rec-${ram}gb-onboarding`);
          await ctx.close();
          await bridge.close();
        }
      } finally {
        await d.stop();
        fs.rmSync(d.home, { recursive: true, force: true });
      }
    }
  }
} finally {
  await browser.close();
}

// Download time table for the real manifest.
const speeds = [10, 50, 200];
const timeFor = (bytes, mbit) => bytes / ((mbit * 1e6) / 8);
const table = real.models.map((m) => ({
  id: m.id, size_bytes: m.size, size_gb: +(m.size / 1e9).toFixed(2), min_ram_gb: m.min_ram_gb, tiers: m.tiers,
  minutes: Object.fromEntries(speeds.map((s) => [`${s}mbit`, +(timeFor(m.size, s) / 60).toFixed(1)])),
}));
writeResults("recommend", { matrix, table, runtimes: real.runtimes.map((r) => ({ id: r.id, size: r.size })) });
