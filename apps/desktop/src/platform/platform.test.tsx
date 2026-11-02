// @vitest-environment jsdom
/** II4: notification / quit preferences, onboarding opt-ins, deep links and shell labels. */
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, flush, installDomShims, mockDaemon, render } from "../test/render";
import { DeepLinks } from "./DeepLinks";
import { QuitPrompt } from "./QuitPrompt";
import { emit } from "@tauri-apps/api/event";
import { QUIT_ASK_EVENT } from "../api";



async function click(el: HTMLElement) {
  await act(async () => {
    el.click();
  });
  await flush();
}

function button(root: ParentNode, text: RegExp): HTMLButtonElement {
  const b = Array.from(root.querySelectorAll("button")).find((x) => text.test(x.textContent ?? ""));
  if (!b) throw new Error(`no button ${text}`);
  return b;
}

beforeEach(() => installDomShims());
afterEach(() => cleanup());

describe("QuitPrompt", () => {
  async function ask() {
    await act(() => emit(QUIT_ASK_EVENT, null));
    await flush();
  }

  it("asks when the shell says so, saves the answer and quits", async () => {
    const answers: unknown[] = [];
    let acks = 0;
    mockDaemon({ quit_answer: (a: unknown) => (answers.push(a), null), quit_cancel: null, quit_ack: () => (acks++, acks > 1) });
    await render(<QuitPrompt />);
    expect(document.querySelector("dialog")).toBeNull();
    await ask();
    // The shell is told the question shows (else it quits with the default after 2 s).
    expect(acks).toBe(2);
    const dialog = document.querySelector("dialog");
    expect(dialog?.textContent).toContain("When you quit Heed, also stop background indexing and the local AI?");
    await click(button(dialog!, /^Keep running in the background$/));
    expect(answers).toEqual([{ choice: "keep", quit: true }]);
    expect(document.querySelector("dialog")).toBeNull();
    await ask();
    await click(button(document.querySelector("dialog")!, /^Stop everything$/));
    expect(answers.at(-1)).toEqual({ choice: "stop", quit: true });
  });
});

describe("DeepLinks", () => {
  it("asks before opening an item, and opens nothing on Cancel", async () => {
    const calls = mockDaemon({
      deeplink_take: () => ({ kind: "item", source: "notes", key: "plan.md" }),
      search: () => [{ item: { source: "notes", key: "plan.md" }, title: "Plan", snippet: "", score: 1, url: "file:///n/plan.md" }],
      open_url: () => "opened",
    });
    const { container } = await render(<DeepLinks pages={["today"]} go={() => {}} />);
    const dialog = container.ownerDocument.querySelector("dialog");
    expect(dialog?.textContent).toContain("“plan.md” from notes");
    expect(calls).not.toContain("search");
    await click(button(container.ownerDocument, /^Cancel$/));
    expect(calls).not.toContain("open_url");
    expect(container.ownerDocument.querySelector("dialog")).toBeNull();
  });

  it("opens the confirmed item through open_url", async () => {
    const opened: unknown[] = [];
    mockDaemon({
      deeplink_take: () => ({ kind: "item", source: "notes", key: "plan.md" }),
      search: () => [
        { item: { source: "other", key: "plan.md" }, title: "x", snippet: "", score: 2, url: "file:///other" },
        { item: { source: "notes", key: "plan.md" }, title: "Plan", snippet: "", score: 1, url: "file:///n/plan.md" },
      ],
      open_url: (a: unknown) => {
        opened.push(a);
        return "opened";
      },
    });
    const { container } = await render(<DeepLinks pages={["today"]} go={() => {}} />);
    await click(button(container.ownerDocument, /^Open$/));
    expect(opened).toEqual([{ url: "file:///n/plan.md" }]);
  });

  it("asks before handing a plugin file to the Plugins page", async () => {
    const went: string[] = [];
    mockDaemon({ deeplink_take: () => ({ kind: "plugin", path: "/home/a/Downloads/hello.nuggetplugin" }) });
    const { container } = await render(<DeepLinks pages={["plugins"]} go={(p) => went.push(p)} />);
    expect(container.ownerDocument.querySelector("dialog")?.textContent).toContain("hello.nuggetplugin");
    expect(went).toEqual([]);
    await click(button(container.ownerDocument, /^Continue$/));
    expect(went).toEqual(["plugins"]);
  });
});
