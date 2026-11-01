// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act } from "react";
import { emit } from "@tauri-apps/api/event";
import type { RuntimeInfo } from "../api";
import { ChatPage, failedOnDevice } from "./Chat";
import { usePendingPage } from "../hooks.nav";
import { byText, cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";

type Req = { request: Record<string, unknown> };

const chatRuntime = (over: Partial<RuntimeInfo> = {}): RuntimeInfo => ({
  model_id: "qwen3-4b-q4km",
  role: "chat",
  state: "failed",
  provider: "local-chat",
  error: "The on-device chat model stopped unexpectedly.",
  gave_up: true,
  ...over,
});

function setup(runtimes: () => RuntimeInfo[] | Error) {
  const chats: Record<string, unknown>[] = [];
  const retries: string[] = [];
  const calls = mockDaemon({
    chat_history_load: [],
    chat_history_save: null,
    runtime_status: () => {
      const r = runtimes();
      if (r instanceof Error) throw { kind: "rpc", message: "method not found", code: -32601 };
      return r;
    },
    runtime_retry: (a: unknown) => {
      retries.push((a as { role: string }).role);
      return { started: true };
    },
    chat: (a: unknown) => {
      chats.push((a as Req).request);
      return { stream_id: `s-${chats.length}`, provider: "local-chat", locality: "on_device" };
    },
  });
  localStorage.setItem("nugget.chat.useSources", "false");
  return { chats, retries, calls };
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

async function done(streamId: string, error: string | null, text = "") {
  if (text) await act(() => emit("nugget:chat-delta", { stream_id: streamId, text }));
  await act(() => emit("nugget:chat-done", { stream_id: streamId, input_tokens: null, output_tokens: null, error }));
  await flush();
}

function PageSpy({ seen }: { seen: string[] }) {
  usePendingPage((id) => seen.push(id));
  return null;
}

describe("Chat Retry (LL2)", () => {
  beforeEach(() => installDomShims());
  afterEach(() => cleanup());

  it("offers Retry and Status details when the local model crashed, and asks again in place", async () => {
    let state: RuntimeInfo = chatRuntime();
    const { chats, retries } = setup(() => [state]);
    const seen: string[] = [];
    const { container } = await render(
      <>
        <ChatPage />
        <PageSpy seen={seen} />
      </>,
    );
    await say(container, "When is the dentist?");
    await done("s-1", "The on-device chat model stopped unexpectedly; restarting it. Try again in a moment.", "Partial");

    const alert = container.querySelector(".chat-error [role=alert]")!;
    expect(alert.textContent).toContain("stopped unexpectedly");
    const retry = byText(container, "button", "Retry")!;
    expect(retry).toBeTruthy();

    await click(byText(container, "button", "Status details")!);
    expect(seen).toEqual(["status"]);

    // Retry: the gave-up server is started again, then the same question is asked with the same history.
    await click(retry);
    await flush();
    expect(retries).toEqual(["chat"]);
    expect(chats).toHaveLength(2);
    expect(chats[1]!.messages).toEqual([{ role: "user", text: "When is the dentist?" }]);
    expect(container.querySelector(".chat-error")).toBeNull();
    // In place: still one question and one reply.
    expect(container.querySelectorAll(".msg-user")).toHaveLength(1);
    expect(container.querySelectorAll(".msg-assistant")).toHaveLength(1);
    expect(document.activeElement?.id).toBe("chat-input");

    state = chatRuntime({ state: "ready", gave_up: false, error: null });
    await done("s-2", null, "Tuesday at 9.");
    expect(container.querySelector(".msg-assistant")!.textContent).toContain("Tuesday at 9.");
    expect(byText(container, "button", "Retry")).toBeFalsy();
  });

  it("offers Retry on the newest failed reply only, with no Status link for a cloud failure", async () => {
    const chats: Record<string, unknown>[] = [];
    mockDaemon({
      chat_history_load: [],
      chat_history_save: null,
      runtime_status: [chatRuntime({ state: "ready", gave_up: false })],
      chat: (a: unknown) => {
        chats.push((a as Req).request);
        return { stream_id: `s-${chats.length}`, provider: "cloud", locality: "cloud" };
      },
    });
    localStorage.setItem("nugget.chat.useSources", "false");
    const { container } = await render(<ChatPage />);
    await say(container, "One");
    await done("s-1", "rate limited");
    expect(byText(container, "button", "Retry")).toBeTruthy();
    expect(byText(container, "button", "Status details")).toBeFalsy();
    await say(container, "Two");
    await done("s-2", null, "Fine.");
    // The older failure keeps its message but no longer offers Retry.
    expect(container.textContent).toContain("rate limited");
    expect(byText(container, "button", "Retry")).toBeFalsy();
    expect(chats).toHaveLength(2);
  });

  it("retries a later question with sources, with the earlier turns as history, and saves the new answer", async () => {
    const asks: Record<string, unknown>[] = [];
    const saved: { messages: { role: string; text: string; state?: string }[] }[] = [];
    mockDaemon({
      chat_history_load: [],
      chat_history_save: (a: unknown) => {
        saved.push((a as { conversation: (typeof saved)[number] }).conversation);
      },
      runtime_status: [chatRuntime({ state: "ready", gave_up: false, error: null })],
      retrieve_ask: (a: unknown) => {
        asks.push((a as Req).request);
        return {
          stream_id: `s-ask-${asks.length}`,
          provider: "local-chat",
          locality: "on_device",
          citations: [{ n: 1, item: { source: "files-1", key: "a.md" }, chunk_id: 1, title: "A", url: null, sensitive: false }],
          excluded_sensitive: 0,
        };
      },
    });
    const { container } = await render(<ChatPage />);
    await say(container, "One?");
    await done("s-ask-1", null, "First [1].");
    await say(container, "Two?");
    await done("s-ask-2", "The on-device chat model stopped unexpectedly; restarting it.");
    await click(byText(container, "button", "Retry")!);
    await flush();
    expect(asks).toHaveLength(3);
    expect(asks[2]).toMatchObject({
      query: "Two?",
      history: [
        { role: "user", text: "One?" },
        { role: "assistant", text: "First." },
      ],
    });
    await done("s-ask-3", null, "Second [1].");
    const last = saved[saved.length - 1]!;
    expect(last.messages.map((m) => [m.role, m.text, m.state])).toEqual([
      ["user", "One?", undefined],
      ["assistant", "First [1].", "done"],
      ["user", "Two?", undefined],
      ["assistant", "Second [1].", "done"],
    ]);
  });

  it("still offers Retry when the daemon has no runtime status", async () => {
    const { chats } = setup(() => new Error("missing"));
    const { container } = await render(<ChatPage />);
    await say(container, "Hello");
    await done("s-1", "connection refused");
    // Its provider is a local runtime's, so Status has the details even without the list.
    expect(byText(container, "button", "Status details")).toBeTruthy();
    await click(byText(container, "button", "Retry")!);
    await flush();
    expect(chats).toHaveLength(2);
  });
});

describe("failedOnDevice", () => {
  it("knows local providers and a down chat model", () => {
    expect(failedOnDevice({ provider: "local-chat" }, null)).toBe(true);
    expect(failedOnDevice({ provider: "cloud" }, [chatRuntime()])).toBe(false);
    expect(failedOnDevice({ provider: "llama" }, [chatRuntime({ provider: "llama" })])).toBe(true);
    expect(failedOnDevice({}, [chatRuntime()])).toBe(true);
    expect(failedOnDevice({}, [chatRuntime({ state: "ready", gave_up: false })])).toBe(false);
    expect(failedOnDevice({}, null)).toBe(false);
    // An installed model at rest is not why a cloud call failed.
    expect(failedOnDevice({}, [chatRuntime({ state: "stopped", gave_up: false, error: null })])).toBe(false);
    expect(failedOnDevice({}, [chatRuntime({ state: "starting", gave_up: false, error: null })])).toBe(false);
    expect(failedOnDevice({}, [chatRuntime({ state: "starting", gave_up: false, error: "The on-device chat model stopped responding; restarting it." })])).toBe(true);
  });
});
