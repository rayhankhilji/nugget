// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act } from "react";
import { emit } from "@tauri-apps/api/event";
import type { AnswerCheck, AskStarted, ChatMessage, StoredConversation } from "../api";
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

function setup(opts: { cite?: (n: number) => boolean; daemonGeneral?: boolean; citations?: AskStarted["citations"] } = {}) {
  const asks: Record<string, unknown>[] = [];
  const chats: Record<string, unknown>[] = [];
  const cancels: string[] = [];
  const saved: StoredConversation[] = [];
  const calls = mockDaemon({
    retrieve_ask: (a: unknown) => {
      asks.push((a as Req).request);
      const n = asks.length;
      const cite = opts.cite?.(n) ?? true;
      const general = !cite && opts.daemonGeneral === true;
      return { ...askStarted, stream_id: `s-ask-${n}`, citations: cite ? (opts.citations ?? askStarted.citations) : [], ...(general ? { general } : {}) };
    },
    chat_cancel: (a: unknown) => {
      cancels.push((a as { streamId: string }).streamId);
    },
    chat_history_load: [],
    chat_history_save: (a: unknown) => {
      saved.push((a as { conversation: StoredConversation }).conversation);
    },
    chat: (a: unknown) => {
      chats.push((a as Req).request);
      return { stream_id: `s-chat-${chats.length}`, provider: "ollama", locality: "on_device" };
    },
  });
  return { asks, chats, cancels, saved, calls };
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

async function reply(streamId: string, text: string, check?: AnswerCheck) {
  await act(() => emit("nugget:chat-delta", { stream_id: streamId, text }));
  await act(() =>
    emit("nugget:chat-done", { stream_id: streamId, input_tokens: 1, output_tokens: 2, error: null, ...(check ? { check } : {}) }),
  );
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
      { role: "assistant", text: "By the river." },
    ]);
    await reply("s-ask-2", "The second walk [1].");

    // `[1]` is a working citation: it focuses the source in the list under the reply.
    const cites = container.querySelectorAll<HTMLButtonElement>("button.cite");
    expect(cites.length).toBeGreaterThan(0);
    await click(cites[cites.length - 1]);
    expect(document.activeElement?.textContent).toContain("River walk");
    expect(container.textContent).toContain("Searched for: what about the second one? herons river");
  });

  it("lists the cited sources first, once per item, and the rest behind a toggle", async () => {
    const passage = (n: number, key: string, title: string, url: string | null = null) => ({
      n,
      item: { source: "files-1", key },
      chunk_id: n,
      title,
      url,
      sensitive: false,
    });
    const citations = [
      passage(1, "budget.md", "Q4 budget"),
      passage(2, "plan.md", "Launch plan"),
      passage(3, "notes.md", "Team notes"),
      passage(4, "budget.md", "Q4 budget"),
      passage(5, "old.md", "Old memo"),
      passage(6, "old.md", "Old memo"),
      passage(7, "trip.md", "Trip", "https://example.com/trip"),
    ];
    setup({ citations });
    const { container } = await render(<ChatPage />);
    await say(container, "What's the Q4 budget?");
    await reply("s-ask-1", "It is 40k [1], set in the plan [2] and confirmed later [4]. Not `arr[3]`.");

    const sources = container.querySelector(".chat-sources")!;
    const rows = () =>
      [...sources.querySelectorAll("ol.citations li.citation")].map((li) => [
        li.querySelector(".cite-n")!.textContent,
        li.querySelector(".citation-title")!.textContent,
      ]);
    // Cited items only, each once with every marker the answer used for it.
    expect(rows()).toEqual([
      ["[1] [4]", "Q4 budget"],
      ["[2]", "Launch plan"],
    ]);
    const more = byText(sources, "button", "3 more passages the model saw")!;
    expect(more.getAttribute("aria-expanded")).toBe("false");

    await click(more);
    expect(more.getAttribute("aria-expanded")).toBe("true");
    expect(rows()).toEqual([
      ["[1] [4]", "Q4 budget"],
      ["[2]", "Launch plan"],
      ["[3]", "Team notes"],
      ["[5] [6]", "Old memo"],
      ["[7]", "Trip"],
    ]);
    // `[4]` focuses the item it shares with `[1]`.
    await click(byText(container, "button.cite", "[4]"));
    expect(document.activeElement?.textContent).toContain("Q4 budget");
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

  it("answers as a plain chat, and says so, when nothing in the sources matches", async () => {
    // The first question finds passages; the second finds none.
    const { asks, chats, cancels, saved } = setup({ cite: (n) => n === 1 });
    const { container } = await render(<ChatPage />);
    await say(container, "Where did we watch herons?");
    await reply("s-ask-1", "By the river [1].");

    await say(container, "What is a heron?");
    expect(asks).toHaveLength(2);
    expect(cancels).toEqual(["s-ask-2"]);
    // The sourced stream is dropped: its text never shows.
    await reply("s-ask-2", "I can't find that in your sources.");
    expect(container.textContent).not.toContain("I can't find that");
    expect(chats).toHaveLength(1);
    expect(chats[0]!.messages).toEqual([
      { role: "user", text: "Where did we watch herons?" },
      { role: "assistant", text: "By the river." },
      { role: "user", text: "What is a heron?" },
    ]);
    await reply("s-chat-1", "A wading bird.");

    const items = container.querySelectorAll("li.msg-assistant");
    expect(items[1]!.textContent).toContain("A wading bird.");
    expect(items[1]!.textContent).toContain("No matching passages were found in your sources, so this is a general answer.");
    expect(items[0]!.textContent).not.toContain("general answer");
    expect(items[1]!.querySelector(".chat-sources")).toBeNull();

    // Kept with the saved turn, and shown again when the conversation is reopened.
    const last = saved[saved.length - 1]!;
    expect(last.messages[3]).toMatchObject({ role: "assistant", text: "A wading bird.", general_answer: true });
    expect(last.messages[1]!.general_answer).toBeUndefined();
  });

  it("keeps the daemon's own general answer instead of cancelling and re-asking", async () => {
    const { asks, chats, cancels, saved } = setup({ cite: () => false, daemonGeneral: true });
    const { container } = await render(<ChatPage />);
    await say(container, "What is a heron?");
    expect(asks).toHaveLength(1);
    expect(cancels).toEqual([]);
    expect(chats).toHaveLength(0);
    await reply("s-ask-1", "A wading bird.");

    const item = container.querySelector("li.msg-assistant")!;
    expect(item.textContent).toContain("A wading bird.");
    expect(item.textContent).toContain("so this is a general answer");
    expect(item.querySelector(".chat-sources")).toBeNull();
    expect(saved[saved.length - 1]!.messages[1]).toMatchObject({ text: "A wading bird.", general_answer: true });
  });

  it("strips earlier replies' [n] markers from the history it sends", async () => {
    const { asks, chats } = setup();
    const { container } = await render(<ChatPage />);
    await say(container, "Where did we watch herons [x]?");
    await reply("s-ask-1", "By the river [1][2], see [map](https://example.com) and `arr[0]` [12].");
    await say(container, "and then?");
    expect((asks[1]!.history as ChatMessage[])[1]).toEqual({
      role: "assistant",
      text: "By the river, see [map](https://example.com) and `arr[0]`.",
    });
    // The user's own text is sent as typed; the reply on screen keeps its citations.
    expect((asks[1]!.history as ChatMessage[])[0]!.text).toBe("Where did we watch herons [x]?");
    expect(container.querySelectorAll("button.cite").length).toBeGreaterThan(0);

    await reply("s-ask-2", "Home [1].");
    await click(toggle(container));
    await say(container, "thanks");
    const msgs = chats[0]!.messages as ChatMessage[];
    expect(msgs.filter((m) => m.role === "assistant").map((m) => m.text)).toEqual([
      "By the river, see [map](https://example.com) and `arr[0]`.",
      "Home.",
    ]);
  });

  it("restores the general-answer note from history", async () => {
    mockDaemon({
      chat_history_load: [
        {
          id: "c-1",
          title: "q",
          updated_at: 1,
          messages: [
            { role: "user", text: "q" },
            { role: "assistant", text: "general", state: "done", general_answer: true },
          ],
        },
      ],
    });
    const { container } = await render(<ChatPage />);
    expect(container.textContent).toContain("so this is a general answer");
  });
});

describe("Chat answer check", () => {
  beforeEach(() => installDomShims());
  afterEach(() => cleanup());

  const status = (container: HTMLElement) => container.querySelector('[role="status"][aria-live="polite"]')!.textContent!.trim();

  it("says calmly when nothing in the items answers, keeping the passages behind the toggle", async () => {
    const { saved } = setup();
    const { container } = await render(<ChatPage />);
    await say(container, "When is the dentist?");
    const check: AnswerCheck = { not_found: true, claims: 0, flagged: [], invalid_citations: 0 };
    await reply("s-ask-1", "I couldn't find that in your items.", check);

    const item = container.querySelector("li.msg-assistant")!;
    expect(item.querySelector(".chat-not-found")!.textContent).toBe("Nothing in your items answers this.");
    expect(item.querySelector(".chat-check")).toBeNull();
    expect(status(container)).toBe("Answer ready. Nothing in your items answers this.");
    // The passage the model saw is still one toggle away.
    const more = byText(item, "button", "1 more passage the model saw")!;
    await click(more);
    expect(item.querySelector(".chat-citations-more")!.textContent).toContain("River walk");

    expect(saved[saved.length - 1]!.messages[1]).toMatchObject({ check: { not_found: true } });
  });

  it("lists claims not backed by a cited item behind a disclosure", async () => {
    const { saved } = setup();
    const { container } = await render(<ChatPage />);
    await say(container, "Where did we watch herons?");
    const check = {
      not_found: false,
      claims: 3,
      flagged: [
        { text: "It was in May.", reason: "uncited" },
        { text: "We saw 40 herons [1].", reason: "unsupported" },
        { text: "Some newer reason.", reason: "something_new" },
      ],
      invalid_citations: 0,
    } as unknown as AnswerCheck;
    await reply("s-ask-1", "By the river [1]. It was in May. We saw 40 herons [1]. Some newer reason.", check);

    const item = container.querySelector("li.msg-assistant")!;
    expect(item.querySelector(".chat-not-found")).toBeNull();
    expect(status(container)).toBe("Answer ready, 1 source.");
    const toggleBtn = byText(item, "button", "3 statements aren't backed by a cited item")!;
    expect(toggleBtn.getAttribute("aria-expanded")).toBe("false");
    expect(item.querySelector(".chat-check-list")).toBeNull();

    await click(toggleBtn);
    expect(toggleBtn.getAttribute("aria-expanded")).toBe("true");
    const list = item.querySelector(".chat-check-list")!;
    expect(toggleBtn.getAttribute("aria-controls")).toBe(list.id);
    expect([...list.querySelectorAll("li")].map((li) => li.textContent)).toEqual([
      "No source cited: It was in May.",
      "Not in the cited items: We saw 40 herons [1].",
      "Not in the cited items: Some newer reason.",
    ]);
    expect(list.querySelector('li span[dir="auto"]')!.textContent).toBe("It was in May.");

    // Citations still focus their source.
    await click(item.querySelector<HTMLButtonElement>("button.cite"));
    expect(document.activeElement?.textContent).toContain("River walk");

    // Saved with the turn; unknown reasons are kept as `unsupported`.
    const m = saved[saved.length - 1]!.messages[1]!;
    expect(m.check!.flagged.map((f) => f.reason)).toEqual(["uncited", "unsupported", "unsupported"]);
  });

  it("shows nothing extra for an answer without a check or with nothing flagged", async () => {
    const { saved } = setup();
    const { container } = await render(<ChatPage />);
    await say(container, "Where did we watch herons?");
    await reply("s-ask-1", "By the river [1].", { not_found: false, claims: 1, flagged: [], invalid_citations: 0 });
    await say(container, "And then?");
    await reply("s-ask-2", "Home [1].");
    expect(container.querySelector(".chat-check")).toBeNull();
    expect(container.querySelector(".chat-not-found")).toBeNull();
    expect(saved[saved.length - 1]!.messages[3]!.check).toBeUndefined();
  });

  it("restores the check from history, and turns saved without one still load", async () => {
    mockDaemon({
      chat_history_load: [
        {
          id: "c-1",
          title: "q",
          updated_at: 1,
          messages: [
            { role: "user", text: "q1" },
            { role: "assistant", text: "old answer", state: "done" },
            { role: "user", text: "q2" },
            {
              role: "assistant",
              text: "It was in May.",
              state: "done",
              check: { not_found: false, claims: 1, flagged: [{ text: "It was in May.", reason: "uncited" }], invalid_citations: 0 },
            },
            { role: "user", text: "q3" },
            { role: "assistant", text: "I couldn't find that in your items.", state: "done", check: { not_found: true } },
            { role: "user", text: "q4" },
            { role: "assistant", text: "odd", state: "done", check: "garbage" },
          ],
        },
      ],
    });
    const { container } = await render(<ChatPage />);
    const items = container.querySelectorAll("li.msg-assistant");
    expect(items).toHaveLength(4);
    expect(items[0]!.querySelector(".chat-check, .chat-not-found")).toBeNull();
    expect(byText(items[1]!, "button", "1 statement isn't backed by a cited item")).toBeDefined();
    expect(items[2]!.querySelector(".chat-not-found")!.textContent).toBe("Nothing in your items answers this.");
    expect(items[3]!.querySelector(".chat-check, .chat-not-found")).toBeNull();
  });
});
