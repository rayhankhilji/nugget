#!/usr/bin/env node
// Documentation screenshots, taken from the real app: the real nuggetd indexes the fictional
// corpus in bench/fixtures/knowledge/items and the real desktop frontend (dist/) runs in headless
// Chromium against it through the e2e bridge (see e2e/README.md). Every shot is taken twice, with
// `prefers-color-scheme: light` and `dark`, and written to docs/book/src/images/screenshots/ as
// `<name>-light.png` and `<name>-dark.png`. The README and the user guide embed them.
//
// Refreshing them (after a UI change):
//
//   cd apps/desktop
//   pnpm run screenshots          # ~1 min once nuggetd is built; rebuilds dist/ first
//
// Then look at the changed PNGs (git diff --stat docs/book/src/images/screenshots) and commit them.
// The run is idempotent: it deletes the PNGs of the set it writes before taking new ones.
//
// The Chat shot needs a model. No network is used: a tiny OpenAI-compatible server on 127.0.0.1
// (below) answers with a fixed, cited reply built from the passages the daemon sends it, and the
// daemon's temp home configures it as the `local` provider for the synthesis tier. Its deltas
// reach the page the way the shell delivers them (the `nugget:chat-delta` / `nugget:chat-done`
// events), which the plain e2e bridge does not do.
//
// Environment:
//   NUGGETD           a built nuggetd (default: `cargo build -p nuggetd`, as for `pnpm run e2e`)
//   E2E_CHROMIUM      Chromium executable, if Playwright's own download is not installed
//   SHOTS_OUT         output directory (default docs/book/src/images/screenshots)
//   SHOTS_ONLY        comma-separated shot names to take (default: all); others are left alone
//   SHOTS_BUILD=0     use the existing dist/ instead of running `vite build`
//   SHOTS_NOW         the time the browser sees (ISO 8601; default: the time the run started).
//                     The daemon uses the real clock, so the Today page reflects the run date.
//   E2E_INDEX_TIMEOUT_MS, E2E_STEP_TIMEOUT_MS   as for `pnpm run e2e`
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import http from "node:http";
import net from "node:net";
import os from "node:os";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { startBridge } from "./bridge.mjs";
import { REPO, addCorpusSource, countFiles, CORPUS, locateNuggetd, rpc, startDaemon, waitForKnowledge } from "./daemon.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const DESKTOP = path.resolve(here, "..");
const DIST = path.join(DESKTOP, "dist");
const OUT = path.resolve(process.env.SHOTS_OUT || path.join(REPO, "docs", "book", "src", "images", "screenshots"));
const ONLY = new Set((process.env.SHOTS_ONLY || "").split(",").map((s) => s.trim()).filter(Boolean));
const INDEX_TIMEOUT_MS = Number(process.env.E2E_INDEX_TIMEOUT_MS || 180_000);
const STEP_TIMEOUT_MS = Number(process.env.E2E_STEP_TIMEOUT_MS || 15_000);
const VIEWPORT = { width: 1280, height: 800 };
const THEMES = ["light", "dark"];

const started = Date.now();
const NOW = process.env.SHOTS_NOW ? new Date(process.env.SHOTS_NOW) : new Date(started);
const secs = (ms) => `${(ms / 1000).toFixed(1)}s`;
const log = (...a) => console.log(`[shots ${secs(Date.now() - started)}]`, ...a);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const en = JSON.parse(fs.readFileSync(path.join(DESKTOP, "src", "i18n", "locales", "en.json"), "utf8"));
function t(key) {
  const s = en[key];
  if (typeof s !== "string") throw new Error(`no English string for ${key} in src/i18n/locales/en.json`);
  return s;
}
const escapeRe = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/** Poll `probe` (returning `[ok, observed]`) until ok, or throw with `what` and the last observation. */
async function until(what, probe, timeoutMs = STEP_TIMEOUT_MS) {
  const deadline = Date.now() + timeoutMs;
  let observed;
  for (;;) {
    let ok = false;
    try {
      [ok, observed] = await probe();
    } catch (e) {
      observed = `error: ${e.message.split("\n")[0]}`;
    }
    if (ok) return observed;
    if (Date.now() > deadline) throw new Error(`${what}; after ${timeoutMs} ms saw: ${JSON.stringify(observed)}`);
    await sleep(200);
  }
}

async function settle(page) {
  await page.waitForLoadState("networkidle", { timeout: 10_000 }).catch(() => {});
  await page.waitForFunction(() => !document.querySelector('[aria-busy="true"]'), null, { timeout: 8_000 }).catch(() => {});
  await page.evaluate(() => document.fonts?.ready).catch(() => {});
}

// ---- a fake OpenAI-compatible model on 127.0.0.1 ----

/**
 * The reply to the documented question, citing the passages the daemon numbered for it. Falls
 * back to the first passages when the expected ones are missing, so the shot still has citations.
 */
function cannedAnswer(system) {
  const passages = new Map();
  const body = system.split(/<passages>\n/)[1] ?? "";
  let current = null;
  for (const line of body.split("\n")) {
    const m = line.match(/^\s*\[(\d+)\]/);
    if (m) {
      current = m[1];
      passages.set(current, "");
    }
    if (current) passages.set(current, passages.get(current) + line + "\n");
  }
  const find = (re, fallback) => [...passages].find(([, text]) => re.test(text))?.[0] ?? fallback;
  const ids = [...passages.keys()];
  const project = find(/Ines Halloran|Billing system|Vendor contract/i, ids[0] ?? "1");
  const budget = find(/funded|Q4 budget/i, ids[1] ?? project);
  return (
    `Yes. Tidewater Billing is funded in the Q4 budget, together with Harbourline and the Regatta ` +
    `Fundraiser float [${budget}].\n\n` +
    `It is an active Glimmerfen Labs project owned by Ines Halloran. The billing vendor's contract ` +
    `is under review by Vellum & Thistle LLP, with Lucia Penhallow as your lawyer [${project}].`
  );
}

function startFakeModel({ logFile } = {}) {
  const server = http.createServer((req, res) => {
    let body = "";
    req.setEncoding("utf8");
    req.on("data", (c) => (body += c));
    req.on("end", () => {
      const url = new URL(req.url, "http://localhost");
      if (req.method === "GET" && url.pathname.endsWith("/models")) {
        res.writeHead(200, { "Content-Type": "application/json" }).end(JSON.stringify({ object: "list", data: [{ id: "docs-demo", object: "model" }] }));
        return;
      }
      if (req.method !== "POST" || !url.pathname.endsWith("/chat/completions")) {
        res.writeHead(404, { "Content-Type": "application/json" }).end(JSON.stringify({ error: { message: "not found" } }));
        return;
      }
      let request = {};
      try {
        request = JSON.parse(body);
      } catch {}
      if (logFile) fs.appendFileSync(logFile, JSON.stringify(request) + "\n");
      const system = (request.messages ?? [])
        .filter((m) => m.role === "system")
        .map((m) => (typeof m.content === "string" ? m.content : (m.content ?? []).map((p) => p.text ?? "").join("")))
        .join("\n");
      const text = /<passages>/.test(system) ? cannedAnswer(system) : "Hello! Ask me about your notes, mail and calendar.";
      const usage = { prompt_tokens: 512, completion_tokens: 64, total_tokens: 576 };
      if (!request.stream) {
        res.writeHead(200, { "Content-Type": "application/json" }).end(
          JSON.stringify({
            id: "chatcmpl-docs",
            object: "chat.completion",
            created: 0,
            model: "docs-demo",
            choices: [{ index: 0, message: { role: "assistant", content: text }, finish_reason: "stop" }],
            usage,
          }),
        );
        return;
      }
      res.writeHead(200, { "Content-Type": "text/event-stream", "Cache-Control": "no-cache" });
      const chunk = (delta, finish = null, extra = {}) =>
        res.write(
          `data: ${JSON.stringify({ id: "chatcmpl-docs", object: "chat.completion.chunk", created: 0, model: "docs-demo", choices: [{ index: 0, delta, finish_reason: finish }], ...extra })}\n\n`,
        );
      chunk({ role: "assistant", content: "" });
      for (const piece of text.match(/\S+\s*/g) ?? []) chunk({ content: piece });
      chunk({}, "stop");
      res.write(`data: ${JSON.stringify({ id: "chatcmpl-docs", object: "chat.completion.chunk", created: 0, model: "docs-demo", choices: [], usage })}\n\n`);
      res.end("data: [DONE]\n\n");
    });
  });
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => resolve({ port: server.address().port, close: () => new Promise((r) => server.close(() => r())) }));
  });
}

/**
 * startDaemon() writes the e2e config and starts `bin --home <home>`. To add the docs-only
 * settings without forking it, `bin` is a small wrapper that appends them to that config first.
 */
function wrapperFor(realBin, extraConfig, dir) {
  const extra = path.join(dir, "extra-config.toml");
  fs.writeFileSync(extra, extraConfig);
  const wrapper = path.join(dir, "nuggetd-docs");
  const q = (s) => `'${s.replace(/'/g, `'\\''`)}'`;
  fs.writeFileSync(
    wrapper,
    `#!/bin/sh\n# Written by apps/desktop/e2e/screenshots.mjs: append the docs settings, then run nuggetd.\n` +
      `[ "$1" = "--home" ] && cat ${q(extra)} >> "$2/config/config.toml"\nexec ${q(realBin)} "$@"\n`,
    { mode: 0o755 },
  );
  return wrapper;
}

// ---- streamed commands: the shell's event delivery, which the plain bridge leaves out ----

/** Installed after tauri-shim.js: remembers `listen` handlers so the test can emit events. */
function eventShim() {
  const internals = window.__TAURI_INTERNALS__;
  const callbacks = new Map();
  const listeners = new Map();
  const transform = internals.transformCallback.bind(internals);
  internals.transformCallback = (cb, once) => {
    const id = transform(cb, once);
    callbacks.set(id, cb);
    return id;
  };
  const invoke = internals.invoke.bind(internals);
  internals.invoke = async (cmd, args, opts) => {
    if (cmd === "plugin:event|listen") {
      const id = await invoke(cmd, args, opts);
      const list = listeners.get(args.event) ?? [];
      list.push({ id, handler: args.handler });
      listeners.set(args.event, list);
      return id;
    }
    if (cmd === "plugin:event|unlisten") {
      const list = listeners.get(args.event) ?? [];
      listeners.set(
        args.event,
        list.filter((l) => l.id !== args.eventId),
      );
    }
    return invoke(cmd, args, opts);
  };
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
    unregisterListener(event, id) {
      listeners.set(
        event,
        (listeners.get(event) ?? []).filter((l) => l.id !== id),
      );
    },
  };
  window.__docsEmit = (event, payload) => {
    for (const l of listeners.get(event) ?? []) callbacks.get(l.handler)?.({ event, id: l.id, payload });
  };
}

const STREAMED = { retrieve_ask: "retrieve.ask", chat: "chat" };
const EVENTS = { "chat.delta": "nugget:chat-delta", "chat.done": "nugget:chat-done" };

/**
 * Sends a streamed command on its own daemon connection and keeps it open, forwarding the
 * stream's notifications to the page as the shell's events until `chat.done`.
 */
function streamedCall(socket, method, params, page) {
  return new Promise((resolve) => {
    const conn = net.connect(socket);
    const id = 1_000_000 + Math.floor(Math.random() * 1_000_000);
    let buf = "";
    let replied = false;
    const pending = [];
    const emit = (event, payload) => page.evaluate(([e, p]) => window.__docsEmit?.(e, p), [event, payload]).catch(() => {});
    const timer = setTimeout(() => conn.destroy(), 60_000);
    conn.setEncoding("utf8");
    conn.on("connect", () => conn.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n"));
    conn.on("data", async (chunk) => {
      buf += chunk;
      let nl;
      while ((nl = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, nl);
        buf = buf.slice(nl + 1);
        if (!line.trim()) continue;
        let msg;
        try {
          msg = JSON.parse(line);
        } catch {
          continue;
        }
        if (msg.id === id) {
          replied = true;
          resolve(msg.error ? { err: { kind: "rpc", code: msg.error.code, message: msg.error.message } } : { ok: msg.result ?? null });
          // Let the page attach the stream id first; the Chat page also buffers early deltas.
          setTimeout(() => {
            for (const [e, p] of pending.splice(0)) emit(e, p);
          }, 150);
          if (msg.error) conn.destroy();
          continue;
        }
        const event = EVENTS[msg.method];
        if (!event) continue;
        if (replied) await emit(event, msg.params);
        else pending.push([event, msg.params]);
        if (msg.method === "chat.done") {
          setTimeout(() => {
            clearTimeout(timer);
            conn.destroy();
          }, 500);
        }
      }
    });
    conn.on("error", (e) => {
      if (!replied) resolve({ err: { kind: "disconnected", message: e.message } });
    });
  });
}

// ---- the shots ----

const ONBOARDED = { version: 1, step: "done", completed: [], skipped: [], ai: null, finished: true };

/** Each shot opens `page` (a PAGES id) and prepares the view; `onboarding: true` shows the first-run flow. */
const SHOTS = [
  {
    name: "welcome",
    page: "today",
    onboarding: true,
    async prepare(p) {
      await p.getByRole("heading", { name: t("onboarding.welcome.heading") }).waitFor();
    },
  },
  {
    name: "today",
    page: "today",
    async prepare(p) {
      await p.getByText(new RegExp(`^${escapeRe(t("today.brief.title"))}`)).first().waitFor();
    },
  },
  {
    name: "search",
    page: "search",
    async prepare(p) {
      const box = p.getByRole("searchbox", { name: t("search.query") });
      await box.fill("Tidewater budget");
      await box.press("Enter");
      await until("search results", async () => {
        const n = await p.locator("ol.results li.result").count();
        return [n >= 3, n];
      });
      await box.blur();
    },
  },
  {
    name: "chat",
    page: "chat",
    async prepare(p) {
      await until("a provider is listed", async () => {
        const v = await p.getByText(t("chat.noProviders")).isVisible();
        const opts = await p.locator("select option").count();
        return [!v && opts > 0, { noProviders: v, opts }];
      });
      const input = p.getByPlaceholder(t("chat.placeholder"));
      await input.fill("Is Tidewater Billing funded, and who owns it?");
      await p.getByRole("button", { name: t("chat.send"), exact: true }).click();
      await until("the cited answer finished streaming", async () => {
        const log = p.getByRole("log");
        const busy = await log.getAttribute("aria-busy");
        const text = (await log.textContent()) ?? "";
        return [busy !== "true" && text.includes("Lucia Penhallow"), { busy, text: text.slice(-200) }];
      }, 30_000);
      await p.mouse.move(0, VIEWPORT.height - 1);
    },
  },
  {
    name: "knowledge",
    page: "knowledge",
    async prepare(p) {
      await openAda(p);
    },
  },
  {
    name: "knowledge-why",
    page: "knowledge",
    async prepare(p) {
      const detail = await openAda(p);
      const why = detail.getByRole("button", { name: new RegExp(`^${escapeRe(t("entity.why"))}`) });
      const n = await why.count();
      const cal = detail.locator("ul.evidence > li", { has: p.locator(".evidence-title", { hasText: "Wren Mobile usability test" }) }).first();
      for (let i = 0; i < n && !(await cal.isVisible().catch(() => false)); i++) {
        const b = why.nth(i);
        if ((await b.getAttribute("aria-expanded")) !== "true") await b.click();
        await settle(p);
        // Keep only the fact that cites the calendar event open, so the shot is about one answer.
        if (!(await cal.isVisible().catch(() => false))) await b.click();
      }
      await cal.waitFor();
      await cal.locator("blockquote.evidence-quote").waitFor();
      await cal.evaluate((el) => el.scrollIntoView({ block: "center" }));
    },
  },
  {
    name: "graph",
    page: "knowledge",
    async prepare(p) {
      await p.getByRole("tab", { name: t("knowledge.tab.graph") }).click();
      const canvas = p.locator("canvas.graph-canvas");
      await until("the graph is drawn", async () => {
        const label = (await canvas.getAttribute("aria-label", { timeout: 1000 })) ?? "";
        return [Number(label.match(/(\d+)\s+entit/)?.[1] ?? 0) > 0, label];
      });
      await sleep(3000); // let the force layout cool down, then frame it
      await p.getByRole("button", { name: t("graph.fit"), exact: true }).click();
      await sleep(1000);
    },
  },
  {
    name: "sources",
    page: "sources",
    async prepare(p) {
      await until("the corpus source lists its items", async () => {
        const meta = await p.locator("li.source p.source-meta").allTextContents();
        return [meta.some((m) => /\d+\s+items?/.test(m)), meta];
      });
    },
  },
  {
    name: "safety",
    page: "safety",
    async prepare(p) {
      const region = p.getByRole("region", { name: t("safety.defaults.title") });
      await until("the built-in rules table", async () => {
        const n = await region.locator("table tbody tr").count();
        return [n > 0, n];
      });
    },
  },
  { name: "double-tap", page: "doubletap" },
  {
    name: "status",
    page: "status",
    async prepare(p, { expectedItems }) {
      await until(`the Items card shows ${expectedItems}`, async () => {
        const dd = p.locator("dt", { hasText: new RegExp(`^${escapeRe(t("status.items"))}$`) }).first().locator("xpath=following-sibling::dd[1]");
        const v = (await dd.textContent({ timeout: 1000 }))?.trim();
        return [v === String(expectedItems), v];
      });
    },
  },
];

async function openAda(p) {
  const list = p.getByRole("region", { name: t("knowledge.entitiesLabel") });
  await list.getByRole("button", { name: /^Ada Wexford\b/ }).first().click();
  const detail = p.locator("section.entity-detail");
  await detail.getByRole("heading", { name: "Ada Wexford" }).first().waitFor();
  await until("Ada Wexford shows facts", async () => {
    const n = await detail.getByRole("button", { name: new RegExp(`^${escapeRe(t("entity.why"))}`) }).count();
    return [n > 0, n];
  });
  return detail;
}

// ---- main ----

async function main() {
  const require = createRequire(import.meta.url);
  const { chromium } = require("playwright");

  if (process.env.SHOTS_BUILD !== "0" || !fs.existsSync(path.join(DIST, "index.html"))) {
    log("building the frontend (vite build)…");
    const r = spawnSync("pnpm", ["exec", "vite", "build", "--logLevel", "warn"], { cwd: DESKTOP, stdio: "inherit" });
    if (r.status !== 0) throw new Error(`vite build failed (exit ${r.status ?? r.error?.message})`);
  }

  const shots = SHOTS.filter((s) => ONLY.size === 0 || ONLY.has(s.name));
  if (ONLY.size && shots.length !== ONLY.size) throw new Error(`unknown shot in SHOTS_ONLY; known: ${SHOTS.map((s) => s.name).join(", ")}`);
  fs.mkdirSync(OUT, { recursive: true });
  for (const s of shots) for (const theme of THEMES) fs.rmSync(path.join(OUT, `${s.name}-${theme}.png`), { force: true });

  const work = fs.mkdtempSync(path.join(os.tmpdir(), "nugget-shots-"));
  const model = await startFakeModel({ logFile: process.env.SHOTS_DEBUG ? path.join(work, "model-requests.jsonl") : undefined });
  const extraConfig = `
# Appended by apps/desktop/e2e/screenshots.mjs: a fake local model for the Chat shot, and nothing
# that would send it other work (the knowledge extractor and the card judge stay deterministic).
[knowledge.llm]
enabled = false

[proactive]
judge = "off"

[[providers]]
id = "local"
kind = "openai_compatible"
base_url = "http://127.0.0.1:${model.port}/v1"
model = "docs-demo"
tiers = ["synthesis"]
stream_usage = true
llama_cpp = false
`;
  const bin = wrapperFor(locateNuggetd(log), extraConfig, work);
  const expectedItems = countFiles(CORPUS);
  const daemon = await startDaemon(bin, { stderrPath: path.join(work, "nuggetd.out.log") });
  log(`daemon ready, home ${daemon.home}`);
  let bridge;
  let browser;
  const written = [];
  try {
    await addCorpusSource(daemon.socket);
    const settled = await waitForKnowledge(daemon.socket, { timeoutMs: INDEX_TIMEOUT_MS, log });
    log(`indexed ${settled.status.items} items; ${settled.knowledge.entities} entities`);
    // Build Today's cards now rather than after the post-ingest debounce.
    const cards = await rpc(daemon.socket, "proactive.refresh", {}).catch((e) => log(`proactive.refresh: ${e.message}`));
    log(`proactive.refresh: ${JSON.stringify(cards)?.slice(0, 200)}`);

    bridge = await startBridge({ socket: daemon.socket, dist: DIST });
    const launch = process.env.E2E_CHROMIUM ? { executablePath: process.env.E2E_CHROMIUM } : {};
    browser = await chromium.launch(launch);

    for (const theme of THEMES) {
      const ctx = await browser.newContext({
        viewport: VIEWPORT,
        deviceScaleFactor: 1,
        locale: "en-US",
        timezoneId: "UTC",
        colorScheme: theme,
        reducedMotion: "reduce",
      });
      ctx.setDefaultTimeout(STEP_TIMEOUT_MS);
      await ctx.addInitScript({ path: path.join(here, "tauri-shim.js") });
      await ctx.addInitScript(eventShim);
      await ctx.route("**/invoke", async (route) => {
        let body = {};
        try {
          body = JSON.parse(route.request().postData() ?? "{}");
        } catch {}
        const method = STREAMED[body.cmd];
        if (!method) return route.continue();
        const params = body.args?.request ?? null;
        const out = await streamedCall(daemon.socket, method, params, route.request().frame().page());
        await route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(out) });
      });

      for (const shot of shots) {
        const page = await ctx.newPage();
        const errors = [];
        page.on("pageerror", (e) => errors.push(e.message));
        await page.clock.setFixedTime(NOW);
        await page.addInitScript(
          ([pageId, onboarded]) => {
            localStorage.setItem("nugget.page", pageId);
            if (onboarded) localStorage.setItem("nugget.onboarding.v1", JSON.stringify(onboarded));
            else localStorage.removeItem("nugget.onboarding.v1");
          },
          [shot.page, shot.onboarding ? null : ONBOARDED],
        );
        const file = path.join(OUT, `${shot.name}-${theme}.png`);
        try {
          await page.goto(`${bridge.url}index.html`);
          if (!shot.onboarding) {
            await page.locator("main h1").first().waitFor();
            await settle(page);
          }
          await shot.prepare?.(page, { expectedItems });
          await settle(page);
          await sleep(300);
          if (errors.length) throw new Error(`uncaught page errors: ${JSON.stringify(errors)}`);
          await page.screenshot({ path: file, animations: "disabled", caret: "hide" });
          written.push(file);
          log(`${path.basename(file)} (${(fs.statSync(file).size / 1024).toFixed(0)} KB)`);
        } catch (e) {
          await page.screenshot({ path: path.join(work, `FAIL-${shot.name}-${theme}.png`) }).catch(() => {});
          throw new Error(`shot ${shot.name} (${theme}) failed: ${e.message}\n(failure screenshot and logs in ${work})`);
        } finally {
          await page.close();
        }
      }
      await ctx.close();
    }
  } finally {
    await browser?.close().catch(() => {});
    await bridge?.close().catch(() => {});
    await daemon.stop();
    await model.close();
  }
  const total = written.reduce((n, f) => n + fs.statSync(f).size, 0);
  log(`wrote ${written.length} screenshots (${(total / 1024 / 1024).toFixed(2)} MB) to ${path.relative(REPO, OUT)}`);
  fs.rmSync(work, { recursive: true, force: true });
}

main().catch((e) => {
  console.error(`[shots] failed: ${e.stack ?? e}`);
  process.exit(1);
});
