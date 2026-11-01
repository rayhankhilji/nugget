// @vitest-environment jsdom
import axe from "axe-core";
import { act } from "react";
import { emit } from "@tauri-apps/api/event";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { type AskStarted } from "../api";
import { byText, cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";
import { QuickAsk } from "./QuickAsk";

const started: AskStarted = {
  stream_id: "s-q",
  provider: "local-chat",
  locality: "on_device",
  citations: [
    { n: 1, item: { source: "files-1", key: "/notes/plan.md" }, chunk_id: 1, title: "Plan", url: "file:///notes/plan.md" },
    { n: 2, item: { source: "gmail-1", key: "m1" }, chunk_id: 2, title: "Mail from Ada", url: null },
  ],
  excluded_sensitive: 0,
};

function setup(extra: Record<string, unknown> = {}) {
  const asked: unknown[] = [];
  const opened: string[] = [];
  const pages: string[] = [];
  const calls = mockDaemon({
    retrieve_ask: (a: unknown) => {
      asked.push((a as { request: unknown }).request);
      return started;
    },
    open_url: (a: unknown) => {
      opened.push((a as { url: string }).url);
      return "opened";
    },
    quickask_hide: null,
    quickask_open_main: (a: unknown) => {
      pages.push((a as { page: string }).page);
      return null;
    },
    ...extra,
  });
  return { calls, asked, opened, pages };
}

async function typeQuestion(container: HTMLElement, text: string) {
  const input = container.querySelector<HTMLInputElement>("#quickask-q")!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, text);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  return input;
}

async function submit(container: HTMLElement) {
  const form = container.querySelector("form")!;
  await act(async () => {
    form.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
  });
  await flush(3);
}

beforeEach(() => installDomShims());
afterEach(() => cleanup());

describe("quick ask", () => {
  it("asks, streams a cited answer, and opens a source", async () => {
    const { asked, opened } = setup();
    const { container } = await render(<QuickAsk />);
    await typeQuestion(container, "  what is the plan?  ");
    await submit(container);
    expect(asked).toHaveLength(1);
    expect(asked[0]).toMatchObject({ query: "what is the plan?", tier: "synthesis", provider: null });
    expect(container.querySelector('[aria-busy="true"]')).not.toBeNull();

    await act(() => emit("nugget:chat-delta", { stream_id: "s-q", text: "Ship in May [1], says Ada [2]." }));
    await act(() => emit("nugget:chat-done", { stream_id: "s-q", input_tokens: 1, output_tokens: 2, error: null }));
    await flush(3);

    expect(container.querySelector(".quickask-text")?.textContent).toBe("Ship in May [1], says Ada [2].");
    const cites = container.querySelectorAll<HTMLButtonElement>(".quickask-text button.cite");
    expect(cites).toHaveLength(2);
    expect(cites[0]!.getAttribute("aria-label")).toBe("Citation 1: Plan");
    expect(container.querySelector('[role="status"][aria-live="polite"]')?.textContent).toBe("Answer ready, 2 sources.");

    // A citation with a URL opens it; one without moves focus to its source.
    await click(cites[0]);
    expect(opened).toEqual(["file:///notes/plan.md"]);
    await click(cites[1]);
    expect(document.activeElement?.textContent).toContain("Mail from Ada");

    const items = container.querySelectorAll(".quickask-source");
    expect(items).toHaveLength(2);
    await click(byText(container, ".quickask-source button", "Open"));
    expect(opened).toEqual(["file:///notes/plan.md", "file:///notes/plan.md"]);
  });

  it("says a shell or connection failure in plain words, never its raw text (LL2)", async () => {
    setup({
      retrieve_ask: () => {
        throw { kind: "io", message: "Broken pipe (os error 32) at src/ipc.rs:88" };
      },
    });
    const { container } = await render(<QuickAsk />);
    await typeQuestion(container, "q");
    await submit(container);
    const alert = container.querySelector('[role="alert"]')!;
    expect(alert.textContent).toContain("Couldn't answer");
    expect(alert.textContent).not.toContain("os error");
    const result = await axe.run(container, { rules: { "color-contrast": { enabled: false }, region: { enabled: false } } });
    expect(result.violations.map((v) => v.id)).toEqual([]);
  });
});
