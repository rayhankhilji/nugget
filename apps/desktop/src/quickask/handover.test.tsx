// @vitest-environment jsdom
// JJ2: "Open in Chat" and "Show in Search" carry the quick-ask question (and to Chat its answer).
import { act } from "react";
import { emit } from "@tauri-apps/api/event";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { QUICK_ASK_HANDOVER_EVENT, type AskStarted, type QuickAskHandover } from "../api";
import { usePendingSearch } from "../hooks.nav";
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
