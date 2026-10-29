// @vitest-environment jsdom
/** Chat page: streaming with Markdown, stop, errors, and persisted history. */
import { createElement } from "react";
import { beforeEach, describe, expect, it } from "vitest";
import { EVENT, type StoredConversation } from "../src/api";
import { ChatPage, writeUseSources } from "../src/pages/Chat";
import { requestNewChat } from "../src/hooks.nav";
import { click, daemon, emitEvent, flush, press, render, setupHarness, submit, type } from "./harness";

setupHarness();

const started = { stream_id: "s1", provider: "local-chat" };

async function renderChat() {
  const r = await render(createElement(ChatPage));
  await flush(5);
  return r.container;
}

async function send(root: HTMLElement, text: string) {
  await type(root.querySelector("#chat-input"), text);
  await submit(root.querySelector("form.composer"));
}

const delta = (text: string, stream_id = "s1") => emitEvent(EVENT.chatDelta, { stream_id, text });
const done = (error: string | null = null, stream_id = "s1") =>
  emitEvent(EVENT.chatDone, { stream_id, error, input_tokens: 10, output_tokens: 5 });

function savedConversation(d: ReturnType<typeof daemon>): StoredConversation {
  const args = d.last("chat_history_save");
  expect(args).toBeDefined();
  return args!.conversation as StoredConversation;
}

describe("Chat", () => {
  // These flows cover plain chat; answering from sources is covered below.
  beforeEach(() => writeUseSources(false));

  it("streams a reply, renders Markdown when done and saves the turn once", async () => {
    const d = daemon({ chat: started });
    const root = await renderChat();
    expect(root.querySelector(".chat-empty")).not.toBeNull();
    expect(root.textContent).toContain("History is saved on this device, unencrypted");

    await send(root, "Hello <b>there</b>");
    expect(d.last("chat")).toMatchObject({ request: { messages: [{ role: "user", text: "Hello <b>there</b>" }] } });
    const log = root.querySelector('[role="log"]')!;
    expect(log.getAttribute("aria-busy")).toBe("true");
    // User text stays plain.
    expect(root.querySelector(".msg-user .msg-body")?.textContent).toBe("Hello <b>there</b>");

    await delta("Here is **bold** and\n\n```js\nlet x");
    expect(root.querySelector(".msg-assistant strong")?.textContent).toBe("bold");
    expect(root.querySelector(".msg-assistant pre code")?.textContent).toBe("let x");
    expect(d.count("chat_history_save")).toBe(0); // not saved per delta

    await delta(" = 1;\n```\n");
    await done();
    expect(log.getAttribute("aria-busy")).toBe("false");
    expect(root.querySelector(".msg-assistant pre code")?.textContent).toBe("let x = 1;");
    expect(root.querySelector('.msg-assistant .msg-meta button[aria-label="Copy reply"]')).not.toBeNull();

    expect(d.count("chat_history_save")).toBe(1);
    const conv = savedConversation(d);
    expect(conv.title).toBe("Hello <b>there</b>");
    expect(conv.messages).toEqual([
      { role: "user", text: "Hello <b>there</b>" },
      { role: "assistant", text: "Here is **bold** and\n\n```js\nlet x = 1;\n```\n", state: "done", provider: "local-chat", output_tokens: 5 },
    ]);
    // The new conversation shows up in the list.
    expect(root.querySelector('.chat-history-open[aria-current="true"]')?.textContent).toBe("Hello <b>there</b>");
  });

  it("stops with Escape and saves the reply as stopped", async () => {
    const d = daemon({ chat: started, chat_cancel: null });
    const root = await renderChat();
    await send(root, "Long one");
    await delta("partial");
    await press(root.querySelector("#chat-input"), "Escape");
    expect(d.last("chat_cancel")).toEqual({ streamId: "s1" });
    expect(root.querySelector(".msg-assistant .badge, .msg-assistant [class*=badge]")?.textContent).toBe("Stopped");
    // Late deltas from the stopped stream are ignored.
    await delta(" more");
    await done("cancelled");
    expect(root.querySelector(".msg-assistant .msg-md")?.textContent).toBe("partial");
    const conv = savedConversation(d);
    expect(conv.messages[1]).toMatchObject({ role: "assistant", text: "partial", state: "cancelled" });
    expect(d.count("chat_history_save")).toBe(1);
  });

  it("shows errors from the call and from the stream, and saves them", async () => {
    const d = daemon({ chat: started });
    const root = await renderChat();
    await send(root, "Hi");
    await done("provider exploded");
    expect(root.querySelector('.msg-assistant [role="alert"]')?.textContent).toBe("provider exploded");
    expect(savedConversation(d).messages[1]).toMatchObject({ state: "error", error: "provider exploded" });

    d.fail("chat", { kind: "rpc", message: "no provider for tier", code: -32000 });
    await send(root, "Again");
    const alerts = root.querySelectorAll('.msg-assistant [role="alert"]');
    expect(alerts[1]?.textContent).toBe("no provider for tier");
  });

  it("restores the last conversation on mount and switches between saved ones", async () => {
    const convs: StoredConversation[] = [
      {
        id: "c-new",
        title: "Newest",
        updated_at: 2,
        messages: [
          { role: "user", text: "Newest" },
          { role: "assistant", text: "A *saved* reply", state: "done", provider: "anthropic" },
        ],
      },
      { id: "c-old", title: "Older", updated_at: 1, messages: [{ role: "user", text: "Older" }, { role: "assistant", text: "half", state: "cancelled" }] },
    ];
    const d = daemon({ chat_history_load: convs, chat: started });
    const root = await renderChat();
    expect(root.querySelector(".msg-assistant em")?.textContent).toBe("saved");
    expect(root.querySelectorAll(".chat-history-open")).toHaveLength(2);
    expect(d.count("chat_history_save")).toBe(0); // restoring is not a change

    await click(root.querySelector('.chat-history-open[title="Older"]'));
    expect(root.querySelector(".msg-user .msg-body")?.textContent).toBe("Older");
    expect(root.textContent).toContain("Stopped");

    // Continuing a restored chat sends its history and saves under the same id.
    await send(root, "Go on");
    expect((d.last("chat") as { request: { messages: unknown[] } }).request.messages).toEqual([
      { role: "user", text: "Older" },
      { role: "assistant", text: "half" },
      { role: "user", text: "Go on" },
    ]);
    await delta("ok");
    await done();
    expect(savedConversation(d).id).toBe("c-old");

    // New chat (also from the command palette) starts empty.
    requestNewChat();
    await flush(3);
    expect(root.querySelectorAll(".msg")).toHaveLength(0);
  });

  it("clears history after confirming", async () => {
    const convs: StoredConversation[] = [{ id: "c1", title: "One", updated_at: 1, messages: [{ role: "user", text: "One" }] }];
    const d = daemon({ chat_history_load: convs });
    const root = await renderChat();
    const clearBtn = Array.from(root.querySelectorAll("button")).find((b) => b.textContent === "Clear history");
    await click(clearBtn);
    await click(Array.from(root.querySelectorAll("button")).find((b) => b.textContent === "Delete all"));
    expect(d.count("chat_history_clear")).toBe(1);
    expect(root.querySelectorAll(".msg, .chat-history-open")).toHaveLength(0);
  });

  it("keeps working when the shell has no history commands", async () => {
    const d = daemon({ chat: started });
    const missing = { kind: "other", message: "Command chat_history_load not found" };
    d.fail("chat_history_load", missing).fail("chat_history_save", missing);
    const root = await renderChat();
    expect(root.querySelector(".chat-history")).toBeNull();
    expect(root.querySelector('[role="alert"]')).toBeNull();
    await send(root, "Hi");
    await delta("there");
    await done();
    expect(root.querySelector(".msg-assistant .msg-md")?.textContent).toBe("there");
    expect(d.count("chat_history_save")).toBe(0);
  });

  it("answers from sources with the conversation, and its citations open", async () => {
    writeUseSources(true);
    const d = daemon({
      retrieve_ask: {
        stream_id: "s1",
        provider: "local-chat",
        locality: "on_device",
        citations: [{ n: 1, item: { source: "web-1", key: "a" }, chunk_id: 1, title: "Heron notes", url: "https://example.com/herons", sensitive: false }],
        excluded_sensitive: 0,
        query_rewrite: "herons second walk",
      },
      open_url: null,
    });
    const root = await renderChat();
    await send(root, "and the second one?");
    expect(d.count("chat")).toBe(0);
    expect(d.last("retrieve_ask")).toMatchObject({ request: { query: "and the second one?", history: [], tier: "synthesis" } });
    await delta("At the lake [1].");
    await done();
    expect(root.textContent).toContain("Searched for: herons second walk");
    await click(root.querySelector(".msg-assistant button.cite"));
    expect(d.last("open_url")).toMatchObject({ url: "https://example.com/herons" });
    expect(savedConversation(d).messages[1]).toMatchObject({ citations: [{ n: 1, title: "Heron notes" }] });
  });
});
