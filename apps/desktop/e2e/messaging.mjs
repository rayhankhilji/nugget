// Messaging: a synthetic WhatsApp export folder added through Sources → Add source (kept on this
// device), indexed as one conversation and found by Search; a second export dropped into the
// folder is picked up by the watcher.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { startBridge } from "./bridge.mjs";
import { rpc, startDaemon } from "./daemon.mjs";
import { check, checkWire, DIST, exactName, navTo, expect, INDEX_TIMEOUT_MS, KEEP, newContext, OUT, shot, stopDaemon, t, until } from "./harness.mjs";

const PHRASE = "periwinkle lighthouse";

const escapeRe = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

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

  const daemon = await startDaemon(bin, { stderrPath: path.join(OUT, "messaging-nuggetd.out.log") });
  let bridge;
  let ctx;
  const pageErrors = [];
  try {
    bridge = await startBridge({ socket: daemon.socket, dist: DIST, pickFolders: [exports] });
    ctx = await newContext(browser, { onboarded: true, page: "sources" });
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
        await dialog.getByRole("button", { name: exactName("common.next") }).click();
        await dialog.getByRole("button", { name: exactName("addSource.messages.browse") }).click();
        const folder = dialog.getByLabel(t("addSource.whatsapp.folder"), { exact: false });
        await until("the picked folder fills the field", async () => {
          const v = await folder.inputValue();
          return [v === exports, v];
        });
        const priv = dialog.getByRole("checkbox", { name: new RegExp(`^${escapeRe(t("addSource.messages.private"))}`) });
        expect(await priv.isChecked(), "“on this device only” should be checked by default");
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

    await check("messaging: the chat is indexed as one conversation and Search finds it", async () => {
      try {
        await until("the WhatsApp source indexes its conversation", async () => {
          const { items } = await whatsappItems();
          return [items >= 1, items];
        }, INDEX_TIMEOUT_MS);
        await navTo(page, "search");
        const box = page.getByRole("searchbox", { name: t("search.query") });
        await box.fill(PHRASE);
        await box.press("Enter");
        const results = page.locator("ol.results li.result");
        await until(`search "${PHRASE}" shows the chat`, async () => {
          const all = await results.evaluateAll((lis) => lis.map((li) => li.textContent ?? ""));
          return [all.some((x) => x.includes("Ben Marlow") || x.toLowerCase().includes("periwinkle")), all.slice(0, 3).map((x) => x.slice(0, 80))];
        });
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
    });

    await checkWire(bridge, "messaging");
    await check("messaging: no uncaught page errors", async () => expect(pageErrors.length === 0, `uncaught page errors: ${JSON.stringify(pageErrors)}`));
  } finally {
    await ctx?.close().catch(() => {});
    await bridge?.close().catch(() => {});
    await stopDaemon(daemon, "messaging");
    if (!KEEP) fs.rmSync(work, { recursive: true, force: true });
  }
}
