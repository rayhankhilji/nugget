// @vitest-environment jsdom
// JJ2: "Open in Chat" and "Show in Search" carry the quick-ask question (and to Chat its answer).
import { act } from "react";
import { emit } from "@tauri-apps/api/event";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { QUICK_ASK_HANDOVER_EVENT, type AskStarted, type QuickAskHandover, type StoredConversation } from "../api";
import { usePendingSearch } from "../hooks.nav";
import { ChatPage } from "../pages/Chat";
import { byText, cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";
import { deliverHandover, handoverCitations, useQuickAskHandover } from "./handover";
import { QuickAsk } from "./QuickAsk";

const started: AskStarted = {
  stream_id: "s-q",
  provider: "local-chat",
  locality: "on_device",
  citations: [{ n: 1, item: { source: "files-1", key: "/notes/walk.md" }, chunk_id: 1, title: "River walk", url: null }],
  excluded_sensitive: 0,
  partial: [1],
};

function setValue(el: HTMLInputElement | HTMLTextAreaElement, text: string) {
  const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
  Object.getOwnPropertyDescriptor(proto, "value")!.set!.call(el, text);
  el.dispatchEvent(new Event("input", { bubbles: true }));
}

async function askQuick(container: HTMLElement, q: string) {
  await act(async () => setValue(container.querySelector<HTMLInputElement>("#quickask-q")!, q));
  await act(async () => {
    container.querySelector("form")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
  });
  await flush(3);
}

describe("quick ask hands over", () => {
  beforeEach(() => installDomShims());
  afterEach(() => cleanup());

  it("sends the question, and the finished answer with its sources", async () => {
    const sent: { page: string; handover?: QuickAskHandover }[] = [];
    mockDaemon({
      retrieve_ask: started,
      quickask_prefill_take: null,
      quickask_open_main: (a: unknown) => {
        sent.push(a as { page: string; handover?: QuickAskHandover });
        return null;
      },
    });
    const { container } = await render(<QuickAsk />);

    // Before any answer: the question alone.
    await act(async () => setValue(container.querySelector<HTMLInputElement>("#quickask-q")!, "where were the herons?"));
    await click(byText(container, "button", "Open in Chat"));
    expect(sent.at(-1)).toEqual({ page: "chat", handover: { question: "where were the herons?" } });

    await askQuick(container, "where were the herons?");
    // Still streaming: the question alone.
    await act(() => emit("nugget:chat-delta", { stream_id: "s-q", text: "By the river [1]." }));
    await flush();
    await click(byText(container, "button", "Open in Chat"));
    expect(sent.at(-1)!.handover).toEqual({ question: "where were the herons?" });

    await act(() => emit("nugget:chat-done", { stream_id: "s-q", input_tokens: 1, output_tokens: 2, error: null }));
    await flush();
    await click(byText(container, "button", "Open in Chat"));
    const h = sent.at(-1)!.handover!;
    expect(h.question).toBe("where were the herons?");
    expect(h.answer).toMatchObject({ text: "By the river [1].", provider: "local-chat", general: false });
    expect(h.answer!.citations).toEqual([{ ...started.citations[0], partial: true }]);

    await click(byText(container, "button", "Show in Search"));
    expect(sent.at(-1)).toMatchObject({ page: "search", handover: { question: "where were the herons?" } });

    // A new question typed after the answer goes alone.
    await act(async () => setValue(container.querySelector<HTMLInputElement>("#quickask-q")!, "and the swans?"));
    await click(byText(container, "button", "Open in Chat"));
    expect(sent.at(-1)!.handover).toEqual({ question: "and the swans?" });
  });

  it("Chat opens a new, saved conversation with the question and answer", async () => {
    const saved: StoredConversation[] = [];
    mockDaemon({
      chat_history_load: [],
      chat_history_save: (a: unknown) => {
        saved.push((a as { conversation: StoredConversation }).conversation);
      },
    });
    deliverHandover({
      page: "chat",
      question: "where were the herons?",
      answer: {
        text: "By the river [1].",
        provider: "local-chat",
        citations: [{ ...started.citations[0]!, partial: true }],
        date: { phrase: "last week", start: "2026-10-26T00:00:00Z", end: "2026-11-02T00:00:00Z", applied: true },
      },
    });
    const { container } = await render(<ChatPage />);
    await flush(3);
    const msgs = [...container.querySelectorAll(".chat-list > li.msg")];
    expect(msgs.map((m) => m.className)).toEqual(["msg msg-user", "msg msg-assistant"]);
    expect(msgs[0]!.textContent).toContain("where were the herons?");
    expect(msgs[1]!.textContent).toContain("By the river");
    expect(msgs[1]!.querySelector(".ask-partial")).not.toBeNull();
    expect(msgs[1]!.querySelector(".ask-dates")?.textContent).toContain("Searched items from");
    // The [1] marker is a working citation.
    expect(msgs[1]!.querySelector("button.cite")).not.toBeNull();
    expect(saved.at(-1)?.messages.map((m) => m.role)).toEqual(["user", "assistant"]);
    expect(saved.at(-1)?.title).toBe("where were the herons?");
  });

  it("Chat puts a question without an answer in the composer", async () => {
    mockDaemon({ chat_history_load: [], chat_history_save: null });
    const { container } = await render(<ChatPage />);
    await flush();
    await act(async () => deliverHandover({ page: "chat", question: "and the swans?" }));
    await flush();
    expect(container.querySelector<HTMLTextAreaElement>("#chat-input")!.value).toBe("and the swans?");
    expect(container.querySelectorAll(".chat-list > li.msg")).toHaveLength(0);
  });

  it("the main window takes the handover on load and on each event, and Search runs the question", async () => {
    const queue: (QuickAskHandover | null)[] = [{ page: "search", question: "herons" }, { page: "search", question: "swans" }];
    mockDaemon({ quickask_handover_take: () => queue.shift() ?? null });
    const searched: string[] = [];
    function Probe() {
      useQuickAskHandover();
      usePendingSearch((q) => searched.push(q));
      return null;
    }
    await render(<Probe />);
    await flush(3);
    expect(searched).toEqual(["herons"]);
    await act(() => emit(QUICK_ASK_HANDOVER_EVENT, null));
    await flush(3);
    expect(searched).toEqual(["herons", "swans"]);
  });

  it("drops malformed citations and empty questions", () => {
    expect(handoverCitations([{ n: 1, item: { source: "s", key: "k" } }, { n: 2 }, null, "x", { n: 0, item: { source: "s", key: "k" } }])).toHaveLength(1);
    expect(handoverCitations("nope")).toEqual([]);
    const searched: string[] = [];
    deliverHandover({ page: "search", question: "  " });
    deliverHandover(null);
    expect(searched).toEqual([]);
  });
});
