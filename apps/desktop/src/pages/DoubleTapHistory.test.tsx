// @vitest-environment jsdom
import axe from "axe-core";
import { act } from "react";
import { emit } from "@tauri-apps/api/event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DOUBLETAP_HISTORY_EVENT, type DraftHistoryEntry } from "../api";
import { byText, cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";
import { DoubleTapHistory } from "./DoubleTapHistory";

const ENTRIES: DraftHistoryEntry[] = [
  { id: 3, text: "Friday at 10 suits me.", app: "Mail", at: Date.now() - 120_000 },
  { id: 1, text: "شكرًا، أراك يوم الجمعة.", app: "", at: Date.now() - 7_200_000 },
];

function setup(list: unknown = ENTRIES) {
  const calls = mockDaemon({
    doubletap_history: list,
    doubletap_history_remove: (args: unknown) => {
      removed.push((args as { id: number }).id);
      return null;
    },
    doubletap_history_clear: null,
  });
  const removed: number[] = [];
  return { calls, removed };
}

beforeEach(() => installDomShims());
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("Recent drafts", () => {
  it("lists drafts newest first with app and time, with no axe violations", async () => {
    setup();
    const { container } = await render(<DoubleTapHistory />);
    await flush(3);
    expect(container.querySelector("h2")?.textContent).toBe("Recent drafts");
    expect(container.textContent).toContain("never stored on disk");
    const items = container.querySelectorAll("li");
    expect(items).toHaveLength(2);
    expect(items[0]!.textContent).toContain("Friday at 10 suits me.");
    expect(items[0]!.textContent).toContain("Mail · 2 minutes ago");
    expect(items[1]!.textContent).toContain("Unknown app · 2 hours ago");
    expect(items[1]!.querySelector(".dt-history-text")?.getAttribute("dir")).toBe("auto");
    const result = await axe.run(container, { rules: { "color-contrast": { enabled: false } } });
    expect(result.violations.map((v) => v.id)).toEqual([]);
  });

  it("copies a draft", async () => {
    setup();
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    const { container } = await render(<DoubleTapHistory />);
    await flush(3);
    await click(container.querySelectorAll("li")[0]!.querySelector("button"));
    expect(writeText).toHaveBeenCalledWith("Friday at 10 suits me.");
    expect(container.querySelector('[role="status"]')?.textContent).toBe("Copied to the clipboard.");
    writeText.mockRejectedValue(new Error("denied"));
    await click(container.querySelectorAll("li")[0]!.querySelector("button"));
    expect(container.querySelector('[role="status"]')?.textContent).toContain("Couldn't copy");
  });

  it("removes one, and clears all after confirming", async () => {
    const { removed, calls } = setup();
    const { container } = await render(<DoubleTapHistory />);
    await flush(3);
    await click(byText(container.querySelectorAll("li")[1]!, "button", "Remove"));
    expect(removed).toEqual([1]);
    expect(container.querySelectorAll("li")).toHaveLength(1);
    await click(byText(container, "button", "Clear all"));
    expect(calls).not.toContain("doubletap_history_clear");
    await click(byText(container, "button", "Cancel"));
    await click(byText(container, "button", "Clear all"));
    await click(byText(container, "button", "Clear drafts"));
    expect(calls).toContain("doubletap_history_clear");
    expect(container.textContent).toContain("No drafts yet");
  });

  it("follows changes from the shell", async () => {
    setup([]);
    const { container } = await render(<DoubleTapHistory />);
    await flush(3);
    expect(container.textContent).toContain("No drafts yet");
    expect(byText(container, "button", "Clear all")).toBeUndefined();
    await act(() => emit(DOUBLETAP_HISTORY_EVENT, ENTRIES));
    await flush(2);
    expect(container.querySelectorAll("li")).toHaveLength(2);
  });

  it("is hidden in a shell without a history", async () => {
    mockDaemon({});
    const { container } = await render(<DoubleTapHistory />);
    await flush(3);
    expect(container.innerHTML).toBe("");
  });
});
