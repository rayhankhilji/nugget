// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act } from "react";
import { emit } from "@tauri-apps/api/event";
import type { AskStarted, ChatMessage } from "../api";
import { ChatPage, readUseSources, writeUseSources } from "./Chat";
import { byText, cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";

type Req = { request: Record<string, unknown> };

const askStarted: AskStarted = {
  stream_id: "s-ask",
  provider: "ollama",
  locality: "on_device",
  citations: [
    { n: 1, item: { source: "files-1", key: "walk.md" }, chunk_id: 1, title: "River walk", url: null, sensitive: false },
  ],
  excluded_sensitive: 0,
  query_rewrite: "what about the second one? herons river",
};

function setup() {
  const asks: Record<string, unknown>[] = [];
  const chats: Record<string, unknown>[] = [];
  const calls = mockDaemon({
    retrieve_ask: (a: unknown) => {
      asks.push((a as Req).request);
      return { ...askStarted, stream_id: `s-ask-${asks.length}` };
    },
    chat: (a: unknown) => {
      chats.push((a as Req).request);
      return { stream_id: `s-chat-${chats.length}`, provider: "ollama", locality: "on_device" };
    },
  });
  return { asks, chats, calls };
}

async function say(container: HTMLElement, text: string) {
  const area = container.querySelector<HTMLTextAreaElement>("#chat-input")!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(area, text);
    area.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await click(byText(container, "button", "Send"));
  await flush();
}

async function reply(streamId: string, text: string) {
  await act(() => emit("nugget:chat-delta", { stream_id: streamId, text }));
  await act(() => emit("nugget:chat-done", { stream_id: streamId, input_tokens: 1, output_tokens: 2, error: null }));
  await flush();
}

const toggle = (container: HTMLElement) => byText(container, "label", "Use my sources")!.querySelector("input")!;

describe("Chat with sources", () => {
  beforeEach(() => installDomShims());
  afterEach(() => cleanup());

  it("remembers the toggle, defaulting to on", () => {
    expect(readUseSources()).toBe(true);
    writeUseSources(false);
    expect(readUseSources()).toBe(false);
    writeUseSources(true);
    expect(readUseSources()).toBe(true);
  });

  it("asks with the conversation so far and makes citations work", async () => {
    const { asks, chats } = setup();
    const { container } = await render(<ChatPage />);
    expect(toggle(container).checked).toBe(true);

    await say(container, "Where did we watch herons?");
    expect(asks).toHaveLength(1);
    expect(asks[0]).toMatchObject({ query: "Where did we watch herons?", history: [], tier: "synthesis", provider: null });
    expect(asks[0]!.budget_tokens).toBeGreaterThan(0);
    await reply("s-ask-1", "By the river [1].");

    await say(container, "what about the second one?");
    expect(chats).toHaveLength(0);
    const history = asks[1]!.history as ChatMessage[];
    expect(history).toEqual([
      { role: "user", text: "Where did we watch herons?" },
      { role: "assistant", text: "By the river [1]." },
    ]);
    await reply("s-ask-2", "The second walk [1].");

    // `[1]` is a working citation: it focuses the source in the list under the reply.
    const cites = container.querySelectorAll<HTMLButtonElement>("button.cite");
    expect(cites.length).toBeGreaterThan(0);
    await click(cites[cites.length - 1]);
    expect(document.activeElement?.textContent).toContain("River walk");
    expect(container.textContent).toContain("Searched for: what about the second one? herons river");
  });

  it("uses plain chat when the toggle is off, and remembers it", async () => {
    const { asks, chats } = setup();
    const { container, unmount } = await render(<ChatPage />);
    await click(toggle(container));
    expect(toggle(container).checked).toBe(false);
    expect(readUseSources()).toBe(false);
    await say(container, "hello");
    expect(asks).toHaveLength(0);
    expect(chats[0]).toMatchObject({ messages: [{ role: "user", text: "hello" }] });
    unmount();

    const again = await render(<ChatPage />);
    expect(toggle(again.container).checked).toBe(false);
  });
});
