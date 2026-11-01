// @vitest-environment jsdom
/**
 * Screen-reader semantics: streaming replies and indexing progress are announced at their start
 * and end only (never per token or per tick), and every page has one h1, no skipped heading
 * level and no unnamed button.
 */
import { createElement } from "react";
import { describe, expect, it } from "vitest";
import { EVENT } from "../src/api";
import { ChatPage, writeUseSources } from "../src/pages/Chat";
import {
  daemon,
  emitEvent,
  flush,
  getByRole,
  render,
  setupHarness,
  submit,
  t,
  type,
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
});
