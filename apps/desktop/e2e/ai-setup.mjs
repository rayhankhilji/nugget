// AI setup: onboarding with "On this device" installs the recommended model from a local mock
// download server, then Chat streams a cited answer from it and draft.begin drafts with it.
// Nothing leaves localhost: a test manifest signed with a throwaway ed25519 key, `nuggetd-e2e`
// (trusts only that key, fakes a 16 GB / no-GPU machine) and the fake llama-server.
import crypto from "node:crypto";
import fs from "node:fs";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import { startBridge } from "./bridge.mjs";
import { CORPUS, locateE2eBinaries, rpc, rpcStream, startDaemon } from "./daemon.mjs";
import {
  check, checkWire, DIST, exactName, expect, expectNoErrorState, log, navTo, newContext, onboardingAddFolder,
  onboardingRunItYourself, OUT, settle, shot, startsWith, stopDaemon, t, tPattern, until,
} from "./harness.mjs";

/** The model the manifest recommends for a 16 GB machine, and one that needs more RAM. */
export const MODEL_ID = "e2e-chat-q4";
const BIG_MODEL_ID = "e2e-chat-big-q8";
const MODEL_BYTES = 4 * 1024 * 1024;
/** Mock download speed, slow enough for the card to show progress. */
const BYTES_PER_SEC = 1.5 * 1024 * 1024;

const QUERY = "Tidewater Billing vendor contract";
const EXPECT_TITLE = "Tidewater Billing";
/** How the fake llama-server starts a reply to a prompt with passages. */
const CITED_PREFIX = "According to your notes";

const AI_CONFIG = `[runtime]
enabled = true
idle_stop_minutes = 0

[mcp]
http = false
`;

const sha256 = (b) => crypto.createHash("sha256").update(b).digest("hex");
const sha1 = (s) => crypto.createHash("sha1").update(s).digest("hex");

/** Deterministic "weights": the GGUF magic, then a xorshift stream. */
function modelBytes(n) {
  const buf = Buffer.alloc(n);
  buf.write("GGUF", 0, "latin1");
  let x = 0x9e3779b9;
  for (let i = 4; i < n; i++) {
    x ^= x << 13;
    x ^= x >>> 17;
    x ^= x << 5;
    buf[i] = x & 0xff;
  }
  return buf;
}

/** Serve `files` (`/path` -> Buffer) on 127.0.0.1 with Range support, throttled to `bytesPerSec`. */
export function startModelServer(files, bytesPerSec = BYTES_PER_SEC) {
  const stats = { requests: 0, served: 0 };
  const server = http.createServer(async (req, res) => {
    stats.requests++;
    const body = files[new URL(req.url, "http://127.0.0.1").pathname];
    if (!body) return res.writeHead(404).end("not found");
    const range = req.headers.range;
    const m = range && /^bytes=(\d+)-(\d*)$/.exec(range);
    const start = m ? Number(m[1]) : 0;
    const end = m && m[2] ? Math.min(Number(m[2]), body.length - 1) : body.length - 1;
    if ((range && !m) || start > end) return res.writeHead(416, { "Content-Range": `bytes */${body.length}` }).end();
    res.writeHead(m ? 206 : 200, {
      "Content-Type": "application/octet-stream",
      "Content-Length": end - start + 1,
      "Accept-Ranges": "bytes",
      ...(m ? { "Content-Range": `bytes ${start}-${end}/${body.length}` } : {}),
    });
    if (req.method === "HEAD") return res.end();
    const t0 = Date.now();
    let sent = 0;
    for (let at = start; at <= end && !res.destroyed; at += 64 * 1024) {
      const chunk = body.subarray(at, Math.min(at + 64 * 1024, end + 1));
      if (!res.write(chunk)) await new Promise((r) => res.once("drain", r));
      sent += chunk.length;
      stats.served += chunk.length;
      const due = (sent / bytesPerSec) * 1000 - (Date.now() - t0);
      if (due > 0) await new Promise((r) => setTimeout(r, due));
    }
    res.end();
  });
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () =>
      resolve({ port: server.address().port, stats, close: () => new Promise((r) => (server.closeAllConnections?.(), server.close(() => r()))) }),
    );
  });
}

/** Write a manifest for the mock server on `port`, signed with a fresh key; returns its files and `NUGGET_E2E_*` env. */
export function signedTestManifest(dir, port) {
  const bytes = modelBytes(MODEL_BYTES);
  const rev = sha1("nugget-e2e-chat-model");
  const file = "E2E-Chat-Q4_K_M.gguf";
  const entry = (id, revision, fileName, size, sha, minRam) => ({
    id,
    family: "qwen3",
    file_name: fileName,
    url: `http://127.0.0.1:${port}/${revision}/${fileName}`,
    mirrors: [],
    revision,
    size,
    sha256: sha,
    min_ram_gb: minRam,
    tiers: ["triage", "light", "synthesis", "agentic"],
    license: "Apache-2.0",
    min_app_version: "0.0.1",
  });
  const manifest = {
    version: 1,
    models: [
      entry(MODEL_ID, rev, file, bytes.length, sha256(bytes), 4),
      entry(BIG_MODEL_ID, sha1("nugget-e2e-chat-model-big"), "E2E-Chat-Big-Q8_0.gguf", 40 * 1024 ** 3, sha256(Buffer.from("never served")), 64),
    ],
  };
  const json = Buffer.from(JSON.stringify(manifest, null, 2) + "\n");
  const { publicKey, privateKey } = crypto.generateKeyPairSync("ed25519");
  // The raw 32-byte key is the tail of the SPKI DER encoding.
  const pub = publicKey.export({ format: "der", type: "spki" }).subarray(-32).toString("hex");
  const manifestPath = path.join(dir, "manifest.json");
  fs.writeFileSync(manifestPath, json);
  fs.writeFileSync(`${manifestPath}.sig`, crypto.sign(null, json, privateKey).toString("hex") + "\n");
  return {
    files: { [`/${rev}/${file}`]: bytes },
    env: { NUGGET_E2E_MANIFEST: manifestPath, NUGGET_E2E_MANIFEST_SIG: `${manifestPath}.sig`, NUGGET_E2E_MANIFEST_PUBKEY: pub },
  };
}

export async function aiSetupScenario({ browser }) {
  const bins = locateE2eBinaries(log);
  log(`nuggetd-e2e: ${bins.nuggetd}`);
  const fixtures = fs.mkdtempSync(path.join(os.tmpdir(), "nugget-e2e-ai-"));
  // The manifest's URLs need the server's port, so the server starts first and gets its files after.
  const files = {};
  const served = await startModelServer(files);
  const final = signedTestManifest(fixtures, served.port);
  Object.assign(files, final.files);
  const summary = { model: MODEL_ID };

  let daemon;
  try {
    daemon = await startDaemon(bins.nuggetd, {
      stderrPath: path.join(OUT, "ai-setup-nuggetd.out.log"),
      config: AI_CONFIG,
      env: { ...final.env, NUGGET_E2E_LLAMA_SERVER: bins.server, NUGGET_E2E_RAM_GB: "16", NUGGET_E2E_GPU: "unknown" },
    });
  } catch (e) {
    await served.close().catch(() => {});
    fs.rmSync(fixtures, { recursive: true, force: true });
    throw e;
  }
  let bridge;
  let ctx;
  const pageErrors = [];
  try {
    bridge = await startBridge({ socket: daemon.socket, dist: DIST, pickFolders: [CORPUS] });
    ctx = await newContext(browser);
    const page = await ctx.newPage();
    page.on("pageerror", (e) => pageErrors.push(e.message));
    bridge.deliverEvents(page);
    const heading = (key) => page.getByRole("heading", { level: 1, name: t(key), exact: true });
    const cont = () => page.locator("footer.ob-actions").getByRole("button", { name: exactName("onboarding.continue") }).click();
    const nav = async (key) => {
      await navTo(page, key.replace(/^app\.nav\./, ""));
    };

    await check("ai setup: install the recommended model in onboarding, with progress", async () => {
      try {
        await page.goto(`${bridge.url}index.html`);
        // The fork: run it yourself goes to the AI choice; the folder comes after it.
        await onboardingRunItYourself(page);
        await page.getByRole("radio", { name: startsWith("onboarding.ai.local") }).check();
        const cards = page.locator("li.card");
        const card = cards.filter({ has: page.locator(".card-title", { hasText: new RegExp(`^${MODEL_ID}$`) }) });
        await card.waitFor();
        const titles = (await cards.locator(".card-title").allTextContents()).map((s) => s.trim());
        expect(!titles.includes(BIG_MODEL_ID), `the model too big for 16 GB is offered as recommended: ${JSON.stringify(titles)}`);
        const badges = (await card.locator(".badges").textContent()) ?? "";
        expect(badges.includes(t("onboarding.ai.localBadge")), `the ${MODEL_ID} card has no "${t("onboarding.ai.localBadge")}" badge: "${badges}"`);

        await card.getByRole("button", { name: exactName("onboarding.models.install") }).click();
        const samples = [];
        const installedBadge = t("onboarding.models.installed");
        await until(
          `the ${MODEL_ID} card shows download progress, then "${installedBadge}"`,
          async () => {
            const s = await card.evaluate((li) => ({
              value: li.querySelector("progress") ? Number(li.querySelector("progress").value) : null,
              text: li.querySelector(".progress .small")?.textContent?.trim() ?? null,
              badges: li.querySelector(".badges")?.textContent ?? "",
              error: li.querySelector("[role=alert]")?.textContent?.trim() ?? null,
            }));
            if (s.error) throw new Error(`install failed: ${s.error}`);
            if (s.value !== null) samples.push(s);
            return [s.badges.includes(installedBadge), s];
          },
          60_000,
        );
        const pctRe = tPattern("onboarding.models.progress");
        expect(
          samples.some((s) => s.value > 0 && s.value < 100 && s.text && pctRe.test(s.text)),
          `no partial download progress was shown: ${JSON.stringify(samples.slice(-5))}`,
        );
        expect(served.stats.served >= MODEL_BYTES, `the mock server sent ${served.stats.served} of ${MODEL_BYTES} bytes`);
        await cont();

        await heading("onboarding.source.heading").waitFor();
        await onboardingAddFolder(page, CORPUS);
        await cont();

        for (const h of ["onboarding.perm.heading", "onboarding.trigger.heading"]) {
          await heading(h).waitFor();
          await cont();
        }
        await page.getByRole("heading", { level: 2, name: t("onboarding.done.heading"), exact: true }).waitFor();
        await page.locator("footer.ob-actions").getByRole("button", { name: exactName("onboarding.done.search") }).click();
        await page.locator("main[aria-label]").first().waitFor();
      } catch (e) {
        await shot(page, "FAIL-ai-setup-onboarding");
        throw e;
      }
    });

    await check("ai setup: Chat streams a cited answer from the local model", async () => {
      try {
        await nav("app.nav.chat");
        const sources = page.getByRole("checkbox", { name: startsWith("chat.useSources") });
        if ((await sources.count()) > 0 && !(await sources.isChecked())) await sources.check();
        await page.getByRole("textbox", { name: t("chat.message") }).fill(QUERY);
        await page.getByRole("button", { name: exactName("chat.send") }).click();
        const reply = page.locator("li.msg-assistant").last();
        const got = await until(
          `the reply cites the corpus ([n] buttons) and lists "${EXPECT_TITLE}" among its sources`,
          async () => {
            const r = await reply.evaluate((li) => ({
              text: li.querySelector(".msg-body")?.textContent?.trim() ?? "",
              streaming: !!li.querySelector(".caret"),
              marks: [...li.querySelectorAll(".msg-body button.cite")].map((b) => b.textContent),
              citations: [...li.querySelectorAll("ol.chat-citations:not(.chat-citations-more) li.citation")].map((c) => ({
                n: c.querySelector(".cite-n")?.textContent?.trim() ?? "",
                title: c.querySelector(".citation-title")?.textContent?.trim() ?? "",
              })),
              provider: li.querySelector(".msg-meta")?.firstChild?.textContent?.trim() ?? "",
              error: li.querySelector("[role=alert]")?.textContent?.trim() ?? null,
            }));
            if (r.error) throw new Error(`the reply failed: ${r.error}`);
            const ok =
              !r.streaming &&
              r.text.startsWith(CITED_PREFIX) &&
              r.marks.length > 0 &&
              r.citations.some((c) => c.title.includes(EXPECT_TITLE)) &&
              r.marks.every((m) => r.citations.some((c) => c.n.split(/\s+/).includes(m)));
            return [ok, r];
          },
          60_000,
        );
        summary.answer = got.text;
        expect(got.provider === "local-chat", `the reply came from "${got.provider}", not the local model (local-chat)`);
        const deltas = bridge.notifications.filter((n) => n.method === "chat.delta").length;
        expect(deltas >= 2, `the answer arrived in ${deltas} chat.delta notification(s); expected a stream`);
        await settle(page);
        await shot(page, "ai-setup-chat");
        await expectNoErrorState(page, "Chat after the cited answer");
      } catch (e) {
        await shot(page, "FAIL-ai-setup-chat");
        throw e;
      }
    });

    await check("ai setup: Status shows the local chat runtime ready", async () => {
      const row = () =>
        page.getByRole("region", { name: t("status.localRuntimes") }).locator("tbody tr", { has: page.locator("th code", { hasText: MODEL_ID }) });
      // The page loads runtime.status when opened; reopen it until the server reports ready.
      await until(
        `Status shows ${MODEL_ID} "${t("status.runtime.ready")}"`,
        async () => {
          await nav("app.nav.search");
          await nav("app.nav.status");
          const r = await row().evaluate((tr) => ({ role: tr.cells[1]?.textContent?.trim(), state: tr.cells[2]?.textContent?.trim(), provider: tr.cells[3]?.textContent?.trim() }));
          return [r.role === t("status.role.chat") && r.provider === "local-chat" && r.state === t("status.runtime.ready"), r];
        },
        30_000,
      );
      await expectNoErrorState(page, "Status with the local model");
    });

    await check("ai setup: a Double Tap draft from the local model (draft.begin)", async () => {
      const req = {
        context: {
          app: { id: "org.nugget.e2e.mail", name: "E2E Mail", window_title: "Re: Tidewater Billing" },
          field: { role: "text_area", label: "Message body", value: "" },
          thread: [{ author: "Ada Wexford", text: "Where are we with the Tidewater Billing vendor contract?", from_me: false }],
          recipients: ["ada.wexford@glimmerfen.test"],
          subject: "Re: Tidewater Billing vendor contract",
        },
        instruction: null,
        provider: null,
        max_tokens: null,
      };
      const { result, notes } = await rpcStream(daemon.socket, "draft.begin", req, (r, ns) => ns.some((n) => n.method === "chat.done" && n.params?.stream_id === r.stream_id), 60_000);
      const mine = notes.filter((n) => n.params?.stream_id === result.stream_id);
      const done = mine.find((n) => n.method === "chat.done").params;
      const text = mine.filter((n) => n.method === "chat.delta").map((n) => n.params.text).join("");
      expect(!done.error, `the draft ended with an error: ${JSON.stringify(done.error)}`);
      expect(result.provider === "local-chat", `the draft came from "${result.provider}", not local-chat`);
      expect(text.trim().length > 0, "the draft is empty");
    });

    await check("ai setup: the model download is in the egress ledger", async () => {
      const recent = await rpc(daemon.socket, "egress.recent", { limit: 100 });
      const dl = recent.filter((e) => e.host.startsWith("127.0.0.1") && e.feature === "model_download");
      expect(dl.length > 0, `no model_download row for 127.0.0.1 in egress.recent: ${JSON.stringify(recent.slice(0, 5))}`);
      expect(pageErrors.length === 0, `${pageErrors.length} uncaught page error(s): ${JSON.stringify(pageErrors)}`);
    });
    await checkWire(bridge, "ai setup");
    summary.rpcErrors = bridge.calls.filter((c) => !c.ok);
  } finally {
    await ctx?.close().catch(() => {});
    await bridge?.close().catch(() => {});
    await served.close().catch(() => {});
    await stopDaemon(daemon, "ai-setup");
    fs.rmSync(fixtures, { recursive: true, force: true });
  }
  return summary;
}
