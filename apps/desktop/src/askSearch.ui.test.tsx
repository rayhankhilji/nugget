// @vitest-environment jsdom
// JJ2: the answer views show how an Ask searched: the question's date range and partial matches.
import { act } from "react";
import { emit } from "@tauri-apps/api/event";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { AskStarted } from "./api";
import { ChatPage } from "./pages/Chat";
import { byText, cleanup, click, flush, installDomShims, mockDaemon, render } from "./test/render";

const year = new Date().getFullYear();

const started: AskStarted = {
  stream_id: "s-1",
  provider: "local-chat",
  locality: "on_device",
  citations: [
    { n: 1, item: { source: "files-1", key: "walk.md" }, chunk_id: 1, title: "River walk", url: null },
    { n: 2, item: { source: "files-1", key: "tokyo.md" }, chunk_id: 2, title: "Trip ideas", url: null },
  ],
  excluded_sensitive: 0,
  date: { phrase: "last week", start: `${year}-10-26T00:00:00+01:00`, end: `${year}-11-02T00:00:00+01:00`, applied: true, listed: false },
  partial: [2],
};

async function answer(text: string) {
  await act(() => emit("nugget:chat-delta", { stream_id: "s-1", text }));
  await act(() => emit("nugget:chat-done", { stream_id: "s-1", input_tokens: 1, output_tokens: 2, error: null }));
  await flush();
}

function setValue(el: HTMLInputElement | HTMLTextAreaElement, text: string) {
  const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
  Object.getOwnPropertyDescriptor(proto, "value")!.set!.call(el, text);
  el.dispatchEvent(new Event("input", { bubbles: true }));
}

describe("how an Ask searched", () => {
  beforeEach(() => installDomShims());
  afterEach(() => cleanup());

  it("Chat says which dates were searched and labels partial matches", async () => {
    const saved: unknown[] = [];
    mockDaemon({
      retrieve_ask: started,
      chat_history_load: [],
      chat_history_save: (a: unknown) => {
        saved.push((a as { conversation: unknown }).conversation);
      },
    });
    const { container } = await render(<ChatPage />);
    await act(async () => setValue(container.querySelector<HTMLTextAreaElement>("#chat-input")!, "what about Tokyo last week?"));
    await click(byText(container, "button", "Send"));
    await flush();
    await answer("By the river [1] and maybe [2].");

    expect(container.querySelector(".ask-dates")?.textContent).toBe("Searched items from Oct 26 to Nov 1");
    const sources = [...container.querySelectorAll(".chat-citations li")];
    expect(sources).toHaveLength(2);
    expect(sources[0]!.querySelector(".ask-partial")).toBeNull();
    const badge = sources[1]!.querySelector(".ask-partial");
    expect(badge?.textContent).toContain("Partial match");
    // Screen readers hear why, not just the label.
    expect(badge?.querySelector(".visually-hidden")?.textContent).toContain("key words");
    // The label is kept with the saved citations.
    expect(JSON.stringify(saved.at(-1))).toContain('"partial":true');
  });
});
