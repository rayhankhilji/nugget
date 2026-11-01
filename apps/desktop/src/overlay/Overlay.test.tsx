// @vitest-environment jsdom
import axe from "axe-core";
import { act } from "react";
import { emit } from "@tauri-apps/api/event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DOUBLETAP_HISTORY_EVENT, DOUBLETAP_OVERLAY_EVENT, OVERLAY_MAX_HEIGHT, OVERLAY_MIN_HEIGHT, type DraftHistoryEntry, type OverlayView } from "../api";
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
const HISTORY: DraftHistoryEntry[] = [
  { id: 2, text: "Friday at 10 suits me.", app: "Mail", at: Date.now() - 60_000 },
  { id: 1, text: "Sounds good, see you then.", app: "", at: Date.now() - 3_600_000 },
];

/** The polite live region's text. */
const said = (root: ParentNode) => root.querySelector('[aria-live="polite"]')?.textContent;

async function type(area: HTMLTextAreaElement, value: string) {
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(area, value);
    area.dispatchEvent(new Event("input", { bubbles: true }));
  });
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
    // LL2 (GG7-10): the draft's direction is its majority script's.
    expect(container.querySelector(".overlay-text")?.getAttribute("dir")).toBe("ltr");
  });

  it("tells how to retry after an error, without reading out the status glyph", async () => {
    setup({ ...EMPTY_VIEW, phase: "error", message: "doubletap.error.draft_failed" });
    const { container } = await render(<Overlay />);
    await flush(5);
    expect(container.querySelector(".overlay-hint")?.textContent).toBe("To try again, close this and use your trigger again.");
    expect(container.querySelector(".overlay-icon")?.getAttribute("aria-hidden")).toBe("true");
    expect(container.querySelector(".overlay-phase")?.textContent).toBe("Something went wrong");
    // The live region says what went wrong, once.
    expect(said(container)).toBe("The draft couldn't be written. Try again.");
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

  it("offers Retry after a failed draft when the shell can, instead of the hint", async () => {
    const { sent } = setup({ ...EMPTY_VIEW, phase: "error", message: "doubletap.error.draft_failed", can_retry: true });
    const { container } = await render(<Overlay />);
    await flush(5);
    expect(container.querySelector(".overlay-hint")).toBeNull();
    expect(container.querySelector(".overlay-refine")).toBeNull();
    await click(byText(container, "button", /^Retry/));
    expect(sent.map((c) => c.cmd)).toEqual(["overlay_retry"]);
  });

  it("hides Retry and the refinements for an older shell", async () => {
    setup({ ...streaming, phase: "done" });
    const { container } = await render(<Overlay />);
    await flush(5);
    expect(container.querySelector(".overlay-tools")).toBeNull();
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

  it("a refinement while editing sends the edited text; the phase says what is happening", async () => {
    const { sent } = setup(ready);
    const { container } = await render(<Overlay />);
    await flush(5);
    await key("Tab");
    await type(container.querySelector("textarea")!, "Thursday works for me.");
    // Letters are text while editing.
    await key("s", {}, container.querySelector("textarea")!);
    expect(sent).toEqual([]);
    await click(byText(container, ".overlay-refine button", /^Shorter/));
    expect(sent).toEqual([{ cmd: "overlay_refine", args: { kind: "shorter", base: "Thursday works for me." } }]);
    await act(() => emit(DOUBLETAP_OVERLAY_EVENT, { ...EMPTY_VIEW, phase: "reading", can_retry: true, refinement: "shorter" }));
    await flush(2);
    expect(container.querySelector(".overlay-phase")?.textContent).toBe("Making it shorter…");
    expect(said(container)).toBe("Making it shorter…");
    expect(container.querySelector("textarea")).toBeNull();
    expect(document.activeElement).toBe(container.querySelector("main"));
  });

  it("after an edit: Insert sends the edit, Insert as is the draft shown", async () => {
    const { accepted } = setup(ready);
    const { container } = await render(<Overlay />);
    await flush(5);
    await key("Tab");
    expect(byText(container, ".overlay-foot button", /^Insert as is/)).toBeUndefined();
    const area = container.querySelector("textarea")!;
    await type(area, "Thursday works!");
    const asIs = byText(container, ".overlay-foot button", /^Insert as is/)!;
    const insert = byText(container, ".overlay-foot button", /^Insert(?! as is)/)!;
    expect(insert.className).toContain("btn-primary");
    expect(asIs.getAttribute("aria-keyshortcuts")).toBe("Control+Shift+Enter");
    await click(asIs);
    expect(accepted).toEqual([null]);
  });

  it("Ctrl+Shift+Enter inserts as is, Ctrl+Enter the edit", async () => {
    const { accepted } = setup(ready);
    const { container } = await render(<Overlay />);
    await flush(5);
    await key("Tab");
    const area = container.querySelector("textarea")!;
    await type(area, "Thursday works!");
    await key("Enter", { ctrlKey: true, shiftKey: true }, area);
    expect(accepted).toEqual([null]);
  });

  it("lists previous drafts and inserts the one picked; Esc closes the list, then cancels", async () => {
    const { sent, calls } = setup(ready, { doubletap_history: HISTORY });
    const { container } = await render(<Overlay />);
    await flush(5);
    const toggle = byText(container, "button", /^Previous drafts/)!;
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    await key("h");
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    const items = container.querySelectorAll<HTMLButtonElement>("#overlay-history button");
    expect(items).toHaveLength(2);
    expect(document.activeElement).toBe(items[0]);
    // The whole draft is in the name (truncated only on screen), with its app and time.
    expect(items[0]!.textContent).toContain("Friday at 10 suits me.");
    expect(items[0]!.textContent).toContain("Mail · 1 minute ago");
    expect(items[1]!.textContent).toContain("Unknown app");
    expect(items[0]!.querySelector("[dir=auto]")).not.toBeNull();
    // Arrows move within the list.
    await key("ArrowDown", {}, items[0]);
    expect(document.activeElement).toBe(items[1]);
    await key("Escape", {}, items[1]);
    expect(container.querySelector("#overlay-history")).toBeNull();
    expect(document.activeElement).toBe(toggle);
    expect(calls).not.toContain("overlay_cancel");
    await key("h");
    await click(container.querySelectorAll("#overlay-history button")[1]);
    expect(sent).toEqual([{ cmd: "overlay_insert_previous", args: { id: 1 } }]);
    await key("Escape");
    expect(calls).toContain("overlay_cancel");
  });

  it("follows history changes, and hides it when empty or practising", async () => {
    setup(ready, { doubletap_history: [] });
    const { container } = await render(<Overlay />);
    await flush(5);
    expect(byText(container, "button", /^Previous drafts/)).toBeUndefined();
    await act(() => emit(DOUBLETAP_HISTORY_EVENT, HISTORY));
    await flush(2);
    expect(byText(container, "button", /^Previous drafts/)).toBeDefined();
    await act(() => emit(DOUBLETAP_OVERLAY_EVENT, { ...ready, practice: true }));
    await flush(2);
    expect(byText(container, "button", /^Previous drafts/)).toBeUndefined();
    expect(container.querySelector(".overlay-badges")?.textContent).toContain("Practice");
  });

  it("announces state changes, not each streamed delta", async () => {
    setup({ ...EMPTY_VIEW, phase: "reading" });
    const { container } = await render(<Overlay />);
    await flush(5);
    const region = container.querySelector('[aria-live="polite"]')!;
    expect(region.className).toContain("visually-hidden");
    expect(container.querySelectorAll("[aria-live=polite], [role=status]")).toHaveLength(1);
    expect(region.textContent).toBe("Reading the field…");
    await act(() => emit(DOUBLETAP_OVERLAY_EVENT, { ...streaming, text: "Thanks" }));
    await flush(2);
    expect(region.textContent).toBe("Drafting…");
    const changes: string[] = [];
    const mo = new MutationObserver(() => changes.push(region.textContent ?? ""));
    mo.observe(region, { childList: true, characterData: true, subtree: true });
    await act(() => emit(DOUBLETAP_OVERLAY_EVENT, { ...streaming, text: "Thanks Ada" }));
    await act(() => emit(DOUBLETAP_OVERLAY_EVENT, { ...streaming, text: "Thanks Ada, Thursday" }));
    await flush(2);
    expect(changes).toEqual([]);
    await act(() => emit(DOUBLETAP_OVERLAY_EVENT, ready));
    await flush(2);
    expect(region.textContent).toBe("Draft ready, 4 words");
    await act(() => emit(DOUBLETAP_OVERLAY_EVENT, { ...EMPTY_VIEW, phase: "inserted", verified: true, method: "accessibility" }));
    await flush(2);
    expect(region.textContent).toBe("The draft is in the field, and it reads back correctly.");
    mo.disconnect();
  });

  it("the draft is a labelled, focusable region; R retries and Tab edits from it", async () => {
    const { sent } = setup(ready);
    const { container } = await render(<Overlay />);
    await flush(5);
    const region = container.querySelector<HTMLElement>(".overlay-text")!;
    expect(region.getAttribute("role")).toBe("region");
    expect(region.getAttribute("aria-label")).toBe("Draft");
    expect(region.tabIndex).toBe(0);
    region.focus();
    // Bare R on the draft region (and on the body) retries.
    await key("r", {}, region);
    expect(sent.map((c) => c.cmd)).toEqual(["overlay_retry"]);
    await act(() => emit(DOUBLETAP_OVERLAY_EVENT, { ...EMPTY_VIEW, phase: "reading", can_retry: true }));
    await act(() => emit(DOUBLETAP_OVERLAY_EVENT, ready));
    await flush(2);
    await key("R", { code: "KeyR" }, document.body);
    expect(sent.map((c) => c.cmd)).toEqual(["overlay_retry", "overlay_retry"]);
    await act(() => emit(DOUBLETAP_OVERLAY_EVENT, { ...EMPTY_VIEW, phase: "reading", can_retry: true }));
    await act(() => emit(DOUBLETAP_OVERLAY_EVENT, ready));
    await flush(2);
    const again = container.querySelector<HTMLElement>(".overlay-text")!;
    again.focus();
    await key("Tab", {}, again);
    expect(container.querySelector("textarea")).not.toBeNull();
  });

  it("asks the shell to fit the window to the content, within limits, ignoring failures", async () => {
    const heights: number[] = [];
    setup(ready, {
      overlay_resize: (args: unknown) => {
        heights.push((args as { height: number }).height);
        return (args as { height: number }).height;
      },
    });
    const rect = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({ height: 180 } as DOMRect);
    const { container } = await render(<Overlay />);
    await flush(5);
    await act(() => new Promise((r) => requestAnimationFrame(() => r(null))));
    await flush(2);
    expect(heights.at(-1)).toBe(180);
    // A long draft: the overflow is added, capped at the maximum.
    const text = container.querySelector<HTMLElement>(".overlay-text")!;
    Object.defineProperty(text, "scrollHeight", { configurable: true, value: 2000 });
    await act(() => emit(DOUBLETAP_OVERLAY_EVENT, { ...ready, text: "long ".repeat(400) }));
    await act(() => new Promise((r) => requestAnimationFrame(() => r(null))));
    await flush(2);
    expect(heights.at(-1)).toBe(OVERLAY_MAX_HEIGHT);
    // Small changes (under 4 px) aren't sent.
    const n = heights.length;
    rect.mockReturnValue({ height: 30 } as DOMRect);
    Object.defineProperty(text, "scrollHeight", { configurable: true, value: 0 });
    await act(() => emit(DOUBLETAP_OVERLAY_EVENT, { ...ready, text: "short" }));
    await act(() => new Promise((r) => requestAnimationFrame(() => r(null))));
    await flush(2);
    expect(heights.length).toBe(n + 1);
    expect(heights.at(-1)).toBe(OVERLAY_MIN_HEIGHT);
    rect.mockRestore();
  });

  it("has no axe violations with every control on screen", async () => {
    setup(ready, { doubletap_history: HISTORY });
    const { container } = await render(<Overlay />);
    await flush(5);
    await key("h");
    let result = await axe.run(container, { rules: { "color-contrast": { enabled: false } } });
    expect(result.violations.map((v) => v.id)).toEqual([]);
    await key("Escape");
    await key("Tab");
    await type(container.querySelector("textarea")!, "Edited");
    result = await axe.run(container, { rules: { "color-contrast": { enabled: false } } });
    expect(result.violations.map((v) => v.id)).toEqual([]);
  });
  it("has one heading naming the window, for heading navigation (LL2)", async () => {
    setup(ready);
    const { container } = await render(<Overlay />);
    await flush(5);
    const result = await axe.run(document.documentElement, { runOnly: { type: "rule", values: ["page-has-heading-one"] } });
    expect(result.violations).toEqual([]);
    expect(Array.from(container.querySelectorAll("h1")).map((h) => h.textContent)).toEqual(["Double Tap"]);
  });

  it("says when a refinement missed after its second try, with the draft", async () => {
    setup({ ...streaming, phase: "done", can_retry: true, message: "doubletap.overlay.refineMissed" });
    const { container } = await render(<Overlay />);
    await flush(5);
    expect(container.textContent).toContain("Couldn't make that change this time.");
    expect(container.textContent).toContain("Thanks Ada, Thursday works.");
    expect(container.querySelector('[role="alert"]')).toBeNull();
  });
});
