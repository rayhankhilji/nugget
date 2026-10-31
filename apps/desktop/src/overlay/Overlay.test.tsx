// @vitest-environment jsdom
import axe from "axe-core";
import { act } from "react";
import { emit } from "@tauri-apps/api/event";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { DOUBLETAP_OVERLAY_EVENT, type OverlayView } from "../api";
import { cleanup, flush, installDomShims, mockDaemon, render } from "../test/render";
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

function setup(initial: OverlayView = streaming) {
  const accepted: unknown[] = [];
  const calls = mockDaemon({
    overlay_state: initial,
    overlay_accept: (args: unknown) => {
      accepted.push((args as { text: string | null }).text);
      return null;
    },
    overlay_cancel: null,
  });
  return { calls, accepted };
}

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

  it("merges the locality and provider into one pill, dropping raw or generic values", async () => {
    setup({ ...streaming, provider: "anthropic", locality: "cloud" });
    const { container } = await render(<Overlay />);
    await flush(5);
    const pills = container.querySelectorAll(".overlay-badges .badge");
    expect(pills).toHaveLength(1);
    expect(pills[0]!.textContent).toBe("Cloud · anthropic");
    expect(pills[0]!.className).toContain("badge-warn");

    await act(() => emit(DOUBLETAP_OVERLAY_EVENT, { ...streaming, provider: "local", locality: "on_device" }));
    await flush(2);
    expect(container.querySelector(".overlay-badges")?.textContent).toBe("On device");
    // A locality this version doesn't know (and a generic provider) shows nothing raw.
    await act(() => emit(DOUBLETAP_OVERLAY_EVENT, { ...streaming, provider: "local", locality: "somewhere" }));
    await flush(2);
    expect(container.querySelector(".overlay-badges")).toBeNull();
  });

  it("says Accept takes what is shown while the draft is still streaming", async () => {
    setup();
    const { container } = await render(<Overlay />);
    await flush(5);
    const accept = () => container.querySelector<HTMLButtonElement>(".overlay-foot button")!;
    expect(accept().textContent).toContain("Accept so far");
    expect(accept().title).toBe("Inserts the text shown now and stops the rest of the draft.");
    expect(accept().className).not.toContain("btn-primary");
    await act(() => emit(DOUBLETAP_OVERLAY_EVENT, { ...streaming, phase: "done" }));
    await flush(2);
    expect(accept().textContent).toMatch(/^Accept/);
    expect(accept().textContent).not.toContain("so far");
    expect(accept().className).toContain("btn-primary");
    expect(container.querySelector(".overlay-text")?.getAttribute("dir")).toBe("auto");
  });

  it("tells how to retry after an error, without reading out the status glyph", async () => {
    setup({ ...EMPTY_VIEW, phase: "error", message: "doubletap.error.draft_failed" });
    const { container } = await render(<Overlay />);
    await flush(5);
    expect(container.querySelector(".overlay-hint")?.textContent).toBe("To try again, close this and use your trigger again.");
    expect(container.querySelector(".overlay-icon")?.getAttribute("aria-hidden")).toBe("true");
    expect(container.querySelector('[role="status"]')?.textContent).toBe("Something went wrong");
  });

  it("Enter accepts unchanged", async () => {
    const { accepted } = setup();
    await render(<Overlay />);
    await flush(5);
    await key("Enter");
    expect(accepted).toEqual([null]);
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

  it("Esc cancels; follows overlay events", async () => {
    const { calls } = setup({ ...EMPTY_VIEW });
    const { container } = await render(<Overlay />);
    await flush(5);
    await act(() => emit(DOUBLETAP_OVERLAY_EVENT, { ...EMPTY_VIEW, phase: "refused", message: "doubletap.refused.secure" }));
    await flush(2);
    expect(container.textContent).toContain("secure field");
    await act(() => emit(DOUBLETAP_OVERLAY_EVENT, { ...EMPTY_VIEW, phase: "inserted", method: "clipboard", text: "x" }));
    await flush(2);
    expect(container.textContent).toContain("clipboard");
    await key("Escape");
    expect(calls).toContain("overlay_cancel");
  });
});
