// @vitest-environment jsdom
/**
 * Screen-reader semantics: streaming replies and indexing progress are announced at their start
 * and end only (never per token or per tick), and every page has one h1, no skipped heading
 * level and no unnamed button.
 */
import { createElement } from "react";
import { describe, expect, it } from "vitest";
import { EVENT, type PageId, type SourceInfo } from "../src/api";
import { PAGES } from "../src/App";
import { ChatPage, writeUseSources } from "../src/pages/Chat";
import { SearchPage } from "../src/pages/Search";
import { Onboarding } from "../src/onboarding/Onboarding";
import { INITIAL } from "../src/onboarding/machine";
import {
  accessibleName,
  click,
  daemon,
  emitEvent,
  flush,
  getByRole,
  isHidden,
  main,
  render,
  renderApp,
  setupHarness,
  submit,
  t,
  type,
  waitFor,
} from "./harness";

setupHarness();

/** The page's announcer: the visually hidden status region `<Announce>` renders. */
function announcer(root: ParentNode = document): HTMLElement {
  const regions = [...root.querySelectorAll<HTMLElement>('[role="status"][aria-atomic="true"]')];
  expect(regions).toHaveLength(1);
  return regions[0]!;
}

/** Records every text the region shows from now on (what a screen reader would read). */
function record(region: HTMLElement): string[] {
  const said: string[] = [];
  let last = region.textContent ?? "";
  new MutationObserver(() => {
    const now = region.textContent ?? "";
    if (now !== last) said.push(now.trim());
    last = now;
  }).observe(region, { childList: true, characterData: true, subtree: true });
  return said;
}

/** Live regions (explicit or by role) that are not switched off. */
function liveRegions(): HTMLElement[] {
  return [...document.querySelectorAll<HTMLElement>("[aria-live], [role=status], [role=log], [role=alert]")].filter(
    (el) => el.getAttribute("aria-live") !== "off",
  );
}

const delta = (text: string, stream_id = "s1") => emitEvent(EVENT.chatDelta, { stream_id, text });
const done = (stream_id = "s1") => emitEvent(EVENT.chatDone, { stream_id, error: null, input_tokens: 10, output_tokens: 40 });
const citation = (n: number, title: string) => ({ n, item: { source: "files-1", key: `${title}.md` }, chunk_id: n, title, url: null, sensitive: false });

describe("streamed answers", () => {
  it("Chat announces a reply's start and end, not its tokens", async () => {
    writeUseSources(true);
    daemon({
      retrieve_ask: { stream_id: "s1", provider: "local-chat", locality: "on_device", citations: [citation(1, "Herons"), citation(2, "Lakes")], excluded_sensitive: 0 },
    });
    const { container } = await render(createElement(ChatPage));
    await flush(5);
    const said = record(announcer(container));

    await type(container.querySelector("#chat-input"), "where do herons nest?");
    await submit(container.querySelector("form.composer"));
    const log = getByRole("log", { name: t("chat.conversation") });
    // The log holds the streaming reply: busy, and not live (it would be read token by token).
    expect(log.getAttribute("aria-live")).toBe("off");
    expect(log.getAttribute("aria-busy")).toBe("true");

    for (let i = 0; i < 40; i++) await delta(i === 20 ? " by the lake [1]" : ` word${i}`);
    expect(liveRegions().some((el) => el.textContent?.includes("word3"))).toBe(false);
    await done();

    expect(log.getAttribute("aria-busy")).toBe("false");
    expect(said).toEqual([t("announce.answering"), t("announce.answerReady", { count: 1 })]);
  });

  it("Search's answer streams outside a live region and is announced once ready", async () => {
    const cites = [citation(1, "Budget"), citation(2, "Redlines")];
    daemon({
      retrieve_context: { text: "[1] The budget.\n[2] The redlines.", citations: cites, excluded_sensitive: 0 },
      retrieve_ask: { stream_id: "s1", provider: "local-chat", locality: "on_device", citations: cites, excluded_sensitive: 0 },
    });
    const { container } = await render(createElement(SearchPage));
    await flush(5);
    await click(getByRole("button", { name: t("search.ask") }));
    await type(container.querySelector("#ask-q"), "budget?");
    await submit(container.querySelector("form.ask-form"));
    const said = record(announcer(container.querySelector("#ask-panel")!));

    await click(getByRole("button", { name: t("search.askWithContext") }));
    const answer = container.querySelector<HTMLElement>(".answer")!;
    expect(answer.hasAttribute("aria-live")).toBe(false);
    expect(answer.getAttribute("aria-busy")).toBe("true");
    for (let i = 0; i < 30; i++) await delta(` token${i}`);
    await delta(" See [1] and [2].");
    await done();

    expect(answer.getAttribute("aria-busy")).toBe("false");
    expect(said).toEqual([t("announce.answering"), t("announce.answerReady", { count: 2 })]);
  });
});

describe("indexing progress", () => {
  const progress = (phase: string, done: number, total: number | null = null) =>
    emitEvent(EVENT.ingestProgress, { source: "files-1", phase, done, total, error: null });

  async function pass() {
    await progress("scanning", 0);
    for (let n = 1; n <= 25; n++) await progress("indexing", n * 4, 100);
    await progress("idle", 0);
  }

  it.each<[PageId]>([["sources"], ["status"]])("the %s page announces a pass's start and end", async (page) => {
    daemon();
    await renderApp({ page });
    // Wait for the page's source list (Sources) or indexing card (Status): it names the source.
    await waitFor(() => expect(main().textContent).toContain(page === "sources" ? "Documents" : t("status.freshnessP50")));
    const said = record(announcer(main()));

    await pass();
    // The documents source has 1,234 items (fixtures); the pass saw fewer.
    expect(said).toEqual([t("announce.indexing", { name: "Documents" }), t("announce.indexed", { name: "Documents", count: 1234 })]);

    // A failure is announced once, however often the retry fails the same way.
    said.length = 0;
    await emitEvent(EVENT.ingestProgress, { source: "files-1", phase: "error", done: 0, error: "disk full" });
    await emitEvent(EVENT.ingestProgress, { source: "files-1", phase: "error", done: 0, error: "disk full" });
    expect(said).toEqual([t("announce.indexFailed", { name: "Documents", error: "disk full" })]);
  });

  it("onboarding's source step announces when indexing starts and when search works", async () => {
    const folder: SourceInfo = { id: "files-9", kind: "files", name: "notes", enabled: true, state: "syncing", items: 0, chunks: 0 };
    let listed: SourceInfo[] = [];
    daemon({
      pick_folders: () => ["/home/me/notes"],
      sources_add: () => ((listed = [folder]), folder),
      sources_list: () => listed,
    });
    const { container } = await render(
      createElement(Onboarding, { connected: true, onExit: () => {}, initial: { ...INITIAL, step: "source", completed: ["welcome", "ai"] } }),
    );
    const said = record(announcer(container));
    await click(getByRole("button", { name: t("onboarding.source.addFolder") }));
    await click(getByRole("button", { name: t("addSource.folder.choose") }));
    await click(getByRole("button", { name: t("addSource.folder.submit") }));
    await flush();
    const list = container.querySelector<HTMLElement>(".ob-index")!;
    expect(list.hasAttribute("aria-live")).toBe(false);
    expect(list.getAttribute("aria-busy")).toBe("true");

    for (const items of [5, 12, 30, 61]) {
      listed = [{ ...folder, items }];
      await emitEvent(EVENT.sourcesChanged, {});
      await flush();
    }
    listed = [{ ...folder, state: "watching", items: 93, chunks: 120 }];
    await emitEvent(EVENT.sourcesChanged, {});
    await flush();

    expect(list.getAttribute("aria-busy")).toBe("false");
    expect(said).toEqual([
      t("onboarding.source.addedAnnounce", { name: "notes" }),
      t("announce.indexing", { name: "notes" }),
      t("onboarding.source.ready", { name: "notes", count: 93 }),
    ]);
  });
});

describe("page structure", () => {
  it.each(PAGES.map((p) => [p.id]))("%s has one h1, no skipped heading level and no unnamed button", async (id) => {
    daemon();
    await renderApp({ page: id as PageId });
    await flush(5);
    const root = main();
    const headings = [...root.querySelectorAll("h1, h2, h3, h4, h5, h6")].filter((h) => !isHidden(h) && !h.closest("dialog"));
    expect(headings.filter((h) => h.tagName === "H1")).toHaveLength(1);
    let prev = 0;
    const skips: string[] = [];
    for (const h of headings) {
      const level = Number(h.tagName[1]);
      if (level > prev + 1) skips.push(`h${prev} then ${h.tagName}: ${h.textContent}`);
      prev = level;
    }
    expect(skips).toEqual([]);
    const unnamed = [...document.querySelectorAll("button, a[href], [role=button]")]
      .filter((b) => !isHidden(b) && !accessibleName(b).trim())
      .map((b) => b.outerHTML.slice(0, 120));
    expect(unnamed).toEqual([]);
  });
});
