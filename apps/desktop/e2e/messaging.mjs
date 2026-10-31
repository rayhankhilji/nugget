// Messaging scenario (II1): import a WhatsApp chat export through Sources → Add source.
//
// 1. A daemon starts on an empty home. A temp folder holds a synthetic WhatsApp "Export chat" file
//    (Android format, multi-line message, a system line and a media placeholder).
// 2. Through the UI: Sources → Add source → WhatsApp → Next, Choose… (the bridge's folder picker
//    returns the temp folder), keep "on this device only" checked, Add.
// 3. The source syncs: its conversation month is indexed as one item with its participants and
//    marked sensitive (private chats never go to cloud models).
// 4. Search finds a phrase from the chat in the UI.
// 5. A second export dropped into the watched folder is picked up without pressing anything.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { startBridge, METHOD_NOT_FOUND } from "./bridge.mjs";
import { rpc, startDaemon } from "./daemon.mjs";
import { check, DIST, expect, INDEX_TIMEOUT_MS, KEEP, log, newContext, OUT, settle, shot, t, until } from "./harness.mjs";

const ONBOARDED = { version: 1, step: "done", completed: [], skipped: [], ai: null, finished: true };
const PHRASE = "periwinkle lighthouse";

const escapeRe = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const exactName = (key) => new RegExp(`^${escapeRe(t(key))}$`);

const CHAT_1 = [
  "12/03/2024, 09:15 - Messages and calls are end-to-end encrypted. No one outside of this chat can read them.",
  "12/03/2024, 09:16 - Ada Quill: Are we still meeting at the periwinkle lighthouse on Saturday?",
  "12/03/2024, 09:17 - Ben Marlow: Yes!",
  "Bring the tide tables.",
  "13/03/2024, 18:02 - Ada Quill: <Media omitted>",
  "",
].join("\n");

const CHAT_2 = [
  "[14/03/2024, 10:00:00] Cy Hollis: The saffron kite festival moved to April.",
  "[14/03/2024, 10:01:12] Ada Quill: Noted, thanks",
  "",
].join("\n");

export async function messagingScenario({ browser, bin }) {
  const work = fs.mkdtempSync(path.join(os.tmpdir(), "nugget-e2e-messaging-"));
  const exports = path.join(work, "WhatsApp exports");
  fs.mkdirSync(exports);
  fs.writeFileSync(path.join(exports, "WhatsApp Chat with Ben Marlow.txt"), CHAT_1);
  const summary = { items: null, sensitive: null, watched: false };

  const daemon = await startDaemon(bin, { stderrPath: path.join(OUT, "messaging-nuggetd.out.log") });
  log(`messaging: daemon home ${daemon.home}`);
  let bridge;
  let ctx;
  const pageErrors = [];
  try {
    bridge = await startBridge({ socket: daemon.socket, dist: DIST, pickFolders: [exports] });
    ctx = await newContext(browser);
    await ctx.addInitScript(([key, state]) => localStorage.setItem(key, JSON.stringify(state)), ["nugget.onboarding.v1", ONBOARDED]);
    await ctx.addInitScript(() => localStorage.setItem("nugget.page", "sources"));
    const page = await ctx.newPage();
    page.on("pageerror", (e) => pageErrors.push(e.message));
    bridge.deliverEvents(page);

    const whatsappItems = async () => {
      const sources = await rpc(daemon.socket, "sources.list");
      const s = sources.find((x) => x.kind === "whatsapp");
      return s ? { source: s, items: s.items ?? 0 } : { source: null, items: 0 };
    };

    await check("messaging: add a WhatsApp export folder from Sources → Add source", async () => {
      try {
        await page.goto(`${bridge.url}index.html`);
        await page.locator("main").getByRole("button", { name: exactName("sources.add") }).first().click();
        const dialog = page.locator("dialog[open]");
        await dialog.getByRole("radio", { name: new RegExp(`^${escapeRe(t("format.sourceKind.whatsapp"))}\\b`) }).check();
        await shot(page, "messaging-kind-picker");
        await dialog.getByRole("button", { name: exactName("common.next") }).click();
        await dialog.getByRole("button", { name: exactName("addSource.messages.browse") }).click();
        const folder = dialog.getByLabel(t("addSource.whatsapp.folder"), { exact: false });
        await until("the picked folder fills the field", async () => {
          const v = await folder.inputValue();
          return [v === exports, v];
        });
        const priv = dialog.getByRole("checkbox", { name: new RegExp(`^${escapeRe(t("addSource.messages.private"))}`) });
        expect(await priv.isChecked(), "“on this device only” should be checked by default");
        await shot(page, "messaging-whatsapp-form");
        await dialog.getByRole("button", { name: exactName("addSource.messages.submit") }).click();
        await until("the wizard closes", async () => [(await page.locator("dialog[open]").count()) === 0, null]);
        const { source } = await whatsappItems();
        expect(source, "sources.list has no whatsapp source after Add");
        expect(source.settings?.folder === exports && source.settings?.private === true, `settings ${JSON.stringify(source.settings)}`);
      } catch (e) {
        await shot(page, "FAIL-messaging-add");
        throw e;
      }
    });

    await check("messaging: the chat is indexed as a private conversation", async () => {
      const n = await until("the WhatsApp source indexes its conversation", async () => {
        const { items } = await whatsappItems();
        return [items >= 1, items];
      }, INDEX_TIMEOUT_MS);
      summary.items = n;
      const hits = await until(`search "${PHRASE}" over IPC finds the chat`, async () => {
        const r = await rpc(daemon.socket, "search", { query: PHRASE, limit: 5 });
        return [Array.isArray(r) && r.length > 0, r];
      }, INDEX_TIMEOUT_MS);
      const hit = hits[0];
      const text = JSON.stringify(hit);
      expect(/Ben Marlow/.test(text), `the hit should name the conversation: ${text.slice(0, 400)}`);
      const meta = hit.metadata ?? hit.item?.metadata;
      if (meta) {
        summary.sensitive = meta.sensitive;
        expect(meta.sensitive === true, `the chat should be marked sensitive: ${JSON.stringify(meta).slice(0, 400)}`);
      }
    });

    await check("messaging: Search in the UI finds a phrase from the chat", async () => {
      try {
        await page.getByRole("navigation").getByRole("button", { name: t("app.nav.search"), exact: true }).click();
        const box = page.getByRole("searchbox", { name: t("search.query") });
        await box.fill(PHRASE);
        await box.press("Enter");
        const results = page.locator("ol.results li.result");
        await until(`search "${PHRASE}" shows the chat`, async () => {
          const all = await results.evaluateAll((lis) => lis.map((li) => li.textContent ?? ""));
          return [all.some((x) => x.includes("Ben Marlow") || x.toLowerCase().includes("periwinkle")), all.slice(0, 3).map((x) => x.slice(0, 80))];
        });
        await settle(page);
        await shot(page, "messaging-search");
      } catch (e) {
        await shot(page, "FAIL-messaging-search");
        throw e;
      }
    });

    await check("messaging: a new export in the watched folder is picked up", async () => {
      fs.writeFileSync(path.join(exports, "WhatsApp Chat with Cy Hollis.txt"), CHAT_2);
      await until("the second chat is searchable", async () => {
        const r = await rpc(daemon.socket, "search", { query: "saffron kite festival", limit: 5 });
        return [Array.isArray(r) && r.length > 0, r?.length];
      }, INDEX_TIMEOUT_MS);
      summary.watched = true;
    });

    await check("messaging: every command the UI called exists and accepted its params", async () => {
      const bad = bridge.calls.filter((c) => c.via === "daemon" && (c.code === METHOD_NOT_FOUND || c.code === -32602));
      expect(bad.length === 0, `wire mismatches: ${JSON.stringify(bad)}`);
      expect(pageErrors.length === 0, `uncaught page errors: ${JSON.stringify(pageErrors)}`);
    });
    return summary;
  } finally {
    await ctx?.close().catch(() => {});
    await bridge?.close().catch(() => {});
    if (fs.existsSync(daemon.logDir)) fs.cpSync(daemon.logDir, path.join(OUT, "messaging-daemon-logs"), { recursive: true });
    await daemon.stop({ keep: KEEP });
    if (KEEP) log(`kept the messaging work folder: ${work}`);
    else fs.rmSync(work, { recursive: true, force: true });
  }
}
