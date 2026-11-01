// @vitest-environment jsdom
import axe from "axe-core";
import { act } from "react";
import { emit } from "@tauri-apps/api/event";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { DOUBLETAP_OVERLAY_EVENT, type OverlayView } from "../api";
import { byText, cleanup, click, flush, installDomShims, mockDaemon, render } from "../test/render";
import { Overlay } from "./Overlay";
import { EMPTY_VIEW } from "./state";

const streaming: OverlayView = {
  ...EMPTY_VIEW,
  phase: "streaming",
  text: "Thanks Ada, Thursday works.",
  provider: "local-chat",
  locality: "on_device",
  citations: 2,
  excluded_sensitive: 1,
};

function setup(initial: OverlayView = streaming, extra: Record<string, unknown> = {}) {
  const accepted: unknown[] = [];
  const sent: { cmd: string; args: unknown }[] = [];
  const record = (cmd: string, result: unknown = null) => (args: unknown) => {
    sent.push({ cmd, args });
    return result;
  };
  const calls = mockDaemon({
    overlay_state: initial,
    overlay_accept: (args: unknown) => {
      accepted.push((args as { text: string | null }).text);
      return null;
    },
    overlay_cancel: null,
    overlay_retry: record("overlay_retry"),
    overlay_refine: record("overlay_refine"),
    overlay_insert_previous: record("overlay_insert_previous"),
    ...extra,
  });
  return { calls, accepted, sent };
}

const ready: OverlayView = { ...streaming, phase: "done", can_retry: true };

async function key(k: string, opts: KeyboardEventInit = {}, target: EventTarget = window) {
  await act(async () => {
    target.dispatchEvent(new KeyboardEvent("keydown", { key: k, bubbles: true, cancelable: true, ...opts }));
  });
  await flush(2);
}

beforeEach(() => installDomShims());
afterEach(() => cleanup());

describe("overlay", () => {
  it("shows the draft, badges and counts, with no axe violations", async () => {
    setup();
    const { container } = await render(<Overlay />);
    await flush(5);
    expect(container.textContent).toContain("Thanks Ada, Thursday works.");
    expect(container.textContent).toContain("local-chat");
    expect(container.textContent).toContain("On device");
    expect(container.textContent).toContain("2 sources");
    expect(container.textContent).toContain("1 sensitive item withheld");
    const result = await axe.run(container, { rules: { "color-contrast": { enabled: false } } });
    expect(result.violations.map((v) => v.id)).toEqual([]);
  });

  it("Tab edits, Ctrl+Enter accepts the edited text", async () => {
    const { accepted } = setup({ ...streaming, phase: "done" });
    const { container } = await render(<Overlay />);
    await flush(5);
    await key("Tab");
    const area = container.querySelector("textarea")!;
    expect(area.value).toBe("Thanks Ada, Thursday works.");
    expect(document.activeElement).toBe(area);
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(area, "Thursday works!");
      area.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await key("Enter", {}, area);
    expect(accepted).toEqual([]);
    await key("Enter", { ctrlKey: true }, area);
    expect(accepted).toEqual(["Thursday works!"]);
  });

  it("Retry asks for a new draft and keeps focus on the card", async () => {
    const { sent } = setup(ready);
    const { container } = await render(<Overlay />);
    await flush(5);
    const btn = byText(container, ".overlay-tools button", /^Retry/)!;
    expect(btn.getAttribute("aria-keyshortcuts")).toBe("R");
    btn.focus();
    await click(btn);
    expect(sent.map((c) => c.cmd)).toEqual(["overlay_retry"]);
    await act(() => emit(DOUBLETAP_OVERLAY_EVENT, { ...EMPTY_VIEW, phase: "reading", can_retry: true }));
    await flush(2);
    expect(document.activeElement).toBe(container.querySelector("main"));
    // R from the keyboard, too (once the new draft is on screen).
    await act(() => emit(DOUBLETAP_OVERLAY_EVENT, ready));
    await flush(2);
    await key("r");
    expect(sent.map((c) => c.cmd)).toEqual(["overlay_retry", "overlay_retry"]);
  });

  it("each refinement rewrites the text shown, by button and by key", async () => {
    const { sent } = setup(ready);
    const { container } = await render(<Overlay />);
    await flush(5);
    const group = container.querySelector('[role="group"]')!;
    expect(group.getAttribute("aria-label")).toBe("Rewrite the draft");
    const labels = Array.from(group.querySelectorAll("button")).map((b) => [b.textContent, b.getAttribute("aria-keyshortcuts")]);
    expect(labels).toEqual([
      ["ShorterS", "S"],
      ["LongerL", "L"],
      ["More formalF", "F"],
      ["More casualC", "C"],
    ]);
    const kinds = ["shorter", "longer", "formal", "casual"];
    for (const [i, kind] of kinds.entries()) {
      await click(container.querySelectorAll('[role="group"] button')[i]);
      expect(sent.at(-1)).toEqual({ cmd: "overlay_refine", args: { kind, base: null } });
      // The shell starts the refinement: reading, then streaming again.
      await act(() => emit(DOUBLETAP_OVERLAY_EVENT, { ...EMPTY_VIEW, phase: "reading", can_retry: true, refinement: kind }));
      await flush(2);
      await act(() => emit(DOUBLETAP_OVERLAY_EVENT, ready));
      await flush(2);
    }
    await key("l");
    expect(sent.at(-1)).toEqual({ cmd: "overlay_refine", args: { kind: "longer", base: null } });
  });
});
