// AI-setup scenario: onboarding with "On this device", the recommended local model installed from
// a local mock download server, then Chat answering from the fixture corpus with [n] citations
// streamed by that model, and a Double Tap draft through the daemon. Times app start to the first
// cited model answer.
//
// Nothing leaves localhost and no real model runs:
// - a test manifest (one recommended chat model, one too big for the machine) signed with a
//   throwaway ed25519 key made for this run; the daemon trusts only that key;
// - `nuggetd-e2e` (crates/nuggetd/src/bin/nuggetd-e2e.rs): the real daemon with the manifest,
//   the fake llama-server and a 16 GB / no-GPU machine given through `NUGGET_E2E_*`;
// - `nuggetd-fake-llama-server` (crates/nugget-runtime/src/bin/fake-llama-server.rs): answers a
//   prompt with numbered passages by quoting passage [1] and citing it;
// - a throttled HTTP server with Range support serving the model bytes (a few MB, so the
//   onboarding card shows progress for a few seconds).
import crypto from "node:crypto";
import fs from "node:fs";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import { startBridge, METHOD_NOT_FOUND } from "./bridge.mjs";
import { CORPUS, locateE2eBinaries, rpc, rpcStream, startDaemon } from "./daemon.mjs";
import {
  check,
  DIST,
  expect,
  expectNoErrorState,
  INDEX_TIMEOUT_MS,
  KEEP,
  log,
  newContext,
  OUT,
  secs,
  settle,
  shot,
  t,
  tPattern,
  until,
} from "./harness.mjs";

export const AI_BUDGET_MS = Number(process.env.E2E_AI_BUDGET_MS || 180_000);

/** The model the manifest recommends for a 16 GB machine, and one that needs more RAM. */
export const MODEL_ID = "e2e-chat-q4";
const BIG_MODEL_ID = "e2e-chat-big-q8";
const MODEL_BYTES = Number(process.env.E2E_MODEL_BYTES || 4 * 1024 * 1024);
/** Download speed of the mock server: the model takes about MODEL_BYTES / this seconds. */
const BYTES_PER_SEC = Number(process.env.E2E_MODEL_BYTES_PER_SEC || 1.5 * 1024 * 1024);

/** Same question and corpus facts as the first run (notes/projects/Tidewater Billing.md). */
const QUERY = "Tidewater Billing vendor contract";
const EXPECT_TITLE = "Tidewater Billing";
/** How the fake llama-server starts a reply to a prompt with passages. */
const CITED_PREFIX = "According to your notes";

/** The daemon config: the local runtime on (never stopped for idleness), no MCP HTTP port. */
export const AI_CONFIG = `# Written by apps/desktop/e2e/ai-setup.mjs.
[runtime]
enabled = true
idle_stop_minutes = 0

[mcp]
http = false
`;

const escapeRe = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const exactName = (key) => new RegExp(`^${escapeRe(t(key))}$`);
const startsWith = (key) => new RegExp(`^${escapeRe(t(key))}`);
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

/**
 * Serve `files` (`/path` -> Buffer) on 127.0.0.1 with Range support, at most `bytesPerSec`.
 * `requests` records each `{ method, path, range, status }`; `served` counts body bytes sent.
 */
export function startModelServer(files, bytesPerSec = BYTES_PER_SEC) {
  const stats = { requests: [], served: 0 };
  const server = http.createServer(async (req, res) => {
    const url = new URL(req.url, "http://127.0.0.1");
    const body = files[url.pathname];
    const range = req.headers.range ?? null;
    const entry = { method: req.method, path: url.pathname, range, status: 0 };
    stats.requests.push(entry);
    if (!body) {
      entry.status = 404;
      res.writeHead(404).end("not found");
      return;
    }
    let start = 0;
    let end = body.length - 1;
    const m = range && /^bytes=(\d+)-(\d*)$/.exec(range);
    if (range && !m) {
      entry.status = 416;
      res.writeHead(416, { "Content-Range": `bytes */${body.length}` }).end();
      return;
    }
    if (m) {
      start = Number(m[1]);
      end = m[2] ? Math.min(Number(m[2]), body.length - 1) : body.length - 1;
      if (start > end) {
        entry.status = 416;
        res.writeHead(416, { "Content-Range": `bytes */${body.length}` }).end();
        return;
      }
    }
    entry.status = m ? 206 : 200;
    res.writeHead(entry.status, {
      "Content-Type": "application/octet-stream",
      "Content-Length": end - start + 1,
      "Accept-Ranges": "bytes",
      ...(m ? { "Content-Range": `bytes ${start}-${end}/${body.length}` } : {}),
    });
    if (req.method === "HEAD") return res.end();
    // Throttle: 64 KiB slices, paced to bytesPerSec.
    const slice = 64 * 1024;
    const t0 = Date.now();
    let sent = 0;
    for (let at = start; at <= end && !res.destroyed; at += slice) {
      const chunk = body.subarray(at, Math.min(at + slice, end + 1));
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
      resolve({
        port: server.address().port,
        stats,
        close: () => new Promise((r) => (server.closeAllConnections?.(), server.close(() => r()))),
      }),
    );
  });
}

/**
 * Write a manifest for the mock server on `port`, sign it with a fresh ed25519 key and return
 * the files, the model bytes and the `NUGGET_E2E_*` environment for `nuggetd-e2e`.
 */
export function signedTestManifest(dir, port) {
  const bytes = modelBytes(MODEL_BYTES);
  const rev = sha1("nugget-e2e-chat-model");
  const bigRev = sha1("nugget-e2e-chat-model-big");
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
      // Too big for the 16 GB machine: listed, never recommended, never downloaded.
      entry(BIG_MODEL_ID, bigRev, "E2E-Chat-Big-Q8_0.gguf", 40 * 1024 ** 3, sha256(Buffer.from("never served")), 64),
    ],
  };
  const json = Buffer.from(JSON.stringify(manifest, null, 2) + "\n");
  const { publicKey, privateKey } = crypto.generateKeyPairSync("ed25519");
  const sig = crypto.sign(null, json, privateKey);
  // The raw 32-byte key is the tail of the SPKI DER encoding.
  const pub = publicKey.export({ format: "der", type: "spki" }).subarray(-32).toString("hex");
  const manifestPath = path.join(dir, "manifest.json");
  const sigPath = path.join(dir, "manifest.json.sig");
  fs.writeFileSync(manifestPath, json);
  fs.writeFileSync(sigPath, sig.toString("hex") + "\n");
  return {
    manifest,
    files: { [`/${rev}/${file}`]: bytes },
    env: { NUGGET_E2E_MANIFEST: manifestPath, NUGGET_E2E_MANIFEST_SIG: sigPath, NUGGET_E2E_MANIFEST_PUBKEY: pub },
  };
}

/**
 * Run the scenario with `browser` (Playwright). Builds or locates `nuggetd-e2e` and the fake
 * llama-server first (outside the clock). Returns the summary for summary.json (`first_run_ai`).
 */
export async function aiSetupScenario({ browser }) {
  const bins = locateE2eBinaries(log);
  log(`nuggetd-e2e: ${bins.nuggetd}`);
  const fixtures = fs.mkdtempSync(path.join(os.tmpdir(), "nugget-e2e-ai-"));
  // The manifest's URLs need the server's port, so the server starts first and gets its files after.
  const files = {};
  const served = await startModelServer(files);
  const final = signedTestManifest(fixtures, served.port);
  Object.assign(files, final.files);

  const summary = {
    budget_ms: AI_BUDGET_MS,
    query: QUERY,
    model: MODEL_ID,
    model_bytes: MODEL_BYTES,
    time_to_cited_model_answer_ms: null,
    marks: {},
    steps: {},
  };
  const t0 = Date.now();
  let prev = 0;
  const mark = (name) => {
    const at = Date.now() - t0;
    summary.marks[name] = at;
    summary.steps[name] = at - prev;
    prev = at;
    log(`  ai setup: ${name} at ${secs(at)} (+${secs(summary.steps[name])})`);
  };

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
  mark("daemon_ready");
  log(`ai setup: daemon home ${daemon.home}`);
  let bridge;
  let ctx;
  const pageErrors = [];
  const consoleErrors = [];
  try {
    bridge = await startBridge({ socket: daemon.socket, dist: DIST, pickFolders: [CORPUS] });
    ctx = await newContext(browser);
    const page = await ctx.newPage();
    page.on("pageerror", (e) => pageErrors.push(e.message));
    page.on("console", (m) => m.type() === "error" && consoleErrors.push(m.text()));
    // Install progress and streamed answers arrive as events, like in the shell.
    bridge.deliverEvents(page);
    const heading = (key) => page.getByRole("heading", { level: 1, name: t(key), exact: true });
    const cont = () => page.locator("footer.ob-actions").getByRole("button", { name: exactName("onboarding.continue") }).click();
    const nav = async (key) => {
      await page.locator("nav .nav-item", { hasText: exactName(key) }).first().click();
      await page.locator(`main[aria-label="${t(key)}"]`).waitFor();
    };

    await check("ai setup: install the recommended model in onboarding, with progress", async () => {
      try {
        await page.goto(`${bridge.url}index.html`);
        await heading("onboarding.welcome.heading").waitFor();
        mark("app_open");
        await cont();

        await heading("onboarding.ai.heading").waitFor();
        await page.getByRole("radio", { name: startsWith("onboarding.ai.local") }).check();
        const cards = page.locator("li.card");
        const card = cards.filter({ has: page.locator(".card-title", { hasText: new RegExp(`^${escapeRe(MODEL_ID)}$`) }) });
        await card.waitFor();
        const titles = await until("the AI step lists the recommended model only", async () => {
          const shown = await cards.locator(".card-title").allTextContents();
          return [shown.map((s) => s.trim()).includes(MODEL_ID), shown];
        });
        expect(!titles.includes(BIG_MODEL_ID), `the model too big for 16 GB is offered as recommended: ${JSON.stringify(titles)}`);
        const badges = (await card.locator(".badges").textContent()) ?? "";
        expect(badges.includes(t("onboarding.ai.localBadge")), `the ${MODEL_ID} card has no "${t("onboarding.ai.localBadge")}" badge: "${badges}"`);
        await shot(page, "ai-setup-models");

        await card.getByRole("button", { name: exactName("onboarding.models.install") }).click();
        mark("install_clicked");
        // Watch the card until it says Installed; record every progress state it showed.
        const samples = [];
        const installedBadge = t("onboarding.models.installed");
        let shotTaken = false;
        await until(
          `the ${MODEL_ID} card shows download progress, then "${installedBadge}"`,
          async () => {
            const s = await card.evaluate((li) => {
              const p = li.querySelector("progress");
              return {
                value: p ? Number(p.value) : null,
                text: li.querySelector(".progress .small")?.textContent?.trim() ?? null,
                badges: li.querySelector(".badges")?.textContent ?? "",
                error: li.querySelector("[role=alert]")?.textContent?.trim() ?? null,
              };
            });
            if (s.error) throw new Error(`install failed: ${s.error}`);
            const last = samples[samples.length - 1];
            if (s.value !== null && (!last || last.value !== s.value || last.text !== s.text)) {
              samples.push({ value: s.value, text: s.text, at_ms: Date.now() - t0 });
              if (!shotTaken && s.value > 0) {
                shotTaken = true;
                await shot(page, "ai-setup-downloading");
              }
            }
            return [s.badges.includes(installedBadge), { last: samples[samples.length - 1], badges: s.badges }];
          },
          Math.max(60_000, (MODEL_BYTES / BYTES_PER_SEC) * 1000 * 4),
        );
        mark("model_installed");
        summary.progress = samples;
        const partial = samples.filter((s) => s.value > 0 && s.value < 100);
        const pctRe = tPattern("onboarding.models.progress");
        expect(
          partial.length > 0 && partial.some((s) => s.text && pctRe.test(s.text)),
          `no partial download progress was shown (a <progress> between 0 and 100 with "${t("onboarding.models.progress")}"): ${JSON.stringify(samples)}`,
        );
        summary.download = { requests: served.stats.requests.length, bytes_served: served.stats.served };
        expect(served.stats.served >= MODEL_BYTES, `the mock server sent ${served.stats.served} of ${MODEL_BYTES} bytes`);
        log(`  ai setup: ${samples.length} progress states, ${partial.length} partial; ${served.stats.requests.length} download request(s)`);
        await shot(page, "ai-setup-installed");
        await cont();
        mark("ai_step");

        // Source step, as in the first run.
        await heading("onboarding.source.heading").waitFor();
        await page.getByRole("button", { name: exactName("onboarding.source.addFolder") }).click();
        const dialog = page.locator("dialog[open]");
        await dialog.getByRole("button", { name: exactName("addSource.folder.choose") }).click();
        await until("the picked folder is listed in the wizard", async () => {
          const paths = await dialog.locator(".path-list code").allTextContents();
          return [paths.includes(CORPUS), paths];
        });
        await dialog.getByRole("button", { name: exactName("addSource.folder.submit") }).click();
        await dialog.waitFor({ state: "detached" });
        const readyRe = tPattern("onboarding.source.ready");
        await until(
          "the source step says the folder is indexed and search works",
          async () => {
            const row = await page.locator(".ob-index li").first().evaluate((li) => ({ phase: li.dataset.phase, text: li.textContent.trim() }));
            if (row.phase === "failed") throw new Error(`indexing failed: ${row.text}`);
            return [row.phase === "ready" && readyRe.test(row.text), row];
          },
          INDEX_TIMEOUT_MS,
        );
        mark("source_searchable");
        await cont();
        await heading("onboarding.perm.heading").waitFor();
        await cont();
        await heading("onboarding.trigger.heading").waitFor();
        await cont();
        await heading("onboarding.done.heading").waitFor();
        await shot(page, "ai-setup-done");
        const finish = new RegExp(`^(${escapeRe(t("onboarding.done.search"))}|${escapeRe(t("onboarding.done.start"))})$`);
        await page.locator("footer.ob-actions").getByRole("button", { name: finish }).click();
        await page.locator("main[aria-label]").first().waitFor();
        mark("setup_done");
      } catch (e) {
        await shot(page, "FAIL-ai-setup-onboarding");
        throw e;
      }
    });

    const runtimeRow = () =>
      page.getByRole("region", { name: t("status.localRuntimes") }).locator("tbody tr", { has: page.locator("th code", { hasText: MODEL_ID }) });
    const rowState = async () =>
      runtimeRow().evaluate((tr) => ({ role: tr.cells[1]?.textContent?.trim(), state: tr.cells[2]?.textContent?.trim(), provider: tr.cells[3]?.textContent?.trim() }));

    await check("ai setup: Status lists the installed model's local runtime", async () => {
      try {
        await nav("app.nav.status");
        const row = await until(`Status lists ${MODEL_ID} as the chat runtime`, async () => {
          const r = await rowState();
          return [r.role === t("status.role.chat") && r.provider === "local-chat", r];
        });
        summary.status_before_chat = row;
        log(`  ai setup: Status before the first chat: ${JSON.stringify(row)}`);
        await settle(page);
        await shot(page, "ai-setup-status");
      } catch (e) {
        await shot(page, "FAIL-ai-setup-status");
        throw e;
      }
    });

    await check("ai setup: Chat streams a cited answer from the local model", async () => {
      try {
        await nav("app.nav.chat");
        const sources = page.getByRole("checkbox", { name: startsWith("chat.useSources") });
        if ((await sources.count()) > 0 && !(await sources.isChecked())) await sources.check();
        const input = page.getByRole("textbox", { name: t("chat.message") });
        await input.fill(QUERY);
        await page.getByRole("button", { name: exactName("chat.send") }).click();
        const reply = page.locator("li.msg-assistant").last();
        // Seen mid-stream at least once: the reply grows in several deltas.
        const lengths = new Set();
        const got = await until(
          `the reply cites the corpus ([n] buttons) and lists "${EXPECT_TITLE}" among its sources`,
          async () => {
            const r = await reply.evaluate((li) => ({
              text: li.querySelector(".msg-body")?.textContent?.trim() ?? "",
              streaming: !!li.querySelector(".caret"),
              marks: [...li.querySelectorAll(".msg-body button.cite")].map((b) => b.textContent),
              // Cited sources only (uncited passages sit behind "more", not rendered until opened).
              citations: [...li.querySelectorAll("ol.chat-citations:not(.chat-citations-more) li.citation")].map((c) => ({
                n: c.querySelector(".cite-n")?.textContent?.trim() ?? "",
                title: c.querySelector(".citation-title")?.textContent?.trim() ?? "",
              })),
              provider: li.querySelector(".msg-meta")?.firstChild?.textContent?.trim() ?? "",
              error: li.querySelector("[role=alert]")?.textContent?.trim() ?? null,
            }));
            if (r.error) throw new Error(`the reply failed: ${r.error}`);
            lengths.add(r.text.length);
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
        mark("cited_model_answer");
        summary.time_to_cited_model_answer_ms = summary.marks.cited_model_answer;
        summary.from_app_open_ms = summary.marks.cited_model_answer - summary.marks.app_open;
        summary.answer = got.text;
        summary.answer_provider = got.provider;
        summary.citations = got.citations.slice(0, 5);
        expect(got.provider === "local-chat", `the reply came from "${got.provider}", not the local model (local-chat)`);
        const deltas = bridge.notifications.filter((n) => n.method === "chat.delta");
        expect(deltas.length >= 2, `the answer arrived in ${deltas.length} chat.delta notification(s); expected a stream`);
        summary.chat_deltas = deltas.length;
        log(`  ai setup: "${got.text}" (${deltas.length} deltas; sources ${got.citations.map((c) => `${c.n} ${c.title}`).join(", ")})`);
        await settle(page);
        await shot(page, "ai-setup-chat");
        await expectNoErrorState(page, "Chat after the cited answer");
      } catch (e) {
        await shot(page, "FAIL-ai-setup-chat");
        throw e;
      }
    });

    await check(`ai setup: a cited model answer within ${secs(AI_BUDGET_MS)} of app start`, async () => {
      expect(summary.time_to_cited_model_answer_ms !== null, "never reached a cited model answer");
      expect(
        summary.time_to_cited_model_answer_ms <= AI_BUDGET_MS,
        `took ${secs(summary.time_to_cited_model_answer_ms)} (budget ${secs(AI_BUDGET_MS)}): ${JSON.stringify(summary.steps)}`,
      );
    });

    await check("ai setup: Status shows the local chat runtime ready after the first chat", async () => {
      try {
        // The page loads runtime.status when opened; reopen it until the server reports ready.
        const row = await until(
          `Status shows ${MODEL_ID} "${t("status.runtime.ready")}"`,
          async () => {
            await nav("app.nav.search");
            await nav("app.nav.status");
            const r = await rowState();
            return [r.state === t("status.runtime.ready"), r];
          },
          30_000,
        );
        summary.status_after_chat = row;
        await settle(page);
        await shot(page, "ai-setup-status-ready");
        await expectNoErrorState(page, "Status with the local model");
      } catch (e) {
        await shot(page, "FAIL-ai-setup-status-ready");
        throw e;
      }
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
      const { result, notes } = await rpcStream(
        daemon.socket,
        "draft.begin",
        req,
        (r, ns) => ns.some((n) => n.method === "chat.done" && n.params?.stream_id === r.stream_id),
        60_000,
      );
      const mine = notes.filter((n) => n.params?.stream_id === result.stream_id);
      const done = mine.find((n) => n.method === "chat.done").params;
      const text = mine.filter((n) => n.method === "chat.delta").map((n) => n.params.text).join("");
      summary.draft = { provider: result.provider, locality: result.locality, field_kind: result.field_kind, citations: result.citations?.length ?? 0, text };
      log(`  ai setup: draft from ${result.provider} (${result.locality}, ${result.field_kind}): "${text}"`);
      expect(!done.error, `the draft ended with an error: ${JSON.stringify(done.error)}`);
      expect(result.provider === "local-chat", `the draft came from "${result.provider}", not local-chat`);
      expect(text.trim().length > 0, "the draft is empty");
    });

    await check("ai setup: the model download is in the egress ledger", async () => {
      const recent = await rpc(daemon.socket, "egress.recent", { limit: 100 });
      const dl = recent.filter((e) => (e.host === "127.0.0.1" || e.host.startsWith("127.0.0.1:")) && e.feature === "model_download");
      expect(dl.length > 0, `no model_download row for 127.0.0.1 in egress.recent: ${JSON.stringify(recent.slice(0, 5))}`);
    });

    await check("ai setup: every daemon command the UI called exists and accepted its params", async () => {
      const bad = bridge.calls.filter((c) => c.via === "daemon" && (c.code === METHOD_NOT_FOUND || c.code === -32602));
      expect(bad.length === 0, `wire mismatches: ${JSON.stringify(bad)}`);
      expect(pageErrors.length === 0, `${pageErrors.length} uncaught page error(s): ${JSON.stringify(pageErrors)}`);
    });

    summary.rpcErrors = bridge.calls.filter((c) => !c.ok);
    summary.pageErrors = pageErrors;
    summary.consoleErrors = consoleErrors;
    summary.commands = [...new Set(bridge.calls.map((c) => c.cmd))].sort();
  } finally {
    await ctx?.close().catch(() => {});
    await bridge?.close().catch(() => {});
    await served.close().catch(() => {});
    fs.cpSync(daemon.logDir, path.join(OUT, "ai-setup-daemon-logs"), { recursive: true });
    await daemon.stop({ keep: KEEP });
    if (KEEP) log(`kept the ai-setup daemon home: ${daemon.home}`);
    fs.rmSync(fixtures, { recursive: true, force: true });
  }

  if (summary.time_to_cited_model_answer_ms !== null) {
    const parts = Object.entries(summary.steps).map(([k, v]) => `${k} +${secs(v)}`).join(", ");
    log(`ai setup: cited model answer ${secs(summary.time_to_cited_model_answer_ms)} after app start: ${parts}`);
  }
  return summary;
}
