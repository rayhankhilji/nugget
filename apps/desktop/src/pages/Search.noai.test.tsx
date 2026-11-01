// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act } from "react";
import { byText, cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";
import { SearchPage } from "./Search";
import type { ContextResponse } from "../api";

beforeEach(() => {
  installDomShims();
});
afterEach(cleanup);

const ctx: ContextResponse = {
  text: "[1] Ada wrote the first program",
  citations: [{ n: 1, item: { source: "files-1", key: "a.md" }, chunk_id: 1, title: "Notes" }],
};

async function openAskWithContext(container: HTMLElement) {
  await click(byText(container, "button", "Ask a question"));
  const q = container.querySelector<HTMLInputElement>("#ask-q")!;
  await act(async () => {
    const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
    set.call(q, "who wrote the first program?");
    q.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await click(byText(container, ".ask-form button", "Find context"));
  await flush();
}

describe("Ask on Search with no AI (GG6-14)", () => {
  it("explains what to do with a Set up AI button instead of an Ask button", async () => {
    mockDaemon({ providers_list: [], retrieve_context: ctx });
    const { container } = await render(<SearchPage />);
    await openAskWithContext(container);
    expect(container.textContent).toContain("No AI is set up yet, so Nugget can't write an answer.");
    expect(byText(container, "button", "Set up AI")).toBeDefined();
    expect(byText(container, "button", "Ask with this context")).toBeUndefined();
  });

  it("turns the daemon's no-provider error into the same explanation", async () => {
    mockDaemon({
      retrieve_context: ctx,
      retrieve_ask: () => {
        throw { kind: "rpc", message: "no provider is available for the synthesis tier", code: -32004 };
      },
    });
    const { container } = await render(<SearchPage />);
    await openAskWithContext(container);
    await click(container.querySelector<HTMLElement>(".ask-actions button"));
    await flush();
    const alert = container.querySelector('[role="alert"]');
    expect(alert?.textContent).toContain("No AI is set up yet");
    expect(alert?.textContent).not.toContain("synthesis tier");
  });
});
